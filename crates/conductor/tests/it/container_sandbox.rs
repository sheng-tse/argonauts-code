//! The container sandbox end to end, with a real docker or podman. Set
//! `ARGONAUTS_TEST_CONTAINER_IMAGE` to an image with a POSIX shell (such as `alpine:latest`) to
//! run these; without it, or without a runtime, they pass without running.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use argonauts_conductor::sandbox::SandboxWrap;
use argonauts_conductor::sandbox::container::Container;
use argonauts_core::CommandSpec;
use argonauts_core::agents::session_runner::{SandboxMode, SandboxProfile};

const IMAGE_ENV: &str = "ARGONAUTS_TEST_CONTAINER_IMAGE";

fn container() -> Option<Container> {
    let image = std::env::var(IMAGE_ENV)
        .ok()
        .filter(|image| !image.is_empty())?;
    let container = Container::detect(&image);
    container.runtime()?;
    Some(container)
}

fn run(container: &Container, profile: &SandboxProfile, cwd: &Path, script: &str) -> String {
    let command = CommandSpec::new("/bin/sh", cwd).args(["-c", script]);
    let wrapped = container.wrap(command, profile).unwrap();
    let mut process = Command::new(&wrapped.program);
    process
        .args(&wrapped.args)
        .current_dir(&wrapped.cwd)
        .stdin(Stdio::null());
    process.envs(wrapped.env_set.iter().map(|(k, v)| (k, v)));
    let output = process.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "{}\nstdout: {stdout}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

fn said(stdout: &str, line: &str) -> bool {
    stdout.lines().any(|out| out.trim() == line)
}

#[test]
fn a_container_session_sees_its_worktree_and_nothing_else() {
    let Some(container) = container() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let worktree = root.join("work");
    let secrets = worktree.join(".secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    std::fs::write(secrets.join("key"), "secret").unwrap();
    std::fs::write(worktree.join("src.rs"), "mine").unwrap();
    let outside = root.join("outside.txt");
    std::fs::write(&outside, "not for the session").unwrap();
    let state = root.join("state");
    let session: PathBuf = state.join("s1");
    std::fs::create_dir_all(&session).unwrap();
    std::fs::write(session.join("status.json"), "{}").unwrap();
    std::fs::write(session.join("raw.stdout.jsonl"), "raw").unwrap();
    let report = session.join("report.json");
    let profile = SandboxProfile {
        mode: SandboxMode::Container,
        writable: vec![worktree.clone(), report.clone()],
        readable: vec![
            worktree.clone(),
            session.join("status.json"),
            report.clone(),
        ],
        deny_read: vec![state.clone(), secrets.clone()],
        network: false,
        proxy_port: None,
        ask_port: None,
        closed: Vec::new(),
        listable: Vec::new(),
        loopback: false,
        outside: Default::default(),
    };
    let stdout = run(
        &container,
        &profile,
        &worktree,
        &format!(
            r#"
            if cat src.rs >/dev/null; then echo own=ok; fi
            if echo hi > made.txt; then echo write=ok; fi
            if cat {outside} >/dev/null 2>&1; then echo outside=ok; else echo outside=blocked; fi
            if cat {secret} >/dev/null 2>&1; then echo secret=ok; else echo secret=blocked; fi
            if cat {status} >/dev/null 2>&1; then echo status=ok; else echo status=blocked; fi
            if [ "$(cat {raw} 2>/dev/null)" = raw ]; then echo raw=ok; else echo raw=blocked; fi
            if echo '{{}}' > {report}; then echo report=ok; else echo report=blocked; fi
            if echo x > {status} 2>/dev/null; then echo status-write=ok; else echo status-write=blocked; fi
            if wget -q -T 3 -O /dev/null http://1.1.1.1 2>/dev/null; then echo network=ok; else echo network=blocked; fi
            "#,
            outside = outside.display(),
            secret = secrets.join("key").display(),
            status = session.join("status.json").display(),
            raw = session.join("raw.stdout.jsonl").display(),
            report = report.display(),
        ),
    );
    for line in [
        "own=ok",
        "write=ok",
        "outside=blocked",
        "secret=blocked",
        "status=ok",
        "raw=blocked",
        "report=ok",
        "status-write=blocked",
        "network=blocked",
    ] {
        assert!(said(&stdout, line), "expected {line}, got:\n{stdout}");
    }
    assert_eq!(
        std::fs::read_to_string(worktree.join("made.txt")).unwrap(),
        "hi\n"
    );
    assert_eq!(std::fs::read_to_string(&report).unwrap(), "{}\n");
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        "not for the session"
    );
}

/// Where the launcher sits for the docker host to bind into the container, when the test runs
/// somewhere the host can't see its own target folder.
#[cfg(target_os = "linux")]
const LAUNCHER_ENV: &str = "ARGONAUTS_TEST_CONTAINER_LAUNCHER";

/// Without network, the container's CLI still reaches the proxy, through the bridge outside
/// and the relay inside, and nothing else. The image needs bash.
#[cfg(target_os = "linux")]
#[test]
fn a_container_session_without_network_reaches_only_the_proxy() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{SocketAddr, TcpListener};

    let Some(container) = container() else {
        return;
    };
    let launcher = std::env::var_os(LAUNCHER_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_argonauts-sandbox")));
    let container = container.with_launcher(launcher);
    let echo = |name: &'static str| -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || {
                    let mut writer = stream.try_clone().unwrap();
                    for line in BufReader::new(stream).lines().map_while(Result::ok) {
                        if writeln!(writer, "{name}: {line}").is_err() {
                            return;
                        }
                    }
                });
            }
        });
        addr
    };
    let proxy = echo("proxy");
    let other = echo("other");
    let check = |label: &str, addr: SocketAddr, expect: &str| {
        format!(
            r#"
            answer=$( (exec 3<>/dev/tcp/{ip}/{port} && echo ping >&3 && timeout 5 head -n 1 <&3) 2>/dev/null )
            if [ "$answer" = '{expect}' ]; then echo {label}=ok; else echo {label}=blocked; fi
            "#,
            ip = addr.ip(),
            port = addr.port(),
        )
    };
    let dir = tempfile::tempdir().unwrap();
    let worktree = dir.path().canonicalize().unwrap();
    let mut script = check("proxy", proxy, "proxy: ping");
    script.push_str(&check("other-local", other, "other: ping"));
    script.push_str("if echo hi > made.txt; then echo write=ok; fi\n");
    let mut command = CommandSpec::new("/bin/bash", &worktree).args(["-c", script.as_str()]);
    command
        .env_set
        .push(("HTTPS_PROXY".to_owned(), format!("http://{proxy}")));
    let profile = SandboxProfile {
        mode: SandboxMode::Container,
        writable: vec![worktree.clone()],
        readable: vec![worktree.clone()],
        deny_read: Vec::new(),
        network: false,
        proxy_port: None,
        ask_port: None,
        closed: Vec::new(),
        listable: Vec::new(),
        loopback: false,
        outside: Default::default(),
    };
    let wrapped = container.wrap(command, &profile).unwrap();
    let mut process = Command::new(&wrapped.program);
    process
        .args(&wrapped.args)
        .current_dir(&wrapped.cwd)
        .stdin(Stdio::null());
    process.envs(wrapped.env_set.iter().map(|(k, v)| (k, v)));
    let output = process.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    for line in ["proxy=ok", "other-local=blocked", "write=ok"] {
        assert!(said(&stdout, line), "expected {line}, got:\n{stdout}");
    }
}
