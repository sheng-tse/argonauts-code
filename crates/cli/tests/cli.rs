//! The binary end to end, offline: a temp repo and home, no terminal, and fake agent CLIs where
//! a command looks for them.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use argonauts_testkit::scenario::FakeScenario;
use argonauts_testkit::{FakeVendors, Key, Pty, PtyCommand, TempRepo};

fn argonauts(repo: &TempRepo, dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_argonauts"))
        .args(args)
        .current_dir(dir)
        .envs(repo.envs())
        .env_remove("ARGONAUTS_SESSION")
        .env_remove("ARGONAUTS_LOG")
        .stdin(Stdio::null())
        .output()
        .expect("the argonauts binary runs")
}

/// A folder outside any git repository.
fn elsewhere(repo: &TempRepo) -> PathBuf {
    let dir = repo.scratch().join("elsewhere");
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn describe(output: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        stdout(output),
        stderr(output)
    )
}

fn script(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// The real git on `PATH`, so a test's `PATH` can hold git and the fakes alone.
fn real_git() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("git"))
        .find(|git| git.is_file())
        .expect("git on PATH")
}

#[test]
fn help_lists_the_commands_but_not_the_supervisor() {
    let repo = TempRepo::new().unwrap();
    let output = argonauts(&repo, repo.root(), &["--help"]);
    assert!(output.status.success(), "{}", describe(&output));
    let help = stdout(&output);
    for command in [
        "run",
        "resume",
        "status",
        "init",
        "doctor",
        "config",
        "models",
        "approve",
        "approvals",
        "auto",
        "inbox",
        "mcp",
    ] {
        assert!(
            help.contains(&format!("  {command} ")),
            "{command} in {help}"
        );
    }
    assert!(!help.contains("supervise"), "{help}");
}

#[test]
fn config_set_of_a_safety_key_without_a_terminal_refuses() {
    let repo = TempRepo::new().unwrap();
    let output = argonauts(
        &repo,
        repo.root(),
        &["config", "set", "approvals.merge.mode", "auto", "--global"],
    );
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    assert!(
        stderr(&output).contains("needs you at a terminal"),
        "{}",
        describe(&output)
    );
    assert!(!repo.global_config().exists());
}

#[test]
fn config_show_prints_the_settings_as_toml() {
    let repo = TempRepo::new().unwrap();
    repo.write(".argonauts/config.toml", "[triage]\nsolo_max_files = 5\n")
        .unwrap();
    let output = argonauts(
        &repo,
        repo.root(),
        &["config", "show", "--set", "ui.bell=false"],
    );
    assert!(output.status.success(), "{}", describe(&output));
    let text = stdout(&output);
    assert!(text.starts_with("# Resolved settings, sha256 "), "{text}");
    assert!(text.contains("# --set ui.bell=false"), "{text}");
    assert!(text.contains(".argonauts/config.toml (read)"), "{text}");
    let table: toml::Table = text.parse().unwrap();
    assert_eq!(table["triage"]["solo_max_files"].as_integer(), Some(5));
    assert_eq!(table["ui"]["bell"].as_bool(), Some(false));
}

#[test]
fn a_bad_set_is_a_usage_error() {
    let repo = TempRepo::new().unwrap();
    let output = argonauts(&repo, repo.root(), &["config", "show", "--set", "ui.bell"]);
    assert_eq!(output.status.code(), Some(2), "{}", describe(&output));
}

#[test]
fn config_schema_is_json_schema() {
    let repo = TempRepo::new().unwrap();
    let output = argonauts(&repo, repo.root(), &["config", "schema"]);
    assert!(output.status.success(), "{}", describe(&output));
    let schema: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        schema["properties"]["default_agent"].is_object(),
        "{schema}"
    );
}

/// `ARGONAUTS_HOME` holds everything a first start writes: the setup's answers, the preset
/// files, the terms note. Nothing lands under `HOME/.argonauts`. Esc on the first card, once it
/// has the keys, says how to walk through the setup again.
#[test]
fn a_first_start_with_argonauts_home_set_writes_nothing_under_your_home() {
    let repo = TempRepo::new().unwrap();
    let bin = repo.scratch().join("bin");
    fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(real_git(), bin.join("git")).unwrap();
    // Answers every call with its version, so nothing can reach a vendor.
    script(&bin, "claude", "echo '2.1.284 (Claude Code)'");
    let command = PtyCommand::new(env!("CARGO_BIN_EXE_argonauts"))
        .cwd(repo.root())
        .envs(repo.envs())
        .env("PATH", bin.clone().into_os_string())
        .env_remove("ARGONAUTS_SESSION")
        .env_remove("ARGONAUTS_LOG")
        .size(34, 100);
    let mut pty = Pty::spawn(command).unwrap();
    let wait = Duration::from_secs(30);
    pty.wait_for(r"Welcome to Argonauts(.|\n)*Continue \(recommended\)", wait)
        .unwrap();
    // Esc in the input never answers a card, so ↑ hands it the keys first.
    pty.press(Key::Up).unwrap();
    pty.wait_for(r"Esc to take the recommended answers", wait)
        .unwrap();
    pty.press(Key::Esc).unwrap();
    pty.wait_for(r"/setup \(or argonauts init\) walks through", wait)
        .unwrap();
    // The second Ctrl+D leaves.
    pty.press(Key::Ctrl('d')).unwrap();
    pty.press(Key::Ctrl('d')).unwrap();
    pty.wait_for_exit(wait).unwrap();

    assert!(repo.global_config().is_file(), "the answers are saved");
    assert!(repo.home().join("presets").is_dir());
    let stray = repo.user_home().join(".argonauts");
    assert!(!stray.exists(), "{} was written", stray.display());
}

#[test]
fn doctor_fails_without_an_agent_cli_and_passes_with_one() {
    let repo = TempRepo::new().unwrap();
    let bin = repo.scratch().join("bin");
    fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(real_git(), bin.join("git")).unwrap();
    let doctor = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_argonauts"))
            .args(args)
            .current_dir(elsewhere(&repo))
            .envs(repo.envs())
            .env("PATH", &bin)
            .env_remove("ARGONAUTS_SESSION")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };

    let output = doctor(&["doctor"]);
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    assert!(
        stdout(&output).contains("Not ready"),
        "{}",
        describe(&output)
    );

    script(&bin, "claude", "echo '2.1.284 (Claude Code)'");
    let output = doctor(&["doctor"]);
    assert!(output.status.success(), "{}", describe(&output));
    assert!(
        stdout(&output).contains("Ready with 1 of"),
        "{}",
        describe(&output)
    );

    let output = doctor(&["doctor", "--json"]);
    assert!(output.status.success(), "{}", describe(&output));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(json["agents"].is_array(), "{json}");
    assert_eq!(json["parked"], serde_json::json!([]));
}

#[test]
fn doctor_lists_every_cli_with_its_version() {
    let repo = TempRepo::new().unwrap();
    let scenario = repo.scratch().join("scenario.toml");
    FakeScenario::default()
        .version("claude", "2.1.284 (Claude Code)")
        .version("codex", "codex-cli 0.158.0")
        .save(&scenario)
        .unwrap();
    let vendors = FakeVendors::install(repo.scratch(), &scenario).unwrap();
    for absent in ["agy", "opencode"] {
        fs::remove_file(vendors.bin_dir().join(absent)).unwrap();
    }
    // The fakes and git alone, so no CLI installed on this machine shows up.
    let git = repo.scratch().join("git-bin");
    fs::create_dir_all(&git).unwrap();
    std::os::unix::fs::symlink(real_git(), git.join("git")).unwrap();
    let path = std::env::join_paths([vendors.bin_dir(), git.as_path()]).unwrap();
    let doctor = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_argonauts"))
            .args(args)
            .current_dir(elsewhere(&repo))
            .envs(repo.envs())
            .envs(vendors.envs())
            .env("PATH", &path)
            .env_remove("ARGONAUTS_SESSION")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };

    let output = doctor(&["doctor"]);
    let report = describe(&output);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let text = stdout(&output);
    let line = |id: &str| {
        text.lines()
            .find(|line| line.split_whitespace().next() == Some(id))
            .unwrap_or_else(|| panic!("no {id} line in {report}"))
            .to_owned()
    };
    // The exact word: "untested" holds "tested" too.
    let says = |line: &str, word: &str| line.split_whitespace().any(|w| w == word);
    assert!(says(&line("claude-code"), "2.1.284"), "{report}");
    assert!(says(&line("claude-code"), "tested"), "{report}");
    assert!(says(&line("codex"), "0.158.0"), "{report}");
    assert!(says(&line("codex"), "tested"), "{report}");
    assert!(
        line("antigravity-cli").contains("not installed"),
        "{report}"
    );
    assert!(line("opencode").contains("not installed"), "{report}");
    assert!(text.contains("Ready with 2 of "), "{report}");
    assert!(!text.contains("Untested versions"), "{report}");

    let output = doctor(&["doctor", "--json"]);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let status = |id: &str| {
        json["agents"]
            .as_array()
            .and_then(|agents| agents.iter().find(|agent| agent["id"] == id))
            .map(|agent| agent["status"].clone())
    };
    assert_eq!(
        status("claude-code"),
        Some(serde_json::json!("tested")),
        "{json}"
    );
    assert_eq!(status("codex"), Some(serde_json::json!("tested")), "{json}");
    assert_eq!(
        status("antigravity-cli"),
        Some(serde_json::json!("not_installed")),
        "{json}"
    );
}

#[test]
fn run_outside_a_repository_fails() {
    let repo = TempRepo::new().unwrap();
    let output = argonauts(&repo, &elsewhere(&repo), &["run", "--solo", "Add a flag"]);
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    assert!(
        stderr(&output).contains("isn't inside a git repository"),
        "{}",
        describe(&output)
    );
}

#[test]
fn run_flag_mistakes_are_usage_errors() {
    let repo = TempRepo::new().unwrap();
    for args in [
        &["run", "--gate", "test -f 'x", "task"][..],
        &["run", "--solo", "--crew", "task"][..],
        &["run", "--budget-usd", "0", "task"][..],
    ] {
        let output = argonauts(&repo, repo.root(), args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            describe(&output)
        );
    }
}

#[test]
fn run_gate_and_merge_without_a_terminal_are_refused_by_name() {
    let repo = TempRepo::new().unwrap();
    for (args, flag) in [
        (
            &["run", "--solo", "--merge", "leave", "--detach", "say hi"][..],
            "--merge",
        ),
        (
            &["run", "--solo", "--gate", "true", "--detach", "say hi"][..],
            "--gate",
        ),
    ] {
        let output = argonauts(&repo, repo.root(), args);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{args:?}: {}",
            describe(&output)
        );
        let error = stderr(&output);
        assert!(
            error.contains(&format!("so {flag} and --set")),
            "{}",
            describe(&output)
        );
        assert!(
            error.contains("set it in .argonauts/config.toml"),
            "{}",
            describe(&output)
        );
        assert!(stdout(&output).is_empty(), "{}", describe(&output));
    }
}

#[test]
fn status_outside_a_repo_lists_nothing() {
    let repo = TempRepo::new().unwrap();
    let output = argonauts(&repo, &elsewhere(&repo), &["status", "--json"]);
    assert!(output.status.success(), "{}", describe(&output));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json, serde_json::json!({ "live": [], "runs": [] }));
    let output = argonauts(&repo, &elsewhere(&repo), &["status"]);
    assert_eq!(stdout(&output), "Nothing is running.\n");
}

#[test]
fn inbox_needs_a_repo() {
    let repo = TempRepo::new().unwrap();
    let output = argonauts(&repo, &elsewhere(&repo), &["inbox"]);
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    assert!(stderr(&output).contains("inside a git repository"));
}

#[test]
fn auto_off_leaves_its_marker_in_your_home() {
    let repo = TempRepo::new().unwrap();
    for dir in [elsewhere(&repo), repo.root().to_owned()] {
        let output = argonauts(&repo, &dir, &["auto", "off"]);
        assert!(output.status.success(), "{}", describe(&output));
        assert!(stdout(&output).starts_with("Auto is off."));
        assert!(repo.home().join("auto-off").exists());
    }
}

#[test]
fn models_says_how_to_discover_some() {
    let repo = TempRepo::new().unwrap();
    let output = argonauts(&repo, &elsewhere(&repo), &["models"]);
    assert!(output.status.success(), "{}", describe(&output));
    assert!(
        stdout(&output).contains("--discover"),
        "{}",
        describe(&output)
    );
}

#[test]
fn approve_needs_a_terminal_and_refuses_inside_an_agent() {
    let repo = TempRepo::new().unwrap();
    let output = argonauts(&repo, repo.root(), &["approve", "run-03", "cp-001"]);
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    assert!(
        stderr(&output).contains("approving needs you at a terminal"),
        "{}",
        describe(&output)
    );

    let output = Command::new(env!("CARGO_BIN_EXE_argonauts"))
        .args(["approve", "run-03", "cp-001"])
        .current_dir(repo.root())
        .envs(repo.envs())
        .env("OPENCLAW_SHELL", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    assert!(
        stderr(&output).contains("inside an agent session (OPENCLAW_SHELL is set)"),
        "{}",
        describe(&output)
    );

    for args in [
        &["approve", "run/03", "cp-001"][..],
        &["approve", "run-03", "cp 1"][..],
        &["approve", "run-03"][..],
    ] {
        let output = argonauts(&repo, repo.root(), args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            describe(&output)
        );
    }
}

#[test]
fn mcp_serve_refuses_a_repo_you_havent_set_up() {
    let repo = TempRepo::new().unwrap();
    let root = repo.root().to_str().unwrap();
    let output = argonauts(&repo, &elsewhere(&repo), &["mcp", "serve", "--repo", root]);
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    assert!(
        stderr(&output).contains("this repo isn't trusted"),
        "{}",
        describe(&output)
    );
    assert!(stdout(&output).is_empty(), "{}", describe(&output));
}

/// An MCP client on the server's stdin and stdout, one JSON-RPC message a line.
struct McpClient {
    stdin: std::process::ChildStdin,
    lines: mpsc::Receiver<String>,
    next_id: u64,
}

impl McpClient {
    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.next_id += 1;
        let id = self.next_id;
        let message = serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        });
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(20))
                .expect("an answer in time");
            let message: serde_json::Value = serde_json::from_str(&line).unwrap();
            if message["id"] == serde_json::json!(id) && message.get("method").is_none() {
                return message;
            }
        }
    }

    fn notify(&mut self, method: &str) {
        let message = serde_json::json!({ "jsonrpc": "2.0", "method": method });
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }
}

#[test]
fn mcp_serve_rereads_the_repos_trust_on_every_call() {
    let repo = TempRepo::new().unwrap();
    fs::create_dir_all(repo.root().join(".argonauts")).unwrap();
    let root = repo.root().to_str().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_argonauts"))
        .args(["mcp", "serve", "--repo", root])
        .current_dir(elsewhere(&repo))
        .envs(repo.envs())
        .env_remove("ARGONAUTS_SESSION")
        .env_remove("ARGONAUTS_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let (sender, lines) = mpsc::channel();
    let stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let mut client = McpClient {
        stdin: child.stdin.take().unwrap(),
        lines,
        next_id: 0,
    };

    let hello = client.request(
        "initialize",
        serde_json::json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "cli-test", "version": "1.0" }
        }),
    );
    assert!(hello["result"]["serverInfo"].is_object(), "{hello}");
    client.notify("notifications/initialized");
    let tools = client.request("tools/list", serde_json::json!({}));
    assert!(
        tools["result"]["tools"]
            .as_array()
            .is_some_and(|t| !t.is_empty()),
        "{tools}"
    );

    // A settings file you haven't trusted turns up while the server runs.
    fs::write(
        repo.root().join(".argonauts/config.toml"),
        "[mcp]\nruns_per_day = 300\n",
    )
    .unwrap();
    let refused = client.request("tools/list", serde_json::json!({}));
    assert!(
        refused["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("isn't trusted any more")),
        "{refused}"
    );

    drop(client);
    let _ = child.kill();
    let _ = child.wait();
}
