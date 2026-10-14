//! Whether a process descends from an agent, which the approval path refuses. An agent is a
//! process group registered in `~/.argonauts/agents/`, found by walking the parent chain,
//! since pids and process groups stay visible. Other processes' environments are never read,
//! as some systems (macOS 27) hide them, so a marker such as `ARGONAUTS_SESSION` or one of
//! `approvals.refuse_ancestor_env` counts only in this process's own environment.

use std::collections::HashSet;
use std::io;
use std::path::PathBuf;

use anyhow::{Context as _, anyhow, bail};
use argonauts_core::Paths;
use argonauts_core::agents::AgentGroup;
use argonauts_core::layout::user_files;
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::ApprovalSettings;
use nix::errno::Errno;
use nix::sys::signal::killpg;
use nix::unistd::{Pid as NixPid, getpgid};
use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System};

use crate::env::SESSION_ENV;

/// Far deeper than any real process tree, so a loop in parent links can't hang the walk.
const MAX_DEPTH: usize = 256;

/// Environment variables that mark an agent's shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentMarkers {
    pub env: Vec<String>,
}

impl AgentMarkers {
    /// `ARGONAUTS_SESSION` plus the settings' `approvals.refuse_ancestor_env`.
    pub(crate) fn from_settings(settings: &Settings) -> Self {
        Self::with(&settings.approvals.refuse_ancestor_env)
    }

    /// `ARGONAUTS_SESSION` plus the built-in list, for callers without resolved settings.
    pub(crate) fn builtin() -> Self {
        Self::with(&ApprovalSettings::default().refuse_ancestor_env)
    }

    /// `ARGONAUTS_SESSION` alone: Argonauts Code's own agents, which must never host a run.
    pub(crate) fn own() -> Self {
        Self::with(&[])
    }

    fn with(extra: &[String]) -> Self {
        let mut env = vec![SESSION_ENV.to_owned()];
        for name in extra {
            if !name.is_empty() && !env.contains(name) {
                env.push(name.clone());
            }
        }
        Self { env }
    }

    /// The first marker set in this process's environment. Set but empty still counts.
    fn set_here(&self) -> Option<&str> {
        self.env
            .iter()
            .map(String::as_str)
            .find(|name| std::env::var_os(name).is_some())
    }
}

pub(crate) fn agents_dir(home: &Paths) -> PathBuf {
    home.home().join(user_files::AGENTS)
}

/// Entries whose group is gone are skipped. A folder or entry that can't be read is an error,
/// since a check that skipped it could pass an agent.
pub(crate) fn registered_agents(
    home: &Paths,
    processes: &mut dyn ProcessSource,
) -> anyhow::Result<Vec<AgentGroup>> {
    let dir = agents_dir(home);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("reading {}", dir.display())),
    };
    let mut groups = Vec::new();
    for entry in entries {
        let path = entry
            .with_context(|| format!("reading {}", dir.display()))?
            .path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            // Removed since the listing, so its group is gone.
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        let group: AgentGroup = serde_json::from_slice(&bytes)
            .with_context(|| format!("{} isn't an agent entry", path.display()))?;
        if group_is_live(&group, processes) {
            groups.push(group);
        }
    }
    Ok(groups)
}

/// The group's first process still runs, or it exited and left members behind. Its pid now
/// held by a process with another start time is a reused pid, so the group is someone else's.
fn group_is_live(group: &AgentGroup, processes: &mut dyn ProcessSource) -> bool {
    match processes.inspect(group.pid) {
        Some(first) => first.start_time == group.start_time,
        None => processes.group_has_members(group.pgid),
    }
}

/// Why `pid`, or one of its ancestors, counts as an agent; `None` when none does. An error when
/// the registered agents or an ancestor can't be read, so the caller refuses.
pub(crate) fn agent_ancestor(
    pid: u32,
    home: &Paths,
    markers: &AgentMarkers,
) -> anyhow::Result<Option<String>> {
    agent_ancestor_in(&mut Processes::new(), pid, home, markers)
}

/// The same walk over any process table.
pub(crate) fn agent_ancestor_in(
    processes: &mut dyn ProcessSource,
    pid: u32,
    home: &Paths,
    markers: &AgentMarkers,
) -> anyhow::Result<Option<String>> {
    let groups = registered_agents(home, processes)
        .context("the registered agents can't be read, so the ancestry can't be checked")?;
    let own = std::process::id();
    let peer = pid;
    let mut seen = HashSet::new();
    let mut current = Some(pid);
    while let Some(pid) = current {
        if pid == 0 {
            break;
        }
        if !seen.insert(pid) || seen.len() > MAX_DEPTH {
            bail!("the parents of process {peer} loop or run too deep to check");
        }
        // A process that can't be read can't be vouched for, peer or ancestor.
        let Some(process) = processes.inspect(pid) else {
            if pid == peer {
                bail!("process {pid} is gone, so its ancestry can't be checked");
            }
            bail!("process {pid}, an ancestor of {peer}, can't be read, so it can't be checked");
        };
        if pid == own
            && let Some(name) = markers.set_here()
        {
            return Ok(Some(format!("process {pid} has {name} set")));
        }
        let pgid = processes.group_of(pid);
        let registered = groups.iter().find(|group| {
            (group.pid == pid && group.start_time == process.start_time) || pgid == Some(group.pgid)
        });
        if let Some(group) = registered {
            return Ok(Some(format!(
                "process {pid} is in agent process group {}",
                group.pgid
            )));
        }
        current = process.parent;
    }
    Ok(None)
}

/// Refuses when this process runs inside an agent session or descends from an agent.
pub(crate) fn check_self(home: &Paths, markers: &AgentMarkers) -> anyhow::Result<()> {
    if let Some(name) = markers.set_here() {
        bail!("Argonauts Code won't do this inside an agent session ({name} is set).");
    }
    if let Some(reason) = agent_ancestor(std::process::id(), home, markers)? {
        bail!("Argonauts Code won't do this from inside an agent: {reason}.");
    }
    Ok(())
}

/// For `argonauts resume` and `argonauts config set`: the same check,
/// with the built-in markers and `ARGONAUTS_HOME`.
pub fn refuse_inside_agent() -> anyhow::Result<()> {
    let home = Paths::from_env()
        .map_err(|_| anyhow!("can't find your home directory; set ARGONAUTS_HOME"))?;
    check_self(&home, &AgentMarkers::builtin())
}

/// One process as the ancestry walk needs it: links and times, never its environment.
#[derive(Debug, Clone, Default)]
pub(crate) struct ProcessInfo {
    pub parent: Option<u32>,
    /// Seconds since the epoch.
    pub start_time: u64,
}

/// A process table the ancestry walk reads. Tests pass a scripted one.
pub(crate) trait ProcessSource {
    /// `None` when the process is gone or a zombie.
    fn inspect(&mut self, pid: u32) -> Option<ProcessInfo>;
    fn group_of(&mut self, pid: u32) -> Option<u32>;
    /// Whether group `pgid` still has a process we can see.
    fn group_has_members(&mut self, pgid: u32) -> bool;
}

/// Reads processes one at a time, since refreshing the whole table is slow.
pub(crate) struct Processes {
    system: System,
}

impl Processes {
    pub(crate) fn new() -> Self {
        Self {
            system: System::new(),
        }
    }

    /// `None` when the process is gone or a zombie.
    pub(crate) fn start_time(&mut self, pid: u32) -> Option<u64> {
        self.refresh(pid).map(sysinfo::Process::start_time)
    }

    fn refresh(&mut self, pid: u32) -> Option<&sysinfo::Process> {
        let pid = Pid::from_u32(pid);
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing(),
        );
        self.system
            .process(pid)
            .filter(|process| process.status() != ProcessStatus::Zombie)
    }
}

impl ProcessSource for Processes {
    fn inspect(&mut self, pid: u32) -> Option<ProcessInfo> {
        self.refresh(pid).map(|process| ProcessInfo {
            parent: process.parent().map(|parent| parent.as_u32()),
            start_time: process.start_time(),
        })
    }

    fn group_of(&mut self, pid: u32) -> Option<u32> {
        let pid = i32::try_from(pid).ok()?;
        let group = getpgid(Some(NixPid::from_raw(pid))).ok()?;
        u32::try_from(group.as_raw()).ok()
    }

    /// A signal-0 probe. EPERM is a group of zombies on macOS, or someone else's group.
    fn group_has_members(&mut self, pgid: u32) -> bool {
        // 0 and 1 name this process's own group and every process, never an agent's.
        let Some(pgid) = i32::try_from(pgid).ok().filter(|pgid| *pgid > 1) else {
            return false;
        };
        match killpg(NixPid::from_raw(pgid), None) {
            Ok(()) => true,
            Err(Errno::ESRCH | Errno::EPERM) => false,
            Err(_) => true,
        }
    }
}

/// This process's pid and start time, for records keyed by both.
pub(crate) fn own_process() -> (u32, u64) {
    let pid = std::process::id();
    let started = Processes::new().start_time(pid).unwrap_or(0);
    (pid, started)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

    /// Spawns `sh -c script` in its own process group, with `env` set on the shell.
    pub(crate) fn spawn_group(script: &str, env: &[(&str, &str)]) -> Child {
        use std::os::unix::process::CommandExt as _;
        let mut command = Command::new("sh");
        command
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .process_group(0);
        for (name, value) in env {
            command.env(name, value);
        }
        command.spawn().unwrap()
    }

    /// The first child of `parent` sysinfo sees, waiting up to five seconds for it.
    pub(crate) fn child_of(parent: u32) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut system = System::new();
            system.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::nothing(),
            );
            if let Some((pid, _)) = system
                .processes()
                .iter()
                .find(|(_, process)| process.parent() == Some(Pid::from_u32(parent)))
            {
                return pid.as_u32();
            }
            assert!(Instant::now() < deadline, "no child of {parent} appeared");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub(crate) fn write_agent(home: &Paths, group: &AgentGroup) {
        let dir = agents_dir(home);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{}.json", group.pgid)),
            serde_json::to_vec(group).unwrap(),
        )
        .unwrap();
    }

    /// Registers the real group that `pid` is first in.
    pub(crate) fn register_group(home: &Paths, pid: u32) {
        let start_time = Processes::new().start_time(pid).unwrap();
        write_agent(
            home,
            &AgentGroup {
                pgid: pid,
                pid,
                start_time,
            },
        );
    }

    /// Only a name no real agent sets, so a test run from inside an agent still passes.
    pub(crate) fn test_markers() -> AgentMarkers {
        AgentMarkers {
            env: vec!["ARGONAUTS_TEST_ONLY_MARKER".to_owned()],
        }
    }

    fn stop(mut child: Child) {
        let _ = killpg(
            NixPid::from_raw(child.id() as i32),
            nix::sys::signal::SIGKILL,
        );
        let _ = child.wait();
    }

    #[test]
    fn markers_join_the_session_variable_and_settings() {
        let markers = AgentMarkers::builtin();
        assert_eq!(
            markers.env,
            ["ARGONAUTS_SESSION", "OPENCLAW_SHELL", "HERMES_SESSION_ID"]
        );
        let mut settings = Settings::default();
        settings.approvals.refuse_ancestor_env = vec!["MY_AGENT".to_owned(), String::new()];
        assert_eq!(
            AgentMarkers::from_settings(&settings).env,
            ["ARGONAUTS_SESSION", "MY_AGENT"]
        );
    }

    /// A process table written by hand: who started whom, and in which group.
    #[derive(Clone, Default)]
    pub(crate) struct ScriptedProcesses {
        table: HashMap<u32, (ProcessInfo, u32)>,
    }

    impl ScriptedProcesses {
        pub(crate) fn add(&mut self, pid: u32, parent: Option<u32>, group: u32) {
            let info = ProcessInfo {
                parent,
                start_time: 1_000 + u64::from(pid),
            };
            self.table.insert(pid, (info, group));
        }

        /// Agents 20 and 21, each first in its group, started by a clean 10. Peer 30 runs in
        /// 20's group, peer 31 is a child of 21 in a group of its own, and peer 32 sits right
        /// under 10.
        pub(crate) fn agent_shells() -> Self {
            let mut table = Self::default();
            table.add(10, None, 10);
            table.add(20, Some(10), 20);
            table.add(21, Some(10), 21);
            table.add(30, Some(20), 20);
            table.add(31, Some(21), 31);
            table.add(32, Some(10), 10);
            table
        }

        /// Writes the `agents/` entries for the agents of [`Self::agent_shells`].
        pub(crate) fn register_agents(&self, home: &Paths) {
            for pid in [20, 21] {
                let (info, group) = &self.table[&pid];
                write_agent(
                    home,
                    &AgentGroup {
                        pgid: *group,
                        pid,
                        start_time: info.start_time,
                    },
                );
            }
        }
    }

    impl ProcessSource for ScriptedProcesses {
        fn inspect(&mut self, pid: u32) -> Option<ProcessInfo> {
            self.table.get(&pid).map(|(info, _)| info.clone())
        }

        fn group_of(&mut self, pid: u32) -> Option<u32> {
            self.table.get(&pid).map(|(_, group)| *group)
        }

        fn group_has_members(&mut self, pgid: u32) -> bool {
            self.table.values().any(|(_, group)| *group == pgid)
        }
    }

    #[test]
    fn ancestry_finds_a_registered_group_up_the_parent_chain() {
        let home = tempfile::tempdir().unwrap();
        let home = Paths::new(home.path());
        let mut table = ScriptedProcesses::agent_shells();
        table.register_agents(&home);
        let markers = AgentMarkers::builtin();
        let found = |table: &mut ScriptedProcesses, pid| {
            agent_ancestor_in(table, pid, &home, &markers).unwrap()
        };
        let in_group = found(&mut table, 30).unwrap();
        assert!(
            in_group.contains("process 30 is in agent process group 20"),
            "{in_group}"
        );
        let below = found(&mut table, 31).unwrap();
        assert!(
            below.contains("process 21 is in agent process group 21"),
            "{below}"
        );
        assert_eq!(found(&mut table, 32), None);
        assert!(agent_ancestor_in(&mut table, 99, &home, &markers).is_err());
    }

    #[test]
    fn a_group_counts_while_it_has_members_though_its_first_process_left() {
        let home = tempfile::tempdir().unwrap();
        let home = Paths::new(home.path());
        let mut table = ScriptedProcesses::agent_shells();
        table.register_agents(&home);
        // 21 exited; 31 was in its own group, so nothing is left of group 21.
        table.table.remove(&21);
        table.add(31, Some(10), 31);
        assert_eq!(
            registered_agents(&home, &mut table)
                .unwrap()
                .iter()
                .map(|group| group.pgid)
                .collect::<Vec<_>>(),
            [20]
        );
        // 20 exited too, but 30 still runs in its group.
        table.table.remove(&20);
        table.add(30, Some(10), 20);
        let reason = agent_ancestor_in(&mut table, 30, &home, &test_markers())
            .unwrap()
            .unwrap();
        assert!(reason.contains("agent process group 20"), "{reason}");
    }

    #[test]
    fn what_cant_be_read_refuses() {
        let home = tempfile::tempdir().unwrap();
        let home = Paths::new(home.path());
        let markers = AgentMarkers::builtin();
        let check = |table: &mut ScriptedProcesses, pid| {
            agent_ancestor_in(table, pid, &home, &markers)
                .unwrap_err()
                .to_string()
        };

        // An ancestor that can't be read.
        let mut table = ScriptedProcesses::agent_shells();
        table.add(33, Some(77), 33);
        let err = check(&mut table, 33);
        assert!(err.contains("process 77, an ancestor of 33"), "{err}");

        // Parent links that loop.
        let mut table = ScriptedProcesses::default();
        table.add(40, Some(41), 40);
        table.add(41, Some(40), 41);
        let err = check(&mut table, 40);
        assert!(err.contains("loop"), "{err}");

        // An agents entry that doesn't parse.
        let mut table = ScriptedProcesses::agent_shells();
        let dir = agents_dir(&home);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("12.json"), b"{not json").unwrap();
        assert!(registered_agents(&home, &mut table).is_err());
        let err = check(&mut table, 32);
        assert!(err.contains("registered agents"), "{err}");
        std::fs::remove_file(dir.join("12.json")).unwrap();

        // An agents folder that can't be listed.
        std::fs::remove_dir(&dir).unwrap();
        std::fs::write(&dir, b"").unwrap();
        assert!(registered_agents(&home, &mut table).is_err());
        let err = check(&mut table, 32);
        assert!(err.contains("registered agents"), "{err}");
    }

    /// A shell's marker is in its own environment, which the walk never reads, so only a
    /// registered group gives an agent away from outside it.
    #[test]
    fn a_marked_shell_is_found_by_its_record_not_its_environment() {
        let home = tempfile::tempdir().unwrap();
        let home = Paths::new(home.path());
        let markers = AgentMarkers::with(&["ARGONAUTS_TEST_SHELL_MARKER".to_owned()]);
        let shell = spawn_group("sleep 30; true", &[("ARGONAUTS_TEST_SHELL_MARKER", "1")]);
        let sleeper = child_of(shell.id());
        assert_eq!(agent_ancestor(sleeper, &home, &markers).unwrap(), None);
        register_group(&home, shell.id());
        let reason = agent_ancestor(sleeper, &home, &markers).unwrap().unwrap();
        assert!(
            reason.contains(&format!("agent process group {}", shell.id())),
            "{reason}"
        );
        stop(shell);
    }

    #[test]
    fn this_process_is_checked_by_its_own_environment() {
        let home = tempfile::tempdir().unwrap();
        let home = Paths::new(home.path());
        let markers = AgentMarkers {
            env: vec!["PATH".to_owned()],
        };
        let reason = agent_ancestor(std::process::id(), &home, &markers)
            .unwrap()
            .unwrap();
        assert!(reason.contains("has PATH set"), "{reason}");
        assert_eq!(
            agent_ancestor(std::process::id(), &home, &test_markers()).unwrap(),
            None
        );
    }

    #[test]
    fn ancestry_finds_a_registered_group_and_skips_a_reused_pid() {
        let home = tempfile::tempdir().unwrap();
        let home = Paths::new(home.path());
        let shell = spawn_group("sleep 30; true", &[]);
        let sleeper = child_of(shell.id());
        register_group(&home, shell.id());
        assert_eq!(
            registered_agents(&home, &mut Processes::new())
                .unwrap()
                .len(),
            1
        );
        let reason = agent_ancestor(sleeper, &home, &test_markers())
            .unwrap()
            .unwrap();
        assert!(reason.contains("agent process group"), "{reason}");

        // The same pid with another start time is a different process.
        let path = agents_dir(&home).join(format!("{}.json", shell.id()));
        let mut group: AgentGroup = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        group.start_time += 1;
        std::fs::write(&path, serde_json::to_vec(&group).unwrap()).unwrap();
        assert!(
            registered_agents(&home, &mut Processes::new())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            agent_ancestor(sleeper, &home, &test_markers()).unwrap(),
            None
        );
        stop(shell);
    }

    #[test]
    fn a_conductor_record_with_more_fields_still_counts() {
        let home = tempfile::tempdir().unwrap();
        let home = Paths::new(home.path());
        let shell = spawn_group("sleep 30; true", &[]);
        let sleeper = child_of(shell.id());
        let start_time = Processes::new().start_time(shell.id()).unwrap();
        let dir = agents_dir(&home);
        std::fs::create_dir_all(&dir).unwrap();
        let record = serde_json::json!({
            "pgid": shell.id(),
            "pid": shell.id(),
            "start_time": start_time,
            "session": "s-001",
            "agent_id": "codex",
        });
        std::fs::write(
            dir.join(format!("{}.json", shell.id())),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        assert!(
            agent_ancestor(sleeper, &home, &test_markers())
                .unwrap()
                .is_some()
        );
        stop(shell);
    }

    #[test]
    fn a_gone_peer_is_an_error() {
        let home = tempfile::tempdir().unwrap();
        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(agent_ancestor(pid, &Paths::new(home.path()), &AgentMarkers::builtin()).is_err());
    }

    #[test]
    fn check_self_refuses_a_marker_in_this_process() {
        let home = tempfile::tempdir().unwrap();
        let markers = AgentMarkers {
            env: vec!["PATH".to_owned()],
        };
        let err = check_self(&Paths::new(home.path()), &markers).unwrap_err();
        assert!(err.to_string().contains("PATH is set"), "{err}");
    }
}
