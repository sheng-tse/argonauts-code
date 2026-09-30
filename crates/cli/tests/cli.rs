//! End-to-end runs of the binary, offline: each agent CLI is a fake script on `PATH` that
//! writes `hello.txt` and replays a recorded fixture.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

const TASK: &str = "Create a file named hello.txt containing the word hi";

struct Fake {
    agent: &'static str,
    binary: &'static str,
    version_output: &'static str,
    fixture: &'static str,
    cheapest_model: &'static str,
}

const CLAUDE: Fake = Fake {
    agent: "claude-code",
    binary: "claude",
    version_output: "2.1.284 (Claude Code)",
    fixture: "claude-code/2.1.284/write",
    cheapest_model: "haiku",
};

const CODEX: Fake = Fake {
    agent: "codex",
    binary: "codex",
    version_output: "codex-cli 0.158.0",
    fixture: "codex/0.158.0/build_hello",
    cheapest_model: "gpt-5.6-luna",
};

const AGY: Fake = Fake {
    agent: "antigravity-cli",
    binary: "agy",
    version_output: "1.2.13",
    fixture: "antigravity-cli/1.2.13/build_hello_schema",
    cheapest_model: "gemini-3.6-flash-low",
};

struct Sandbox {
    _dir: TempDir,
    root: PathBuf,
    repo: PathBuf,
    bin: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // Canonical, so paths match what git reports on macOS, where /var is /private/var.
        let root = dir.path().canonicalize().unwrap();
        let sandbox = Self {
            repo: root.join("repo"),
            bin: root.join("bin"),
            root,
            _dir: dir,
        };
        for sub in ["repo", "bin", "home", "config", "argonauts"] {
            fs::create_dir_all(sandbox.root.join(sub)).unwrap();
        }
        sandbox.git(&["init", "-q", "-b", "main"]);
        fs::write(sandbox.repo.join("README.md"), "# demo\n").unwrap();
        sandbox.git(&["add", "README.md"]);
        sandbox.git(&["commit", "-q", "-m", "initial"]);
        sandbox
    }

    /// `name` is `<adapter>/<version>/<file>`; each adapter keeps its fixtures in its own crate.
    fn fixture(name: &str) -> PathBuf {
        let (adapter, rest) = name
            .split_once('/')
            .expect("fixture names start with the adapter");
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../adapters")
            .join(adapter)
            .join("fixtures")
            .join(rest)
    }

    /// `work` runs in the agent's working directory before the replay.
    fn install(&self, fake: &Fake, work: &str) {
        let stdout = Self::fixture(&format!("{}.stdout.jsonl", fake.fixture));
        let stderr = Self::fixture(&format!("{}.stderr.txt", fake.fixture));
        assert!(stdout.is_file(), "missing fixture {}", stdout.display());
        let args = self.args_file(fake);
        let script = format!(
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo '{version}'; exit 0; fi
printf '%s\n' "$@" > '{args}'
cat > /dev/null
{work}
cat '{stdout}'
cat '{stderr}' >&2
"#,
            version = fake.version_output,
            args = args.display(),
            stdout = stdout.display(),
            stderr = stderr.display(),
        );
        let path = self.bin.join(fake.binary);
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn args_file(&self, fake: &Fake) -> PathBuf {
        self.bin.join(format!("{}.args", fake.binary))
    }

    fn recorded_args(&self, fake: &Fake) -> Vec<String> {
        fs::read_to_string(self.args_file(fake))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn argonauts(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_argonauts"));
        command
            .args(args)
            .current_dir(&self.repo)
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("ARGONAUTS_HOME", self.root.join("argonauts"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("NO_COLOR", "1")
            .env_remove("ARGONAUTS_LOG")
            .stdin(Stdio::null());
        for key in argonauts_core::VENDOR_KEY_ENV_VARS
            .iter()
            .chain(argonauts_core::PARENT_SESSION_ENV_VARS)
        {
            command.env_remove(key);
        }
        command
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    fn card_branches(&self) -> Vec<String> {
        self.git(&[
            "branch",
            "--list",
            "argonauts/*",
            "--format=%(refname:short)",
        ])
        .lines()
        .map(str::to_owned)
        .collect()
    }

    fn event_logs(&self) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut pending = vec![self.root.join("argonauts")];
        while let Some(dir) = pending.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.file_name().is_some_and(|n| n == "events.jsonl") {
                    found.push(path);
                }
            }
        }
        found
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn describe(output: &Output) -> String {
    format!(
        "status {:?}\n--- stdout\n{}\n--- stderr\n{}",
        output.status,
        text(&output.stdout),
        text(&output.stderr)
    )
}

fn run_and_leave(fake: &Fake) {
    let sandbox = Sandbox::new();
    sandbox.install(fake, "printf 'hi\\n' > hello.txt");
    let output = sandbox
        .argonauts(&[
            "run",
            "--solo",
            "--plain",
            "--agent",
            fake.agent,
            "--model",
            fake.cheapest_model,
            "--effort",
            "low",
            "--merge",
            "leave",
            "--gate",
            "test -f hello.txt",
            TASK,
        ])
        .output()
        .unwrap();
    let report = describe(&output);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let stdout = text(&output.stdout);
    assert!(stdout.contains("run finished: branch left"), "{report}");
    assert!(stdout.contains("test -f hello.txt"), "{report}");
    assert!(stdout.contains("log: "), "{report}");

    let branches = sandbox.card_branches();
    assert_eq!(branches.len(), 1, "{branches:?}");
    let committed = sandbox.git(&["show", &format!("{}:hello.txt", branches[0])]);
    assert_eq!(committed, "hi\n");
    assert!(
        !sandbox.repo.join("hello.txt").exists(),
        "leave must not merge"
    );

    let args = sandbox.recorded_args(fake);
    assert!(
        args.iter().any(|a| a == fake.cheapest_model),
        "the model flag reached the CLI: {args:?}"
    );

    let logs = sandbox.event_logs();
    assert_eq!(logs.len(), 1, "{logs:?}");
    let log = fs::read_to_string(&logs[0]).unwrap();
    assert!(log.contains(r#""passed":true"#), "the gate passed");
    assert!(log.contains(r#""outcome":"branch_left""#), "{log}");
}

#[test]
fn run_with_claude_code_leaves_a_committed_branch() {
    run_and_leave(&CLAUDE);
}

#[test]
fn run_with_codex_leaves_a_committed_branch() {
    run_and_leave(&CODEX);
}

#[test]
fn run_with_antigravity_cli_leaves_a_committed_branch() {
    run_and_leave(&AGY);
}

#[test]
fn approving_on_stdin_merges_into_the_starting_branch() {
    let sandbox = Sandbox::new();
    sandbox.install(&CLAUDE, "printf 'hi\\n' > hello.txt");
    let mut child = sandbox
        .argonauts(&["run", "--plain", "--gate", "test -f hello.txt", TASK])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"a\ny\n").unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let report = describe(&output);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert!(
        text(&output.stdout).contains("run finished: merged"),
        "{report}"
    );
    assert_eq!(
        fs::read_to_string(sandbox.repo.join("hello.txt")).unwrap(),
        "hi\n"
    );
}

#[test]
fn an_agent_that_changes_nothing_fails_the_run() {
    let sandbox = Sandbox::new();
    sandbox.install(&CLAUDE, ":");
    let output = sandbox
        .argonauts(&["run", "--plain", "--merge", "leave", TASK])
        .output()
        .unwrap();
    let report = describe(&output);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert!(
        text(&output.stdout).contains("run finished: failed"),
        "{report}"
    );
    assert!(
        sandbox.card_branches().is_empty(),
        "the empty branch is removed"
    );
}

fn start_run(sandbox: &Sandbox, work: &str) -> std::process::Child {
    sandbox.install(&CLAUDE, work);
    let child = sandbox
        .argonauts(&["run", "--plain", "--merge", "leave", TASK])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let started = sandbox
            .event_logs()
            .iter()
            .any(|log| fs::read_to_string(log).is_ok_and(|text| text.contains("session_started")));
        if started {
            break;
        }
        assert!(Instant::now() < deadline, "the session never started");
        std::thread::sleep(Duration::from_millis(50));
    }
    // Give the fake agent time to write its file.
    std::thread::sleep(Duration::from_millis(300));
    child
}

fn interrupt(child: &std::process::Child) {
    let sent = Command::new("/bin/kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(sent.success());
}

fn fake_agent_running(sandbox: &Sandbox) -> bool {
    let script = sandbox.bin.join(CLAUDE.binary);
    Command::new("/usr/bin/pgrep")
        .args(["-f", &script.display().to_string()])
        .stdout(Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[test]
fn ctrl_c_stops_the_agent_and_keeps_its_work() {
    let sandbox = Sandbox::new();
    // Writes its file, then works "forever" in short sleeps, as a responsive CLI would.
    let child = start_run(
        &sandbox,
        "printf 'hi\\n' > hello.txt\nwhile true; do sleep 0.1; done",
    );
    interrupt(&child);

    let output = child.wait_with_output().unwrap();
    let report = describe(&output);
    assert_eq!(output.status.code(), Some(130), "{report}");
    assert!(
        text(&output.stdout).contains("run finished: cancelled"),
        "{report}"
    );
    let branches = sandbox.card_branches();
    assert_eq!(branches.len(), 1, "{report}");
    let committed = sandbox.git(&["show", &format!("{}:hello.txt", branches[0])]);
    assert_eq!(committed, "hi\n");
}

#[test]
fn a_second_ctrl_c_quits_at_once_and_kills_the_agent() {
    let sandbox = Sandbox::new();
    // Ignores SIGINT and SIGTERM, so a clean stop would take the whole stop plan.
    let child = start_run(
        &sandbox,
        "trap '' INT TERM\nprintf 'hi\\n' > hello.txt\nwhile true; do sleep 0.1; done",
    );
    let started = Instant::now();
    interrupt(&child);
    std::thread::sleep(Duration::from_millis(300));
    interrupt(&child);

    let output = child.wait_with_output().unwrap();
    let report = describe(&output);
    assert_eq!(output.status.code(), Some(130), "{report}");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "quit without waiting for the stop plan: {report}"
    );
    assert!(
        text(&output.stderr).contains("Quit without waiting"),
        "{report}"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while fake_agent_running(&sandbox) {
        assert!(Instant::now() < deadline, "the agent outlived argonauts");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn run_fails_early_when_the_agent_cli_is_missing() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .argonauts(&["run", "--plain", "--agent", "codex", TASK])
        .output()
        .unwrap();
    let report = describe(&output);
    assert_eq!(output.status.code(), Some(1), "{report}");
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("Codex (`codex`) is not installed"),
        "{report}"
    );
    assert!(stderr.contains("argonauts doctor"), "{report}");
    assert!(sandbox.event_logs().is_empty(), "nothing started");
}

#[test]
fn run_outside_a_repository_fails() {
    let sandbox = Sandbox::new();
    sandbox.install(&CLAUDE, ":");
    let output = sandbox
        .argonauts(&["run", "--plain", TASK])
        .current_dir(sandbox.root.join("home"))
        .output()
        .unwrap();
    let report = describe(&output);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert!(
        text(&output.stderr).contains("is not inside a git repository"),
        "{report}"
    );
}

#[test]
fn unknown_agents_are_usage_errors() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .argonauts(&["run", "--agent", "gemini", TASK])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", describe(&output));
    assert!(text(&output.stderr).contains("claude-code"));
}

#[test]
fn bad_gates_and_overrides_are_usage_errors() {
    let sandbox = Sandbox::new();
    sandbox.install(&CLAUDE, ":");
    for args in [
        &["run", "--plain", "--gate", "test -f 'x", TASK][..],
        &["run", "--plain", "--set", "budget.unit=yen", TASK][..],
    ] {
        let output = sandbox.argonauts(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{}", describe(&output));
        assert!(sandbox.event_logs().is_empty());
    }
}

#[test]
fn a_dollar_budget_codex_cannot_report_is_refused() {
    let sandbox = Sandbox::new();
    sandbox.install(&CODEX, "printf 'hi\\n' > hello.txt");
    let output = sandbox
        .argonauts(&["run", "--plain", "--agent", "codex", TASK])
        .output()
        .unwrap();
    let report = describe(&output);
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert!(
        text(&output.stderr).contains("can't enforce a dollar budget"),
        "{report}"
    );
    assert!(
        text(&output.stderr).contains("--model gpt-5.6-luna"),
        "{report}"
    );
    assert!(sandbox.event_logs().is_empty(), "nothing started");
}

#[test]
fn cheapest_picks_the_cheapest_model_and_effort() {
    let sandbox = Sandbox::new();
    sandbox.install(&CODEX, "printf 'hi\\n' > hello.txt");
    let output = sandbox
        .argonauts(&[
            "run",
            "--plain",
            "--agent",
            "codex",
            "--cheapest",
            "--merge",
            "leave",
            TASK,
        ])
        .output()
        .unwrap();
    let report = describe(&output);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let args = sandbox.recorded_args(&CODEX);
    assert!(args.iter().any(|a| a == "gpt-5.6-luna"), "{args:?}");
    assert!(
        args.iter().any(|a| a == "model_reasoning_effort=\"low\""),
        "{args:?}"
    );
}

#[test]
fn doctor_lists_every_cli_with_its_version() {
    let sandbox = Sandbox::new();
    sandbox.install(&CLAUDE, ":");
    sandbox.install(&CODEX, ":");
    let output = sandbox.argonauts(&["doctor"]).output().unwrap();
    let report = describe(&output);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let stdout = text(&output.stdout);
    let line = |id: &str| {
        stdout
            .lines()
            .find(|l| l.starts_with(id))
            .unwrap_or_else(|| panic!("no {id} line in {report}"))
            .to_owned()
    };
    assert!(line("claude-code").contains("2.1.284"), "{report}");
    assert!(line("claude-code").contains("tested"), "{report}");
    assert!(line("codex").contains("0.158.0"), "{report}");
    assert!(
        line("antigravity-cli").contains("not installed"),
        "{report}"
    );
    assert!(stdout.contains("Ready with 2 of 3 agent CLIs."), "{report}");

    let output = sandbox.argonauts(&["doctor", "--json"]).output().unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["agents"][0]["status"], "tested");
    assert_eq!(json["agents"][0]["cheapest_model"], "haiku");
    assert_eq!(json["agents"][2]["status"], "not_installed");
}

#[test]
fn doctor_fails_when_no_agent_cli_is_installed() {
    let sandbox = Sandbox::new();
    let output = sandbox.argonauts(&["doctor"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    assert!(text(&output.stdout).contains("Not ready"));
}

#[test]
fn config_show_includes_the_project_file_and_overrides() {
    let sandbox = Sandbox::new();
    fs::create_dir_all(sandbox.repo.join(".argonauts")).unwrap();
    fs::write(
        sandbox.repo.join(".argonauts/config.toml"),
        "default_agent = \"codex\"\n[budget]\nper_card = 1.5\n",
    )
    .unwrap();
    // Your own project file sits between the shared one and the flags.
    fs::write(
        sandbox.repo.join(".argonauts/config.local.toml"),
        "[budget]\nper_card = 2.5\n",
    )
    .unwrap();
    let output = sandbox
        .argonauts(&["config", "show", "--set", "merge.mode=leave"])
        .output()
        .unwrap();
    let report = describe(&output);
    assert_eq!(output.status.code(), Some(0), "{report}");
    let stdout = text(&output.stdout);
    assert!(stdout.contains("config.toml (applied)"), "{report}");
    assert!(stdout.contains("config.local.toml (applied)"), "{report}");
    assert!(stdout.contains("default_agent = \"codex\""), "{report}");
    assert!(stdout.contains("per_card = 2.5"), "{report}");
    assert!(!stdout.contains("per_card = 1.5"), "{report}");
    assert!(stdout.contains("mode = \"leave\""), "{report}");

    let output = sandbox
        .argonauts(&["config", "show", "--set", "budget.unit=yen"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", describe(&output));
    assert!(text(&output.stderr).contains("budget.unit=yen"));
}

#[test]
fn config_schema_is_json_schema() {
    let sandbox = Sandbox::new();
    let output = sandbox.argonauts(&["config", "schema"]).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    let schema: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(schema["properties"]["budget"].is_object(), "{schema}");
    assert!(schema["properties"]["gates"].is_object(), "{schema}");
}
