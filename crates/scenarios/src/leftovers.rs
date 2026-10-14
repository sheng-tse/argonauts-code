//! Stopping what a scenario left running when it drops, before its folder goes: run hosts
//! keep going after the app closes, by design, and a hanging fake agent would outlive the
//! test. Each pid is checked against its command line first, so a pid reused by another
//! program is left alone.

use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use argonauts_core::layout::run_files;
use argonauts_testkit::journal;

use crate::Scenario;

/// How long the watchers of killed run hosts get to stop the agents they guard.
const SWEEP: Duration = Duration::from_secs(10);

pub(crate) fn stop(scenario: &Scenario) {
    let root = scenario.repo.root();
    let running = hosting(root);
    // Each run host forks a watcher that stops its agents once the run host dies, so the
    // run hosts go first: the ones whose parent isn't another.
    let mut run_hosts: Vec<u32> = running
        .iter()
        .filter(|process| !running.iter().any(|other| other.pid == process.ppid))
        .map(|process| process.pid)
        .collect();
    for dir in journal::run_dirs(scenario.repo.home()) {
        let Some(pid) = recorded_pid(&dir) else {
            continue;
        };
        let run = dir.file_name().unwrap_or_default().to_string_lossy();
        let theirs = command_line(pid)
            .is_some_and(|line| line.contains(" host ") && line.contains(&format!("--run {run}")));
        if theirs && !run_hosts.contains(&pid) {
            run_hosts.push(pid);
        }
    }
    for pid in run_hosts {
        kill_with_group(pid);
    }
    let deadline = Instant::now() + SWEEP;
    while !hosting(root).is_empty() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    for process in hosting(root) {
        kill_with_group(process.pid);
    }

    let bin_dir = scenario.clis.bin_dir().to_string_lossy().into_owned();
    for invocation in scenario.clis.invocations().unwrap_or_default() {
        if command_line(invocation.pid).is_some_and(|line| line.contains(&bin_dir)) {
            kill_with_group(invocation.pid);
        }
    }
}

struct Process {
    pid: u32,
    ppid: u32,
}

/// `argonauts host` processes for the repo at `root`: run hosts and their watchers.
fn hosting(root: &Path) -> Vec<Process> {
    let mut roots = vec![root.to_path_buf()];
    if let Ok(real) = root.canonicalize()
        && real != root
    {
        roots.push(real);
    }
    let needles: Vec<String> = roots
        .iter()
        .map(|root| format!(" host --repo {} ", root.display()))
        .collect();
    let Ok(output) = Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid=,command="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| needles.iter().any(|needle| line.contains(needle.as_str())))
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let pid = words.next()?.parse().ok()?;
            let ppid = words.next()?.parse().ok()?;
            Some(Process { pid, ppid })
        })
        .collect()
}

/// The run host pid a run recorded.
fn recorded_pid(run_dir: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(run_dir.join(run_files::PID)).ok()?;
    text.split(|c: char| !c.is_ascii_digit())
        .find(|part| !part.is_empty())?
        .parse()
        .ok()
}

/// The process's command line, or `None` when it's gone.
fn command_line(pid: u32) -> Option<String> {
    let output = Command::new("/bin/ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let line = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !line.is_empty()).then_some(line)
}

fn group_of(pid: u32) -> Option<u32> {
    let output = Command::new("/bin/ps")
        .args(["-o", "pgid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// SIGKILL to `pid`, and to the rest of its process group when it heads one, as a run host
/// and an agent each do.
fn kill_with_group(pid: u32) {
    let target = if group_of(pid) == Some(pid) {
        format!("-{pid}")
    } else {
        pid.to_string()
    };
    let _ = Command::new("/bin/kill")
        .args(["-s", "KILL", "--", &target])
        .stderr(Stdio::null())
        .status();
}
