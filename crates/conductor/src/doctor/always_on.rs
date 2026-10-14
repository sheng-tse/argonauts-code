//! Always-on agents on this machine, such as an OpenClaw gateway listening on 18789 or a
//! `hermes gateway` process. They run as you, outside any sandbox Argonauts Code controls, and
//! could reach your approval commands, so doctor names them. It reads no config and no credentials.

use std::ffi::OsString;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use serde::Serialize;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tokio::net::TcpStream;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AlwaysOnAgent {
    pub name: String,
    /// How it was found, such as `listening on 127.0.0.1:18789` or `process 4242`.
    pub evidence: String,
}

/// Loopback ports the agents listen on by default.
pub const KNOWN_PORTS: &[(u16, &str)] = &[
    (18789, "OpenClaw gateway"),
    (9119, "Hermes dashboard"),
    (8642, "Hermes API server"),
];

/// Program names and the subcommand that keeps them running. OpenClaw is found by its port.
const KNOWN_PROCESSES: &[(&str, &str, &str)] = &[("hermes", "gateway", "Hermes gateway")];

const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);

pub async fn always_on_agents() -> Vec<AlwaysOnAgent> {
    let mut found = Vec::new();
    for (port, name) in KNOWN_PORTS {
        if listening(*port).await {
            found.push(AlwaysOnAgent {
                name: (*name).to_owned(),
                evidence: format!("listening on 127.0.0.1:{port}"),
            });
        }
    }
    match tokio::task::spawn_blocking(agent_processes).await {
        Ok(processes) => found.extend(processes),
        Err(err) => tracing::warn!("could not list processes: {err}"),
    }
    found
}

async fn listening(port: u16) -> bool {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    matches!(
        tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address)).await,
        Ok(Ok(_))
    )
}

/// Your own processes whose command line runs a known agent's long-lived subcommand.
fn agent_processes() -> Vec<AlwaysOnAgent> {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_user(UpdateKind::Always),
    );
    let me = sysinfo::get_current_pid()
        .ok()
        .and_then(|pid| system.process(pid))
        .and_then(|process| process.user_id().cloned());
    let mut found: Vec<(u32, &str)> = system
        .processes()
        .iter()
        .filter(|(_, process)| me.is_none() || process.user_id() == me.as_ref())
        .filter_map(|(pid, process)| Some((pid.as_u32(), known_agent(process.cmd())?)))
        .collect();
    found.sort();
    found
        .into_iter()
        .map(|(pid, name)| AlwaysOnAgent {
            name: name.to_owned(),
            evidence: format!("process {pid}"),
        })
        .collect()
}

/// Matches `hermes gateway` whether it runs directly or through an interpreter, such as
/// `python3 /usr/local/bin/hermes gateway run`.
fn known_agent(cmd: &[OsString]) -> Option<&'static str> {
    KNOWN_PROCESSES
        .iter()
        .find(|(program, subcommand, _)| {
            cmd.iter()
                .position(|arg| Path::new(arg).file_name().is_some_and(|n| n == *program))
                .is_some_and(|at| cmd[at + 1..].iter().any(|arg| arg == *subcommand))
        })
        .map(|(_, _, name)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(parts: &[&str]) -> Vec<OsString> {
        parts.iter().map(OsString::from).collect()
    }

    #[test]
    fn command_lines() {
        assert_eq!(
            known_agent(&cmd(&["hermes", "gateway"])),
            Some("Hermes gateway")
        );
        assert_eq!(
            known_agent(&cmd(&[
                "/usr/bin/python3",
                "/opt/bin/hermes",
                "gateway",
                "run"
            ])),
            Some("Hermes gateway")
        );
        assert_eq!(known_agent(&cmd(&["hermes", "chat"])), None);
        assert_eq!(known_agent(&cmd(&["gateway", "hermes"])), None);
        assert_eq!(known_agent(&cmd(&["vim", "hermes-notes.md"])), None);
        assert_eq!(known_agent(&[]), None);
    }

    #[tokio::test]
    async fn doctor_detects_always_on_agents() {
        // A listener on OpenClaw's port, unless something already holds it; either way doctor
        // must report the port.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 18789)).ok();

        let dir = tempfile::tempdir().unwrap();
        let hermes = dir.path().join("hermes");
        std::fs::write(&hermes, "#!/bin/sh\nread line\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hermes, std::fs::Permissions::from_mode(0o755)).unwrap();
        // `sh hermes gateway` keeps that command line while it waits on its stdin.
        let mut child = std::process::Command::new("/bin/sh")
            .arg(&hermes)
            .arg("gateway")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id();
        let hermes_seen = |found: &[AlwaysOnAgent]| {
            found
                .iter()
                .any(|agent| agent.evidence == format!("process {pid}"))
        };

        // A process just started may not show its command line yet on a busy machine.
        let mut found = always_on_agents().await;
        for _ in 0..30 {
            if hermes_seen(&found) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            found = always_on_agents().await;
        }
        child.kill().unwrap();
        child.wait().unwrap();
        drop(listener);

        assert!(
            found.contains(&AlwaysOnAgent {
                name: "OpenClaw gateway".into(),
                evidence: "listening on 127.0.0.1:18789".into(),
            }),
            "{found:?}"
        );
        assert!(
            found.contains(&AlwaysOnAgent {
                name: "Hermes gateway".into(),
                evidence: format!("process {pid}"),
            }),
            "{found:?}"
        );
    }
}
