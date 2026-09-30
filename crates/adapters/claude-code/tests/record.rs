//! Records Claude Code fixtures with live calls on the cheapest model. Ignored by default.
//!
//! A test runs only when `ARGONAUTS_RECORD` names it and `ARGONAUTS_LIVE_DIR` is an absolute
//! scratch folder outside this repository. Record one call per run, for example
//!
//! ```text
//! ARGONAUTS_LIVE_DIR=<dir> ARGONAUTS_RECORD=record_write \
//!     cargo test -p argonauts-adapter-claude-code --test record -- --ignored record_write
//! ```

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use argonauts_adapter_claude_code::ClaudeCodeAdapter;
use argonauts_core::{
    Adapter, BudgetLimit, Card, CommandSpec, SessionRequest, SessionRole, render_solo_prompt,
    report_json_schema,
};
use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const TIMEOUT: Duration = Duration::from_secs(300);

struct Recording {
    test: &'static str,
    name: &'static str,
    role: SessionRole,
    prompt: String,
    report_schema: bool,
    max_budget_usd: Option<f64>,
}

#[test]
#[ignore = "live call that spends Claude quota"]
fn record_read_only() {
    record(Recording {
        test: "record_read_only",
        name: "read-only",
        role: SessionRole::ReadOnly,
        prompt: "Reply with exactly the word: ok".into(),
        report_schema: false,
        max_budget_usd: None,
    });
}

#[test]
#[ignore = "live call that spends Claude quota"]
fn record_write() {
    let task = "Create a file named hello.txt containing the word hi. Then reply with: done";
    let card = Card::solo(task, BudgetLimit::Usd(0.5));
    record(Recording {
        test: "record_write",
        name: "write",
        role: SessionRole::Build,
        prompt: render_solo_prompt(&card, &report_json_schema()),
        report_schema: true,
        max_budget_usd: Some(0.5),
    });
}

#[test]
#[ignore = "live call that spends Claude quota"]
fn record_shell() {
    record(Recording {
        test: "record_shell",
        name: "shell",
        role: SessionRole::Build,
        prompt: "Run `ls` and reply with the number of files".into(),
        report_schema: false,
        max_budget_usd: None,
    });
}

fn record(recording: Recording) {
    let Some(live_root) = live_root(recording.test) else {
        return;
    };
    let adapter = ClaudeCodeAdapter::new();
    let version = cli_version(&adapter);
    let repo = throwaway_repo(&live_root, recording.name);

    let (model, effort) = adapter.cheapest_model();
    let req = SessionRequest {
        workdir: repo.clone(),
        prompt: recording.prompt.clone(),
        model: Some(model.to_owned()),
        effort: effort.map(str::to_owned),
        role: recording.role,
        json_schema: recording.report_schema.then(report_json_schema),
        max_budget_usd: recording.max_budget_usd,
        ..SessionRequest::default()
    };
    let spec = adapter.command(&req).expect("command");
    let Run {
        status,
        stdout,
        stderr,
        duration_ms,
    } = run(&spec);

    let scrub = Scrubber::new(&live_root);
    let dir = fixture_dir().join(&version);
    fs::create_dir_all(&dir).expect("fixture dir");
    let base = dir.join(recording.name);
    fs::write(with_ext(&base, "stdout.jsonl"), scrub.stdout(&stdout)).expect("write stdout");
    fs::write(with_ext(&base, "stderr.txt"), scrub.apply(&stderr)).expect("write stderr");
    let meta = json!({
        "adapter": adapter.id(),
        "cli_version": version,
        "date": OffsetDateTime::now_utc().format(&Rfc3339).expect("date"),
        "program": spec.program,
        "args": spec.args.iter().map(|a| scrub.apply(a)).collect::<Vec<_>>(),
        "stdin": scrub.apply(spec.stdin.as_deref().unwrap_or_default()),
        "env_removed": spec.env_remove,
        "env_set": spec.env_set.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>(),
        "model": model,
        "effort": effort,
        "role": recording.role,
        "exit_code": status.code(),
        "duration_ms": duration_ms,
        "files_after": files_in(&repo),
    });
    let meta = serde_json::to_string_pretty(&meta).expect("meta json");
    fs::write(with_ext(&base, "meta.json"), meta + "\n").expect("write meta");
    println!(
        "recorded {} (exit {:?}, {duration_ms} ms) in {}",
        recording.name,
        status.code(),
        dir.display()
    );
}

struct Run {
    status: ExitStatus,
    stdout: String,
    stderr: String,
    duration_ms: u64,
}

fn run(spec: &CommandSpec) -> Run {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in &spec.env_remove {
        command.env_remove(name);
    }
    for (name, value) in &spec.env_set {
        command.env(name, value);
    }

    let started = Instant::now();
    let mut child = command.spawn().expect("spawn claude");
    let mut stdin = child.stdin.take().expect("stdin");
    stdin
        .write_all(spec.stdin.as_deref().unwrap_or_default().as_bytes())
        .expect("write prompt");
    drop(stdin);
    let stdout = read_in_background(child.stdout.take().expect("stdout"));
    let stderr = read_in_background(child.stderr.take().expect("stderr"));
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if started.elapsed() > TIMEOUT {
            child.kill().expect("kill");
            break child.wait().expect("wait after kill");
        }
        thread::sleep(Duration::from_millis(200));
    };
    Run {
        status,
        duration_ms: started.elapsed().as_millis() as u64,
        stdout: stdout.join().expect("stdout reader"),
        stderr: stderr.join().expect("stderr reader"),
    }
}

/// Makes no live calls, but is gated like the recorders.
#[test]
#[ignore = "rewrites fixture files"]
fn scrub_existing_fixtures() {
    let Some(live_root) = live_root("scrub_existing_fixtures") else {
        return;
    };
    let scrub = Scrubber::new(&live_root);
    for version in fs::read_dir(fixture_dir()).expect("fixture dir") {
        for file in fs::read_dir(version.expect("entry").path()).expect("version dir") {
            let path = file.expect("entry").path();
            let text = fs::read_to_string(&path).expect("read fixture");
            let scrubbed = if path.to_string_lossy().ends_with(".stdout.jsonl") {
                scrub.stdout(&text)
            } else {
                scrub.apply(&text)
            };
            fs::write(&path, scrubbed).expect("write fixture");
        }
    }
}

fn live_root(test: &str) -> Option<PathBuf> {
    if env::var("ARGONAUTS_RECORD").ok().as_deref() != Some(test) {
        eprintln!("skipping {test}: set ARGONAUTS_RECORD={test} and ARGONAUTS_LIVE_DIR");
        return None;
    }
    let root = PathBuf::from(env::var("ARGONAUTS_LIVE_DIR").expect("set ARGONAUTS_LIVE_DIR"));
    assert!(root.is_absolute(), "ARGONAUTS_LIVE_DIR must be absolute");
    fs::create_dir_all(&root).expect("live dir");
    let root = root.canonicalize().expect("canonical live dir");
    let repo_root = repo_root();
    assert!(
        !root.starts_with(&repo_root),
        "live calls must run outside the Argonauts repository"
    );
    Some(root)
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn cli_version(adapter: &ClaudeCodeAdapter) -> String {
    let output = Command::new(adapter.binary())
        .args(adapter.version_args())
        .output()
        .expect("run claude --version");
    adapter
        .parse_version(&String::from_utf8_lossy(&output.stdout))
        .expect("parse version")
}

fn throwaway_repo(live_root: &Path, name: &str) -> PathBuf {
    let stamp = OffsetDateTime::now_utc().unix_timestamp();
    let repo = live_root.join(format!("{name}-{stamp}"));
    fs::create_dir_all(&repo).expect("repo dir");
    fs::write(repo.join("README.md"), "# Throwaway\n").expect("readme");
    fs::write(repo.join("notes.txt"), "scratch\n").expect("notes");
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
            ])
            .args([
                "-c",
                "user.name=Argonauts",
                "-c",
                "user.email=argonauts@example.invalid",
            ])
            .args(args)
            .current_dir(&repo)
            .stdout(Stdio::null())
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "init"]);
    repo
}

fn files_in(repo: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(repo)
        .expect("read repo")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name != ".git")
        .collect();
    names.sort();
    names
}

fn read_in_background(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).expect("read pipe");
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

fn with_ext(base: &Path, ext: &str) -> PathBuf {
    let mut name = base.as_os_str().to_owned();
    name.push(".");
    name.push(ext);
    PathBuf::from(name)
}

/// Replaces local paths and the user name, and refuses to write anything that looks like a key.
struct Scrubber {
    replacements: Vec<(String, String)>,
}

const PLACEHOLDER_ROOT: &str = "/tmp/argonauts-live/claude-code";

/// Init fields that describe the recording machine's own setup rather than the session.
const LOCAL_SETUP_FIELDS: &[&str] = &[
    "slash_commands",
    "terminal_slash_commands",
    "skills",
    "agents",
    "plugins",
];

impl Scrubber {
    fn new(live_root: &Path) -> Self {
        let mut replacements = Vec::new();
        for entry in fs::read_dir(live_root)
            .expect("live dir")
            .filter_map(Result::ok)
        {
            let dir_name = entry.file_name().to_string_lossy().into_owned();
            let Some((name, stamp)) = dir_name.rsplit_once('-') else {
                continue;
            };
            if !entry.path().is_dir() || !stamp.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let placeholder = format!("{PLACEHOLDER_ROOT}/{name}");
            push_both_forms(&mut replacements, &entry.path(), &placeholder);
        }
        // Longer paths first, so a repo is replaced before the live root that contains it.
        replacements.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));
        push_both_forms(&mut replacements, live_root, PLACEHOLDER_ROOT);
        if let Some(home) = env::var_os("HOME") {
            replacements.push((home.to_string_lossy().into_owned(), "/home/user".to_owned()));
        }
        if let Ok(user) = env::var("USER")
            && user.len() >= 3
        {
            replacements.push((user, "user".to_owned()));
        }
        Self { replacements }
    }

    fn apply(&self, text: &str) -> String {
        let scrubbed = self
            .replacements
            .iter()
            .fold(text.to_owned(), |acc, (from, to)| acc.replace(from, to));
        for marker in ["sk-ant-", "sk-proj-", "ghp_", "github_pat_", "AIza"] {
            assert!(
                !scrubbed.contains(marker),
                "output contains something that looks like a secret ({marker}...)"
            );
        }
        scrubbed
    }

    fn stdout(&self, stdout: &str) -> String {
        let mut lines: Vec<Line> = stdout
            .lines()
            .map(|line| {
                let text = self.apply(line);
                Line {
                    value: serde_json::from_str(&text).ok(),
                    text,
                    changed: false,
                }
            })
            .collect();
        self.rejoin_fragments(&mut lines);
        for line in &mut lines {
            if let Some(value) = &mut line.value {
                line.changed |= scrub_init(value);
            }
        }
        let mut out = String::with_capacity(stdout.len());
        for line in lines {
            match (line.changed, line.value) {
                (true, Some(value)) => out.push_str(&value.to_string()),
                _ => out.push_str(&line.text),
            }
            out.push('\n');
        }
        out
    }

    /// A path can be split across delta events, so each content block's fragments are joined,
    /// scrubbed, and written back whole into the block's first delta.
    fn rejoin_fragments(&self, lines: &mut [Line]) {
        let mut open: HashMap<u64, Vec<usize>> = HashMap::new();
        for i in 0..lines.len() {
            let Some(event) = lines[i].value.as_ref().and_then(stream_event) else {
                continue;
            };
            let index = event["index"].as_u64().unwrap_or(0);
            match event["type"].as_str() {
                Some("content_block_start") => {
                    open.insert(index, Vec::new());
                }
                Some("content_block_delta") => open.entry(index).or_default().push(i),
                Some("content_block_stop") => {
                    if let Some(group) = open.remove(&index) {
                        self.rewrite_group(lines, &group);
                    }
                }
                _ => {}
            }
        }
        for group in open.into_values() {
            self.rewrite_group(lines, &group);
        }
    }

    fn rewrite_group(&self, lines: &mut [Line], group: &[usize]) {
        let joined: String = group
            .iter()
            .filter_map(|&i| fragment(lines[i].value.as_ref()?))
            .collect();
        let scrubbed = self.apply(&joined);
        if scrubbed == joined {
            return;
        }
        for (n, &i) in group.iter().enumerate() {
            let Some(value) = lines[i].value.as_mut() else {
                continue;
            };
            let delta = &mut value["event"]["delta"];
            let Some(key) = fragment_key(delta) else {
                continue;
            };
            delta[key] = json!(if n == 0 { scrubbed.as_str() } else { "" });
            lines[i].changed = true;
        }
    }
}

/// macOS temp paths show up both with and without their `/private` prefix.
fn push_both_forms(replacements: &mut Vec<(String, String)>, path: &Path, placeholder: &str) {
    let real = path.display().to_string();
    replacements.push((real.replacen("/private/", "/", 1), placeholder.to_owned()));
    replacements.push((real, placeholder.to_owned()));
}

struct Line {
    text: String,
    value: Option<serde_json::Value>,
    changed: bool,
}

fn stream_event(value: &serde_json::Value) -> Option<&serde_json::Value> {
    (value["type"] == "stream_event").then(|| &value["event"])
}

fn fragment_key(delta: &serde_json::Value) -> Option<&'static str> {
    match delta["type"].as_str()? {
        "text_delta" => Some("text"),
        "input_json_delta" => Some("partial_json"),
        "thinking_delta" => Some("thinking"),
        _ => None,
    }
}

fn fragment(value: &serde_json::Value) -> Option<&str> {
    let delta = &stream_event(value)?["delta"];
    delta[fragment_key(delta)?].as_str()
}

fn scrub_init(value: &mut serde_json::Value) -> bool {
    if value["type"] != "system" || value["subtype"] != "init" {
        return false;
    }
    let Some(init) = value.as_object_mut() else {
        return false;
    };
    for field in LOCAL_SETUP_FIELDS {
        if init.contains_key(*field) {
            init.insert((*field).to_owned(), json!([]));
        }
    }
    if init.contains_key("messaging_socket_path") {
        init.insert("messaging_socket_path".into(), json!("/tmp/redacted.sock"));
    }
    true
}
