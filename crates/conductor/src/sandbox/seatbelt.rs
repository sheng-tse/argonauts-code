//! macOS Seatbelt: `sandbox-exec -p <profile>` with an SBPL profile generated per session.
//!
//! Everything is allowed by default, then writes are denied everywhere but the profile's
//! writable paths, and reads and socket connections are denied in its denied areas. Seatbelt
//! applies the last rule that matches, so rules go out shallowest path first and an exception
//! beats its parent.
//!
//! macOS refuses to start a Seatbelt inside a restrictive one, so a CLI's own sandbox can't
//! run in this wrap.

use std::path::Path;

use argonauts_core::CommandSpec;
use argonauts_core::session_runner::{SandboxMode, SandboxProfile};

use super::profile::{PathRule, RuleKind, check_profile, ordered_rules};
use super::{SandboxError, SandboxWrap, describe_network};
use crate::process::resolve_program;

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Devices any program writes: output sinks, its terminal, and ptys for CLIs that start
/// commands in one. A session may open only the ptys it made itself, which carry the pty
/// extension. Any other `/dev/ttys*` is one of your terminals, and raw escape sequences written
/// there could set titles, fill the clipboard, or type into your shell.
const DEVICE_WRITES: &str = r#"(allow file-write*
    (literal "/dev/null")
    (literal "/dev/zero")
    (literal "/dev/tty")
    (literal "/dev/dtracehelper")
    (literal "/dev/ptmx")
    (subpath "/dev/fd"))
(deny file-read* file-write* (regex #"^/dev/ttys[0-9]+$"))
(allow pseudo-tty)
(allow file-read* file-write* file-ioctl
    (require-all
        (regex #"^/dev/ttys[0-9]+$")
        (extension "com.apple.sandbox.pty")))
"#;

/// Tools that hand work to launchd or LaunchServices, which run it outside the sandbox. A copy
/// of one, or a compiled program, still meets the services below, and launchd itself refuses a
/// sandboxed caller.
const LAUNCHERS: &str = r#"(deny process-exec
    (literal "/usr/bin/open")
    (literal "/bin/launchctl")
    (literal "/usr/bin/osascript")
    (literal "/usr/bin/shortcuts")
    (literal "/usr/bin/automator"))
"#;

/// Services that act for you outside the sandbox: LaunchServices opens apps and URLs, Apple
/// Events drive other apps, the pasteboard is your clipboard, and the window server takes
/// synthetic keystrokes. No CLI needs them, and the agents' own sandboxes close them too.
const OUTSIDE_SERVICES: &str = r#"(deny mach-lookup
    (global-name "com.apple.coreservices.launchservicesd")
    (global-name "com.apple.coreservices.appleevents")
    (global-name "com.apple.pasteboard.1")
    (global-name "com.apple.windowserver.active"))
"#;

/// Signals reach only the session's own processes, never your shell, your editor, or the
/// run host. Node opens its debugger on SIGUSR1, so a signal could hand a session another
/// program to drive.
const SIGNALS: &str = "(deny signal)\n(allow signal (target same-sandbox))\n";

/// Everything is blocked, DNS and Unix sockets included, though a local server may still listen.
/// What may be reached comes after: one loopback port, or all of them.
const NETWORK_OFF: &str = r#"(deny network*)
(allow network-bind network-inbound (local ip "localhost:*"))
"#;

/// For checks: test suites start local servers and connect to them.
const LOOPBACK: &str = "(allow network-outbound (remote ip \"localhost:*\"))\n";

#[derive(Debug, Clone)]
pub struct Seatbelt {
    program: String,
}

impl Seatbelt {
    pub fn new() -> Self {
        Self {
            program: SANDBOX_EXEC.to_owned(),
        }
    }
}

impl Default for Seatbelt {
    fn default() -> Self {
        Self::new()
    }
}

/// The SBPL text for `profile`.
pub fn render_profile(profile: &SandboxProfile) -> String {
    let mut sbpl = String::from("(version 1)\n(allow default)\n(deny file-write*)\n");
    sbpl.push_str(DEVICE_WRITES);
    sbpl.push_str(LAUNCHERS);
    sbpl.push_str(OUTSIDE_SERVICES);
    sbpl.push_str(SIGNALS);
    for rule in ordered_rules(profile) {
        push_rule(&mut sbpl, &rule, profile.network);
    }
    if !profile.network {
        sbpl.push_str(NETWORK_OFF);
        match profile.proxy_port {
            Some(port) if !profile.loopback => sbpl.push_str(&format!(
                "(allow network-outbound (remote ip \"localhost:{port}\"))\n"
            )),
            _ => sbpl.push_str(LOOPBACK),
        }
        // A direct turn's questions go to the app, which may be asked before an edit.
        if let Some(port) = profile.ask_port.filter(|_| profile.proxy_port.is_some()) {
            sbpl.push_str(&format!(
                "(allow network-outbound (remote ip \"localhost:{port}\"))\n"
            ));
        }
    }
    sbpl
}

fn push_rule(sbpl: &mut String, rule: &PathRule, network: bool) {
    let (actions, filter): (&[&str], &str) = match rule.kind {
        RuleKind::Write => (&["allow file-write*"], "subpath"),
        RuleKind::Deny => (&["deny file-read* file-write*"], "subpath"),
        RuleKind::Stat => (&["allow file-read-metadata"], "literal"),
        RuleKind::List => (&["allow file-read-metadata file-read-data"], "literal"),
        RuleKind::Read => (&["allow file-read*"], "subpath"),
        RuleKind::ReadOnly => (&["allow file-read*", "deny file-write*"], "subpath"),
    };
    // Seatbelt matches the resolved path. The path as given is kept too, in case the two are
    // different only because the file isn't there yet.
    let mut paths = vec![rule.resolved.as_path()];
    if rule.path != rule.resolved {
        paths.push(&rule.path);
    }
    let filters = |filter: &str| -> Vec<String> {
        paths
            .iter()
            .map(|path| format!("({filter} \"{}\")", escape(path)))
            .collect()
    };
    for action in actions {
        push_action(sbpl, action, &filters(filter));
    }
    // A Unix socket connect is checked as network, not as a file read, so a denied area needs
    // its own rule: the run's control socket and docker's sit in one. With network on, a
    // writable folder inside one, such as the session's worktree, may hold the session's own
    // sockets. With it off the session can't make a socket at all, and the only ones a writable
    // folder could offer are other programs', such as tmux's or an editor's in temp.
    let sockets = match rule.kind {
        RuleKind::Deny => "deny network-outbound",
        RuleKind::Write if network => "allow network-outbound",
        RuleKind::Write | RuleKind::Stat | RuleKind::List | RuleKind::Read | RuleKind::ReadOnly => {
            return;
        }
    };
    let remote: Vec<String> = filters("subpath")
        .iter()
        .map(|path| format!("(remote unix-socket {path})"))
        .collect();
    push_action(sbpl, sockets, &remote);
}

fn push_action(sbpl: &mut String, action: &str, filters: &[String]) {
    sbpl.push_str(&format!("({action}"));
    for filter in filters {
        sbpl.push(' ');
        sbpl.push_str(filter);
    }
    sbpl.push_str(")\n");
}

fn escape(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

impl SandboxWrap for Seatbelt {
    fn mode(&self) -> SandboxMode {
        SandboxMode::Os
    }

    fn wrap(
        &self,
        command: CommandSpec,
        profile: &SandboxProfile,
    ) -> Result<CommandSpec, SandboxError> {
        if !Path::new(&self.program).is_file() {
            return Err(SandboxError::Unavailable(format!(
                "{} is missing, so Seatbelt can't run here and CLI sessions are refused",
                self.program
            )));
        }
        check_profile(profile)?;
        // Resolved here, since the spawner only sees `sandbox-exec`. The same check refuses a
        // program inside the agent's working directory.
        let program = resolve_program(
            &command.program,
            std::env::var_os("PATH").as_deref(),
            &command.cwd,
        )
        .map_err(|err| SandboxError::Invalid(format!("can't sandbox the command: {err}")))?;

        let mut args = vec!["-p".to_owned(), render_profile(profile)];
        args.push(program.to_string_lossy().into_owned());
        args.extend(command.args);
        Ok(CommandSpec {
            program: self.program.clone(),
            args,
            ..command
        })
    }

    fn describe(&self, profile: &SandboxProfile) -> String {
        format!(
            "seatbelt · writes {} · denies {} · {}",
            count(profile.writable.len(), "path"),
            count(profile.deny_read.len(), "path"),
            describe_network(profile)
        )
    }
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::path::PathBuf;

    use argonauts_core::SpecFile;
    use argonauts_core::session_runner::Access;

    use super::super::profile::{ProfileInput, launch_paths, session_profile};
    use super::*;

    /// Runs `script` under the wrap with `args` as `$1`, `$2`, ... and returns its stdout.
    fn run(profile: &SandboxProfile, cwd: &Path, script: &str, args: &[&Path]) -> String {
        let mut command = CommandSpec::new("/bin/sh", cwd).args(["-c", script, "sh"]);
        command
            .args
            .extend(args.iter().map(|path| path.to_string_lossy().into_owned()));
        let wrapped = Seatbelt::new().wrap(command, profile).unwrap();
        let output = std::process::Command::new(&wrapped.program)
            .args(&wrapped.args)
            .current_dir(&wrapped.cwd)
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Prints `read` or `blocked` for `$1`.
    const TRY_READ: &str = r#"if cat "$1" >/dev/null 2>&1; then echo read; else echo blocked; fi"#;
    /// Prints `wrote` or `blocked` for `$1`.
    const TRY_WRITE: &str =
        r#"if (printf x > "$1") 2>/dev/null; then echo wrote; else echo blocked; fi"#;

    fn scratch() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        (temp, root)
    }

    fn strict(writable: Vec<PathBuf>, deny_read: Vec<PathBuf>) -> SandboxProfile {
        SandboxProfile {
            mode: SandboxMode::Os,
            writable,
            readable: Vec::new(),
            deny_read,
            network: false,
            proxy_port: None,
            ask_port: None,
            closed: Vec::new(),
            listable: Vec::new(),
            loopback: false,
        }
    }

    #[test]
    fn seatbelt_blocks_write_outside_worktree() {
        let (_temp, root) = scratch();
        let tree = root.join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        let profile = strict(vec![tree.clone()], Vec::new());
        assert_eq!(
            run(&profile, &tree, TRY_WRITE, &[&tree.join("ok.txt")]),
            "wrote\n"
        );
        let outside = root.join("outside.txt");
        assert_eq!(run(&profile, &tree, TRY_WRITE, &[&outside]), "blocked\n");
        assert!(!outside.exists());
    }

    #[test]
    fn seatbelt_opens_only_its_own_ptys() {
        use std::io::{BufRead, BufReader};

        let (_temp, root) = scratch();
        let profile = strict(Vec::new(), Vec::new());
        // A terminal made outside the sandbox, in place of yours.
        let mut yours = std::process::Command::new("/usr/bin/script")
            .args(["-q", "/dev/null", "/bin/sh", "-c", "tty; sleep 10"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        let stdout = yours.stdout.take().unwrap();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        // `script` echoes the end of its empty stdin as `^D` before the name.
        let at = line.find("/dev/").unwrap();
        let tty = PathBuf::from(line[at..].trim());

        let wrote = run(&profile, &root, TRY_WRITE, &[&tty]);
        let opens = r#"if (: < "$1") 2>/dev/null; then echo read; else echo blocked; fi"#;
        let read = run(&profile, &root, opens, &[&tty]);
        let own = run(
            &profile,
            &root,
            "/usr/bin/script -q /dev/null /bin/sh -c 'echo own-pty' </dev/null",
            &[],
        );
        yours.kill().unwrap();
        yours.wait().unwrap();
        assert_eq!(wrote, "blocked\n");
        assert_eq!(read, "blocked\n");
        assert!(own.contains("own-pty"), "{own:?}");
    }

    #[test]
    fn seatbelt_blocks_state_read() {
        let (_temp, root) = scratch();
        let run_dir = root.join("home/.argonauts/state/repo/runs/run-1");
        let session = run_dir.join("sessions/s-1");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(run_dir.join("approve.token"), "secret").unwrap();
        std::fs::write(session.join("status.json"), "{}").unwrap();
        let mut profile = strict(Vec::new(), vec![root.join("home/.argonauts")]);
        profile.readable.push(session.join("status.json"));

        let token = run_dir.join("approve.token");
        assert_eq!(run(&profile, &root, TRY_READ, &[&token]), "blocked\n");
        let status = session.join("status.json");
        assert_eq!(run(&profile, &root, TRY_READ, &[&status]), "read\n");
        assert_eq!(run(&profile, &root, TRY_WRITE, &[&status]), "blocked\n");
        let listing = r#"if ls "$1" >/dev/null 2>&1; then echo read; else echo blocked; fi"#;
        assert_eq!(run(&profile, &root, listing, &[&run_dir]), "blocked\n");
    }

    /// A direct turn writes your checkout, but never the files every later session and your
    /// own CLI load, and git's own files only as far as a commit needs when agents commit.
    #[test]
    fn a_direct_turn_writes_your_checkout_but_not_what_configures_agents() {
        let (_temp, root) = scratch();
        let home = root.join("home");
        let argonauts = home.join(".argonauts");
        let session = argonauts.join("state/repo/conversations/conversation-1/turn-1");
        let checkout = root.join("checkout");
        std::fs::create_dir_all(&session).unwrap();
        for dir in [
            ".claude",
            ".agents",
            ".codex",
            ".argonauts",
            ".git/hooks",
            "src",
        ] {
            std::fs::create_dir_all(checkout.join(dir)).unwrap();
        }
        std::fs::write(checkout.join(".git/config"), "").unwrap();
        for commits in [true, false] {
            let input = ProfileInput {
                access: Access::Direct,
                agent_id: "claude-code",
                workdir: &checkout,
                session_dir: &session,
                argonauts_home: &argonauts,
                user_home: &home,
                base_worktree: None,
                status_file: None,
                extra_writable: &[],
                sees_other_worktrees: false,
                commits,
            };
            let profile = SandboxProfile {
                mode: SandboxMode::Os,
                deny_read: vec![argonauts.clone()],
                ..session_profile(&SandboxProfile::default(), &input)
            };
            let write = |path: &str| run(&profile, &checkout, TRY_WRITE, &[&checkout.join(path)]);
            assert_eq!(write("src/money.py"), "wrote\n");
            assert_eq!(write("new.txt"), "wrote\n");
            for protected in [
                ".claude/settings.json",
                ".agents/hook.sh",
                ".codex/config.toml",
                ".argonauts/config.toml",
                ".mcp.json",
                ".git/hooks/pre-commit",
                ".git/config",
            ] {
                assert_eq!(write(protected), "blocked\n", "{protected}");
            }
            let index = if commits { "wrote\n" } else { "blocked\n" };
            assert_eq!(write(".git/index"), index, "agents_commit = {commits}");
            let state = session.join("x");
            assert_eq!(run(&profile, &checkout, TRY_READ, &[&state]), "blocked\n");
        }
    }

    #[test]
    fn a_fallback_reads_the_transcript_written_for_it() {
        let (_temp, root) = scratch();
        let home = root.join("home");
        let argonauts = home.join(".argonauts");
        let run_dir = argonauts.join("state/repo/runs/run-1");
        let session = run_dir.join("sessions/s-2");
        let tree = argonauts.join("worktrees/repo/run-1/task-003");
        let transcript = tree.join(".argonauts/fallback/1/transcript.md");
        let kept = run_dir.join("tasks/task-003/fallback-1/transcript.md");
        std::fs::create_dir_all(&session).unwrap();
        for path in [&transcript, &kept] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "# task-003").unwrap();
        }
        for agent_id in ["claude-code", "codex", "antigravity-cli", "opencode"] {
            let input = ProfileInput {
                access: Access::Build,
                agent_id,
                workdir: &tree,
                session_dir: &session,
                argonauts_home: &argonauts,
                user_home: &home,
                base_worktree: None,
                status_file: None,
                extra_writable: &[],
                sees_other_worktrees: false,
                commits: false,
            };
            let profile = SandboxProfile {
                mode: SandboxMode::Os,
                ..session_profile(&SandboxProfile::default(), &input)
            };
            assert_eq!(run(&profile, &tree, TRY_READ, &[&transcript]), "read\n");
            assert_eq!(run(&profile, &tree, TRY_WRITE, &[&transcript]), "blocked\n");
            assert_eq!(run(&profile, &tree, TRY_READ, &[&kept]), "blocked\n");
            let code = tree.join("limit.rs");
            assert_eq!(run(&profile, &tree, TRY_WRITE, &[&code]), "wrote\n");
        }
    }

    #[test]
    fn seatbelt_blocks_network_when_off() {
        let (_temp, root) = scratch();
        // A loopback server stays reachable; anything else, name lookups included, fails.
        let script = r#"
            if /usr/bin/python3 -c 'import socket; socket.getaddrinfo("example.com", 443)' 2>/dev/null
            then echo dns; else echo no-dns; fi
            if /usr/bin/python3 -c 'import socket; socket.create_connection(("1.1.1.1", 443), timeout=3)' 2>/dev/null
            then echo remote; else echo no-remote; fi
            /usr/bin/python3 -c '
import socket
server = socket.socket(); server.bind(("127.0.0.1", 0)); server.listen(1)
socket.create_connection(server.getsockname(), timeout=3); print("loopback")' 2>/dev/null || echo no-loopback
        "#;
        let off = strict(super::super::profile::temp_dirs(), Vec::new());
        assert_eq!(
            run(&off, &root, script, &[]),
            "no-dns\nno-remote\nloopback\n"
        );
        let on = SandboxProfile {
            network: true,
            ..off
        };
        assert!(!render_profile(&on).contains("(deny network*)"));
    }

    /// An upstream web server on loopback that answers `ok` to every request.
    fn ok_server() -> u16 {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                std::thread::spawn(move || {
                    let mut head = Vec::new();
                    let mut buf = [0u8; 512];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut buf) {
                            Ok(0) | Err(_) => return,
                            Ok(read) => head.extend_from_slice(&buf[..read]),
                        }
                    }
                    let reply =
                        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
                    let _ = stream.write_all(reply.as_bytes());
                });
            }
        });
        port
    }

    /// Runs `script` under the wrap with `env` set and `port` as `$1`.
    fn run_with_env(
        profile: &SandboxProfile,
        script: &str,
        port: u16,
        env: &[(String, String)],
    ) -> String {
        let (_temp, root) = scratch();
        let mut command =
            CommandSpec::new("/bin/sh", &root).args(["-c", script, "sh", &port.to_string()]);
        command.env_set = env.to_vec();
        let wrapped = Seatbelt::new().wrap(command, profile).unwrap();
        let output = std::process::Command::new(&wrapped.program)
            .args(&wrapped.args)
            .envs(wrapped.env_set.iter().map(|(k, v)| (k, v)))
            .current_dir(&wrapped.cwd)
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    #[test]
    fn a_proxied_session_reaches_only_its_hosts_through_the_proxy() {
        use crate::sandbox::proxy::{self, Allowlist, Proxy, Why};

        let upstream = ok_server();
        let proxy =
            Proxy::with_resolver(proxy::tests::to_loopback(upstream, Default::default())).unwrap();
        let allow = Allowlist::parse([format!("api.example.test:{upstream}").as_str()]).unwrap();
        let env = proxy::session_env(&proxy.session_url(&allow));
        let profile = SandboxProfile {
            proxy_port: Some(proxy.port()),
            ..strict(super::super::profile::temp_dirs(), Vec::new())
        };
        let sbpl = render_profile(&profile);
        assert!(sbpl.contains(&format!("(remote ip \"localhost:{}\")", proxy.port())));
        assert!(!sbpl.contains("(remote ip \"localhost:*\")"), "{sbpl}");

        // curl finds the proxy and its key in `http_proxy`; `-p` asks for a tunnel.
        let script = r#"
            /usr/bin/curl -sp --max-time 5 "http://api.example.test:$1/" || printf no-tunnel; echo
            if /usr/bin/curl -s --noproxy '*' --max-time 3 "http://127.0.0.1:$1/" >/dev/null 2>&1
            then echo direct; else echo no-direct; fi
            if /usr/bin/python3 -c 'import socket; socket.create_connection(("1.1.1.1", 443), timeout=3)' 2>/dev/null
            then echo remote; else echo no-remote; fi
            if /usr/bin/python3 -c 'import socket; socket.getaddrinfo("example.com", 443)' 2>/dev/null
            then echo dns; else echo no-dns; fi
            if /usr/bin/curl -sp --max-time 5 "http://evil.example.test:$1/" >/dev/null 2>&1
            then echo evil; else echo no-evil; fi
        "#;
        assert_eq!(
            run_with_env(&profile, script, upstream, &env),
            "ok\nno-direct\nno-remote\nno-dns\nno-evil\n"
        );
        let refusals = proxy.refusals();
        assert_eq!(refusals.len(), 1, "{refusals:?}");
        assert_eq!(
            (refusals[0].host.as_str(), refusals[0].why),
            ("evil.example.test", Why::NotListed)
        );
    }

    /// With the network off, a supervised direct turn's session still reaches the app's port
    /// for its questions, as it reaches the proxy, and no other loopback port.
    #[test]
    fn a_supervised_direct_turn_with_the_network_off_reaches_the_app_to_ask() {
        let app = ok_server();
        let other = ok_server();
        let script = format!(
            r#"
            /usr/bin/curl -s --noproxy '*' --max-time 3 "http://127.0.0.1:$1/" || printf no-ask; echo
            /usr/bin/curl -s --noproxy '*' --max-time 3 "http://127.0.0.1:{other}/" || printf no-direct; echo
        "#
        );
        // The proxy's port; nothing needs to answer there for this.
        let proxied = |ask_port| SandboxProfile {
            proxy_port: Some(9),
            ask_port,
            ..strict(super::super::profile::temp_dirs(), Vec::new())
        };
        assert_eq!(
            run_with_env(&proxied(Some(app)), &script, app, &[]),
            "ok\nno-direct\n"
        );
        assert_eq!(
            run_with_env(&proxied(None), &script, app, &[]),
            "no-ask\nno-direct\n",
            "without its port the session couldn't ask"
        );
    }

    /// OpenCode's project config and a hooks folder `core.hooksPath` moves into your checkout
    /// are closed too, whether or not they exist yet.
    #[test]
    fn a_direct_turn_cant_plant_opencode_config_or_a_hook() {
        let (_temp, root) = scratch();
        let home = root.join("home");
        let argonauts = home.join(".argonauts");
        let session = argonauts.join("state/repo/conversations/conversation-1/turn-1");
        let checkout = root.join("checkout");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::create_dir_all(checkout.join("src")).unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&checkout)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "--quiet"]);
        git(&["config", "core.hooksPath", ".husky"]);
        let input = ProfileInput {
            access: Access::Direct,
            agent_id: "opencode",
            workdir: &checkout,
            session_dir: &session,
            argonauts_home: &argonauts,
            user_home: &home,
            base_worktree: None,
            status_file: None,
            extra_writable: &[],
            sees_other_worktrees: false,
            commits: true,
        };
        let profile = SandboxProfile {
            mode: SandboxMode::Os,
            deny_read: vec![argonauts.clone()],
            ..session_profile(&SandboxProfile::default(), &input)
        };
        assert!(
            profile.closed.contains(&checkout.join(".husky")),
            "{:?}",
            profile.closed
        );
        let write = |path: &str| run(&profile, &checkout, TRY_WRITE, &[&checkout.join(path)]);
        assert_eq!(write("src/app.ts"), "wrote\n");
        assert_eq!(write("new.txt"), "wrote\n");
        let make = |path: &str| {
            let script = r#"if (mkdir -p "$(dirname "$1")" && printf x > "$1") 2>/dev/null; then echo wrote; else echo blocked; fi"#;
            run(&profile, &checkout, script, &[&checkout.join(path)])
        };
        for protected in [
            "opencode.json",
            "opencode.jsonc",
            ".opencode/plugin/run.js",
            ".husky/pre-commit",
        ] {
            assert_eq!(make(protected), "blocked\n", "{protected}");
        }
    }

    /// OpenCode finds the real path of Claude Code's folder before it serves anything, which
    /// opens the folder. That works in its sessions, while the files in it stay unreadable, and
    /// another CLI's sessions can't open the folder at all.
    #[test]
    fn opencode_finds_claude_codes_folder_but_reads_nothing_in_it() {
        let (_temp, root) = scratch();
        let home = root.join("home");
        let argonauts = home.join(".argonauts");
        let session = argonauts.join("state/repo/conversations/conversation-1/turn-1");
        let checkout = root.join("checkout");
        let claude = home.join(".claude");
        for dir in [&session, &checkout, &claude] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let settings = claude.join("settings.json");
        std::fs::write(&settings, "{}").unwrap();
        let profile_of = |agent_id| {
            let input = ProfileInput {
                access: Access::Direct,
                agent_id,
                workdir: &checkout,
                session_dir: &session,
                argonauts_home: &argonauts,
                user_home: &home,
                base_worktree: None,
                status_file: None,
                extra_writable: &[],
                sees_other_worktrees: false,
                commits: false,
            };
            SandboxProfile {
                mode: SandboxMode::Os,
                ..session_profile(&SandboxProfile::default(), &input)
            }
        };
        let opens = r#"if /bin/realpath "$1" >/dev/null 2>&1 && /bin/ls "$1" >/dev/null 2>&1; then echo opened; else echo blocked; fi"#;
        let opencode = profile_of("opencode");
        assert_eq!(run(&opencode, &checkout, opens, &[&claude]), "opened\n");
        assert_eq!(
            run(&opencode, &checkout, TRY_READ, &[&settings]),
            "blocked\n"
        );
        let codex = profile_of("codex");
        assert_eq!(run(&codex, &checkout, opens, &[&claude]), "blocked\n");
    }

    #[test]
    fn a_check_with_loopback_open_gets_no_tunnel() {
        use crate::sandbox::proxy::{self, Allowlist, Proxy, Why};

        let upstream = ok_server();
        let proxy =
            Proxy::with_resolver(proxy::tests::to_loopback(upstream, Default::default())).unwrap();
        // A session's list exists, but the check has no key for it.
        let allow = Allowlist::parse([format!("api.example.test:{upstream}").as_str()]).unwrap();
        proxy.session_url(&allow);
        let check = strict(super::super::profile::temp_dirs(), Vec::new());
        let script = format!(
            r#"
            /usr/bin/curl -sp -x http://127.0.0.1:{} --max-time 5 "http://api.example.test:$1/" || printf no-tunnel; echo
            /usr/bin/curl -s --noproxy '*' --max-time 3 "http://127.0.0.1:$1/" || printf no-loopback; echo
        "#,
            proxy.port()
        );
        assert_eq!(
            run_with_env(&check, &script, upstream, &[]),
            "no-tunnel\nok\n"
        );
        assert_eq!(proxy.refusals()[0].why, Why::NoKey);
    }

    /// Seatbelt applies the last rule that matches, which is why rules go out shallowest first.
    #[test]
    fn seatbelt_applies_the_last_matching_rule() {
        let (_temp, root) = scratch();
        let inner = root.join("denied/inner");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("f"), "ok").unwrap();
        let deny = format!(
            "(deny file-read* (subpath \"{}\"))",
            root.join("denied").display()
        );
        let allow = format!("(allow file-read* (subpath \"{}\"))", inner.display());
        let read = |rules: [&str; 2]| {
            let sbpl = format!("(version 1)\n(allow default)\n{}\n{}\n", rules[0], rules[1]);
            std::process::Command::new(SANDBOX_EXEC)
                .args(["-p", &sbpl, "/bin/cat"])
                .arg(inner.join("f"))
                .output()
                .unwrap()
                .status
                .success()
        };
        assert!(read([&deny, &allow]));
        assert!(!read([&allow, &deny]));
    }

    #[test]
    fn an_exception_beats_its_parent_deny() {
        let (_temp, root) = scratch();
        let inner = root.join("denied/inner");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("f"), "ok").unwrap();
        std::fs::write(root.join("denied/g"), "no").unwrap();
        let mut profile = strict(Vec::new(), vec![root.join("denied")]);
        profile.readable.push(inner.clone());
        assert_eq!(
            run(&profile, &root, TRY_READ, &[&inner.join("f")]),
            "read\n"
        );
        assert_eq!(
            run(&profile, &root, TRY_WRITE, &[&inner.join("f")]),
            "blocked\n"
        );
        assert_eq!(
            run(&profile, &root, TRY_READ, &[&root.join("denied/g")]),
            "blocked\n"
        );
    }

    #[test]
    fn git_works_in_a_worktree_inside_a_denied_folder() {
        let (_temp, root) = scratch();
        let home = root.join("home");
        let argonauts = home.join(".argonauts");
        let session = argonauts.join("state/repo/runs/run-1/sessions/s-1");
        let tree = argonauts.join("worktrees/repo/run-1/task-a");
        let other = argonauts.join("worktrees/repo/run-1/task-b");
        for dir in [&session, &tree, &other] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(other.join("hidden.rs"), "secret").unwrap();
        let input = ProfileInput {
            access: Access::Build,
            agent_id: "codex",
            workdir: &tree,
            session_dir: &session,
            argonauts_home: &argonauts,
            user_home: &home,
            base_worktree: None,
            status_file: None,
            extra_writable: &[],
            sees_other_worktrees: false,
            commits: false,
        };
        git(&tree, &["init", "-q", "."]);
        let profile = session_profile(&SandboxProfile::default(), &input);
        let script = r#"
            printf x > f && git status --short
            if cat "$1" >/dev/null 2>&1; then echo read-other; else echo other-blocked; fi
        "#;
        let out = run(&profile, &tree, script, &[&other.join("hidden.rs")]);
        assert_eq!(out, "?? f\nother-blocked\n");

        // With `sandbox.agents_see_other_worktrees` on, the other worktree reads but never changes.
        let open = session_profile(
            &SandboxProfile::default(),
            &ProfileInput {
                sees_other_worktrees: true,
                ..input
            },
        );
        let out = run(&open, &tree, script, &[&other.join("hidden.rs")]);
        assert_eq!(out, "?? f\nread-other\n");
        let other_file = other.join("hidden.rs");
        assert_eq!(run(&open, &tree, TRY_WRITE, &[&other_file]), "blocked\n");
        assert_eq!(std::fs::read_to_string(&other_file).unwrap(), "secret");
    }

    /// Runs git outside the sandbox, as Argonauts Code does.
    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    /// Prints one word per step, so a failure shows which one went wrong.
    const GIT_STEPS: &str = r#"
        printf 'new\n' > new.rs && echo edited
        git status --short >/dev/null 2>&1 && echo status-ok
        if git add new.rs 2>/dev/null; then echo added; else echo add-blocked; fi
        if git -c user.name=a -c user.email=a@a commit -qm x 2>/dev/null; then echo committed; else echo commit-blocked; fi
        if (printf 'exit 0\n' > "$1/hooks/pre-commit") 2>/dev/null; then echo hook-planted; else echo hook-blocked; fi
        if (printf '[core]\n' >> "$1/config") 2>/dev/null; then echo config-changed; else echo config-blocked; fi
        if git update-ref refs/heads/main HEAD 2>/dev/null; then echo ref-moved; else echo ref-blocked; fi
    "#;

    const GIT_BLOCKED: &str = "edited\nstatus-ok\nadd-blocked\ncommit-blocked\nhook-blocked\n\
                               config-blocked\nref-blocked\n";

    fn build_input<'a>(
        tree: &'a Path,
        session: &'a Path,
        argonauts: &'a Path,
        home: &'a Path,
    ) -> ProfileInput<'a> {
        ProfileInput {
            access: Access::Build,
            agent_id: "claude-code",
            workdir: tree,
            session_dir: session,
            argonauts_home: argonauts,
            user_home: home,
            base_worktree: None,
            status_file: None,
            extra_writable: &[],
            sees_other_worktrees: false,
            commits: false,
        }
    }

    /// agy writes its `agentapi` shim before each command, in a `bin` it puts first on the
    /// agent's `PATH`. The session may write that one file, and no other name there.
    #[test]
    fn agy_rewrites_its_shim_and_nothing_else_in_its_bin() {
        let (_temp, root) = scratch();
        let home = root.join("home");
        let argonauts = home.join(".argonauts");
        let session = argonauts.join("state/repo/runs/run-1/sessions/s-1");
        let tree = argonauts.join("worktrees/repo/run-1/task-a");
        let bin = home.join(".gemini/antigravity-cli/bin");
        for dir in [&session, &tree, &bin] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let input = ProfileInput {
            agent_id: "antigravity-cli",
            ..build_input(&tree, &session, &argonauts, &home)
        };
        let profile = session_profile(&SandboxProfile::default(), &input);
        let shim = bin.join("agentapi");
        assert_eq!(run(&profile, &tree, TRY_WRITE, &[&shim]), "wrote\n");
        assert_eq!(
            run(&profile, &tree, TRY_WRITE, &[&bin.join("git")]),
            "blocked\n"
        );
        assert!(!bin.join("git").exists());
    }

    #[test]
    fn agent_commands_cannot_change_git_state() {
        let (_temp, root) = scratch();
        let home = root.join("home");
        let argonauts = home.join(".argonauts");
        let session = argonauts.join("state/repo/runs/run-1/sessions/s-1");
        let tree = argonauts.join("worktrees/repo/run-1/task-a");
        for dir in [&session, &tree] {
            std::fs::create_dir_all(dir).unwrap();
        }
        git(&tree, &["init", "-q", "."]);
        std::fs::write(tree.join("lib.rs"), "fn main() {}\n").unwrap();
        git(&tree, &["add", "lib.rs"]);
        git(&tree, &["commit", "-qm", "base"]);
        let head = std::fs::read_to_string(tree.join(".git/refs/heads/main")).unwrap();

        let profile = session_profile(
            &SandboxProfile::default(),
            &build_input(&tree, &session, &argonauts, &home),
        );
        let out = run(&profile, &tree, GIT_STEPS, &[&tree.join(".git")]);
        assert_eq!(out, GIT_BLOCKED);
        assert!(!tree.join(".git/hooks/pre-commit").exists());
        let after = std::fs::read_to_string(tree.join(".git/refs/heads/main")).unwrap();
        assert_eq!(after, head);
        assert_eq!(
            std::fs::read_to_string(tree.join("new.rs")).unwrap(),
            "new\n"
        );
    }

    /// A task's worktree is linked to your repository, whose git folder holds the hooks, config,
    /// and refs every checkout shares. Here the repository sits in temp, which a session may write.
    #[test]
    fn a_linked_worktree_cannot_change_the_repository_behind_it() {
        let (_temp, root) = scratch();
        let home = root.join("home");
        let argonauts = home.join(".argonauts");
        let session = argonauts.join("state/repo/runs/run-1/sessions/s-1");
        let repo = root.join("repo");
        let tree = argonauts.join("worktrees/repo/run-1/task-a");
        for dir in [&session, &repo, tree.parent().unwrap()] {
            std::fs::create_dir_all(dir).unwrap();
        }
        git(&repo, &["init", "-q", "."]);
        std::fs::write(repo.join("lib.rs"), "fn main() {}\n").unwrap();
        git(&repo, &["add", "lib.rs"]);
        git(&repo, &["commit", "-qm", "base"]);
        let tree_arg = tree.to_string_lossy().into_owned();
        git(&repo, &["worktree", "add", "-q", "-b", "task-a", &tree_arg]);

        let profile = session_profile(
            &SandboxProfile::default(),
            &build_input(&tree, &session, &argonauts, &home),
        );
        let out = run(&profile, &tree, GIT_STEPS, &[&repo.join(".git")]);
        assert_eq!(out, GIT_BLOCKED);
        assert!(!repo.join(".git/hooks/pre-commit").exists());
        // The checkout stays read-only though it sits in temp, and the rest of temp is writable.
        assert_eq!(
            run(&profile, &tree, TRY_WRITE, &[&repo.join("notes.txt")]),
            "blocked\n"
        );
        assert_eq!(
            run(&profile, &tree, TRY_WRITE, &[&root.join("notes.txt")]),
            "wrote\n"
        );
    }

    /// With `sandbox.agents_commit` on, an agent commits on its task's branch with git as you
    /// would, and still can't plant a hook, change config, move another branch, point `HEAD`
    /// elsewhere, or touch the packs that hold your history.
    #[test]
    fn an_agent_commits_on_its_own_branch_and_nothing_else() {
        let (_temp, root) = scratch();
        let home = root.join("home");
        let argonauts = home.join(".argonauts");
        let session = argonauts.join("state/repo/runs/run-1/sessions/s-1");
        let repo = root.join("repo");
        let tree = argonauts.join("worktrees/repo/run-1/task-a");
        for dir in [&session, &repo, tree.parent().unwrap()] {
            std::fs::create_dir_all(dir).unwrap();
        }
        git(&repo, &["init", "-q", "."]);
        std::fs::write(repo.join("lib.rs"), "fn main() {}\n").unwrap();
        git(&repo, &["add", "lib.rs"]);
        git(&repo, &["commit", "-qm", "base"]);
        git(&repo, &["branch", "argonauts/run-1/merged"]);
        git(&repo, &["gc", "-q"]);
        let tree_arg = tree.to_string_lossy().into_owned();
        let branch = "argonauts/run-1/task-a";
        git(&repo, &["worktree", "add", "-q", "-b", branch, &tree_arg]);
        let common = repo.join(".git");
        let head_of = |name: &str| {
            let out = std::process::Command::new("git")
                .args(["rev-parse", name])
                .current_dir(&repo)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        let base = head_of("main");

        let input = ProfileInput {
            commits: true,
            ..build_input(&tree, &session, &argonauts, &home)
        };
        let profile = session_profile(&SandboxProfile::default(), &input);
        let script = r#"
            printf 'new\n' > new.rs && echo edited
            if git add new.rs 2>/dev/null; then echo added; else echo add-blocked; fi
            if git -c user.name=a -c user.email=a@a commit -qm x 2>/dev/null; then echo committed; else echo commit-blocked; fi
            if (printf 'exit 0\n' > "$1/hooks/pre-commit") 2>/dev/null; then echo hook-planted; else echo hook-blocked; fi
            if (printf '[core]\n' >> "$1/config") 2>/dev/null; then echo config-changed; else echo config-blocked; fi
            if git update-ref refs/heads/main HEAD 2>/dev/null; then echo main-moved; else echo main-blocked; fi
            if git update-ref refs/heads/argonauts/run-1/merged HEAD 2>/dev/null; then echo merge-moved; else echo merge-blocked; fi
            if git symbolic-ref HEAD refs/heads/main 2>/dev/null; then echo head-moved; else echo head-blocked; fi
            if (: > "$1/objects/pack/planted") 2>/dev/null; then echo pack-planted; else echo pack-blocked; fi
            if (printf '/tmp\n' > "$1/worktrees/task-a/commondir") 2>/dev/null; then echo commondir-changed; else echo commondir-blocked; fi
        "#;
        let out = run(&profile, &tree, script, &[&common]);
        assert_eq!(
            out,
            "edited\nadded\ncommitted\nhook-blocked\nconfig-blocked\nmain-blocked\n\
             merge-blocked\nhead-blocked\npack-blocked\ncommondir-blocked\n"
        );
        let task = head_of(branch);
        assert_ne!(task, base);
        assert_eq!(head_of(&format!("{task}^")), base);
        assert_eq!(head_of("main"), base);
        assert_eq!(head_of("argonauts/run-1/merged"), base);
        assert!(!common.join("hooks/pre-commit").exists());

        // With the switch off, the same commit is refused and Argonauts Code commits instead.
        let off = session_profile(
            &SandboxProfile::default(),
            &build_input(&tree, &session, &argonauts, &home),
        );
        std::fs::write(tree.join("more.rs"), "\n").unwrap();
        let out = run(&off, &tree, GIT_STEPS, &[&common]);
        assert_eq!(out, GIT_BLOCKED);
        assert_eq!(head_of(branch), task);
    }

    /// Prints `connected` or `blocked` for the socket at `$1`.
    const TRY_CONNECT: &str = r#"if /usr/bin/nc -U -w 2 "$1" </dev/null >/dev/null 2>&1; then echo connected; else echo blocked; fi"#;

    /// A listener that takes each connection and closes it, so `nc` returns at once.
    fn listen(path: &Path) {
        let listener = std::os::unix::net::UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                drop(stream);
            }
        });
    }

    #[test]
    fn seatbelt_blocks_sockets_in_denied_folders() {
        let (_temp, root) = scratch();
        let tree = root.join("d/tree");
        std::fs::create_dir_all(&tree).unwrap();
        let control = root.join("d/control.sock");
        let own = tree.join("t.sock");
        listen(&control);
        listen(&own);
        let mut profile = strict(vec![tree.clone()], vec![root.join("d")]);
        profile.network = true;
        assert_eq!(run(&profile, &root, TRY_CONNECT, &[&control]), "blocked\n");
        assert_eq!(run(&profile, &root, TRY_CONNECT, &[&own]), "connected\n");
        // The same socket outside any denied area stays reachable.
        let open = strict(Vec::new(), Vec::new());
        let open = SandboxProfile {
            network: true,
            ..open
        };
        assert_eq!(run(&open, &root, TRY_CONNECT, &[&control]), "connected\n");
    }

    /// Without network a session can't make a socket, so the only sockets in its writable
    /// folders are other programs': tmux's in temp is a way to type into your shell.
    #[test]
    fn without_network_no_socket_in_a_writable_folder_answers() {
        let (_temp, root) = scratch();
        let tree = root.join("tree");
        let temp = root.join("tmp");
        let tmux = temp.join("tmux-501");
        for dir in [&tree, &tmux] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let server = tmux.join("default");
        listen(&server);
        let off = strict(vec![tree.clone(), temp.clone()], Vec::new());
        assert_eq!(run(&off, &tree, TRY_CONNECT, &[&server]), "blocked\n");
        let make = r#"/usr/bin/python3 -c 'import socket, sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "$1" 2>/dev/null && echo made || echo blocked"#;
        assert_eq!(
            run(&off, &tree, make, &[&tree.join("own.sock")]),
            "blocked\n"
        );
        // With network on, which you choose, sockets outside denied areas answer.
        let on = SandboxProfile {
            network: true,
            ..off
        };
        assert_eq!(run(&on, &tree, TRY_CONNECT, &[&server]), "connected\n");
    }

    #[test]
    fn seatbelt_blocks_tools_that_run_work_outside_it() {
        let (_temp, root) = scratch();
        let script =
            r#"if /bin/launchctl version >/dev/null 2>&1; then echo ran; else echo blocked; fi"#;
        let profile = strict(Vec::new(), Vec::new());
        assert_eq!(run(&profile, &root, script, &[]), "blocked\n");
        // A copied tool still has to ask LaunchServices, which the session can't reach.
        let copy = root.join("lsappinfo");
        std::fs::copy("/usr/bin/lsappinfo", &copy).unwrap();
        let asked = run(&profile, &root, r#""$1" front 2>&1"#, &[&copy]);
        assert!(!asked.contains("ASN:"), "{asked}");
        let rendered = render_profile(&profile);
        for service in [
            "com.apple.coreservices.launchservicesd",
            "com.apple.coreservices.appleevents",
            "com.apple.pasteboard.1",
            "com.apple.windowserver.active",
        ] {
            assert!(
                rendered.contains(&format!("(global-name \"{service}\")")),
                "{service}"
            );
        }
    }

    /// A session can signal its own processes but nothing else of yours, such as a Node program
    /// that SIGUSR1 would open to a debugger.
    #[test]
    fn signals_stay_inside_the_session() {
        let (_temp, root) = scratch();
        let mut outside = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let script = r#"
            if kill -0 "$1" 2>/dev/null; then echo outside-reached; else echo outside-blocked; fi
            /bin/sleep 30 & own=$!
            if kill "$own" 2>/dev/null; then echo own-stopped; else echo own-blocked; fi
        "#;
        let pid = PathBuf::from(outside.id().to_string());
        let profile = strict(Vec::new(), Vec::new());
        let out = run(&profile, &root, script, &[&pid]);
        let _ = outside.kill();
        let _ = outside.wait();
        assert_eq!(out, "outside-blocked\nown-stopped\n");
    }

    /// Each CLI's launch shape: Claude Code's settings and MCP config, Codex's output
    /// schema, and a read-only agy session's agent folder, all in the denied session folder.
    #[test]
    fn a_session_reads_the_files_its_launch_names() {
        let (_temp, root) = scratch();
        let home = root.join("home");
        let argonauts = home.join(".argonauts");
        let session = argonauts.join("state/repo/runs/run-1/sessions/s-1");
        let tree = argonauts.join("worktrees/repo/run-1/base");
        std::fs::create_dir_all(&tree).unwrap();
        let agent_file = session.join("agy-agent/.agents/agents/reader.md");
        std::fs::create_dir_all(agent_file.parent().unwrap()).unwrap();
        std::fs::write(&agent_file, "---").unwrap();
        let raw = session.join("raw.stdout.jsonl");
        std::fs::write(&raw, "secret").unwrap();
        let file = |name: &str| SpecFile {
            path: session.join(name),
            contents: "{}".into(),
        };
        let launches = [
            (
                "claude-code",
                vec![
                    "--settings".to_owned(),
                    session.join("claude-settings.json").display().to_string(),
                    "--mcp-config".to_owned(),
                    session.join("mcp.json").display().to_string(),
                ],
                vec![file("claude-settings.json"), file("mcp.json")],
            ),
            (
                "codex",
                vec![
                    "--output-schema".to_owned(),
                    session
                        .join("codex-output-schema.json")
                        .display()
                        .to_string(),
                ],
                vec![file("codex-output-schema.json")],
            ),
            (
                "antigravity-cli",
                vec![
                    "--add-dir".to_owned(),
                    session.join("agy-agent").display().to_string(),
                    "--agent".to_owned(),
                    "reader".to_owned(),
                ],
                Vec::new(),
            ),
        ];
        // Opens every absolute path argument as the CLI would: files read, folders listed.
        let cli = r#"
            for arg; do
              case "$arg" in
                /*) if [ -d "$arg" ]; then find "$arg" -type f -exec cat {} + >/dev/null 2>&1 && echo ok || echo "blocked $arg"
                    else cat "$arg" >/dev/null 2>&1 && echo ok || echo "blocked $arg"; fi ;;
              esac
            done
            cat "$RAW" >/dev/null 2>&1 && echo "read raw" || true
        "#;
        for (agent_id, args, files) in launches {
            for file in &files {
                std::fs::write(&file.path, &file.contents).unwrap();
            }
            let mut command = CommandSpec::new("/bin/sh", &tree).args(["-c", cli, "sh"]);
            command.args.extend(args);
            command.files = files;
            command
                .env_set
                .push(("RAW".into(), raw.display().to_string()));
            let input = ProfileInput {
                access: Access::ReadOnly,
                agent_id,
                workdir: &tree,
                session_dir: &session,
                argonauts_home: &argonauts,
                user_home: &home,
                base_worktree: None,
                status_file: None,
                extra_writable: &[],
                sees_other_worktrees: false,
                commits: false,
            };
            let mut profile = session_profile(&SandboxProfile::default(), &input);
            profile.readable.extend(launch_paths(&command, &session));
            let wrapped = Seatbelt::new().wrap(command, &profile).unwrap();
            let output = std::process::Command::new(&wrapped.program)
                .args(&wrapped.args)
                .envs(wrapped.env_set.iter().map(|(k, v)| (k, v)))
                .current_dir(&wrapped.cwd)
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(!stdout.is_empty(), "{agent_id}: {output:?}");
            assert!(
                stdout.lines().all(|line| line == "ok"),
                "{agent_id}: {stdout}"
            );
        }
    }

    #[test]
    fn wrap_keeps_env_cwd_stdin_and_files() {
        let (_temp, root) = scratch();
        let mut command = CommandSpec::new("sh", &root).args(["-c", "true"]);
        command.env_set.push(("A".into(), "1".into()));
        command.env_remove.push("B".into());
        command.stdin = Some("hello".into());
        command.keep_stdin_open = true;
        command.files.push(SpecFile {
            path: root.join("schema.json"),
            contents: "{}".into(),
        });
        let wrapped = Seatbelt::new()
            .wrap(command.clone(), &strict(Vec::new(), Vec::new()))
            .unwrap();
        assert_eq!(wrapped.program, SANDBOX_EXEC);
        assert_eq!(wrapped.args[0], "-p");
        assert!(wrapped.args[2].starts_with('/') && wrapped.args[2].ends_with("/sh"));
        assert_eq!(wrapped.args[3..], ["-c", "true"]);
        assert_eq!(
            (wrapped.env_set, wrapped.env_remove, wrapped.stdin),
            (command.env_set, command.env_remove, command.stdin)
        );
        assert_eq!((wrapped.cwd, wrapped.files), (command.cwd, command.files));
        assert!(wrapped.keep_stdin_open);
    }

    #[test]
    fn wrap_refuses_relative_paths_and_missing_programs() {
        let (_temp, root) = scratch();
        let relative = strict(vec!["relative".into()], Vec::new());
        let command = CommandSpec::new("/bin/sh", &root);
        assert!(matches!(
            Seatbelt::new().wrap(command.clone(), &relative),
            Err(SandboxError::Invalid(_))
        ));
        let missing = CommandSpec::new("no-such-program-anywhere", &root);
        assert!(matches!(
            Seatbelt::new().wrap(missing, &strict(Vec::new(), Vec::new())),
            Err(SandboxError::Invalid(_))
        ));
        let broken = Seatbelt {
            program: "/nonexistent/sandbox-exec".into(),
        };
        assert!(matches!(
            broken.wrap(command, &strict(Vec::new(), Vec::new())),
            Err(SandboxError::Unavailable(_))
        ));
    }

    #[test]
    fn odd_characters_in_paths_are_escaped() {
        let (_temp, root) = scratch();
        let odd = root.join("a \"quoted\\\" dir");
        std::fs::create_dir_all(&odd).unwrap();
        std::fs::write(odd.join("f"), "x").unwrap();
        let profile = strict(Vec::new(), vec![odd.clone()]);
        assert_eq!(
            run(&profile, &root, TRY_READ, &[&odd.join("f")]),
            "blocked\n"
        );
    }
}
