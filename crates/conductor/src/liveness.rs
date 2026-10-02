//! Whether a recorded process is still the same one: its pid is alive and its start time
//! matches, so a reused pid is never taken for it. Shared files keyed by process (the agent
//! registry, `vendors.json`, slots) and the reaper all ask this.

use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System};

/// The OS's start time for `pid`, in seconds since the Unix epoch. `None` when it's gone,
/// including a zombie that exited but hasn't been reaped.
pub fn start_time(pid: u32) -> Option<u64> {
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    let process = system.process(pid)?;
    if matches!(
        process.status(),
        ProcessStatus::Zombie | ProcessStatus::Dead
    ) {
        return None;
    }
    Some(process.start_time())
}

/// Whether `pid` is alive and started at `start_time`.
pub fn is_same_process(pid: u32, start_time: u64) -> bool {
    self::start_time(pid) == Some(start_time)
}

/// This process's pid and start time, as other processes will check them.
pub fn this_process() -> Option<(u32, u64)> {
    let pid = std::process::id();
    Some((pid, start_time(pid)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn liveness_rejects_reused_pid() {
        let (me, started) = this_process().unwrap();
        assert!(is_same_process(me, started));
        // The same pid with another start time is a different process that reused the pid.
        assert!(!is_same_process(me, started - 1));
        assert!(!is_same_process(me, started + 1));

        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 30"])
            .spawn()
            .unwrap();
        let pid = child.id();
        let child_started = start_time(pid).unwrap();
        assert!(is_same_process(pid, child_started));
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(start_time(pid), None);
        assert!(!is_same_process(pid, child_started));

        assert_eq!(start_time(u32::MAX - 7), None);
    }
}
