//! Records Claude Code fixtures with live calls on the cheapest model. Ignored by default.
//!
//! A test runs only when `ARGONAUTS_RECORD` names it and `ARGONAUTS_LIVE_DIR` is an absolute
//! scratch folder outside this repository. Record one call per run, for example
//!
//! ```text
//! ARGONAUTS_LIVE_DIR=<dir> ARGONAUTS_RECORD=record_write \
//!     cargo test -p argonauts-adapter-claude-code --test it -- --ignored record::record_write
//! ```

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use argonauts_adapter_claude_code::{ClaudeCodeAdapter, STATUS_SERVER};
use argonauts_core::settings::sessions::{Compaction, NativeFanout};
use argonauts_core::{
    Adapter, CommandSpec, MeterLimit, SessionRequest, SessionRole, Task, render_captain_prompt,
    report_json_schema, sha256_hex,
};
use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

#[path = "../../../recorded_shape.rs"]
mod recorded_shape;

const TIMEOUT: Duration = Duration::from_secs(300);
/// A steered session whose last expected turn never comes is closed after this much quiet.
const QUIET_AFTER_RESULT: Duration = Duration::from_secs(45);

struct Recording {
    test: &'static str,
    name: &'static str,
    role: SessionRole,
    prompt: String,
    report_schema: bool,
    spend_cap_usd: Option<f64>,
    session: SessionShape,
}

/// What the newer recordings add: stdin kept open, session ids, and the status server.
#[derive(Default)]
struct SessionShape {
    keep_input_open: bool,
    /// Sent once the first tool call shows the first turn is under way.
    follow_up: Option<&'static str>,
    /// Stdin closes after this many results.
    results: usize,
    choose_session_id: bool,
    /// Resumes the session recorded under this fixture name, in the same repository.
    resume_from: Option<&'static str>,
    status_server: StatusServer,
    native_fanout: Option<NativeFanout>,
    compaction: Option<Compaction>,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum StatusServer {
    #[default]
    None,
    Working,
    /// An entry Claude Code rejects, to record `mcp_server_errors`.
    Broken,
}

#[test]
#[ignore = "live call that spends Claude plan usage"]
fn record_read_only() {
    record(Recording {
        test: "record_read_only",
        name: "read-only",
        role: SessionRole::ReadOnly,
        prompt: "Reply with exactly the word: ok".into(),
        report_schema: false,
        spend_cap_usd: None,
        session: SessionShape::default(),
    });
}

#[test]
#[ignore = "live call that spends Claude plan usage"]
fn record_write() {
    let request = "Create a file named hello.txt containing the word hi. Then reply with: done";
    let task = Task::captain(request, MeterLimit::Usd(0.5));
    record(Recording {
        test: "record_write",
        name: "write",
        role: SessionRole::Build,
        prompt: render_captain_prompt(&task, &report_json_schema()),
        report_schema: true,
        spend_cap_usd: Some(0.5),
        session: SessionShape::default(),
    });
}

#[test]
#[ignore = "live call that spends Claude plan usage"]
fn record_shell() {
    record(Recording {
        test: "record_shell",
        name: "shell",
        role: SessionRole::Build,
        prompt: "Run `ls` and reply with the number of files".into(),
        report_schema: false,
        spend_cap_usd: None,
        session: SessionShape::default(),
    });
}

/// Stream-json input with a second message sent mid-turn, a chosen session id, the status
/// tool, and one subagent.
#[test]
#[ignore = "live call that spends Claude plan usage"]
fn record_stream_input() {
    record(Recording {
        test: "record_stream_input",
        name: "stream-input",
        role: SessionRole::Build,
        prompt: "Do these steps in order. 1. Call the argonauts_status tool once. 2. Use the \
                 Agent tool once to start a general-purpose subagent with the prompt: Read \
                 README.md and reply with its first line. 3. Reply with the subagent's answer \
                 and the word: done."
            .into(),
        report_schema: false,
        spend_cap_usd: Some(0.5),
        session: SessionShape {
            keep_input_open: true,
            follow_up: Some("After that, reply with only the word: second"),
            results: 2,
            choose_session_id: true,
            status_server: StatusServer::Working,
            native_fanout: Some(NativeFanout::Limit { max: 1 }),
            compaction: Some(Compaction::Auto),
            ..SessionShape::default()
        },
    });
}

/// Resumes `stream-input` with every flag again.
#[test]
#[ignore = "live call that spends Claude plan usage"]
fn record_resume() {
    record(Recording {
        test: "record_resume",
        name: "resume",
        role: SessionRole::Build,
        prompt: "Which word did my second message ask you to reply with? Reply with just \
                 that word."
            .into(),
        report_schema: false,
        spend_cap_usd: Some(0.5),
        session: SessionShape {
            keep_input_open: true,
            results: 1,
            resume_from: Some("stream-input"),
            status_server: StatusServer::Working,
            native_fanout: Some(NativeFanout::Limit { max: 1 }),
            compaction: Some(Compaction::Auto),
            ..SessionShape::default()
        },
    });
}

/// An MCP config entry Claude Code rejects, which it reports only in `init`.
#[test]
#[ignore = "live call that spends Claude plan usage"]
fn record_status_tool_error() {
    record(Recording {
        test: "record_status_tool_error",
        name: "status-tool-error",
        role: SessionRole::ReadOnly,
        prompt: "Reply with exactly the word: ok".into(),
        report_schema: false,
        spend_cap_usd: None,
        session: SessionShape {
            status_server: StatusServer::Broken,
            ..SessionShape::default()
        },
    });
}

fn record(recording: Recording) {
    let Some(live_root) = live_root(recording.test) else {
        return;
    };
    let adapter = ClaudeCodeAdapter::new();
    let version = cli_version(&adapter);
    let shape = &recording.session;
    let (repo, resume_session) = match shape.resume_from {
        Some(earlier) => (
            latest_repo(&live_root, earlier),
            Some(recorded_session_id(earlier)),
        ),
        None => (throwaway_repo(&live_root, recording.name), None),
    };
    let scratch = live_root.join("sessions").join(recording.name);
    fs::create_dir_all(&scratch).expect("scratch dir");
    let mcp_config = status_server_config(&live_root, &scratch, shape.status_server);

    let (model, effort) = adapter.cheapest_model();
    let req = SessionRequest {
        workdir: repo.clone(),
        prompt: recording.prompt.clone(),
        model: Some(model.to_owned()),
        effort: effort.map(str::to_owned),
        role: recording.role,
        json_schema: recording.report_schema.then(report_json_schema),
        spend_cap_usd: recording.spend_cap_usd,
        goal_hash: (recording.role == SessionRole::Build)
            .then(|| sha256_hex(recording.prompt.as_bytes())),
        keep_input_open: shape.keep_input_open,
        session_id: shape.choose_session_id.then(new_uuid),
        resume_session,
        mcp_config,
        scratch_dir: Some(scratch),
        native_fanout: shape.native_fanout,
        compaction: shape.compaction,
        ..SessionRequest::default()
    };
    let spec = adapter.command(&req).expect("command");
    write_spec_files(&spec);
    let follow_up = shape.follow_up.and_then(|text| adapter.input_line(text));
    let Run {
        status,
        stdout,
        stderr,
        stdin_sent,
        duration_ms,
    } = if spec.keep_stdin_open {
        run_steered(&spec, follow_up, shape.results.max(1))
    } else {
        run(&spec)
    };

    let scrub = Scrubber::new(&live_root);
    let dir = fixture_dir();
    fs::create_dir_all(&dir).expect("fixture dir");
    let base = dir.join(recording.name);
    let stdout = scrub.stdout(&stdout);
    recorded_shape::report(&with_ext(&base, "stdout.jsonl"), &stdout);
    fs::write(with_ext(&base, "stdout.jsonl"), stdout).expect("write stdout");
    fs::write(with_ext(&base, "stderr.txt"), scrub.apply(&stderr)).expect("write stderr");
    let meta = json!({
        "adapter": adapter.id(),
        "cli_version": version,
        "also_matches": [],
        "date": OffsetDateTime::now_utc().format(&Rfc3339).expect("date"),
        "program": spec.program,
        "args": spec.args.iter().map(|a| scrub.apply(a)).collect::<Vec<_>>(),
        "stdin": scrub.apply(&stdin_sent),
        "session_id": req.session_id,
        "resume_session": req.resume_session,
        "keep_input_open": req.keep_input_open,
        "files": spec.files.iter().map(|f| json!({
            "path": scrub.apply(&f.path.display().to_string()),
            "contents": f.contents,
        })).collect::<Vec<_>>(),
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
    /// Everything written to stdin, in order.
    stdin_sent: String,
    duration_ms: u64,
}

fn command_for(spec: &CommandSpec) -> Command {
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
    command
}

fn run(spec: &CommandSpec) -> Run {
    let started = Instant::now();
    let mut child = command_for(spec).spawn().expect("spawn claude");
    let mut stdin = child.stdin.take().expect("stdin");
    let prompt = spec.stdin.clone().unwrap_or_default();
    stdin.write_all(prompt.as_bytes()).expect("write prompt");
    drop(stdin);
    let stdout = read_in_background(child.stdout.take().expect("stdout"));
    let stderr = read_in_background(child.stderr.take().expect("stderr"));
    let status = wait_with_timeout(&mut child, started);
    Run {
        status,
        duration_ms: started.elapsed().as_millis() as u64,
        stdout: stdout.join().expect("stdout reader"),
        stderr: stderr.join().expect("stderr reader"),
        stdin_sent: prompt,
    }
}

/// Keeps stdin open the way the CLI runner does: the first message, then `follow_up`
/// once the first tool call arrives, then stdin closes after `results` results.
fn run_steered(spec: &CommandSpec, follow_up: Option<String>, results: usize) -> Run {
    let started = Instant::now();
    let mut child = command_for(spec).spawn().expect("spawn claude");
    let mut stdin = Some(child.stdin.take().expect("stdin"));
    let mut sent = spec.stdin.clone().unwrap_or_default();
    write_input(stdin.as_mut(), &sent);
    let stderr = read_in_background(child.stderr.take().expect("stderr"));
    let (line_tx, lines) = mpsc::channel::<String>();
    let reader = BufReader::new(child.stdout.take().expect("stdout"));
    let stdout_reader = thread::spawn(move || {
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });

    let mut follow_up = follow_up;
    let mut stdout = String::new();
    let mut seen_results = 0;
    let mut last_result: Option<Instant> = None;
    while started.elapsed() < TIMEOUT {
        match lines.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                let value: serde_json::Value =
                    serde_json::from_str(&line).unwrap_or(serde_json::Value::Null);
                stdout.push_str(&line);
                stdout.push('\n');
                if value["type"] == "result" {
                    seen_results += 1;
                    last_result = Some(Instant::now());
                }
                if is_tool_call(&value)
                    && let Some(text) = follow_up.take()
                {
                    write_input(stdin.as_mut(), &text);
                    sent.push_str(&text);
                }
                if seen_results >= results {
                    stdin = None;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if last_result.is_some_and(|at| at.elapsed() > QUIET_AFTER_RESULT) {
                    stdin = None;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if stdin.is_none() && child.try_wait().expect("wait").is_some() {
            break;
        }
    }
    drop(stdin);
    let status = wait_with_timeout(&mut child, started);
    stdout_reader.join().expect("stdout reader");
    stdout.extend(lines.try_iter().map(|line| line + "\n"));
    Run {
        status,
        duration_ms: started.elapsed().as_millis() as u64,
        stdout,
        stderr: stderr.join().expect("stderr reader"),
        stdin_sent: sent,
    }
}

fn write_input(stdin: Option<&mut ChildStdin>, text: &str) {
    if let Some(stdin) = stdin {
        stdin.write_all(text.as_bytes()).expect("write input");
        stdin.flush().expect("flush input");
    }
}

fn is_tool_call(line: &serde_json::Value) -> bool {
    line["type"] == "assistant"
        && line["message"]["content"]
            .as_array()
            .is_some_and(|blocks| blocks.iter().any(|b| b["type"] == "tool_use"))
}

fn wait_with_timeout(child: &mut std::process::Child, started: Instant) -> ExitStatus {
    loop {
        if let Some(status) = child.try_wait().expect("wait") {
            return status;
        }
        if started.elapsed() > TIMEOUT {
            child.kill().expect("kill");
            return child.wait().expect("wait after kill");
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// Argonauts Code writes these before the spawn, readable only by you.
fn write_spec_files(spec: &CommandSpec) {
    for file in &spec.files {
        if let Some(parent) = file.path.parent() {
            fs::create_dir_all(parent).expect("spec file dir");
        }
        fs::write(&file.path, &file.contents).expect("spec file");
        fs::set_permissions(&file.path, fs::Permissions::from_mode(0o600)).expect("chmod");
    }
}

/// A stdio MCP server with one read-only tool, standing in for `argonauts mcp session`.
const STATUS_SERVER_SCRIPT: &str = r#"import json, sys
STATUS = "Run r-01: task-001 is building. No messages for you."
def reply(msg_id, result):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg_id, "result": result}) + "\n")
    sys.stdout.flush()
for raw in sys.stdin:
    try:
        msg = json.loads(raw)
    except ValueError:
        continue
    method, msg_id = msg.get("method"), msg.get("id")
    if msg_id is None:
        continue
    if method == "initialize":
        version = msg.get("params", {}).get("protocolVersion", "2025-06-18")
        reply(msg_id, {"protocolVersion": version, "capabilities": {"tools": {}},
                       "serverInfo": {"name": "argonauts_status", "version": "0.0.1"}})
    elif method == "tools/list":
        reply(msg_id, {"tools": [{"name": "argonauts_status",
            "description": "The run's status for this session, read-only.",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": {"readOnlyHint": True}}]})
    elif method == "tools/call":
        reply(msg_id, {"content": [{"type": "text", "text": STATUS}]})
    elif method == "ping":
        reply(msg_id, {})
    else:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg_id,
            "error": {"code": -32601, "message": "unknown method"}}) + "\n")
        sys.stdout.flush()
"#;

fn status_server_config(live_root: &Path, scratch: &Path, server: StatusServer) -> Option<PathBuf> {
    let entry = match server {
        StatusServer::None => return None,
        StatusServer::Working => {
            let script = live_root.join("status_server.py");
            fs::write(&script, STATUS_SERVER_SCRIPT).expect("status server script");
            json!({"command": "python3", "args": [script.display().to_string()]})
        }
        StatusServer::Broken => json!({"type": "stdio"}),
    };
    let path = scratch.join("mcp.json");
    let config = json!({"mcpServers": {STATUS_SERVER: entry}});
    fs::write(&path, config.to_string()).expect("mcp config");
    Some(path)
}

/// The repository an earlier recording ran in, so a resume runs where the session began.
fn latest_repo(live_root: &Path, name: &str) -> PathBuf {
    let prefix = format!("{name}-");
    let mut repos: Vec<PathBuf> = fs::read_dir(live_root)
        .expect("live dir")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .map(|e| e.path())
        .collect();
    repos.sort();
    repos.pop().unwrap_or_else(|| panic!("record {name} first"))
}

fn recorded_session_id(name: &str) -> String {
    let path = fixture_dir().join(format!("{name}.meta.json"));
    let meta: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).expect("earlier meta")).expect("meta");
    meta["session_id"]
        .as_str()
        .expect("the earlier recording chose a session id")
        .to_owned()
}

/// A random version 4 UUID, without a dependency for one test.
fn new_uuid() -> String {
    let mut bytes = [0u8; 16];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .expect("random bytes");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// Makes no live calls, but is ignored like the recorders.
#[test]
#[ignore = "rewrites fixture files"]
fn scrub_existing_fixtures() {
    let Some(live_root) = live_root("scrub_existing_fixtures") else {
        return;
    };
    let scrub = Scrubber::new(&live_root);
    for file in fs::read_dir(fixture_dir()).expect("fixture dir") {
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
        "live calls must run outside the Argonauts Code repository"
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
                "user.name=Argonauts Code",
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
        // Left by recordings made before the real path was replaced ahead of its short form.
        replacements.push((
            format!("/private{PLACEHOLDER_ROOT}"),
            PLACEHOLDER_ROOT.to_owned(),
        ));
        replacements.extend(temp_folders());
        if let Some(home) = env::var_os("HOME") {
            replacements.push((home.to_string_lossy().into_owned(), "~".to_owned()));
        }
        if let Ok(user) = env::var("USER")
            && user.len() >= 3
        {
            // Earlier scrubs replaced the user name inside flattened paths already.
            let flattened: Vec<(String, String)> = replacements
                .iter()
                .filter(|(from, _)| from.starts_with('-') && from.contains(&user))
                .map(|(from, to)| (from.replace(&user, "user"), to.clone()))
                .collect();
            replacements.extend(flattened);
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
/// Claude Code also names per-project folders after the working path with every character
/// other than a letter or digit turned into `-`, so that form is replaced too.
fn push_both_forms(replacements: &mut Vec<(String, String)>, path: &Path, placeholder: &str) {
    let real = path.display().to_string();
    let short = real.replacen("/private/", "/", 1);
    replacements.push((flatten(&real), flatten(placeholder)));
    replacements.push((flatten(&short), flatten(placeholder)));
    // The real path first, since the short one is part of it.
    replacements.push((real, placeholder.to_owned()));
    replacements.push((short, placeholder.to_owned()));
}

/// Claude Code keeps task output under `/tmp/claude-<uid>`, which names the recording
/// machine's user id, and tools may write under `$TMPDIR`.
fn temp_folders() -> Vec<(String, String)> {
    let mut replacements = Vec::new();
    if let Some(uid) = current_uid() {
        let tmp = Path::new("/tmp")
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from("/tmp"));
        push_both_forms(
            &mut replacements,
            &tmp.join(format!("claude-{uid}")),
            "/tmp/claude-uid",
        );
    }
    if let Some(temp) = env::var_os("TMPDIR").map(PathBuf::from) {
        let temp = temp.canonicalize().unwrap_or(temp);
        let real = temp.display().to_string().trim_end_matches('/').to_owned();
        let short = real.replacen("/private/", "/", 1);
        // Only the plain path, since a short folder flattened could match ordinary words.
        if !real.is_empty() && short != "/tmp" {
            replacements.push((real, "/tmp".to_owned()));
            replacements.push((short, "/tmp".to_owned()));
        }
    }
    // Either folder can hold the other, so the longer goes first.
    replacements.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));
    replacements
}

fn current_uid() -> Option<u32> {
    let out = Command::new("id").arg("-u").output().ok()?;
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

fn flatten(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
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
    // The recording machine's own MCP servers and their tools say what it has installed; only
    // Argonauts Code's own server, which a recording may set up, stays.
    let own = |name: &str| name.starts_with("argonauts");
    if let Some(servers) = init.get_mut("mcp_servers").and_then(|v| v.as_array_mut()) {
        servers.retain(|server| server["name"].as_str().is_some_and(own));
    }
    if let Some(tools) = init.get_mut("tools").and_then(|v| v.as_array_mut()) {
        tools.retain(|tool| {
            let name = tool.as_str().unwrap_or_default();
            match name.strip_prefix("mcp__") {
                Some(rest) => own(rest),
                None => true,
            }
        });
    }
    true
}
