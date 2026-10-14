//! Agent process groups a crashed run host left behind. Each is killed before anything
//! resumes, so two processes never share one CLI session.

use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use argonauts_core::Paths;
use argonauts_core::agents::AgentGroup;
use argonauts_core::layout::{run_files, session_files};
use nix::errno::Errno;
use nix::sys::signal::{SIGKILL, SIGTERM, Signal, killpg};
use nix::unistd::{Pid, getpgid};

use crate::ancestry::{Processes, agents_dir};

/// How long the groups get to exit on SIGTERM before SIGKILL.
const TERM_GRACE: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(25);

/// Kills every live group a session's `process.json` names and removes its
/// `~/.argonauts/agents/` entry. Returns the groups killed. The wait runs off the async runtime.
pub(crate) async fn kill_orphans(run_dir: &Path, home: &Paths) -> anyhow::Result<Vec<u32>> {
    let run_dir = run_dir.to_path_buf();
    let home = home.clone();
    tokio::task::spawn_blocking(move || kill_now(&run_dir, &home))
        .await
        .context("stopping agents a crashed run host left")?
}

fn kill_now(run_dir: &Path, home: &Paths) -> anyhow::Result<Vec<u32>> {
    let mut processes = Processes::new();
    let groups = orphan_groups(run_dir, &mut processes)?;
    for group in &groups {
        signal(group, SIGTERM)?;
    }
    let deadline = Instant::now() + TERM_GRACE;
    while Instant::now() < deadline
        && groups
            .iter()
            .any(|group| still_running(group, &mut processes))
    {
        std::thread::sleep(POLL);
    }
    let mut killed = Vec::new();
    for group in groups {
        match signal(&group, SIGKILL) {
            // macOS answers EPERM for a group left with only zombies, which have nothing to kill.
            Err(_) if processes.start_time(group.pid) != Some(group.start_time) => {}
            result => result?,
        }
        forget_agent(home, &group);
        killed.push(group.pgid);
    }
    Ok(killed)
}

/// The recorded groups that still have processes and are still ours.
fn orphan_groups(run_dir: &Path, processes: &mut Processes) -> anyhow::Result<Vec<AgentGroup>> {
    let sessions = run_dir.join(run_files::SESSIONS);
    let entries = match std::fs::read_dir(&sessions) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("reading {}", sessions.display())),
    };
    let own_group = getpgid(None).ok();
    let mut groups = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path().join(session_files::PROCESS);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        let group: AgentGroup = match serde_json::from_slice(&bytes) {
            Ok(group) => group,
            Err(err) => {
                tracing::warn!("skipping {}: {err}", path.display());
                continue;
            }
        };
        // 0 and 1 would signal this process's own group or every process.
        let Some(pgid) = group_pid(&group).filter(|pgid| pgid.as_raw() > 1) else {
            continue;
        };
        if Some(pgid) == own_group {
            continue;
        }
        let ours = match processes.start_time(group.pid) {
            Some(started) if started == group.start_time => i32::try_from(group.pid)
                .is_ok_and(|pid| getpgid(Some(Pid::from_raw(pid))).ok() == Some(pgid)),
            // The pid of the group's first process now belongs to another.
            Some(_) => false,
            // That process exited, but a build or a watcher it started may still run. A group id
            // isn't reused while its group has members, so with no process holding that pid
            // the members left are the agent's.
            None => processes.start_time(group.pgid).is_none() && has_members(pgid),
        };
        if ours {
            groups.push(group);
        } else if processes.start_time(group.pid) == Some(group.start_time) {
            tracing::warn!(
                "not killing process group {} from {}: it isn't the recorded agent's group",
                group.pgid,
                path.display()
            );
        }
    }
    Ok(groups)
}

fn still_running(group: &AgentGroup, processes: &mut Processes) -> bool {
    processes.start_time(group.pid) == Some(group.start_time)
        || group_pid(group).is_some_and(has_members)
}

/// The group as a signal target. A pgid past `i32` can't be a real group.
fn group_pid(group: &AgentGroup) -> Option<Pid> {
    i32::try_from(group.pgid).ok().map(Pid::from_raw)
}

/// A signal-0 probe. EPERM is a group of zombies on macOS, or someone else's group.
fn has_members(pgid: Pid) -> bool {
    killpg(pgid, None).is_ok()
}

fn signal(group: &AgentGroup, signal: Signal) -> anyhow::Result<()> {
    let pgid = group.pgid;
    let Some(target) = group_pid(group) else {
        bail!("process group {pgid} isn't one a signal can reach");
    };
    match killpg(target, signal) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(err) => bail!("sending {signal} to process group {pgid}: {err}"),
    }
}

/// Removes the group's `agents/` entry when it names this same process.
fn forget_agent(home: &Paths, group: &AgentGroup) {
    let path = agents_dir(home).join(format!("{}.json", group.pgid));
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    if serde_json::from_slice::<AgentGroup>(&bytes).is_ok_and(|entry| entry == *group)
        && let Err(err) = std::fs::remove_file(&path)
    {
        tracing::warn!("removing {}: {err}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::layout::session_dir;
    use argonauts_core::names::SessionKey;

    use super::*;
    use crate::ancestry::tests::{child_of, register_group, spawn_group};

    fn record(run_dir: &Path, session: u32, group: &AgentGroup) {
        let dir = session_dir(run_dir, &SessionKey::numbered(session));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(session_files::PROCESS),
            serde_json::to_vec(group).unwrap(),
        )
        .unwrap();
    }

    fn group_of(pid: u32) -> AgentGroup {
        AgentGroup {
            pgid: pid,
            pid,
            start_time: Processes::new().start_time(pid).unwrap(),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn orphans_are_killed_by_start_time_only() {
        let dir = tempfile::tempdir().unwrap();
        let home = Paths::new(dir.path().join("home"));
        let run_dir = dir.path().join("run");

        let mut orphan = spawn_group("sleep 30; true", &[]);
        let orphan_child = child_of(orphan.id());
        let orphan_group = group_of(orphan.id());
        record(&run_dir, 1, &orphan_group);
        register_group(&home, orphan.id());

        let mut stranger = spawn_group("sleep 30; true", &[]);
        let mut reused = group_of(stranger.id());
        reused.start_time += 1;
        record(&run_dir, 2, &reused);

        let mut gone = std::process::Command::new("true").spawn().unwrap();
        let gone_pid = gone.id();
        gone.wait().unwrap();
        record(
            &run_dir,
            3,
            &AgentGroup {
                pgid: gone_pid,
                pid: gone_pid,
                start_time: 1,
            },
        );

        let killed = kill_orphans(&run_dir, &home).await.unwrap();
        assert_eq!(killed, vec![orphan.id()]);
        assert!(orphan.wait().unwrap().code().is_none(), "ended by a signal");
        assert!(dies(orphan_child), "the whole group dies");
        assert!(
            !agents_dir(&home)
                .join(format!("{}.json", orphan.id()))
                .exists()
        );
        assert!(
            Processes::new().start_time(stranger.id()).is_some(),
            "a pid with another start time is someone else's"
        );

        let _ = killpg(Pid::from_raw(stranger.id() as i32), SIGKILL);
        let _ = stranger.wait();
    }

    fn dies(pid: u32) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Processes::new().start_time(pid).is_some() {
            if Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(POLL);
        }
        true
    }

    /// A shell that starts `sleep` in the background and exits at once, as a CLI that
    /// crashed would leave the build it started. Returns the shell's pid and the sleep's.
    fn group_left_behind() -> (u32, u32) {
        use std::io::BufRead as _;
        use std::os::unix::process::CommandExt as _;
        let mut shell = std::process::Command::new("sh")
            .args(["-c", "sleep 30 & echo $!"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(shell.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        shell.wait().unwrap();
        (shell.id(), line.trim().parse().unwrap())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_group_whose_first_process_exited_is_still_killed() {
        let dir = tempfile::tempdir().unwrap();
        let home = Paths::new(dir.path().join("home"));
        let run_dir = dir.path().join("run");
        let (shell, member) = group_left_behind();
        let group = AgentGroup {
            pgid: shell,
            pid: shell,
            start_time: 1,
        };
        record(&run_dir, 1, &group);

        let killed = kill_orphans(&run_dir, &home).await.unwrap();
        assert_eq!(killed, vec![shell]);
        assert!(dies(member), "the build the agent left behind stops too");
    }

    #[tokio::test]
    async fn no_sessions_means_nothing_to_kill() {
        let dir = tempfile::tempdir().unwrap();
        let home = Paths::new(dir.path().join("home"));
        assert!(
            kill_orphans(&dir.path().join("run"), &home)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn groups_that_ignore_sigterm_share_one_grace() {
        let dir = tempfile::tempdir().unwrap();
        let home = Paths::new(dir.path().join("home"));
        let run_dir = dir.path().join("run");
        let mut stubborn: Vec<_> = (1..=3)
            .map(|session| {
                let child = spawn_group("trap '' TERM; sleep 30; true", &[]);
                // The shell forks `sleep` only after the trap is set.
                child_of(child.id());
                record(&run_dir, session, &group_of(child.id()));
                child
            })
            .collect();

        let began = Instant::now();
        let killed = kill_orphans(&run_dir, &home).await.unwrap();
        let took = began.elapsed();
        assert_eq!(killed.len(), 3);
        assert!(took >= TERM_GRACE, "each got its SIGTERM grace: {took:?}");
        assert!(
            took < TERM_GRACE * 2,
            "one grace, not one per group: {took:?}"
        );
        for child in &mut stubborn {
            assert!(child.wait().unwrap().code().is_none(), "ended by SIGKILL");
        }
    }
}
