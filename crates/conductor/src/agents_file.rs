//! `~/.argonauts/agents.json`: plan usage and health for every agent, shared by the app and every
//! run host under a file lock and re-read when it changes. Whatever a process owns there (a
//! breaker's test task, session counts) carries that process's pid and start time, and readers
//! drop what belongs to a process that's gone. The app wakes waiting runs from this file alone.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use argonauts_core::limits::{
    AgentHealth, HealthState, PlanUsageReading, PlanUsageStatus, PlanWindows,
};
use argonauts_core::names::AgentId;
use argonauts_core::plan_usage::LiveUsage;
use argonauts_core::{Paths, create_private_dir_all};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::health::HealthMark;
use crate::liveness;
use crate::model_health::ModelRecord;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentsFile {
    /// By agent id, such as `codex`.
    #[serde(default)]
    pub agent_ids: BTreeMap<String, AgentRecord>,
}

/// The accounts whose live readings are kept per CLI, so signing back in to one shows its
/// numbers without the file growing for every account ever used.
const LIVE_ACCOUNTS_KEPT: usize = 4;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentRecord {
    /// The window that binds soonest. It's what a file from before windows holds alone, and
    /// what one number per agent shows.
    pub plan_usage: Option<PlanUsageReading>,
    /// The newest reading of each window the agent reports.
    #[serde(default, skip_serializing_if = "PlanWindows::is_empty")]
    pub windows: PlanWindows,
    /// The account's plan, as the CLI names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// The newest live reading of each account the CLI was signed in as, the account signed
    /// in last at the end.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub live: Vec<LiveUsage>,
    pub health: Option<AgentHealth>,
    /// When the agent last left `Down`, which wakes runs waiting for it.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub left_down_at: Option<OffsetDateTime>,
    /// The session running the breaker's test task, while one runs.
    #[serde(default)]
    pub test_task: Option<Holder>,
    /// Sessions running on this agent, one per live session.
    #[serde(default)]
    pub sessions: Vec<Holder>,
    /// The errors and retries the breaker counts, within its window.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marks: Vec<HealthMark>,
    /// Each model's own breaker, by its name (empty for the agent's default), once anything
    /// was held against it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<String, ModelRecord>,
    /// The process that disabled the agent. The disable lapses with it, so the next process
    /// checks the CLI again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_by: Option<Holder>,
    /// When a plan usage probe last started, in any process, so processes take turns probing.
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub probed_at: Option<OffsetDateTime>,
}

/// The process that holds an entry. Readers drop entries whose process is gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    pub pid: u32,
    pub start_time: u64,
    pub session: Option<String>,
}

impl Holder {
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

impl AgentRecord {
    /// Keeps the reading for its window, unless that window has a newer one, and `plan_usage`
    /// follows the window that binds soonest. Says whether it was kept.
    pub fn observe_plan_usage(&mut self, reading: PlanUsageReading) -> bool {
        if self.windows.is_empty()
            && let Some(old) = self.plan_usage.take()
        {
            self.windows.put(old);
        }
        let kept = self.windows.observe(reading);
        self.plan_usage = self.windows.binding().cloned();
        kept
    }

    /// Keeps `reading` as its account's, unless that account has a newer one. A reading of
    /// another account than the last means the CLI was signed in again: the windows read
    /// before, which name no account, are dropped with it, so they never speak for the new
    /// one. Says whether it was kept.
    pub fn observe_live(&mut self, reading: LiveUsage) -> bool {
        if self
            .live
            .iter()
            .any(|known| known.account == reading.account && known.read_at > reading.read_at)
        {
            return false;
        }
        self.live.retain(|known| known.account != reading.account);
        // Read before the account signed in last, as another process may report late: it
        // is kept for its account, and the last stays the one signed in.
        if let Some(last) = self.live.last()
            && last.read_at > reading.read_at
        {
            let at = self.live.len() - 1;
            self.live.insert(at, reading);
            self.keep_few_accounts();
            return true;
        }
        let switched = self
            .live
            .last()
            .is_some_and(|last| last.account != reading.account);
        if switched {
            self.windows = PlanWindows::default();
            self.plan_usage = None;
            self.plan = None;
        }
        self.live.push(reading);
        self.keep_few_accounts();
        true
    }

    fn keep_few_accounts(&mut self) {
        let extra = self.live.len().saturating_sub(LIVE_ACCOUNTS_KEPT);
        self.live.drain(..extra);
    }

    /// The live reading of the account signed in last.
    pub fn signed_in(&self) -> Option<&LiveUsage> {
        self.live.last()
    }

    /// Every window's newest reading. A file from before windows has its one reading.
    pub fn plan_usage_windows(&self) -> Vec<PlanUsageReading> {
        if self.windows.is_empty() {
            self.plan_usage.iter().cloned().collect()
        } else {
            self.windows.readings().to_vec()
        }
    }

    pub fn state(&self) -> HealthState {
        self.health
            .as_ref()
            .map_or(HealthState::Healthy, |health| health.state)
    }

    /// When the agent may take work again: its plan usage reset once the plan usage ran out, its
    /// next test once it's down, and the later of the two when both hold. `None` when neither
    /// holds.
    pub fn back_at(&self) -> Option<OffsetDateTime> {
        let plan_usage_reset = self
            .plan_usage
            .as_ref()
            .filter(|plan_usage| plan_usage.status == PlanUsageStatus::Rejected)
            .and_then(|plan_usage| plan_usage.resets_at);
        let next_test = self
            .health
            .as_ref()
            .filter(|health| health.state == HealthState::Down)
            .and_then(|health| health.next_test_at);
        plan_usage_reset.max(next_test)
    }

    /// The reset this agent reported, or when its breaker tests it again.
    fn reported_reset(&self) -> Option<OffsetDateTime> {
        let plan_usage_reset = self
            .plan_usage
            .as_ref()
            .and_then(|plan_usage| plan_usage.resets_at);
        let next_test = self
            .health
            .as_ref()
            .filter(|health| health.state == HealthState::Down)
            .and_then(|health| health.next_test_at);
        match (plan_usage_reset, next_test) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Drops what belongs to a process that's gone. Says whether anything was dropped.
    fn drop_dead_holders(&mut self, alive: &mut impl FnMut(&Holder) -> bool) -> bool {
        let before = (self.sessions.len(), self.test_task.is_some());
        self.sessions.retain(|holder| alive(holder));
        if self.test_task.as_ref().is_some_and(|holder| !alive(holder)) {
            self.test_task = None;
        }
        let mut changed = before != (self.sessions.len(), self.test_task.is_some());
        if self
            .disabled_by
            .as_ref()
            .is_some_and(|holder| !alive(holder))
        {
            self.disabled_by = None;
            if let Some(health) = &mut self.health {
                health.disabled = None;
            }
            changed = true;
        }
        changed
    }
}

impl AgentsFile {
    /// The earliest reset any agent reported, for `AllAgentsOut { until }`. An agent that's
    /// down reports its next test.
    pub fn earliest_reset(&self) -> Option<OffsetDateTime> {
        self.agent_ids
            .values()
            .filter_map(AgentRecord::reported_reset)
            .min()
    }

    pub fn entry(&self, agent_id: &AgentId) -> Option<&AgentRecord> {
        self.agent_ids.get(&agent_id.0)
    }

    /// Whether a run that has waited for `agent` since `waiting_at` should wake at `now`: the agent
    /// left `Down` since, or its plan usage reset or next test has come. The app decides from this
    /// file alone, so a test that's due counts even when no run host has marked it yet.
    pub fn agent_back(
        &self,
        agent_id: &AgentId,
        waiting_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> bool {
        let Some(entry) = self.entry(agent_id) else {
            return false;
        };
        entry.left_down_at.is_some_and(|left| left > waiting_at)
            || entry.back_at().is_some_and(|back| back <= now)
    }

    /// Whether a run waiting for `WaitingForAgent { agent, until }` should wake at `now`: its
    /// `until` has passed, or [`AgentsFile::agent_back`] says so.
    pub fn wait_over(
        &self,
        agent_id: &AgentId,
        until: Option<OffsetDateTime>,
        waiting_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> bool {
        until.is_some_and(|until| until <= now) || self.agent_back(agent_id, waiting_at, now)
    }

    fn drop_dead_holders(&mut self) -> bool {
        // Many sessions share a process, so each process is looked up once.
        let mut known = BTreeMap::new();
        let mut alive = |holder: &Holder| {
            *known
                .entry((holder.pid, holder.start_time))
                .or_insert_with(|| holder.is_alive())
        };
        let mut changed = false;
        for entry in self.agent_ids.values_mut() {
            changed |= entry.drop_dead_holders(&mut alive);
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
pub struct AgentsStore {
    path: PathBuf,
    /// The last version parsed, so a reader re-parses only when the file changed.
    cache: Arc<Mutex<Option<(Stamp, AgentsFile)>>>,
}

impl AgentsStore {
    pub fn new(paths: &Paths) -> Self {
        Self {
            path: paths
                .home()
                .join(argonauts_core::layout::user_files::AGENTS_FILE),
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
    pub fn read(&self) -> io::Result<AgentsFile> {
        let mut file = self.read_raw()?;
        file.drop_dead_holders();
        Ok(file)
    }

    /// Reads, changes, and writes the file under its lock, atomically.
    pub fn update<T>(&self, change: impl FnOnce(&mut AgentsFile) -> T) -> io::Result<T> {
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
                AgentsFile::default()
            }
            Err(err) => return Err(err),
        };
        let mut file = before.clone();
        file.drop_dead_holders();
        let result = change(&mut file);
        if file != before {
            self.write(&file)?;
        }
        drop(lock);
        Ok(result)
    }

    /// Adds `session` of this process to the agent's live sessions.
    pub fn add_session(&self, agent_id: &AgentId, session: &str) -> io::Result<()> {
        let holder = Holder::this_process(Some(session.to_owned()))?;
        self.update(|file| {
            let entry = file.agent_ids.entry(agent_id.0.clone()).or_default();
            if !entry.sessions.contains(&holder) {
                entry.sessions.push(holder);
            }
        })
    }

    pub fn remove_session(&self, agent_id: &AgentId, session: &str) -> io::Result<()> {
        let holder = Holder::this_process(Some(session.to_owned()))?;
        self.update(|file| {
            if let Some(entry) = file.agent_ids.get_mut(&agent_id.0) {
                entry.sessions.retain(|s| *s != holder);
            }
        })
    }

    fn read_raw(&self) -> io::Result<AgentsFile> {
        // Stat before reading: if a writer renames in between, the stamp is the older one and
        // the next read parses again.
        let stamp = match fs::metadata(&self.path) {
            Ok(meta) => Stamp::of(&meta),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Ok(AgentsFile::default());
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
                return Ok(AgentsFile::default());
            }
            Err(err) => return Err(err),
        };
        let file: AgentsFile = serde_json::from_slice(&bytes).map_err(|err| {
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
    fn write(&self, file: &AgentsFile) -> io::Result<()> {
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
    // for every plan usage reading and health mark; losing the last of those in a power cut is
    // fine.
    nix::unistd::fsync(&out).map_err(io::Error::from)
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> io::Error {
    io::Error::other("the agents.json cache is poisoned")
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::limits::PlanUsageSource;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::thread;
    use time::Duration;

    fn dead_holder() -> Holder {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let pid = child.id();
        let start_time = liveness::start_time(pid).unwrap_or(1);
        child.wait().unwrap();
        Holder {
            pid,
            start_time,
            session: Some("s-dead".into()),
        }
    }

    fn plan_usage(
        agent_id: &str,
        status: PlanUsageStatus,
        resets_at: Option<OffsetDateTime>,
    ) -> PlanUsageReading {
        PlanUsageReading {
            agent_id: AgentId(agent_id.into()),
            status,
            used: Some(0.5),
            resets_at,
            window: Some("five_hour".into()),
            source: PlanUsageSource::RateLimitEvent,
            observed_at: OffsetDateTime::now_utc(),
            detail: None,
            wrap_up: false,
        }
    }

    #[test]
    fn agents_file_drops_dead_holders() {
        let home = tempfile::tempdir().unwrap();
        let store = AgentsStore::new(&Paths::new(home.path()));
        assert_eq!(store.read().unwrap(), AgentsFile::default());

        let alive = Holder::this_process(Some("s-001".into())).unwrap();
        let reused = Holder {
            start_time: alive.start_time - 3,
            session: Some("s-reused".into()),
            ..alive.clone()
        };
        let dead = dead_holder();
        let mut written = AgentsFile::default();
        let health = AgentHealth {
            agent_id: AgentId("codex".into()),
            state: HealthState::Testing,
            since: OffsetDateTime::now_utc(),
            reason: None,
            next_test_at: None,
            disabled: Some("codex 9.9 is untested".into()),
        };
        written.agent_ids.insert(
            "codex".into(),
            AgentRecord {
                health: Some(health),
                test_task: Some(dead.clone()),
                sessions: vec![alive.clone(), dead.clone(), reused],
                disabled_by: Some(dead),
                ..AgentRecord::default()
            },
        );
        fs::write(store.path(), serde_json::to_vec(&written).unwrap()).unwrap();

        let read = store.read().unwrap();
        let codex = &read.agent_ids["codex"];
        assert_eq!(codex.sessions, vec![alive.clone()]);
        assert_eq!(codex.test_task, None);
        assert_eq!(codex.disabled_by, None);
        assert_eq!(codex.health.as_ref().unwrap().disabled, None);
        assert_eq!(codex.state(), HealthState::Testing);

        // An update writes the pruned file back.
        store.update(|_| ()).unwrap();
        let on_disk: AgentsFile = serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
        assert_eq!(on_disk.agent_ids["codex"].sessions, vec![alive]);

        // Sessions of this process come and go.
        let claude = AgentId("claude-code".into());
        store.add_session(&claude, "s-002").unwrap();
        store.add_session(&claude, "s-002").unwrap();
        assert_eq!(
            store.read().unwrap().agent_ids["claude-code"]
                .sessions
                .len(),
            1
        );
        store.remove_session(&claude, "s-002").unwrap();
        assert!(
            store.read().unwrap().agent_ids["claude-code"]
                .sessions
                .is_empty()
        );
    }

    #[test]
    fn agents_file_update_is_locked_and_atomic() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path().join("fresh"));
        let holder = Holder::this_process(None).unwrap();
        let writers: Vec<_> = (0..6)
            .map(|w| {
                // Each thread has its own store, like a separate process: its own lock file
                // handle and its own cache.
                let store = AgentsStore::new(&paths);
                let holder = holder.clone();
                thread::spawn(move || {
                    for n in 0..15 {
                        store
                            .update(|file| {
                                let entry = file.agent_ids.entry("codex".into()).or_default();
                                entry.sessions.push(Holder {
                                    session: Some(format!("w{w}-{n}")),
                                    ..holder.clone()
                                });
                            })
                            .unwrap();
                    }
                })
            })
            .collect();
        let reader = {
            let store = AgentsStore::new(&paths);
            thread::spawn(move || {
                let mut reads = 0;
                while reads < 300 {
                    // Every read parses: the rename never exposes half a file.
                    let file = store.read().unwrap();
                    assert!(
                        file.agent_ids
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

        let store = AgentsStore::new(&paths);
        let file = store.read().unwrap();
        assert_eq!(
            file.agent_ids["codex"].sessions.len(),
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
            .update(|file| file.agent_ids.entry("agy".into()).or_default().left_down_at = None)
            .unwrap();
        assert!(store.read().unwrap().agent_ids.contains_key("agy"));
    }

    #[test]
    fn a_change_elsewhere_is_read_again() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let app = AgentsStore::new(&paths);
        let run_host = AgentsStore::new(&paths);
        assert!(app.read().unwrap().agent_ids.is_empty());
        run_host
            .update(|file| {
                file.agent_ids.entry("codex".into()).or_default().plan_usage =
                    Some(plan_usage("codex", PlanUsageStatus::Allowed, None))
            })
            .unwrap();
        assert!(app.read().unwrap().agent_ids["codex"].plan_usage.is_some());
    }

    #[test]
    fn earliest_reset_and_wake_checks() {
        let now = OffsetDateTime::now_utc();
        let later = now + Duration::hours(2);
        let soon = now + Duration::minutes(10);
        let mut file = AgentsFile::default();
        assert_eq!(file.earliest_reset(), None);
        file.agent_ids.insert(
            "claude-code".into(),
            AgentRecord {
                plan_usage: Some(plan_usage(
                    "claude-code",
                    PlanUsageStatus::Rejected,
                    Some(later),
                )),
                ..AgentRecord::default()
            },
        );
        let down = AgentHealth {
            agent_id: AgentId("codex".into()),
            state: HealthState::Down,
            since: now,
            reason: None,
            next_test_at: Some(soon),
            disabled: None,
        };
        file.agent_ids.insert(
            "codex".into(),
            AgentRecord {
                health: Some(down),
                ..AgentRecord::default()
            },
        );
        assert_eq!(file.earliest_reset(), Some(soon));
        let claude = AgentId("claude-code".into());
        let codex = AgentId("codex".into());
        assert_eq!(file.entry(&claude).unwrap().back_at(), Some(later));
        assert!(!file.agent_back(&claude, now, now));
        assert!(file.agent_back(&claude, now, later));
        assert!(!file.agent_back(&codex, now, now));
        assert!(file.agent_back(&codex, now, soon));
        assert!(!file.agent_back(&AgentId("agy".into()), now, later));

        let codex_entry = file.agent_ids.get_mut("codex").unwrap();
        codex_entry.health.as_mut().unwrap().state = HealthState::Testing;
        codex_entry.left_down_at = Some(now + Duration::seconds(1));
        assert!(file.agent_back(&codex, now, now));
        assert!(!file.agent_back(&codex, now + Duration::seconds(5), now));
    }

    fn live(account: Option<&str>, minute: i64) -> LiveUsage {
        LiveUsage {
            account: account.map(str::to_owned),
            ..LiveUsage::new(
                AgentId("codex".into()),
                PlanUsageSource::AppServer,
                OffsetDateTime::UNIX_EPOCH + Duration::minutes(minute),
            )
        }
    }

    #[test]
    fn live_readings_are_kept_per_account_with_the_last_signed_in_at_the_end() {
        let mut entry = AgentRecord::default();
        entry.observe_plan_usage(plan_usage("codex", PlanUsageStatus::Allowed, None));
        assert!(entry.observe_live(live(Some("a@example.com"), 1)));
        // The same account's windows stay.
        assert!(!entry.plan_usage_windows().is_empty());
        assert!(entry.observe_live(live(Some("b@example.com"), 2)));
        assert_eq!(
            entry.signed_in().unwrap().account.as_deref(),
            Some("b@example.com")
        );
        // Signing in as another account drops the windows that named none.
        assert!(entry.plan_usage_windows().is_empty());
        // A late reading of the first account is kept for it but doesn't sign it back in.
        assert!(entry.observe_live(live(Some("a@example.com"), 1)));
        assert_eq!(
            entry.signed_in().unwrap().account.as_deref(),
            Some("b@example.com")
        );
        // Nor does an older reading of an account replace its newer one.
        assert!(!entry.observe_live(live(Some("b@example.com"), 0)));
        // Signing back in to the first shows its own reading.
        assert!(entry.observe_live(live(Some("a@example.com"), 3)));
        assert_eq!(
            entry.signed_in().unwrap().read_at,
            OffsetDateTime::UNIX_EPOCH + Duration::minutes(3)
        );
        assert_eq!(entry.live.len(), 2);
    }

    #[test]
    fn only_a_few_accounts_are_kept() {
        let mut entry = AgentRecord::default();
        for n in 0..6 {
            entry.observe_live(live(Some(&format!("{n}@example.com")), n));
        }
        assert_eq!(entry.live.len(), LIVE_ACCOUNTS_KEPT);
        assert_eq!(
            entry.signed_in().unwrap().account.as_deref(),
            Some("5@example.com")
        );
    }
}
