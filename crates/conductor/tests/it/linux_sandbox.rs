//! The Linux sandbox end to end: each test runs a small shell script under the launcher and
//! checks what it could and couldn't do. They need Landlock ABI 3 and unprivileged user
//! namespaces; without them the wrap must refuse, which the last test checks where asked.

#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use argonauts_conductor::sandbox::landlock::Landlock;
use argonauts_conductor::sandbox::{SandboxError, SandboxWrap, self_test};
use argonauts_core::CommandSpec;
use argonauts_core::session_runner::{SandboxMode, SandboxProfile};

const LAUNCHER: &str = env!("CARGO_BIN_EXE_argonauts-sandbox");

/// Set where the kernel refuses user namespaces, as Docker's default seccomp profile does, to
/// check that every session is refused there.
const EXPECT_NO_USERNS: &str = "ARGONAUTS_TEST_NO_USERNS";

/// Set to the `argonauts` binary to run these through the app's own launcher.
const LAUNCHER_ENV: &str = "ARGONAUTS_TEST_LAUNCHER";

fn sandbox() -> Landlock {
    match std::env::var_os(LAUNCHER_ENV) {
        Some(launcher) => Landlock::with_launcher(launcher),
        None => Landlock::with_launcher(LAUNCHER),
    }
}

fn expect_no_userns() -> bool {
    std::env::var_os(EXPECT_NO_USERNS).is_some()
}

/// A scratch folder laid out like a user's home.
struct Scratch {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        Self { _dir: dir, root }
    }

    fn file(&self, path: &str, contents: &str) -> PathBuf {
        let path = self.root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn dir(&self, path: &str) -> PathBuf {
        let path = self.root.join(path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}

fn profile(
    writable: &[&Path],
    readable: &[&Path],
    deny: &[&Path],
    network: bool,
) -> SandboxProfile {
    let paths = |list: &[&Path]| list.iter().map(|path| path.to_path_buf()).collect();
    SandboxProfile {
        mode: SandboxMode::Os,
        writable: paths(writable),
        readable: paths(readable),
        deny_read: paths(deny),
        network,
        proxy_port: None,
        ask_port: None,
        closed: Vec::new(),
    }
}

fn spawn(command: CommandSpec) -> Child {
    let mut process = Command::new(&command.program);
    process
        .args(&command.args)
        .current_dir(&command.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in &command.env_remove {
        process.env_remove(name);
    }
    process.envs(command.env_set.iter().map(|(k, v)| (k, v)));
    process.spawn().unwrap()
}

/// Runs `script` under `profile` in `cwd`, and gives its stdout.
fn run(profile: &SandboxProfile, cwd: &Path, env: &[(&str, String)], script: &str) -> String {
    let mut command = CommandSpec::new("/bin/bash", cwd).args(["-c", script]);
    for (name, value) in env {
        command.env_set.push(((*name).to_owned(), value.clone()));
    }
    let wrapped = sandbox().wrap(command, profile).unwrap();
    let output = spawn(wrapped).wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "the script failed: {}\nstdout: {stdout}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

fn said(stdout: &str, line: &str) -> bool {
    stdout.lines().any(|out| out.trim() == line)
}

fn assert_said(stdout: &str, lines: &[&str]) {
    for line in lines {
        assert!(said(stdout, line), "expected {line}, got:\n{stdout}");
    }
}

#[tokio::test]
async fn the_self_test_passes_under_landlock() {
    if expect_no_userns() {
        return;
    }
    let scratch = Scratch::new();
    let result = self_test(&sandbox(), &scratch.root.join("probe")).await;
    assert!(result.ok, "{}", result.detail);
    assert!(
        result.detail.starts_with("landlock blocks"),
        "{}",
        result.detail
    );
}

#[test]
fn writes_stay_in_the_worktree_and_denied_files_stay_unread() {
    if expect_no_userns() {
        return;
    }
    let s = Scratch::new();
    let worktree = s.dir("work");
    let outside = s.file("outside.txt", "keep");
    let secret = s.file("secrets/key", "secret");
    let stdout = run(
        &profile(&[&worktree], &[&worktree], &[&s.root.join("secrets")], true),
        &worktree,
        &[],
        &format!(
            r#"
            if echo hi > new.txt; then echo worktree=ok; else echo worktree=blocked; fi
            if echo bad > {outside}; then echo outside=ok; else echo outside=blocked; fi
            if truncate -s 0 {outside} 2>/dev/null; then echo truncate=ok; else echo truncate=blocked; fi
            if rm -f {outside} 2>/dev/null && [ ! -e {outside} ]; then echo remove=ok; else echo remove=blocked; fi
            if cat {secret} >/dev/null 2>&1; then echo secret=ok; else echo secret=blocked; fi
            if ls {secrets} >/dev/null 2>&1 && [ -n "$(ls -A {secrets} 2>/dev/null)" ]; then echo list=ok; else echo list=blocked; fi
            if echo x > {secrets}/planted 2>/dev/null; then echo plant=ok; else echo plant=blocked; fi
            if echo x > /dev/null; then echo devnull=ok; fi
            "#,
            outside = outside.display(),
            secret = secret.display(),
            secrets = s.root.join("secrets").display(),
        ),
    );
    assert_said(
        &stdout,
        &[
            "worktree=ok",
            "outside=blocked",
            "truncate=blocked",
            "remove=blocked",
            "secret=blocked",
            "list=blocked",
            "plant=blocked",
            "devnull=ok",
        ],
    );
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
    assert_eq!(
        std::fs::read_to_string(worktree.join("new.txt")).unwrap(),
        "hi\n"
    );
}

/// The session starts inside its worktree, which sits inside the denied `~/.argonauts`. A
/// folder handle from before the covers went up would reach back out through `..`.
#[test]
fn a_worktree_inside_a_denied_folder_cant_climb_out() {
    if expect_no_userns() {
        return;
    }
    let s = Scratch::new();
    let state = s.dir(".argonauts");
    let mine = s.dir(".argonauts/wt/mine");
    s.file(".argonauts/wt/mine/src.rs", "mine");
    s.file(".argonauts/wt/other/hidden_test.rs", "hidden");
    s.file(".argonauts/runs/r1/approve.token", "token");
    let stdout = run(
        &profile(&[&mine], &[&mine], &[&state], true),
        &mine,
        &[],
        r#"
        if cat src.rs >/dev/null; then echo own=ok; fi
        if cat ../other/hidden_test.rs >/dev/null 2>&1; then echo sibling=ok; else echo sibling=blocked; fi
        if ls .. >/dev/null 2>&1; then echo parent-list=ok; else echo parent-list=blocked; fi
        if cat ../../runs/r1/approve.token >/dev/null 2>&1; then echo token=ok; else echo token=blocked; fi
        if echo x > made.rs; then echo write=ok; fi
        pwd
        "#,
    );
    assert_said(
        &stdout,
        &[
            "own=ok",
            "sibling=blocked",
            "parent-list=blocked",
            "token=blocked",
            "write=ok",
            &mine.display().to_string(),
        ],
    );
}

/// Argonauts Code replaces the status file by renaming a new one over it, and the session must
/// see each new one. Its raw log, beside it, stays unreadable, and its report file can be
/// written though it wasn't there at launch.
#[test]
fn session_files_stay_current_and_private_ones_hidden() {
    if expect_no_userns() {
        return;
    }
    let s = Scratch::new();
    let state = s.dir(".argonauts");
    let session = s.dir(".argonauts/runs/r1/sessions/s1");
    let worktree = s.dir("work");
    let status = s.file(".argonauts/runs/r1/sessions/s1/status.json", "first");
    s.file(".argonauts/runs/r1/sessions/s1/raw.stdout.jsonl", "raw");
    let report = session.join("report.json");
    let marker = worktree.join("go");
    let profile = profile(
        &[&worktree, &report],
        &[&worktree, &status, &report],
        &[&state],
        true,
    );
    let script = format!(
        r#"
        cat {status}; echo
        while [ ! -e {marker} ]; do sleep 0.05; done
        cat {status}; echo
        if cat {raw} >/dev/null 2>&1; then echo raw=ok; else echo raw=blocked; fi
        if echo '{{"status":"done"}}' > {report}; then echo report=ok; else echo report=blocked; fi
        if echo x > {session}/planted 2>/dev/null; then echo plant=ok; else echo plant=blocked; fi
        if ls {runs} >/dev/null 2>&1; then echo runs=ok; else echo runs=blocked; fi
        "#,
        status = status.display(),
        marker = marker.display(),
        raw = session.join("raw.stdout.jsonl").display(),
        report = report.display(),
        session = session.display(),
        runs = s.root.join(".argonauts/runs").display(),
    );
    let command = CommandSpec::new("/bin/bash", &worktree).args(["-c", script.as_str()]);
    let mut child = spawn(sandbox().wrap(command, &profile).unwrap());
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    stdout.read_line(&mut first).unwrap();
    assert_eq!(first.trim(), "first");

    let staging = session.join("status.json.tmp");
    std::fs::write(&staging, "second").unwrap();
    std::fs::rename(&staging, &status).unwrap();
    std::fs::write(&marker, "").unwrap();

    let mut rest = String::new();
    stdout.read_to_string(&mut rest).unwrap();
    assert!(child.wait().unwrap().success());
    assert_said(
        &rest,
        &[
            "second",
            "raw=blocked",
            "report=ok",
            "plant=blocked",
            "runs=blocked",
        ],
    );
    assert_eq!(
        std::fs::read_to_string(&report).unwrap(),
        "{\"status\":\"done\"}\n"
    );
    assert!(!session.join("planted").exists());
}

/// A CLI's config sits inside its writable state, and stays read-only so a session can't
/// plant a hook that runs outside any sandbox the next time you start the CLI.
#[test]
fn config_inside_writable_state_stays_read_only() {
    if expect_no_userns() {
        return;
    }
    let s = Scratch::new();
    let state = s.dir(".tool");
    let settings = s.file(".tool/settings.json", "{}");
    let hooks = s.dir(".tool/hooks");
    let history = s.file(".tool/history.jsonl", "your prompts");
    let worktree = s.dir("work");
    let stdout = run(
        &profile(
            &[&worktree, &state],
            &[&worktree, &settings, &hooks],
            &[&history],
            true,
        ),
        &worktree,
        &[],
        &format!(
            r#"
            if echo x > {state}/session.log; then echo state=ok; fi
            if cat {settings} >/dev/null; then echo settings-read=ok; fi
            if echo '{{"hooks":1}}' > {settings} 2>/dev/null; then echo settings-write=ok; else echo settings-write=blocked; fi
            if echo x > {hooks}/pre.sh 2>/dev/null; then echo hook=ok; else echo hook=blocked; fi
            if cat {history} >/dev/null 2>&1; then echo history=ok; else echo history=blocked; fi
            if mv {settings} {state}/moved 2>/dev/null; then echo move=ok; else echo move=blocked; fi
            "#,
            state = state.display(),
            settings = settings.display(),
            hooks = hooks.display(),
            history = history.display(),
        ),
    );
    assert_said(
        &stdout,
        &[
            "state=ok",
            "settings-read=ok",
            "settings-write=blocked",
            "hook=blocked",
            "history=blocked",
            "move=blocked",
        ],
    );
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), "{}");
}

/// Answers each line with `proxy: <line>`, in place of Argonauts Code's proxy.
fn echo_server(name: &'static str) -> SocketAddr {
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
}

/// Talks to `addr` with bash's `/dev/tcp`, and prints `<label>=ok` when the answer comes back,
/// `<label>=blocked` when nothing does.
fn tcp_check(label: &str, addr: SocketAddr, expect: &str) -> String {
    format!(
        r#"
        answer=$( (exec 3<>/dev/tcp/{ip}/{port} && echo ping >&3 && timeout 5 head -n 1 <&3) 2>/dev/null )
        if [ "$answer" = '{expect}' ]; then echo {label}=ok
        elif [ -n "$answer" ]; then echo {label}=wrong
        else echo {label}=blocked; fi
        "#,
        ip = addr.ip(),
        port = addr.port(),
    )
}

#[test]
fn without_network_only_the_proxy_is_reachable() {
    if expect_no_userns() {
        return;
    }
    let s = Scratch::new();
    let worktree = s.dir("work");
    let proxy = echo_server("proxy");
    let other = echo_server("other");
    let proxy_url = format!("http://{proxy}");
    let mut script = tcp_check("proxy", proxy, "proxy: ping");
    script.push_str(&tcp_check("other-local", other, "other: ping"));
    script.push_str(
        r#"
        if timeout 5 bash -c 'exec 3<>/dev/tcp/1.1.1.1/443' 2>/dev/null; then echo internet=ok; else echo internet=blocked; fi
        if timeout 5 getent hosts example.com >/dev/null 2>&1; then echo dns=ok; else echo dns=blocked; fi
        # Loopback inside works, for test suites that start their own servers.
        if command -v python3 >/dev/null; then
            python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); s.listen(); c=socket.create_connection(s.getsockname()); print("own-loopback=ok")'
        else
            echo own-loopback=ok
        fi
        "#,
    );
    let stdout = run(
        &profile(&[&worktree], &[&worktree], &[], false),
        &worktree,
        &[("HTTPS_PROXY", proxy_url)],
        &script,
    );
    assert_said(
        &stdout,
        &[
            "proxy=ok",
            "other-local=blocked",
            "internet=blocked",
            "dns=blocked",
            "own-loopback=ok",
        ],
    );
}

#[test]
fn with_network_the_host_is_reachable() {
    if expect_no_userns() {
        return;
    }
    let s = Scratch::new();
    let worktree = s.dir("work");
    let other = echo_server("other");
    let stdout = run(
        &profile(&[&worktree], &[&worktree], &[], true),
        &worktree,
        &[],
        &tcp_check("other-local", other, "other: ping"),
    );
    assert_said(&stdout, &["other-local=ok"]);
}

/// A web server on loopback that answers `ok` to every request.
fn http_ok_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut head = Vec::new();
                let mut buf = [0u8; 512];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(read) => head.extend_from_slice(&buf[..read]),
                    }
                }
                let reply = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
                let _ = stream.write_all(reply.as_bytes());
            });
        }
    });
    port
}

/// The Argonauts Code proxy itself, as a session's profile names it: a tunnel opens to a listed
/// host, and nothing opens to an unlisted one or around the proxy.
#[test]
fn a_proxied_session_reaches_only_its_hosts_through_the_proxy() {
    use argonauts_conductor::sandbox::proxy::{self, Allowlist, Proxy, Resolver, Why};

    if expect_no_userns() {
        return;
    }
    let s = Scratch::new();
    let worktree = s.dir("work");
    let upstream = http_ok_server();
    // Every host resolves to the upstream, so no test traffic leaves this machine.
    let resolver: Resolver = std::sync::Arc::new(move |_host, _port| {
        Box::pin(async move { Ok(vec![SocketAddr::from(([127, 0, 0, 1], upstream))]) })
    });
    let proxy = Proxy::with_resolver(resolver).unwrap();
    let allow = Allowlist::parse([format!("api.example.test:{upstream}").as_str()]).unwrap();
    let env = proxy::session_env(&proxy.session_url(&allow));
    let env: Vec<(&str, String)> = env
        .iter()
        .map(|(name, value)| (name.as_str(), value.clone()))
        .collect();
    let profile = SandboxProfile {
        proxy_port: Some(proxy.port()),
        ..profile(&[&worktree], &[&worktree], &[], false)
    };
    // curl finds the proxy and its key in `http_proxy`; `-p` asks for a tunnel.
    let script = format!(
        r#"
        curl -sp --max-time 5 "http://api.example.test:{upstream}/" || printf no-tunnel; echo
        if curl -s --noproxy '*' --max-time 3 "http://127.0.0.1:{upstream}/" >/dev/null 2>&1
        then echo direct; else echo no-direct; fi
        if curl -sp --max-time 5 "http://evil.example.test:{upstream}/" >/dev/null 2>&1
        then echo evil; else echo no-evil; fi
        "#
    );
    let stdout = run(&profile, &worktree, &env, &script);
    assert_eq!(stdout, "ok\nno-direct\nno-evil\n");
    let refusals = proxy.refusals();
    assert_eq!(refusals.len(), 1, "{refusals:?}");
    assert_eq!(
        (refusals[0].host.as_str(), refusals[0].why),
        ("evil.example.test", Why::NotListed)
    );
}

/// The relay and connector a session without network leaves behind end with it.
#[test]
fn proxy_helpers_end_with_the_session() {
    if expect_no_userns() {
        return;
    }
    let s = Scratch::new();
    let worktree = s.dir("work");
    let proxy = echo_server("proxy");
    let mut command = CommandSpec::new("/bin/bash", &worktree).args(["-c", "echo $$"]);
    command
        .env_set
        .push(("HTTPS_PROXY".to_owned(), format!("http://{proxy}")));
    let wrapped = sandbox()
        .wrap(command, &profile(&[&worktree], &[&worktree], &[], false))
        .unwrap();
    let child = spawn(wrapped);
    let session = child.id();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let left = helpers(session, &proxy.to_string());
        if left.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "helpers outlived the session: {left:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Live processes left from the session `pid`: its children, and anything still running the
/// launcher with this test's own proxy address.
fn helpers(pid: u32, proxy: &str) -> Vec<u32> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Some(process) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // After the command name come the state and the parent's pid.
        let Some((_, after)) = stat.rsplit_once(')') else {
            continue;
        };
        let mut fields = after.split_whitespace();
        let state = fields.next().unwrap_or("");
        let parent = fields.next().and_then(|p| p.parse::<u32>().ok());
        let cmdline = std::fs::read(entry.path().join("cmdline")).unwrap_or_default();
        let ours = String::from_utf8_lossy(&cmdline).contains(proxy);
        if state != "Z" && (parent == Some(pid) || ours) {
            found.push(process);
        }
    }
    found
}

/// A process of this test's own, as one of your programs would be.
struct Outside(Child);

impl Outside {
    fn start(program: &str, args: &[&str], env: &[(&str, &str)]) -> Self {
        let mut command = Command::new(program);
        command.args(args).envs(env.iter().copied());
        Self(command.stdout(Stdio::piped()).spawn().unwrap())
    }

    /// Its first line of output.
    fn first_line(&mut self) -> String {
        let mut line = String::new();
        BufReader::new(self.0.stdout.as_mut().unwrap())
            .read_line(&mut line)
            .unwrap();
        line.trim().to_owned()
    }
}

impl Drop for Outside {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The session's `/dev/pts` and `/dev/shm` are its own: your terminals' ptys aren't there to take
/// escape sequences, and nothing in the system's shared memory reaches the session or comes back.
#[test]
fn a_session_has_its_own_ptys_and_shared_memory() {
    if expect_no_userns() {
        return;
    }
    let s = Scratch::new();
    let worktree = s.dir("work");
    let mut terminal = Outside::start(
        "python3",
        &[
            "-c",
            "import os, pty, time; m, s = pty.openpty(); print(os.ttyname(s), flush=True); time.sleep(60)",
        ],
        &[],
    );
    let yours = terminal.first_line();
    assert!(yours.starts_with("/dev/pts/"), "{yours}");
    let tag = s.root.file_name().unwrap().to_string_lossy().into_owned();
    let host_shm = PathBuf::from(format!("/dev/shm/argonauts-host-{tag}"));
    std::fs::write(&host_shm, "host").unwrap();
    let session_shm = PathBuf::from(format!("/dev/shm/argonauts-session-{tag}"));
    let stdout = run(
        &profile(&[&worktree], &[&worktree], &[], false),
        &worktree,
        &[],
        &format!(
            r#"
            if [ -e {yours} ]; then echo your-pty=visible; else echo your-pty=hidden; fi
            if (printf 'x' > {yours}) 2>/dev/null; then echo your-pty=written; fi
            python3 -c 'import os, pty; m, s = pty.openpty(); print("own-pty=" + os.ttyname(s))'
            if [ -e {host_shm} ]; then echo host-shm=visible; else echo host-shm=hidden; fi
            if echo x > {session_shm}; then echo own-shm=ok; fi
            "#,
            host_shm = host_shm.display(),
            session_shm = session_shm.display(),
        ),
    );
    let _ = std::fs::remove_file(&host_shm);
    let leaked = session_shm.exists();
    let _ = std::fs::remove_file(&session_shm);
    assert_said(
        &stdout,
        &[
            "your-pty=hidden",
            "own-pty=/dev/pts/0",
            "host-shm=hidden",
            "own-shm=ok",
        ],
    );
    assert!(!said(&stdout, "your-pty=written"), "{stdout}");
    assert!(
        !leaked,
        "the session's shared memory reached the system's /dev/shm"
    );
}

/// The session has a PID namespace of its own: no process of yours shows, so none can be
/// signalled or have its environment read, such as another session's proxy key.
#[test]
fn a_session_sees_and_signals_only_its_own_processes() {
    if expect_no_userns() {
        return;
    }
    let s = Scratch::new();
    let worktree = s.dir("work");
    let yours = Outside::start("sleep", &["60"], &[("ARGONAUTS_TEST_SECRET", "s3cret-key")]);
    let pid = yours.0.id();
    let stdout = run(
        &profile(&[&worktree], &[&worktree], &[], false),
        &worktree,
        &[],
        &format!(
            r#"
            if [ -e /proc/{pid} ]; then echo yours=visible; else echo yours=hidden; fi
            if grep -q s3cret-key /proc/{pid}/environ 2>/dev/null; then echo environ=read; else echo environ=blocked; fi
            if kill -0 {pid} 2>/dev/null; then echo signal=ok; else echo signal=blocked; fi
            echo "me=$$"
            echo "processes=$(ls /proc | grep -c '^[0-9][0-9]*$')"
            "#
        ),
    );
    assert_said(
        &stdout,
        &["yours=hidden", "environ=blocked", "signal=blocked", "me=2"],
    );
    let processes: usize = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("processes="))
        .and_then(|count| count.parse().ok())
        .unwrap();
    assert!(processes < 10, "{stdout}");
}

/// The launcher stays outside the namespace and ends the way the CLI did, by its exit code
/// or its signal, and whatever the CLI left running ends with it.
#[test]
fn the_launcher_ends_as_the_cli_did_and_takes_its_leftovers() {
    if expect_no_userns() {
        return;
    }
    let s = Scratch::new();
    let worktree = s.dir("work");
    let status_of = |script: &str| {
        let command = CommandSpec::new("/bin/bash", &worktree).args(["-c", script]);
        let wrapped = sandbox()
            .wrap(command, &profile(&[&worktree], &[&worktree], &[], false))
            .unwrap();
        spawn(wrapped).wait_with_output().unwrap().status
    };
    assert_eq!(status_of("exit 7").code(), Some(7));
    assert_eq!(status_of("kill -TERM $$").signal(), Some(15));

    let marker = format!(
        "argonauts-leftover-{}",
        s.root.file_name().unwrap().to_string_lossy()
    );
    assert!(status_of(&format!("(exec -a {marker} sleep 300) & exit 0")).success());
    let deadline = Instant::now() + Duration::from_secs(5);
    while still_running(&marker) {
        assert!(Instant::now() < deadline, "{marker} outlived its session");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Whether any live process's command line starts with `marker`.
fn still_running(marker: &str) -> bool {
    std::fs::read_dir("/proc").unwrap().flatten().any(|entry| {
        let cmdline = std::fs::read(entry.path().join("cmdline")).unwrap_or_default();
        let state = std::fs::read_to_string(entry.path().join("stat")).unwrap_or_default();
        let zombie = state
            .rsplit_once(')')
            .is_some_and(|(_, after)| after.trim_start().starts_with('Z'));
        cmdline.starts_with(marker.as_bytes()) && !zombie
    })
}

/// Without network a session opens no Unix socket, so it can't reach yours, such as tmux's in
/// temp, while socket pairs still work. With network on, as you choose, it can.
#[test]
fn without_network_a_session_opens_no_unix_socket() {
    if expect_no_userns() {
        return;
    }
    let s = Scratch::new();
    let worktree = s.dir("work");
    let temp = s.dir("tmp");
    let socket = temp.join("tmux-default");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            drop(stream);
        }
    });
    let script = format!(
        r#"
python3 - <<'PY'
import ctypes, socket
try:
    s = socket.socket(socket.AF_UNIX)
    s.settimeout(2)
    s.connect("{socket}")
    print("connect=ok")
except OSError:
    print("connect=blocked")
a, b = socket.socketpair()
a.send(b"x")
print("pair=ok" if b.recv(1) == b"x" else "pair=bad")
libc = ctypes.CDLL(None, use_errno=True)
params = ctypes.create_string_buffer(256)
ring = libc.syscall(425, 4, params)
print("uring=ok" if ring >= 0 else "uring=errno-%d" % ctypes.get_errno())
PY
"#,
        socket = socket.display()
    );
    let off = run(
        &profile(&[&worktree, &temp], &[&worktree], &[], false),
        &worktree,
        &[],
        &script,
    );
    assert_said(&off, &["connect=blocked", "pair=ok", "uring=errno-1"]);
    let on = run(
        &profile(&[&worktree, &temp], &[&worktree], &[], true),
        &worktree,
        &[],
        &script,
    );
    assert_said(&on, &["connect=ok", "pair=ok"]);
}

#[test]
fn the_wrap_refuses_where_user_namespaces_are_blocked() {
    if !expect_no_userns() {
        return;
    }
    let err = sandbox()
        .wrap(
            CommandSpec::new("/bin/sh", "/"),
            &profile(&[Path::new("/tmp")], &[], &[], false),
        )
        .unwrap_err();
    assert!(
        matches!(&err, SandboxError::Unavailable(why) if why.contains("user namespace")),
        "{err}"
    );
}

#[tokio::test]
async fn the_self_test_fails_closed_where_user_namespaces_are_blocked() {
    if !expect_no_userns() {
        return;
    }
    let scratch = Scratch::new();
    let result = self_test(&sandbox(), &scratch.root.join("probe")).await;
    assert!(!result.ok);
    assert!(
        result.detail.contains("user namespace"),
        "{}",
        result.detail
    );
}

/// Claude Code, Codex, and agy, played by the fake CLI, each run a session in the sandbox
/// without network: it writes its worktree and reaches the proxy it is pointed at, nothing else.
#[test]
fn fake_cli_sessions_run_in_the_sandbox() {
    use argonauts_testkit::FakeClis;
    use argonauts_testkit::scenario::{FakeRule, FakeScenario, FakeStep};

    if expect_no_userns() {
        return;
    }
    let s = Scratch::new();
    let worktree = s.dir("work");
    let fakes = s.dir("fakes");
    let state = s.dir(".argonauts");
    let token = s.file(".argonauts/runs/r1/approve.token", "token");
    let proxy = echo_server("proxy");
    let mut scenario = FakeScenario::default();
    for program in ["claude", "codex", "agy"] {
        scenario = scenario.rule(
            FakeRule::for_program(program)
                .step(FakeStep::Emit(format!(
                    "{{\"type\":\"result\",\"from\":\"{program}\"}}"
                )))
                .step(FakeStep::Write {
                    path: format!("{program}.txt"),
                    content: "written in the sandbox\n".into(),
                })
                .step(FakeStep::Exit(0)),
        );
    }
    let scenario_path = fakes.join("scenario.toml");
    scenario.save(&scenario_path).unwrap();
    let agent_ids = FakeClis::install(&fakes, &scenario_path).unwrap();
    let profile = profile(&[&worktree, &fakes], &[&worktree], &[&state], false);

    for program in ["claude", "codex", "agy"] {
        let mut command = CommandSpec::new(
            agent_ids.bin_dir().join(program).to_string_lossy(),
            &worktree,
        )
        .args(["-p", "hello"]);
        for (name, value) in agent_ids.envs() {
            command
                .env_set
                .push((name, value.to_string_lossy().into_owned()));
        }
        command
            .env_set
            .push(("HTTPS_PROXY".to_owned(), format!("http://{proxy}")));
        let output = spawn(sandbox().wrap(command, &profile).unwrap())
            .wait_with_output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{program}: {}\n{stdout}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains(&format!("\"from\":\"{program}\"")),
            "{stdout}"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join(format!("{program}.txt"))).unwrap(),
            "written in the sandbox\n"
        );
    }
    let invocations = agent_ids.invocations().unwrap();
    assert_eq!(invocations.len(), 3);
    for invocation in &invocations {
        assert!(invocation.env.iter().any(|name| name == "HTTPS_PROXY"));
        assert_eq!(invocation.cwd, worktree);
    }

    // The same fake, asked to read the run's approval token, can't.
    let mut command = CommandSpec::new("/bin/cat", &worktree).arg(token.to_string_lossy());
    command.env_set.push(("LANG".to_owned(), "C".to_owned()));
    let output = spawn(sandbox().wrap(command, &profile).unwrap())
        .wait_with_output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

/// A container's proxy path without the container: the bridge outside, and the relay in a fresh
/// network namespace from `unshare`, as `--network none` gives a container.
#[test]
fn the_container_bridge_and_relay_reach_only_the_proxy() {
    use argonauts_conductor::sandbox::launcher::{bridge_args, relay_args};

    if expect_no_userns() || !Path::new("/usr/bin/unshare").exists() {
        return;
    }
    let proxy = echo_server("proxy");
    let other = echo_server("other");
    let dir = PathBuf::from(format!("/tmp/argonauts-bridge-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut script = tcp_check("proxy", proxy, "proxy: ping");
    script.push_str(&tcp_check("other-local", other, "other: ping"));
    script.push_str(
        "if timeout 5 bash -c 'exec 3<>/dev/tcp/1.1.1.1/443' 2>/dev/null; then echo internet=ok; \
         else echo internet=blocked; fi\n",
    );
    let mut args = bridge_args(&dir, &[proxy], "/usr/bin/unshare");
    args.extend(["-rn".to_owned(), LAUNCHER.to_owned()]);
    args.extend(relay_args(&dir, &[proxy], "/bin/bash"));
    args.extend(["-c".to_owned(), script]);
    let output = Command::new(LAUNCHER).args(&args).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_said(
        &stdout,
        &["proxy=ok", "other-local=blocked", "internet=blocked"],
    );
    assert!(!dir.exists(), "the bridge left its folder behind");
}
