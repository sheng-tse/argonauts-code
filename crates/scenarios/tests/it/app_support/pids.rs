//! On Linux a sandboxed session runs in a PID namespace of its own, so the pid a fake CLI
//! records is its number in there, which means nothing to a test outside it.

use argonauts_testkit::Invocation;

/// The invocation with its pid as this test sees it: the process whose innermost pid is the
/// recorded one and whose arguments are the recorded ones. One that has ended, or that ran
/// outside a namespace, keeps the number it recorded.
#[cfg(target_os = "linux")]
pub(super) fn as_seen_here(mut invocation: Invocation) -> Invocation {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return invocation;
    };
    let recorded = invocation.pid.to_string();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse().ok())
        else {
            continue;
        };
        let Ok(status) = std::fs::read_to_string(entry.path().join("status")) else {
            continue;
        };
        let nested = status
            .lines()
            .find_map(|line| line.strip_prefix("NSpid:"))
            .map(|pids| pids.split_whitespace().collect::<Vec<_>>())
            .is_some_and(|pids| pids.len() > 1 && pids.last() == Some(&recorded.as_str()));
        if nested && arguments(&entry.path()).is_some_and(|args| args == invocation.args) {
            invocation.pid = pid;
            break;
        }
    }
    invocation
}

#[cfg(not(target_os = "linux"))]
pub(super) fn as_seen_here(invocation: Invocation) -> Invocation {
    invocation
}

/// The process's arguments after its program name.
#[cfg(target_os = "linux")]
fn arguments(proc_dir: &std::path::Path) -> Option<Vec<String>> {
    let line = std::fs::read(proc_dir.join("cmdline")).ok()?;
    let line = line.strip_suffix(&[0]).unwrap_or(&line);
    let mut args = line
        .split(|byte| *byte == 0)
        .map(|arg| String::from_utf8_lossy(arg).into_owned());
    args.next()?;
    Some(args.collect())
}
