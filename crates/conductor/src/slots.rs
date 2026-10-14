//! Global concurrency slots: `concurrency.max_sessions` in all and `max_per_agent` per agent,
//! as lock files under `~/.argonauts/slots/`, so every app and run host shares the same count.
//!
//! Each slot is a numbered file held with `File::try_lock`. The OS drops the lock when the file
//! closes, which also happens when the process dies, and Rust opens files close-on-exec, so an
//! agent a run host started never inherits one.

use std::fs::{File, OpenOptions, TryLockError};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use argonauts_core::Paths;
use argonauts_core::create_private_dir_all;
use argonauts_core::names::AgentId;
use argonauts_core::settings::sessions::ConcurrencySettings;

/// How often a waiting [`Slots::acquire`] tries again. Slots free up in other processes, so
/// there's nothing local to wait on.
const RETRY_EVERY: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SlotError {
    #[error("concurrency slots: {0}")]
    Io(String),
}

#[derive(Debug, Clone)]
pub struct Slots {
    dir: PathBuf,
    max_sessions: u32,
    max_per_agent: u32,
}

/// A held slot. The lock is released when it's dropped, and also when the process dies, so a
/// crashed run host never keeps one.
#[derive(Debug)]
pub struct SlotGuard {
    path: PathBuf,
    _lock: File,
    agent_path: PathBuf,
    _agent_lock: File,
}

impl SlotGuard {
    /// The global slot's file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The per-agent slot's file.
    pub fn agent_path(&self) -> &Path {
        &self.agent_path
    }
}

impl Slots {
    pub fn new(paths: &Paths, settings: &ConcurrencySettings) -> Self {
        Self {
            dir: paths.home().join("slots"),
            max_sessions: settings.max_sessions,
            max_per_agent: settings.max_per_agent,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Waits until both a global and a per-agent slot are free, then holds them.
    pub async fn acquire(&self, agent_id: &AgentId) -> Result<SlotGuard, SlotError> {
        loop {
            if let Some(guard) = self.try_acquire(agent_id)? {
                return Ok(guard);
            }
            tokio::time::sleep(RETRY_EVERY).await;
        }
    }

    /// `None` when every slot is taken.
    pub fn try_acquire(&self, agent_id: &AgentId) -> Result<Option<SlotGuard>, SlotError> {
        create_private_dir_all(&self.dir).map_err(|err| self.error(&self.dir, err))?;
        // A cap of 0 reads as 1, so a run can always make progress.
        let Some((path, lock)) = self.take_one("all", self.max_sessions.max(1))? else {
            return Ok(None);
        };
        let agent_name = file_safe(&agent_id.0);
        let Some((agent_path, agent_lock)) =
            self.take_one(&agent_name, self.max_per_agent.max(1))?
        else {
            return Ok(None);
        };
        Ok(Some(SlotGuard {
            path,
            _lock: lock,
            agent_path,
            _agent_lock: agent_lock,
        }))
    }

    /// The first free slot of `count` named `<name>-<n>.lock`.
    fn take_one(&self, name: &str, count: u32) -> Result<Option<(PathBuf, File)>, SlotError> {
        for n in 0..count {
            let path = self.dir.join(format!("{name}-{n}.lock"));
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(&path)
                .map_err(|err| self.error(&path, err))?;
            match file.try_lock() {
                Ok(()) => return Ok(Some((path, file))),
                Err(TryLockError::WouldBlock) => continue,
                Err(TryLockError::Error(err)) => return Err(self.error(&path, err)),
            }
        }
        Ok(None)
    }

    fn error(&self, path: &Path, err: std::io::Error) -> SlotError {
        SlotError::Io(format!("{}: {err}", path.display()))
    }
}

/// An agent id as a file name: anything but letters, digits, `.`, `_`, and `-` becomes `_`.
fn file_safe(id: &str) -> String {
    let name: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    // "all" names the global slots, and a leading dot would hide the file.
    if name.is_empty() || name == "all" || name.starts_with('.') {
        format!("agent-{name}")
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn agent_id(id: &str) -> AgentId {
        AgentId(id.into())
    }

    /// Another test's fork copies every open file for a moment, and with it our locks, so a slot
    /// released a moment ago may take a few tries to get back.
    async fn take(slots: &Slots, id: &str) -> SlotGuard {
        tokio::time::timeout(Duration::from_secs(3), slots.acquire(&agent_id(id)))
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn slots_cap_sessions_and_agents() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let settings = ConcurrencySettings {
            max_sessions: 3,
            max_per_agent: 2,
        };
        // Two boards stand for two processes: each opens its own files, so their locks conflict.
        let app = Slots::new(&paths, &settings);
        let run_host = Slots::new(&paths, &settings);
        assert_eq!(app.dir(), home.path().join("slots"));

        let codex_1 = take(&app, "codex").await;
        let codex_2 = take(&run_host, "codex").await;
        assert_ne!(codex_1.path(), codex_2.path());
        assert_ne!(codex_1.agent_path(), codex_2.agent_path());
        // The agent cap is reached, and the global slot it tried is given back.
        assert!(app.try_acquire(&agent_id("codex")).unwrap().is_none());
        let claude = take(&run_host, "claude-code").await;
        // The global cap is reached.
        assert!(
            app.try_acquire(&agent_id("antigravity-cli"))
                .unwrap()
                .is_none()
        );

        drop(codex_1);
        let codex_3 = take(&app, "codex").await;

        // A waiting acquire gets the slot once it frees.
        let waiter = {
            let app = app.clone();
            tokio::spawn(async move { app.acquire(&agent_id("claude-code")).await })
        };
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(!waiter.is_finished());
        drop(claude);
        let started = Instant::now();
        let claude_2 = waiter.await.unwrap().unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(claude_2.agent_path().ends_with("claude-code-0.lock"));
        drop((codex_2, codex_3, claude_2));

        // A cap of 0 still lets one session run.
        let tight = Slots::new(
            &paths,
            &ConcurrencySettings {
                max_sessions: 0,
                max_per_agent: 0,
            },
        );
        let only = take(&tight, "codex").await;
        assert!(tight.try_acquire(&agent_id("codex")).unwrap().is_none());
        drop(only);
    }

    #[test]
    fn a_dead_process_holds_no_slot() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let settings = ConcurrencySettings {
            max_sessions: 1,
            max_per_agent: 1,
        };
        let slots = Slots::new(&paths, &settings);
        slots.try_acquire(&agent_id("codex")).unwrap();

        // Another process takes the only global slot, then is killed.
        let locker = "use Fcntl ':flock'; open(my $f, '>>', $ARGV[0]) or die; \
                      flock($f, LOCK_EX) or die; $| = 1; print \"held\\n\"; sleep 30";
        let Ok(mut child) = std::process::Command::new("perl")
            .args(["-e", locker])
            .arg(slots.dir().join("all-0.lock"))
            .stdout(std::process::Stdio::piped())
            .spawn()
        else {
            return;
        };
        let mut line = String::new();
        let stdout = child.stdout.take().unwrap();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(stdout), &mut line).unwrap();
        assert_eq!(line.trim(), "held");
        assert!(slots.try_acquire(&agent_id("codex")).unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while slots.try_acquire(&agent_id("codex")).unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "the killed process still holds its slot"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn agent_names_are_file_safe() {
        assert_eq!(file_safe("codex"), "codex");
        assert_eq!(file_safe("opencode/llama"), "opencode_llama");
        assert_eq!(file_safe("../x"), "agent-.._x");
        assert_eq!(file_safe("all"), "agent-all");
        assert_eq!(file_safe(""), "agent-");
    }
}
