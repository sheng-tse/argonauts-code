//! `~/.argonauts/vendors.json`: quota and health for every vendor, shared by the app and every
//! supervisor under a file lock and re-read when it changes. Whatever a process owns there (a
//! breaker's test card, session counts) carries that process's pid and start time, and readers
//! drop what belongs to a process that's gone. The app wakes parked runs from this file alone.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use argonauts_core::limits::{HealthState, QuotaReading, QuotaStatus, VendorHealth};
use argonauts_core::names::VendorId;
use argonauts_core::{Paths, create_private_dir_all};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::health::HealthMark;
use crate::liveness;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VendorsFile {
    /// By vendor id, such as `codex`.
    #[serde(default)]
    pub vendors: BTreeMap<String, VendorEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VendorEntry {
    pub quota: Option<QuotaReading>,
    pub health: Option<VendorHealth>,
    /// When the vendor last left `Down`, which wakes runs waiting for it.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub left_down_at: Option<OffsetDateTime>,
    /// The session running the breaker's test card, while one runs.
    #[serde(default)]
    pub test_card: Option<Owner>,
    /// Sessions running on this vendor, one per live session.
    #[serde(default)]
    pub sessions: Vec<Owner>,
    /// The errors and retries the breaker counts, within its window.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marks: Vec<HealthMark>,
    /// The process that disabled the vendor. The disable lapses with it, so the next process
    /// checks the CLI again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_by: Option<Owner>,
    /// When a quota probe last started, in any process, so processes take turns probing.
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub probed_at: Option<OffsetDateTime>,
}

/// The process that owns an entry. Readers drop entries whose process is gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    pub pid: u32,
    pub start_time: u64,
    pub session: Option<String>,
}

impl Owner {
    /// This process, as other processes will check it.
    pub fn this_process(session: Option<String>) -> io::Result<Self> {
        let (pid, start_time) = liveness::this_process()
            .ok_or_else(|| io::Error::other("could not read this process's start time"))?;
        Ok(Self {
            pid,
            start_time,
            session,
        })
    }

    pub fn is_alive(&self) -> bool {
        liveness::is_same_process(self.pid, self.start_time)
    }
}

impl VendorEntry {
    pub fn state(&self) -> HealthState {
        self.health
            .as_ref()
            .map_or(HealthState::Healthy, |health| health.state)
    }

    /// When the vendor may take work again: its quota reset once the quota ran out, its next
    /// test once it's down, and the later of the two when both hold. `None` when neither holds.
    pub fn back_at(&self) -> Option<OffsetDateTime> {
        let quota_reset = self
            .quota
            .as_ref()
            .filter(|quota| quota.status == QuotaStatus::Rejected)
            .and_then(|quota| quota.resets_at);
        let next_test = self
            .health
            .as_ref()
            .filter(|health| health.state == HealthState::Down)
            .and_then(|health| health.next_test_at);
        quota_reset.max(next_test)
    }

    /// The reset this vendor reported, or when its breaker tests it again.
    fn reported_reset(&self) -> Option<OffsetDateTime> {
        let quota_reset = self.quota.as_ref().and_then(|quota| quota.resets_at);
        let next_test = self
            .health
            .as_ref()
            .filter(|health| health.state == HealthState::Down)
            .and_then(|health| health.next_test_at);
        match (quota_reset, next_test) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Drops what belongs to a process that's gone. Says whether anything was dropped.
    fn drop_dead_owners(&mut self, alive: &mut impl FnMut(&Owner) -> bool) -> bool {
        let before = (self.sessions.len(), self.test_card.is_some());
        self.sessions.retain(|owner| alive(owner));
        if self.test_card.as_ref().is_some_and(|owner| !alive(owner)) {
            self.test_card = None;
        }
        let mut changed = before != (self.sessions.len(), self.test_card.is_some());
        if self.disabled_by.as_ref().is_some_and(|owner| !alive(owner)) {
            self.disabled_by = None;
            if let Some(health) = &mut self.health {
                health.disabled = None;
            }
            changed = true;
        }
        changed
    }
}

impl VendorsFile {
    /// The earliest reset any vendor reported, for `AllVendorsOut { until }`. A vendor that's
    /// down reports its next test.
    pub fn earliest_reset(&self) -> Option<OffsetDateTime> {
        self.vendors
            .values()
            .filter_map(VendorEntry::reported_reset)
            .min()
    }

    pub fn entry(&self, vendor: &VendorId) -> Option<&VendorEntry> {
        self.vendors.get(&vendor.0)
    }

    /// Whether a run parked at `parked_at` waiting for `vendor` should wake at `now`: the vendor
    /// left `Down` since, or its quota reset or next test has come. The app decides from this
    /// file alone, so a test that's due counts even when no supervisor has marked it yet.
    pub fn vendor_back(
        &self,
        vendor: &VendorId,
        parked_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> bool {
        let Some(entry) = self.entry(vendor) else {
            return false;
        };
        entry.left_down_at.is_some_and(|left| left > parked_at)
            || entry.back_at().is_some_and(|back| back <= now)
    }

    /// Whether a run parked for `WaitingForVendor { vendor, until }` should wake at `now`: its
    /// `until` has passed, or [`VendorsFile::vendor_back`] says so.
    pub fn wait_over(
        &self,
        vendor: &VendorId,
        until: Option<OffsetDateTime>,
        parked_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> bool {
        until.is_some_and(|until| until <= now) || self.vendor_back(vendor, parked_at, now)
    }

    fn drop_dead_owners(&mut self) -> bool {
        // Many sessions share a process, so each process is looked up once.
        let mut known = BTreeMap::new();
        let mut alive = |owner: &Owner| {
            *known
                .entry((owner.pid, owner.start_time))
                .or_insert_with(|| owner.is_alive())
        };
        let mut changed = false;
        for entry in self.vendors.values_mut() {
            changed |= entry.drop_dead_owners(&mut alive);
        }
        changed
    }
}

/// Tells one version of the file from another. Every write renames a new file into place, so
/// the inode changes along with the times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl Stamp {
    fn of(meta: &fs::Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mtime: (meta.mtime(), meta.mtime_nsec()),
            ctime: (meta.ctime(), meta.ctime_nsec()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct VendorsStore {
    path: PathBuf,
    /// The last version parsed, so a reader re-parses only when the file changed.
    cache: Arc<Mutex<Option<(Stamp, VendorsFile)>>>,
}

impl VendorsStore {
    pub fn new(paths: &Paths) -> Self {
        Self {
            path: paths
                .home()
                .join(argonauts_core::layout::user_files::VENDORS),
            cache: Arc::default(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock_path(&self) -> PathBuf {
        self.path.with_extension("json.lock")
    }

    /// The file as it is now, with entries of dead processes dropped. A missing file is empty.
    pub fn read(&self) -> io::Result<VendorsFile> {
        let mut file = self.read_raw()?;
        file.drop_dead_owners();
        Ok(file)
    }

    /// Reads, changes, and writes the file under its lock, atomically.
    pub fn update<T>(&self, change: impl FnOnce(&mut VendorsFile) -> T) -> io::Result<T> {
        let home = self.path.parent().unwrap_or(Path::new("."));
        create_private_dir_all(home)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(self.lock_path())?;
        // Held until `lock` drops at the end of this call, or the process dies.
        lock.lock()?;
        let before = match self.read_raw() {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::InvalidData => {
                tracing::warn!("{err}; starting it again empty");
                VendorsFile::default()
            }
            Err(err) => return Err(err),
        };
        let mut file = before.clone();
        file.drop_dead_owners();
        let result = change(&mut file);
        if file != before {
            self.write(&file)?;
        }
        drop(lock);
        Ok(result)
    }

    /// Adds `session` of this process to the vendor's live sessions.
    pub fn add_session(&self, vendor: &VendorId, session: &str) -> io::Result<()> {
        let owner = Owner::this_process(Some(session.to_owned()))?;
        self.update(|file| {
            let entry = file.vendors.entry(vendor.0.clone()).or_default();
            if !entry.sessions.contains(&owner) {
                entry.sessions.push(owner);
            }
        })
    }

    pub fn remove_session(&self, vendor: &VendorId, session: &str) -> io::Result<()> {
        let owner = Owner::this_process(Some(session.to_owned()))?;
        self.update(|file| {
            if let Some(entry) = file.vendors.get_mut(&vendor.0) {
                entry.sessions.retain(|s| *s != owner);
            }
        })
    }

    fn read_raw(&self) -> io::Result<VendorsFile> {
        // Stat before reading: if a writer renames in between, the stamp is the older one and
        // the next read parses again.
        let stamp = match fs::metadata(&self.path) {
            Ok(meta) => Stamp::of(&meta),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Ok(VendorsFile::default());
            }
            Err(err) => return Err(err),
        };
        if let Some((cached, file)) = &*self.cache.lock().map_err(poisoned)?
            && *cached == stamp
        {
            return Ok(file.clone());
        }
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Ok(VendorsFile::default());
            }
            Err(err) => return Err(err),
        };
        let file: VendorsFile = serde_json::from_slice(&bytes).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is not valid: {err}", self.path.display()),
            )
        })?;
        *self.cache.lock().map_err(poisoned)? = Some((stamp, file.clone()));
        Ok(file)
    }

    /// Writes a private temp file beside the target, syncs it, and renames it into place, so a
    /// reader sees the old file or the new one and never half of one.
    fn write(&self, file: &VendorsFile) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(file).map_err(io::Error::other)?;
        let temp = self
            .path
            .with_extension(format!("json.{}.tmp", std::process::id()));
        let written = write_synced(&temp, &json).and_then(|()| fs::rename(&temp, &self.path));
        if let Err(err) = written {
            let _ = fs::remove_file(&temp);
            return Err(err);
        }
        let stamp = Stamp::of(&fs::metadata(&self.path)?);
        *self.cache.lock().map_err(poisoned)? = Some((stamp, file.clone()));
        Ok(())
    }
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut out: File = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    out.write_all(bytes)?;
    // A plain fsync. `sync_all` is F_FULLFSYNC on macOS, which flushes the whole drive's cache
    // for every quota reading and health mark; losing the last of those in a power cut is fine.
    nix::unistd::fsync(&out).map_err(io::Error::from)
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> io::Error {
    io::Error::other("the vendors.json cache is poisoned")
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::limits::QuotaSource;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::thread;
    use time::Duration;

    fn dead_owner() -> Owner {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let pid = child.id();
        let start_time = liveness::start_time(pid).unwrap_or(1);
        child.wait().unwrap();
        Owner {
            pid,
            start_time,
            session: Some("s-dead".into()),
        }
    }

    fn quota(vendor: &str, status: QuotaStatus, resets_at: Option<OffsetDateTime>) -> QuotaReading {
        QuotaReading {
            vendor: VendorId(vendor.into()),
            status,
            used: Some(0.5),
            resets_at,
            window: Some("five_hour".into()),
            source: QuotaSource::RateLimitEvent,
            observed_at: OffsetDateTime::now_utc(),
            detail: None,
        }
    }

    #[test]
    fn vendors_file_drops_dead_owners() {
        let home = tempfile::tempdir().unwrap();
        let store = VendorsStore::new(&Paths::new(home.path()));
        assert_eq!(store.read().unwrap(), VendorsFile::default());

        let alive = Owner::this_process(Some("s-001".into())).unwrap();
        let reused = Owner {
            start_time: alive.start_time - 3,
            session: Some("s-reused".into()),
            ..alive.clone()
        };
        let dead = dead_owner();
        let mut written = VendorsFile::default();
        let health = VendorHealth {
            vendor: VendorId("codex".into()),
            state: HealthState::Testing,
            since: OffsetDateTime::now_utc(),
            reason: None,
            next_test_at: None,
            disabled: Some("codex 9.9 is untested".into()),
        };
        written.vendors.insert(
            "codex".into(),
            VendorEntry {
                health: Some(health),
                test_card: Some(dead.clone()),
                sessions: vec![alive.clone(), dead.clone(), reused],
                disabled_by: Some(dead),
                ..VendorEntry::default()
            },
        );
        fs::write(store.path(), serde_json::to_vec(&written).unwrap()).unwrap();

        let read = store.read().unwrap();
        let codex = &read.vendors["codex"];
        assert_eq!(codex.sessions, vec![alive.clone()]);
        assert_eq!(codex.test_card, None);
        assert_eq!(codex.disabled_by, None);
        assert_eq!(codex.health.as_ref().unwrap().disabled, None);
        assert_eq!(codex.state(), HealthState::Testing);

        // An update writes the pruned file back.
        store.update(|_| ()).unwrap();
        let on_disk: VendorsFile =
            serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
        assert_eq!(on_disk.vendors["codex"].sessions, vec![alive]);

        // Sessions of this process come and go.
        let claude = VendorId("claude-code".into());
        store.add_session(&claude, "s-002").unwrap();
        store.add_session(&claude, "s-002").unwrap();
        assert_eq!(
            store.read().unwrap().vendors["claude-code"].sessions.len(),
            1
        );
        store.remove_session(&claude, "s-002").unwrap();
        assert!(
            store.read().unwrap().vendors["claude-code"]
                .sessions
                .is_empty()
        );
    }

    #[test]
    fn vendors_file_update_is_locked_and_atomic() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path().join("fresh"));
        let owner = Owner::this_process(None).unwrap();
        let writers: Vec<_> = (0..6)
            .map(|w| {
                // Each thread has its own store, like a separate process: its own lock file
                // handle and its own cache.
                let store = VendorsStore::new(&paths);
                let owner = owner.clone();
                thread::spawn(move || {
                    for n in 0..15 {
                        store
                            .update(|file| {
                                let entry = file.vendors.entry("codex".into()).or_default();
                                entry.sessions.push(Owner {
                                    session: Some(format!("w{w}-{n}")),
                                    ..owner.clone()
                                });
                            })
                            .unwrap();
                    }
                })
            })
            .collect();
        let reader = {
            let store = VendorsStore::new(&paths);
            thread::spawn(move || {
                let mut reads = 0;
                while reads < 300 {
                    // Every read parses: the rename never exposes half a file.
                    let file = store.read().unwrap();
                    assert!(
                        file.vendors
                            .get("codex")
                            .is_none_or(|e| e.sessions.len() <= 90)
                    );
                    reads += 1;
                }
            })
        };
        for writer in writers {
            writer.join().unwrap();
        }
        reader.join().unwrap();

        let store = VendorsStore::new(&paths);
        let file = store.read().unwrap();
        assert_eq!(
            file.vendors["codex"].sessions.len(),
            90,
            "no update was lost"
        );
        let mode = fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let leftovers: Vec<_> = fs::read_dir(paths.home())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert_eq!(leftovers, Vec::<String>::new());

        // A broken file is started again rather than blocking every board.
        fs::write(store.path(), "{ not json").unwrap();
        assert_eq!(store.read().unwrap_err().kind(), io::ErrorKind::InvalidData);
        store
            .update(|file| file.vendors.entry("agy".into()).or_default().left_down_at = None)
            .unwrap();
        assert!(store.read().unwrap().vendors.contains_key("agy"));
    }

    #[test]
    fn a_change_elsewhere_is_read_again() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let app = VendorsStore::new(&paths);
        let supervisor = VendorsStore::new(&paths);
        assert!(app.read().unwrap().vendors.is_empty());
        supervisor
            .update(|file| {
                file.vendors.entry("codex".into()).or_default().quota =
                    Some(quota("codex", QuotaStatus::Allowed, None))
            })
            .unwrap();
        assert!(app.read().unwrap().vendors["codex"].quota.is_some());
    }

    #[test]
    fn earliest_reset_and_wake_checks() {
        let now = OffsetDateTime::now_utc();
        let later = now + Duration::hours(2);
        let soon = now + Duration::minutes(10);
        let mut file = VendorsFile::default();
        assert_eq!(file.earliest_reset(), None);
        file.vendors.insert(
            "claude-code".into(),
            VendorEntry {
                quota: Some(quota("claude-code", QuotaStatus::Rejected, Some(later))),
                ..VendorEntry::default()
            },
        );
        let down = VendorHealth {
            vendor: VendorId("codex".into()),
            state: HealthState::Down,
            since: now,
            reason: None,
            next_test_at: Some(soon),
            disabled: None,
        };
        file.vendors.insert(
            "codex".into(),
            VendorEntry {
                health: Some(down),
                ..VendorEntry::default()
            },
        );
        assert_eq!(file.earliest_reset(), Some(soon));
        let claude = VendorId("claude-code".into());
        let codex = VendorId("codex".into());
        assert_eq!(file.entry(&claude).unwrap().back_at(), Some(later));
        assert!(!file.vendor_back(&claude, now, now));
        assert!(file.vendor_back(&claude, now, later));
        assert!(!file.vendor_back(&codex, now, now));
        assert!(file.vendor_back(&codex, now, soon));
        assert!(!file.vendor_back(&VendorId("agy".into()), now, later));

        let codex_entry = file.vendors.get_mut("codex").unwrap();
        codex_entry.health.as_mut().unwrap().state = HealthState::Testing;
        codex_entry.left_down_at = Some(now + Duration::seconds(1));
        assert!(file.vendor_back(&codex, now, now));
        assert!(!file.vendor_back(&codex, now + Duration::seconds(5), now));
    }
}
