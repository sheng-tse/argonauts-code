//! Long-lived vendor sessions for roles with lifetime `project`, one per role and worker. A file
//! lock lease keeps two supervisors from resuming one at once, and a crash never leaves it taken.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::layout::repo_state;
use crate::names::{RoleName, WorkerRef};
use crate::paths::{create_private_dir_all, write_private_file};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MainSession {
    pub role: RoleName,
    /// A role moving to another vendor starts over.
    pub worker: WorkerRef,
    pub vendor_session: String,
    #[serde(with = "time::serde::rfc3339")]
    pub started: OffsetDateTime,
    pub runs: u32,
}

pub struct MainSessions {
    dir: PathBuf,
}

/// Released on drop or when the process exits.
#[derive(Debug)]
pub struct MainSessionLease {
    _lock: File,
    role: RoleName,
    worker: WorkerRef,
}

impl MainSessionLease {
    pub fn role(&self) -> RoleName {
        self.role
    }

    pub fn worker(&self) -> &WorkerRef {
        &self.worker
    }
}

/// Keeps temp file names unique within one process; the pid covers the rest.
static PARTIAL_COUNTER: AtomicU64 = AtomicU64::new(0);

impl MainSessions {
    pub fn open(state_dir: &Path) -> Self {
        Self {
            dir: state_dir.join(repo_state::MAIN_SESSIONS),
        }
    }

    /// `<role>--<worker>`, with the worker percent-encoded so every worker gets its own name.
    fn stem(role: RoleName, worker: &WorkerRef) -> String {
        let mut stem = format!("{}--", role.as_str());
        for byte in worker.to_string().bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
                stem.push(byte as char);
            } else {
                stem.push_str(&format!("%{byte:02X}"));
            }
        }
        stem
    }

    fn file(&self, role: RoleName, worker: &WorkerRef) -> PathBuf {
        self.dir.join(format!("{}.json", Self::stem(role, worker)))
    }

    fn lock_file(&self, role: RoleName, worker: &WorkerRef) -> PathBuf {
        self.dir.join(format!("{}.lock", Self::stem(role, worker)))
    }

    /// An unreadable file counts as none, so the next run starts a fresh session instead of
    /// failing.
    pub fn get(&self, role: RoleName, worker: &WorkerRef) -> Option<MainSession> {
        let text = fs::read_to_string(self.file(role, worker)).ok()?;
        let session: MainSession = serde_json::from_str(&text).ok()?;
        (session.role == role && &session.worker == worker).then_some(session)
    }

    /// Call it while holding the lease.
    pub fn put(&self, session: &MainSession) -> io::Result<()> {
        create_private_dir_all(&self.dir)?;
        let mut json = serde_json::to_vec_pretty(session).map_err(io::Error::other)?;
        json.push(b'\n');
        // Rename a uniquely named sibling into place, so a crash never leaves half a file and
        // two writers never share a temp file.
        let path = self.file(session.role, &session.worker);
        let partial = path.with_extension(format!(
            "json.{}.{}.partial",
            std::process::id(),
            PARTIAL_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        write_private_file(&partial, &json)?;
        fs::rename(&partial, &path)
    }

    pub fn clear(&self, role: RoleName, worker: &WorkerRef) -> io::Result<()> {
        match fs::remove_file(self.file(role, worker)) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        }
    }

    /// `None` when another task or process holds it; that work then runs in an ordinary session.
    pub fn lease(
        &self,
        role: RoleName,
        worker: &WorkerRef,
    ) -> io::Result<Option<MainSessionLease>> {
        create_private_dir_all(&self.dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(self.lock_file(role, worker))?;
        match lock.try_lock() {
            Ok(()) => Ok(Some(MainSessionLease {
                _lock: lock,
                role,
                worker: worker.clone(),
            })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(err)) => Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use time::macros::datetime;

    fn session(role: RoleName, worker: &str) -> MainSession {
        MainSession {
            role,
            worker: worker.parse().unwrap(),
            vendor_session: "vendor-1".into(),
            started: datetime!(2026-09-29 10:00 UTC),
            runs: 2,
        }
    }

    #[test]
    fn one_session_per_role_and_worker() {
        let dir = tempfile::tempdir().unwrap();
        let store = MainSessions::open(dir.path());
        let claude: WorkerRef = "claude-code/opus".parse().unwrap();
        let codex: WorkerRef = "codex".parse().unwrap();
        assert!(store.get(RoleName::Reviewer, &claude).is_none());

        let reviewer = session(RoleName::Reviewer, "claude-code/opus");
        store.put(&reviewer).unwrap();
        let other = session(RoleName::Reviewer, "codex");
        store.put(&other).unwrap();
        assert_eq!(
            store.get(RoleName::Reviewer, &claude),
            Some(reviewer.clone())
        );
        assert_eq!(
            store.get(RoleName::Reviewer, &codex),
            Some(other),
            "two workers in one role keep their own sessions"
        );
        assert!(store.get(RoleName::Owner, &claude).is_none());

        let file = dir
            .path()
            .join("sessions")
            .join("reviewer--claude-code%2Fopus.json");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        store.clear(RoleName::Reviewer, &codex).unwrap();
        assert!(store.get(RoleName::Reviewer, &claude).is_some());
        store.clear(RoleName::Reviewer, &claude).unwrap();
        assert!(store.get(RoleName::Reviewer, &claude).is_none());
        store.clear(RoleName::Reviewer, &claude).unwrap();
    }

    #[test]
    fn a_second_put_replaces_and_a_broken_file_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = MainSessions::open(dir.path());
        let worker: WorkerRef = "codex".parse().unwrap();
        let mut owner = session(RoleName::Owner, "codex");
        store.put(&owner).unwrap();
        owner.runs = 3;
        store.put(&owner).unwrap();
        assert_eq!(store.get(RoleName::Owner, &worker).unwrap().runs, 3);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("sessions"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".partial"))
            .collect();
        assert!(leftovers.is_empty(), "temp files are renamed away");

        std::fs::write(
            dir.path().join("sessions").join("owner--codex.json"),
            "{ torn",
        )
        .unwrap();
        assert!(store.get(RoleName::Owner, &worker).is_none());
    }

    // The lock belongs to the open file, so two opens conflict even inside one process.
    #[test]
    fn main_session_lease_exclusive_across_processes() {
        let dir = tempfile::tempdir().unwrap();
        let first = MainSessions::open(dir.path());
        let second = MainSessions::open(dir.path());
        let worker: WorkerRef = "claude-code/opus".parse().unwrap();

        let lease = first.lease(RoleName::Owner, &worker).unwrap().unwrap();
        assert_eq!(lease.role(), RoleName::Owner);
        assert!(second.lease(RoleName::Owner, &worker).unwrap().is_none());
        assert!(
            second.lease(RoleName::Reviewer, &worker).unwrap().is_some(),
            "another role is free"
        );
        drop(lease);
        assert!(second.lease(RoleName::Owner, &worker).unwrap().is_some());
    }
}
