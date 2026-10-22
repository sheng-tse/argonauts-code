//! Making sure no agent process outlives its session or its run host: killing a recorded
//! group only while it's still the same agent's, sweeping the processes that left the group, and
//! guarding against the run host's own death.

mod procs;
mod table;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod watch;

use std::collections::HashMap;
use std::io;

use nix::errno::Errno;
use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::Pid;

use crate::liveness;
use argonauts_core::agents::AgentGroup;
use table::{Storage, Table};

/// Every agent process's environment carries `ARGONAUTS_LAUNCH=<id>`, with an id unique to that
/// launch, and so does everything it starts, whatever group or session that ends up in.
pub const LAUNCH_MARKER: &str = "ARGONAUTS_LAUNCH";

/// Processes of an agent's tree, by pid, with their start times.
pub type TreeSnapshot = HashMap<u32, u64>;

/// Kills what's left of `record`'s agent: its session, group, and descendants. Returns whether
/// anything was left. After the head dies, its group and session are still swept while they have
/// members, since neither id can be reused until they're empty. A pid reused by another process
/// is never hit.
pub fn kill_recorded_group(record: &AgentGroup) -> io::Result<bool> {
    let head_lives = match liveness::start_time(record.pid) {
        Some(started) if started == record.start_time => true,
        // The pid was free to reuse, so the old group and session were empty.
        Some(_) => return Ok(false),
        None => false,
    };
    let swept = sweep_recorded(record)?;
    Ok(head_lives || swept.in_group + swept.outside_group > 0)
}

/// Kills processes whose environment carries `ARGONAUTS_LAUNCH=<launch>`, such as a shell that
/// left the agent's tree. Returns how many it killed. Only Linux shows other processes'
/// environments, so elsewhere it reads nothing and kills nothing. Blocks for a moment.
pub fn kill_launch_leftovers(launch: &str) -> io::Result<usize> {
    let Some(entry) = launch_entry(launch) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "an empty launch id would match every launch",
        ));
    };
    #[cfg(target_os = "linux")]
    {
        let me = raw_pid(std::process::id())?;
        kill_launch(&mut Storage::allocate().table(), entry.as_bytes(), me)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = entry;
        Ok(0)
    }
}

/// Kills the recorded group together with what left it: processes still in the head's session,
/// and the head's descendants in sessions of their own. Returns how many it killed outside the
/// group. Call it before killing the group, since the children of a head that died are adopted
/// by init and can no longer be traced to it. Blocks for a moment.
pub fn kill_process_tree(record: &AgentGroup) -> io::Result<usize> {
    Ok(sweep_recorded(record)?.outside_group)
}

/// The processes of `record`'s tree now, for [`kill_noted`] to find after their parents die: a
/// process that started a session of its own is adopted by init once its parent is killed with
/// the group, and can no longer be traced to the head. Empty when the head's pid belongs to
/// another process. Blocks while it reads the process table.
pub fn snapshot_tree(record: &AgentGroup) -> io::Result<TreeSnapshot> {
    if liveness::start_time(record.pid).is_some_and(|started| started != record.start_time) {
        return Ok(TreeSnapshot::new());
    }
    let mut storage = Storage::allocate();
    let mut table = storage.table();
    table.read()?;
    table.mark_tree(raw_pid(record.pid)?, raw_pid(record.pgid)?);
    let pids: Vec<u32> = table
        .doomed()
        .filter_map(|proc| u32::try_from(proc.pid).ok())
        .collect();
    Ok(pids
        .into_iter()
        .filter_map(|pid| Some((pid, liveness::start_time(pid)?)))
        .collect())
}

/// Kills what is left of a [`snapshot_tree`]: every process in `seen` but this one that still
/// has its start time, so a reused pid is never hit. Call it after [`kill_process_tree`], which
/// takes what can still be traced to `record`. Returns how many it killed.
pub fn kill_noted(seen: &TreeSnapshot, record: &AgentGroup) -> io::Result<usize> {
    let me = std::process::id();
    let mut killed = 0;
    for (&pid, &started) in seen {
        let ours = pid > 1 && pid != me && pid != record.pid;
        if !ours || liveness::start_time(pid) != Some(started) {
            continue;
        }
        match kill(Pid::from_raw(raw_pid(pid)?), Signal::SIGKILL) {
            Ok(()) => killed += 1,
            // Gone already, or not ours to signal.
            Err(Errno::ESRCH | Errno::EPERM) => {}
            Err(errno) => return Err(io::Error::from(errno)),
        }
    }
    Ok(killed)
}

/// Live processes a sweep found, and killed, in the group and outside it.
#[derive(Debug, Default, Clone, Copy)]
struct Swept {
    in_group: usize,
    outside_group: usize,
}

fn sweep_recorded(record: &AgentGroup) -> io::Result<Swept> {
    if liveness::start_time(record.pid).is_some_and(|started| started != record.start_time) {
        // The head's pid, and with it the session and group ids, belong to someone else now.
        return Ok(Swept::default());
    }
    let me = raw_pid(std::process::id())?;
    sweep_tree(
        &mut Storage::allocate().table(),
        raw_pid(record.pid)?,
        raw_pid(record.pgid)?,
        &[],
        me,
    )
}

/// Kills the head's tree outside the group, and the `noted` processes (pid and start time)
/// that are still the same with their descendants, then the group. A head or group of 0 is
/// skipped. `spare` is never killed. System calls only, for the exit watcher's sake.
fn sweep_tree(
    table: &mut Table,
    head: i32,
    pgid: i32,
    noted: &[(i32, u64)],
    spare: i32,
) -> io::Result<Swept> {
    let read = table.read();
    let mut swept = Swept::default();
    if read.is_ok() {
        table.mark_tree(head, pgid);
        table.mark_noted(noted);
        for proc in table.doomed() {
            if proc.pgid == pgid {
                swept.in_group += 1;
                continue;
            }
            let killable = proc.pid != spare && proc.pid > 1;
            if killable && kill(Pid::from_raw(proc.pid), Signal::SIGKILL).is_ok() {
                swept.outside_group += 1;
            }
        }
    }
    if pgid > 1 {
        match killpg(Pid::from_raw(pgid), Signal::SIGKILL) {
            // macOS reports EPERM for a group whose members have all exited but not been reaped.
            Ok(()) | Err(Errno::ESRCH | Errno::EPERM) => {}
            Err(errno) => return Err(io::Error::from(errno)),
        }
    }
    read.map(|()| swept)
}

/// Kills every process but `spare` whose environment holds `entry`. System calls only, for the
/// exit watcher's sake.
#[cfg(target_os = "linux")]
fn kill_launch(table: &mut Table, entry: &[u8], spare: i32) -> io::Result<usize> {
    table.read()?;
    let mut killed = 0;
    for proc in table.procs() {
        let marked = proc.pid > 1 && proc.pid != spare && procs::environ_has(proc.pid, entry);
        if marked && kill(Pid::from_raw(proc.pid), Signal::SIGKILL).is_ok() {
            killed += 1;
        }
    }
    Ok(killed)
}

fn launch_entry(launch: &str) -> Option<String> {
    (!launch.is_empty()).then(|| format!("{LAUNCH_MARKER}={launch}"))
}

fn raw_pid(pid: u32) -> io::Result<i32> {
    i32::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "process id out of range"))
}

/// Kills the agent's whole tree when the run host dies. A forked watcher waits for the
/// run host's exit (kqueue on macOS, a pidfd on Linux), then sweeps as a release would: the
/// session, the group, the descendants, every process it noted in the tree while it watched, and
/// on Linux the launch's marker. Dropping it stops the watch.
#[derive(Debug)]
pub struct ExitGuard {
    pgid: u32,
    watcher: Pid,
}

impl ExitGuard {
    pub fn pgid(&self) -> u32 {
        self.pgid
    }
}

impl Drop for ExitGuard {
    fn drop(&mut self) {
        let _ = kill(self.watcher, Signal::SIGKILL);
        // SIGKILL can't be caught, so this wait is short.
        let _ = nix::sys::wait::waitpid(self.watcher, None);
    }
}

/// Starts guarding `record`'s agent, launched as `launch`, against the death of this process.
pub fn guard_group(record: &AgentGroup, launch: &str) -> io::Result<ExitGuard> {
    watch_for_exit(std::process::id(), record, launch)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn watch_for_exit(run_host: u32, record: &AgentGroup, launch: &str) -> io::Result<ExitGuard> {
    let entry = launch_entry(launch).unwrap_or_default();
    let watcher = watch::start(run_host, record, entry.as_bytes())?;
    Ok(ExitGuard {
        pgid: record.pgid,
        watcher,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn watch_for_exit(_run_host: u32, _record: &AgentGroup, _launch: &str) -> io::Result<ExitGuard> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "guarding agents against the run host's exit works on Linux and macOS only",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{AgentProcess, ProcessEvent, Stream, signal_group};
    use crate::testutil::{bash_spec, sh_spec, wait_until_gone};
    use argonauts_core::StopSignal;
    use std::time::{Duration, Instant};

    fn group_alive(pgid: u32) -> bool {
        // macOS reports EPERM for a group whose members have all exited but not been reaped.
        !matches!(
            killpg(Pid::from_raw(pgid as i32), None),
            Err(Errno::ESRCH | Errno::EPERM)
        )
    }

    async fn wait_until_group_gone(pgid: u32) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while group_alive(pgid) {
            assert!(Instant::now() < deadline, "group {pgid} is still running");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// The first `count` lines the agent prints on stdout, as pids. Stderr is left out, since the
    /// shell may write there before the pid: a loaded full test run once read a non-number first.
    async fn printed_pids(agent: &mut AgentProcess, count: usize) -> Vec<i32> {
        let mut pids = Vec::new();
        while pids.len() < count {
            match agent.next_event().await.unwrap() {
                ProcessEvent::Line(line)
                    if line.stream == Stream::Stdout && !line.text.is_empty() =>
                {
                    let pid = line.text.trim().parse();
                    pids.push(pid.unwrap_or_else(|err| panic!("{err}: {:?}", line.text)));
                }
                _ => {}
            }
        }
        pids
    }

    /// An agent that stays until its input closes.
    fn waiting_agent(dir: &std::path::Path, script: &str) -> AgentProcess {
        let mut spec = bash_spec(dir, &format!("{script}; read line"));
        spec.keep_stdin_open = true;
        AgentProcess::spawn(&spec).unwrap()
    }

    /// A fake run host, since a test can't let its own process die.
    fn fake_run_host() -> tokio::process::Child {
        tokio::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap()
    }

    fn assert_alive(pid: i32) {
        assert!(kill(Pid::from_raw(pid), None).is_ok(), "{pid} is gone");
    }

    #[tokio::test]
    async fn recorded_group_killed_only_while_same_process() {
        let dir = tempfile::tempdir().unwrap();
        let agent = AgentProcess::spawn(&sh_spec(dir.path(), "sleep 30")).unwrap();
        let record = agent.record().unwrap();
        assert_eq!((record.pid, record.pgid), (agent.id(), agent.id()));

        let reused = AgentGroup {
            start_time: record.start_time - 60,
            ..record
        };
        assert!(!kill_recorded_group(&reused).unwrap());
        assert!(group_alive(record.pgid));

        assert!(kill_recorded_group(&record).unwrap());
        wait_until_gone(record.pid as i32).await;
        assert!(!kill_recorded_group(&record).unwrap());
    }

    #[tokio::test]
    async fn a_dead_heads_group_is_killed_while_it_has_members() {
        let dir = tempfile::tempdir().unwrap();
        let mut agent = waiting_agent(dir.path(), "sleep 30 & echo $!");
        let record = agent.record().unwrap();
        let member = printed_pids(&mut agent, 1).await[0];
        agent.close_input();
        wait_until_gone(agent.id() as i32).await;
        assert_alive(member);

        assert!(kill_recorded_group(&record).unwrap());
        wait_until_gone(member).await;
        assert!(!kill_recorded_group(&record).unwrap());
    }

    /// An agent whose shell put a `sleep` in a group of its own, as a CLI's shell might.
    /// Returns the agent, its record, and the leftover's pid.
    async fn agent_with_a_leftover(dir: &std::path::Path) -> (AgentProcess, AgentGroup, i32) {
        let mut agent =
            AgentProcess::spawn(&bash_spec(dir, "set -m; sleep 30 & echo $!; wait")).unwrap();
        let record = agent.record().unwrap();
        let leftover = printed_pids(&mut agent, 1).await[0];
        let leftover_group = nix::unistd::getpgid(Some(Pid::from_raw(leftover))).unwrap();
        assert_ne!(leftover_group.as_raw() as u32, agent.id());
        (agent, record, leftover)
    }

    #[tokio::test]
    async fn leftovers_swept_by_launch_marker_or_session() {
        let dir = tempfile::tempdir().unwrap();
        let (agent, record, leftover) = agent_with_a_leftover(dir.path()).await;
        let mut other = tokio::process::Command::new("sleep")
            .arg("30")
            .env(LAUNCH_MARKER, uuid::Uuid::new_v4().simple().to_string())
            .spawn()
            .unwrap();

        // With the head gone, only the marker or the session id still reaches the leftover.
        signal_group(agent.id(), StopSignal::Kill).unwrap();
        wait_until_gone(agent.id() as i32).await;
        assert_alive(leftover);
        let by_marker = kill_launch_leftovers(agent.launch_id()).unwrap();
        // macOS 27 hides other processes' environments, so there the session id finds it.
        assert_eq!(by_marker, usize::from(cfg!(target_os = "linux")));
        if by_marker > 0 {
            wait_until_gone(leftover).await;
        }
        let by_session = kill_process_tree(&record).unwrap();
        assert_eq!(by_marker + by_session, 1);
        wait_until_gone(leftover).await;

        assert!(other.try_wait().unwrap().is_none());
        other.kill().await.unwrap();
        assert!(kill_launch_leftovers("").is_err());
    }

    #[tokio::test]
    async fn process_tree_swept_while_head_lives() {
        let dir = tempfile::tempdir().unwrap();
        let (agent, record, leftover) = agent_with_a_leftover(dir.path()).await;
        assert_eq!(kill_process_tree(&record).unwrap(), 1);
        wait_until_gone(leftover).await;
        wait_until_group_gone(agent.id()).await;

        let reused = AgentGroup {
            start_time: record.start_time - 60,
            ..record
        };
        assert_eq!(kill_process_tree(&reused).unwrap(), 0);
    }

    /// A grandchild in a session of its own, as a daemonizing dev server or Node's
    /// `detached: true` makes, whose parent stays in the group. Returns its pid.
    async fn agent_with_a_detached_grandchild(dir: &std::path::Path) -> (AgentProcess, i32) {
        let script = r#"sh -c 'perl -e "use POSIX; POSIX::setsid(); print qq(\$\$\n); close STDOUT; sleep 300" & wait' & wait"#;
        let mut agent = AgentProcess::spawn(&sh_spec(dir, script)).unwrap();
        let detached = printed_pids(&mut agent, 1).await[0];
        let sid = nix::unistd::getsid(Some(Pid::from_raw(detached))).unwrap();
        assert_eq!(
            sid.as_raw(),
            detached,
            "the grandchild heads a session of its own"
        );
        (agent, detached)
    }

    #[tokio::test]
    async fn snapshot_finds_a_detached_grandchild_after_the_group_dies() {
        let dir = tempfile::tempdir().unwrap();
        let (agent, detached) = agent_with_a_detached_grandchild(dir.path()).await;
        let record = agent.record().unwrap();
        let seen = snapshot_tree(&record).unwrap();
        assert!(seen.contains_key(&(detached as u32)), "{seen:?}");

        // Killing the group orphans the grandchild, so only the snapshot still reaches it.
        signal_group(agent.id(), StopSignal::Kill).unwrap();
        wait_until_group_gone(agent.id()).await;
        assert_alive(detached);
        assert_eq!(kill_noted(&seen, &record).unwrap(), 1);
        wait_until_gone(detached).await;

        // A snapshot entry whose pid now has another start time is someone else's.
        let mut other = fake_run_host();
        let stale = TreeSnapshot::from([(other.id().unwrap(), 1)]);
        assert_eq!(kill_noted(&stale, &record).unwrap(), 0);
        assert!(other.try_wait().unwrap().is_none());
        other.kill().await.unwrap();
    }

    #[tokio::test]
    async fn a_reused_head_pid_is_never_signalled() {
        let dir = tempfile::tempdir().unwrap();
        let agent = AgentProcess::spawn(&sh_spec(dir.path(), "sleep 30")).unwrap();
        let record = agent.record().unwrap();
        let reused = AgentGroup {
            start_time: record.start_time - 60,
            ..record
        };
        assert!(snapshot_tree(&reused).unwrap().is_empty());
        assert_eq!(kill_process_tree(&reused).unwrap(), 0);
        assert!(!kill_recorded_group(&reused).unwrap());
        assert!(group_alive(agent.id()));
        assert!(kill_recorded_group(&record).unwrap());
        wait_until_group_gone(agent.id()).await;
    }

    #[tokio::test]
    async fn exit_guard_kills_the_tree_when_run_host_dies() {
        let dir = tempfile::tempdir().unwrap();
        let mut run_host = fake_run_host();
        // A grandchild that stays in the group, and one in a group of its own.
        let mut agent = waiting_agent(dir.path(), "sleep 30 & echo $!; set -m; sleep 30 & echo $!");
        let record = agent.record().unwrap();
        let pids = printed_pids(&mut agent, 2).await;
        let guard = watch_for_exit(run_host.id().unwrap(), &record, agent.launch_id()).unwrap();
        assert_eq!(guard.pgid(), agent.id());

        run_host.kill().await.unwrap();
        // Zombies count as gone, since a container without init never reaps orphans.
        wait_until_gone(agent.id() as i32).await;
        for pid in pids {
            wait_until_gone(pid).await;
        }
        drop(guard);
    }

    #[tokio::test]
    async fn exit_guard_outlives_the_head_while_its_group_runs() {
        let dir = tempfile::tempdir().unwrap();
        let mut run_host = fake_run_host();
        let mut agent = waiting_agent(dir.path(), "sleep 30 & echo $!");
        let record = agent.record().unwrap();
        let member = printed_pids(&mut agent, 1).await[0];
        let guard = watch_for_exit(run_host.id().unwrap(), &record, agent.launch_id()).unwrap();
        agent.close_input();
        wait_until_gone(agent.id() as i32).await;
        // Past a few of the watcher's checks for members.
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_alive(member);

        run_host.kill().await.unwrap();
        wait_until_gone(member).await;
        drop(guard);
    }

    #[tokio::test]
    async fn exit_guard_kills_a_noted_process_that_lost_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        let mut run_host = fake_run_host();
        let (agent, detached) = agent_with_a_detached_grandchild(dir.path()).await;
        let record = agent.record().unwrap();
        let guard = watch_for_exit(run_host.id().unwrap(), &record, agent.launch_id()).unwrap();

        // Once the group dies, nothing but the watcher's notes reaches the grandchild.
        signal_group(agent.id(), StopSignal::Kill).unwrap();
        wait_until_group_gone(agent.id()).await;
        // Past a few of the watcher's checks, which keep it watching for the grandchild.
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_alive(detached);
        let watching =
            nix::sys::wait::waitpid(guard.watcher, Some(nix::sys::wait::WaitPidFlag::WNOHANG))
                .unwrap();
        assert_eq!(watching, nix::sys::wait::WaitStatus::StillAlive);

        run_host.kill().await.unwrap();
        wait_until_gone(detached).await;
        drop(guard);
    }

    #[tokio::test]
    async fn exit_guard_ends_once_the_group_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mut run_host = fake_run_host();
        let mut agent = waiting_agent(dir.path(), "true");
        let record = agent.record().unwrap();
        let guard = watch_for_exit(run_host.id().unwrap(), &record, agent.launch_id()).unwrap();
        agent.close_input();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status =
                nix::sys::wait::waitpid(guard.watcher, Some(nix::sys::wait::WaitPidFlag::WNOHANG))
                    .unwrap();
            if status != nix::sys::wait::WaitStatus::StillAlive {
                break;
            }
            assert!(Instant::now() < deadline, "the watcher is still running");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        run_host.kill().await.unwrap();
    }

    #[tokio::test]
    async fn dropped_exit_guard_stops_watching() {
        let dir = tempfile::tempdir().unwrap();
        let mut run_host = fake_run_host();
        let agent = AgentProcess::spawn(&sh_spec(dir.path(), "sleep 30")).unwrap();
        let record = agent.record().unwrap();
        let guard = watch_for_exit(run_host.id().unwrap(), &record, agent.launch_id()).unwrap();
        drop(guard);
        run_host.kill().await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(group_alive(agent.id()));
    }

    #[tokio::test]
    async fn guard_group_starts_for_a_live_group() {
        let dir = tempfile::tempdir().unwrap();
        let agent = AgentProcess::spawn(&sh_spec(dir.path(), "sleep 30")).unwrap();
        let guard = guard_group(&agent.record().unwrap(), agent.launch_id()).unwrap();
        assert_eq!(guard.pgid(), agent.id());
        drop(guard);
        assert!(group_alive(agent.id()));
    }

    /// A process in a session of its own whose parent exited: nothing but its launch's marker
    /// still reaches it.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn exit_guard_finds_orphans_by_launch_marker() {
        let dir = tempfile::tempdir().unwrap();
        let mut run_host = fake_run_host();
        let mut agent = waiting_agent(dir.path(), "sleep 30 & echo $!; setsid sleep 30 & echo $!");
        let record = agent.record().unwrap();
        let pids = printed_pids(&mut agent, 2).await;
        let (member, orphan) = (pids[0], pids[1]);
        let guard = watch_for_exit(run_host.id().unwrap(), &record, agent.launch_id()).unwrap();
        agent.close_input();
        wait_until_gone(agent.id() as i32).await;
        let orphan_session = nix::unistd::getsid(Some(Pid::from_raw(orphan))).unwrap();
        assert_ne!(orphan_session.as_raw() as u32, agent.id());

        run_host.kill().await.unwrap();
        wait_until_gone(member).await;
        wait_until_gone(orphan).await;
        drop(guard);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn agents_die_with_the_thread_that_spawned_them() {
        // PR_SET_PDEATHSIG follows the spawning thread, so a thread that exits plays the part of a
        // run host that dies.
        let dir = tempfile::tempdir().unwrap();
        let spec = sh_spec(dir.path(), "sleep 30");
        let pid = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let agent = AgentProcess::spawn(&spec).unwrap();
                let pid = agent.id();
                std::mem::forget(agent);
                pid
            })
        })
        .join()
        .unwrap();
        // Nothing reaps the killed agent here, so a zombie counts as gone.
        let gone = || {
            std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |stat| {
                stat.rsplit_once(')')
                    .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z'))
            })
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !gone() {
            assert!(
                Instant::now() < deadline,
                "agent {pid} outlived its spawner"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
