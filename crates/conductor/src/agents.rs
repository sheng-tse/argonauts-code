//! The registry of agent process groups, `~/.argonauts/agents/<pgid>.json`. Every process group
//! any app or run host starts is listed there while it lives, and the approval path refuses a
//! caller descended from one.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use argonauts_core::layout::user_files::AGENTS;
use argonauts_core::{Paths, create_private_dir_all, write_private_file};
use serde::{Deserialize, Serialize};

use crate::liveness;
use argonauts_core::agents::AgentGroup;

/// One file's contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEntry {
    #[serde(flatten)]
    pub group: AgentGroup,
    /// The `ARGONAUTS_SESSION` key it runs under.
    pub session: String,
    /// The adapter id, such as `codex`.
    pub agent_id: String,
}

/// A registered group. Dropping it removes the file, so a reaped group never stays listed.
#[derive(Debug)]
pub struct Registration {
    path: PathBuf,
    group: AgentGroup,
    listed: bool,
}

impl Registration {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Removes the file now and says whether that worked.
    pub fn remove(mut self) -> io::Result<()> {
        self.listed = false;
        unlist(&self.path, &self.group)
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        if self.listed
            && let Err(err) = unlist(&self.path, &self.group)
        {
            tracing::warn!("could not remove {}: {err}", self.path.display());
        }
    }
}

/// The folder of agent files under `paths`.
pub fn dir(paths: &Paths) -> PathBuf {
    paths.home().join(AGENTS)
}

/// Writes `agents/<pgid>.json` under `paths`, private to you.
pub fn register(paths: &Paths, entry: &AgentEntry) -> io::Result<Registration> {
    let dir = dir(paths);
    create_private_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", entry.group.pgid));
    let json = serde_json::to_vec_pretty(entry).map_err(io::Error::other)?;
    // Written beside the target and renamed, so a reader never sees half a file.
    let temp = dir.join(format!(".{}.{}.tmp", entry.group.pgid, std::process::id()));
    write_private_file(&temp, &json)?;
    if let Err(err) = fs::rename(&temp, &path) {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    Ok(Registration {
        path,
        group: entry.group,
        listed: true,
    })
}

/// Every listed group whose head is still alive with the recorded start time. Files of groups
/// that are gone are skipped, not deleted.
pub fn live(paths: &Paths) -> io::Result<Vec<AgentEntry>> {
    let files = match fs::read_dir(dir(paths)) {
        Ok(files) => files,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut entries = Vec::new();
    for file in files {
        let path = file?.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Some(entry) = read_entry(&path) else {
            continue;
        };
        if liveness::is_same_process(entry.group.pid, entry.group.start_time) {
            entries.push(entry);
        }
    }
    entries.sort_by_key(|entry| entry.group.pgid);
    Ok(entries)
}

fn read_entry(path: &Path) -> Option<AgentEntry> {
    let text = fs::read(path).ok()?;
    match serde_json::from_slice(&text) {
        Ok(entry) => Some(entry),
        Err(err) => {
            tracing::debug!("skipping {}: {err}", path.display());
            None
        }
    }
}

/// Removes the file unless a newer group with a reused pgid has taken it over since.
fn unlist(path: &Path, group: &AgentGroup) -> io::Result<()> {
    if let Some(current) = read_entry(path)
        && current.group != *group
    {
        return Ok(());
    }
    match fs::remove_file(path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    fn entry(pid: u32, start_time: u64, session: &str) -> AgentEntry {
        AgentEntry {
            group: AgentGroup {
                pgid: pid,
                pid,
                start_time,
            },
            session: session.into(),
            agent_id: "codex".into(),
        }
    }

    #[test]
    fn agents_file_written_and_removed() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        assert_eq!(live(&paths).unwrap(), Vec::new());

        let (me, started) = liveness::this_process().unwrap();
        let mine = entry(me, started, "s-001");
        let registration = register(&paths, &mine).unwrap();
        let path = registration.path().to_owned();
        assert_eq!(path, dir(&paths).join(format!("{me}.json")));
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dir_mode = fs::metadata(dir(&paths)).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
        let written: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["pgid"], me);
        assert_eq!(written["start_time"], started);
        assert_eq!(written["session"], "s-001");
        // The app reads the same file as the shared type and ignores the extra fields.
        let read: AgentGroup = serde_json::from_value(written).unwrap();
        assert_eq!(read, mine.group);
        assert_eq!(live(&paths).unwrap(), vec![mine.clone()]);

        // A group that's gone, and a pid reused by another process, are listed but not live.
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let dead = child.id();
        child.wait().unwrap();
        let _gone = register(&paths, &entry(dead, 1, "s-002")).unwrap();
        let reused = entry(me + 1_000_000, started, "s-003");
        let _reused = register(&paths, &reused).unwrap();
        fs::write(dir(&paths).join("junk.json"), "{").unwrap();
        assert_eq!(live(&paths).unwrap(), vec![mine.clone()]);
        assert!(dir(&paths).join(format!("{dead}.json")).exists());

        registration.remove().unwrap();
        assert!(!path.exists());
        assert_eq!(live(&paths).unwrap(), Vec::new());

        // Dropping removes it too.
        let registration = register(&paths, &mine).unwrap();
        assert!(path.exists());
        drop(registration);
        assert!(!path.exists());

        // A registration never removes a file a newer group with the same pgid wrote since.
        let older = register(&paths, &entry(me, started - 5, "s-004")).unwrap();
        let newer = register(&paths, &mine).unwrap();
        drop(older);
        assert_eq!(live(&paths).unwrap(), vec![mine]);
        drop(newer);
        assert!(!path.exists());
    }
}
