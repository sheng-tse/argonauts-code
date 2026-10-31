//! `~/.argonauts/registry.json`: every live run host across repos, with its session count, for
//! `argonauts status` and the agent panel's "All repos" tab. Entries are keyed by pid and process
//! start time, and readers drop those whose process is gone.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::PathBuf;

use anyhow::Context as _;
use argonauts_core::base::layout::user_files;
use argonauts_core::base::paths::create_private_dir_all;
use argonauts_core::{Paths, RunId};
use serde::{Deserialize, Serialize};

use crate::startup::ancestry::Processes;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveRunHost {
    pub pid: u32,
    /// Seconds since the epoch, as sysinfo reports it.
    pub start_time: u64,
    pub repo_root: PathBuf,
    pub run: RunId,
    /// Agent sessions running now.
    pub sessions: u32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    run_hosts: Vec<LiveRunHost>,
}

fn registry_path(home: &Paths) -> PathBuf {
    home.home().join(user_files::REGISTRY)
}

/// Adds or replaces this run host's entry, under a file lock.
pub(crate) fn register(home: &Paths, entry: &LiveRunHost) -> anyhow::Result<()> {
    update(home, |entries| {
        entries.retain(|existing| existing.pid != entry.pid);
        entries.push(entry.clone());
    })
}

pub(crate) fn set_sessions(home: &Paths, pid: u32, sessions: u32) -> anyhow::Result<()> {
    update(home, |entries| {
        for entry in entries.iter_mut().filter(|entry| entry.pid == pid) {
            entry.sessions = sessions;
        }
    })
}

pub(crate) fn unregister(home: &Paths, pid: u32) -> anyhow::Result<()> {
    update(home, |entries| entries.retain(|entry| entry.pid != pid))
}

/// The live ones, newest first. A missing or unreadable file lists none.
pub fn live_run_hosts(home: &Paths) -> Vec<LiveRunHost> {
    let Ok(mut file) = File::open(registry_path(home)) else {
        return Vec::new();
    };
    if file.lock_shared().is_err() {
        return Vec::new();
    }
    let mut entries = read_entries(&mut file);
    drop_dead(&mut entries);
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.start_time));
    entries
}

/// Reads, drops dead entries, applies `change`, and writes back, all under an exclusive lock.
fn update(home: &Paths, change: impl FnOnce(&mut Vec<LiveRunHost>)) -> anyhow::Result<()> {
    let path = registry_path(home);
    create_private_dir_all(home.home())
        .with_context(|| format!("creating {}", home.home().display()))?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    file.lock()
        .with_context(|| format!("locking {}", path.display()))?;
    let mut entries = read_entries(&mut file);
    drop_dead(&mut entries);
    change(&mut entries);
    let mut text = serde_json::to_vec_pretty(&RegistryFile { run_hosts: entries })?;
    text.push(b'\n');
    file.set_len(0)
        .and_then(|()| file.seek(SeekFrom::Start(0)))
        .and_then(|_| file.write_all(&text))
        .with_context(|| format!("writing {}", path.display()))
}

/// A torn or foreign file reads as empty, and the next write replaces it.
fn read_entries(file: &mut File) -> Vec<LiveRunHost> {
    let mut text = String::new();
    if file.read_to_string(&mut text).is_err() || text.trim().is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<RegistryFile>(&text)
        .map(|registry| registry.run_hosts)
        .unwrap_or_default()
}

fn drop_dead(entries: &mut Vec<LiveRunHost>) {
    let mut processes = Processes::new();
    entries.retain(|entry| processes.start_time(entry.pid) == Some(entry.start_time));
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use crate::startup::ancestry::own_process;

    fn entry(pid: u32, start_time: u64, run: &str) -> LiveRunHost {
        LiveRunHost {
            pid,
            start_time,
            repo_root: PathBuf::from("/repo"),
            run: RunId::new(run).unwrap(),
            sessions: 0,
        }
    }

    #[test]
    fn registry_drops_dead_pids() {
        let home = tempfile::tempdir().unwrap();
        let home = Paths::new(home.path());
        assert!(live_run_hosts(&home).is_empty());

        let mut gone = Command::new("true").spawn().unwrap();
        let gone_pid = gone.id();
        gone.wait().unwrap();
        let (pid, started) = own_process();
        register(&home, &entry(gone_pid, started, "run-20260930-010000-0001")).unwrap();
        register(&home, &entry(pid, started + 1, "run-20260930-010000-0002")).unwrap();
        assert!(
            live_run_hosts(&home).is_empty(),
            "a gone pid and a reused pid both drop"
        );

        register(&home, &entry(pid, started, "run-20260930-010000-0003")).unwrap();
        set_sessions(&home, pid, 3).unwrap();
        let live = live_run_hosts(&home);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].run.as_str(), "run-20260930-010000-0003");
        assert_eq!(live[0].sessions, 3);

        let written: RegistryFile =
            serde_json::from_slice(&std::fs::read(registry_path(&home)).unwrap()).unwrap();
        assert_eq!(written.run_hosts.len(), 1, "writes drop dead entries too");

        unregister(&home, pid).unwrap();
        assert!(live_run_hosts(&home).is_empty());
    }

    #[test]
    fn a_torn_registry_reads_as_empty_and_is_replaced() {
        let home = tempfile::tempdir().unwrap();
        let home = Paths::new(home.path());
        std::fs::write(registry_path(&home), "{\"run hosts\": [").unwrap();
        assert!(live_run_hosts(&home).is_empty());
        let (pid, started) = own_process();
        register(&home, &entry(pid, started, "run-20260930-010000-0004")).unwrap();
        assert_eq!(live_run_hosts(&home).len(), 1);
    }
}
