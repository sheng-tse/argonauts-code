//! Records Codex fixtures from live runs. Each test makes one real model call and spends plan
//! usage.
//!
//! A test runs only when `ARGONAUTS_RECORD` names it and `ARGONAUTS_LIVE_DIR` is a scratch folder
//! for throwaway repos. Run them one at a time and in order, since resume continues the build
//! session. `ARGONAUTS_CODEX_MODEL` may name the fallback model when the cheapest is unavailable.
//!
//! ```text
//! ARGONAUTS_RECORD=record_1_read_only_ok ARGONAUTS_LIVE_DIR=<scratch>/live/codex \
//!   cargo test -p argonauts-adapter-codex --test it -- --ignored record::record_1
//! ```

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use argonauts_adapter_codex::{CHEAPEST_EFFORT, CodexAdapter, FALLBACK_CHEAPEST_MODEL};
use argonauts_core::settings::sessions::{Compaction, FanoutSwitch, NativeFanout};
use argonauts_core::{
    Adapter, AuthMode, CommandSpec, PROVIDER_KEY_ENV_VARS, SessionRequest, SessionRole,
    report_json_schema,
};
use serde_json::{Value, json};

#[path = "../../../recorded_shape.rs"]
mod recorded_shape;

const TIMEOUT: Duration = Duration::from_secs(600);

struct Recording {
    test: &'static str,
    name: &'static str,
    repo: &'static str,
    role: SessionRole,
    prompt: &'static str,
    schema: bool,
    resume_from: Option<&'static str>,
    compaction: Option<Compaction>,
    native_fanout: Option<NativeFanout>,
    user_hooks_off: bool,
    /// Send SIGINT once a stdout line contains all of these.
    interrupt_on: &'static [&'static str],
}

impl Recording {
    const PLAIN: Self = Self {
        test: "",
        name: "",
        repo: "",
        role: SessionRole::ReadOnly,
        prompt: "",
        schema: false,
        resume_from: None,
        compaction: None,
        native_fanout: None,
        user_hooks_off: false,
        interrupt_on: &[],
    };
}

#[test]
#[ignore = "live Codex call; see the module docs"]
fn record_1_read_only_ok() {
    record(Recording {
        test: "record_1_read_only_ok",
        name: "read_only_ok",
        repo: "read-only",
        role: SessionRole::ReadOnly,
        prompt: "Reply with exactly the word: ok",
        ..Recording::PLAIN
    });
}

#[test]
#[ignore = "live Codex call; see the module docs"]
fn record_2_build_hello() {
    record(Recording {
        test: "record_2_build_hello",
        name: "build_hello",
        repo: "build",
        role: SessionRole::Build,
        prompt: "Create a file named hello.txt containing the word hi. Then reply with: done",
        schema: true,
        ..Recording::PLAIN
    });
}

#[test]
#[ignore = "live Codex call; see the module docs"]
fn record_3_resume_ls() {
    record(Recording {
        test: "record_3_resume_ls",
        name: "resume_ls",
        repo: "build",
        role: SessionRole::ReadOnly,
        prompt: "Run `ls` and reply with the number of files",
        resume_from: Some("build_hello"),
        ..Recording::PLAIN
    });
}

/// Steering: Argonauts Code interrupts a running turn, then resumes it with a message.
#[test]
#[ignore = "live Codex call; see the module docs"]
fn record_4_interrupt_sleep() {
    record(Recording {
        test: "record_4_interrupt_sleep",
        name: "interrupt_sleep",
        repo: "steer",
        role: SessionRole::Build,
        prompt: "Run the shell command `sleep 20`, then reply with: slept",
        compaction: Some(Compaction::Auto),
        native_fanout: Some(NativeFanout::Switch(FanoutSwitch::Off)),
        user_hooks_off: true,
        interrupt_on: &["\"item.started\"", "\"command_execution\""],
        ..Recording::PLAIN
    });
}

#[test]
#[ignore = "live Codex call; see the module docs"]
fn record_5_resume_with_message() {
    record(Recording {
        test: "record_5_resume_with_message",
        name: "resume_with_message",
        repo: "steer",
        role: SessionRole::Build,
        prompt: "Stop waiting for that command. Do not run anything. Reply with exactly: resumed",
        resume_from: Some("interrupt_sleep"),
        compaction: Some(Compaction::Auto),
        native_fanout: Some(NativeFanout::Switch(FanoutSwitch::Off)),
        user_hooks_off: true,
        ..Recording::PLAIN
    });
}

#[test]
#[ignore = "live Codex call; see the module docs"]
fn record_6_subagent_spawn() {
    record(Recording {
        test: "record_6_subagent_spawn",
        name: "subagent_spawn",
        repo: "fanout",
        role: SessionRole::ReadOnly,
        prompt: "Spawn exactly one explorer subagent and ask it to count the files in this \
                 repository and reply with just the number. Wait for it to finish, then close \
                 it, then reply with its number.",
        native_fanout: Some(NativeFanout::Limit { max: 1 }),
        ..Recording::PLAIN
    });
}

/// Reads rate limits over `codex app-server`. It starts no turn and spends nothing.
#[test]
#[ignore = "talks to the Codex app server; see the module docs"]
fn record_7_rate_limits_probe() {
    const TEST: &str = "record_7_rate_limits_probe";
    let Some(live_dir) = live_dir(TEST) else {
        eprintln!("skipped: set ARGONAUTS_RECORD={TEST} and ARGONAUTS_LIVE_DIR to record");
        return;
    };
    let adapter = CodexAdapter::new();
    let version = cli_version(&adapter);
    let spec = adapter
        .plan_usage_probe(&live_dir, None)
        .expect("Codex has a plan usage probe");
    let requests = spec
        .stdin
        .clone()
        .expect("the probe writes its requests to stdin");

    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for name in &spec.env_remove {
        command.env_remove(name);
    }
    let mut child = command.spawn().expect("spawn codex app-server");
    // Kept open until the answer arrives, since the app server exits when stdin closes.
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(requests.as_bytes()).unwrap();
    let (lines, received) = std::sync::mpsc::channel();
    let stdout = child.stdout.take().unwrap();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut text = String::new();
    while adapter.parse_plan_usage_probe(&text).is_none() {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = received
            .recv_timeout(left)
            .expect("the app server answers in time");
        text.push_str(&line);
        text.push('\n');
    }
    child.kill().unwrap();
    child.wait().unwrap();
    drop(stdin);

    let redactor = Redactor::new(&[(&live_dir, "<live>")]);
    let answer: String = text
        .lines()
        .map(|line| format!("{}\n", redactor.apply(&redact_ids(line))))
        .collect();
    let meta = json!({
        "adapter": adapter.id(),
        "cli_version": version,
        "also_matches": [],
        "recorded_at": now_rfc3339(),
        "program": spec.program,
        "args": spec.args,
        "stdin": requests,
        "spends": "nothing",
    });
    let fixtures = fixtures_dir();
    let base = fixtures.join("rate_limits_probe");
    recorded_shape::report(&base.with_extension("app-server.jsonl"), &answer);
    std::fs::write(base.with_extension("app-server.jsonl"), &answer).unwrap();
    let mut meta_text = serde_json::to_string_pretty(&meta).unwrap();
    meta_text.push('\n');
    std::fs::write(base.with_extension("meta.json"), meta_text).unwrap();
    eprintln!("{:?}", adapter.parse_plan_usage_probe(&answer));
}

/// Account and installation ids identify you, so they never reach a fixture.
fn redact_ids(line: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(line) else {
        return line.to_owned();
    };
    redact_keys(
        &mut value,
        &["accountId", "installationId", "environmentId"],
    );
    value.to_string()
}

fn redact_keys(value: &mut Value, keys: &[&str]) {
    match value {
        Value::Object(fields) => {
            for (key, field) in fields.iter_mut() {
                if keys.contains(&key.as_str()) && !field.is_null() {
                    *field = json!("<redacted>");
                } else {
                    redact_keys(field, keys);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| redact_keys(item, keys)),
        _ => {}
    }
}

fn record(rec: Recording) {
    let Some(live_dir) = live_dir(rec.test) else {
        eprintln!(
            "skipped: set ARGONAUTS_RECORD={} and ARGONAUTS_LIVE_DIR to record",
            rec.test
        );
        return;
    };
    let adapter = CodexAdapter::new();
    let version = cli_version(&adapter);
    let fixtures = fixtures_dir();
    let repo = live_dir.join(rec.repo);
    let scratch = live_dir.join(format!("{}-scratch", rec.name));

    let resume_session = rec
        .resume_from
        .map(|from| thread_id(&fixtures.join(format!("{from}.stdout.jsonl"))));
    if resume_session.is_none() {
        init_repo(&repo);
    }
    let model = recording_model(&adapter);

    let req = SessionRequest {
        workdir: repo.clone(),
        prompt: rec.prompt.into(),
        model: Some(model.clone()),
        effort: Some(CHEAPEST_EFFORT.into()),
        role: rec.role,
        auth: AuthMode::Subscription,
        json_schema: rec.schema.then(report_json_schema),
        resume_session,
        scratch_dir: Some(scratch.clone()),
        compaction: rec.compaction,
        native_fanout: rec.native_fanout,
        user_hooks_off: rec.user_hooks_off,
        ..SessionRequest::default()
    };
    let spec = adapter.command(&req).expect("build the command");
    write_spec_files(&spec);

    let started = Instant::now();
    let (status, stdout, stderr) = run(&spec, rec.interrupt_on);
    let duration = started.elapsed();

    let redactor = Redactor::new(&[
        (&repo, "<workdir>"),
        (&scratch, "<scratch>"),
        (&live_dir, "<live>"),
    ]);
    let stdout = redactor.apply(&stdout);
    let stderr = redactor.apply(&stderr);
    let args: Vec<String> = spec.args.iter().map(|a| redactor.apply(a)).collect();
    let meta = json!({
        "adapter": adapter.id(),
        "cli_version": version,
        "also_matches": [],
        "recorded_at": now_rfc3339(),
        "model": model,
        "effort": CHEAPEST_EFFORT,
        "role": rec.role,
        "prompt": rec.prompt,
        "output_schema": rec.schema,
        "resumed_from": rec.resume_from,
        "compaction": rec.compaction,
        "native_fanout": rec.native_fanout,
        "user_hooks_off": rec.user_hooks_off,
        "interrupted": !rec.interrupt_on.is_empty(),
        "program": spec.program,
        "args": args,
        "stdin": "<prompt>",
        "exit_code": status.code(),
        "duration_ms": u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
    });

    std::fs::create_dir_all(&fixtures).unwrap();
    let base = fixtures.join(rec.name);
    write_fixture(&base, &stdout, &stderr, &meta);

    print_events(&adapter, &stdout, &stderr, status.code());
    // An interrupted turn exits 1 with no closing event.
    assert_eq!(
        status.success(),
        rec.interrupt_on.is_empty(),
        "codex exited with {status}; see {}",
        base.display()
    );
}

fn recording_model(adapter: &CodexAdapter) -> String {
    let (cheapest, _) = adapter.cheapest_model();
    let model = std::env::var("ARGONAUTS_CODEX_MODEL").unwrap_or_else(|_| cheapest.to_owned());
    assert!(
        model == cheapest || model == FALLBACK_CHEAPEST_MODEL,
        "recordings use only the cheapest models, not {model}"
    );
    model
}

fn write_spec_files(spec: &CommandSpec) {
    for file in &spec.files {
        std::fs::create_dir_all(file.path.parent().expect("file has a parent")).unwrap();
        std::fs::write(&file.path, &file.contents).unwrap();
    }
}

fn write_fixture(base: &Path, stdout: &str, stderr: &str, meta: &Value) {
    recorded_shape::report(&base.with_extension("stdout.jsonl"), stdout);
    std::fs::write(base.with_extension("stdout.jsonl"), stdout).unwrap();
    std::fs::write(base.with_extension("stderr.txt"), stderr).unwrap();
    let mut meta_text = serde_json::to_string_pretty(meta).unwrap();
    meta_text.push('\n');
    std::fs::write(base.with_extension("meta.json"), meta_text).unwrap();
}

fn print_events(adapter: &CodexAdapter, stdout: &str, stderr: &str, exit_code: Option<i32>) {
    let mut parser = adapter.parser();
    for line in stdout.lines() {
        for event in parser.parse_stdout_line(line) {
            eprintln!("stdout -> {}", serde_json::to_string(&event).unwrap());
        }
    }
    for line in stderr.lines() {
        for event in parser.parse_stderr_line(line) {
            eprintln!("stderr -> {}", serde_json::to_string(&event).unwrap());
        }
    }
    for event in parser.finish(exit_code) {
        eprintln!("finish -> {}", serde_json::to_string(&event).unwrap());
    }
}

fn live_dir(test: &str) -> Option<PathBuf> {
    if std::env::var("ARGONAUTS_RECORD").ok().as_deref() != Some(test) {
        return None;
    }
    let dir = PathBuf::from(std::env::var_os("ARGONAUTS_LIVE_DIR")?);
    assert!(dir.is_absolute(), "ARGONAUTS_LIVE_DIR must be absolute");
    std::fs::create_dir_all(&dir).unwrap();
    Some(dir)
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn cli_version(adapter: &CodexAdapter) -> String {
    let output = Command::new(adapter.binary())
        .args(adapter.version_args())
        .stdin(Stdio::null())
        .output()
        .expect("run codex --version");
    adapter
        .parse_version(&String::from_utf8_lossy(&output.stdout))
        .expect("parse codex --version")
}

fn thread_id(stdout_fixture: &Path) -> String {
    let text = std::fs::read_to_string(stdout_fixture).expect("read the fixture to resume");
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|v| v["type"] == "thread.started")
        .and_then(|v| v["thread_id"].as_str().map(str::to_owned))
        .expect("the fixture has a thread id")
}

/// Only ever used under the live scratch folder.
fn init_repo(repo: &Path) {
    if repo.exists() {
        std::fs::remove_dir_all(repo).unwrap();
    }
    std::fs::create_dir_all(repo).unwrap();
    std::fs::write(repo.join("README.md"), "# Live test repo\n").unwrap();
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["add", "README.md"]);
    git(repo, &["commit", "-q", "-m", "init"]);
}

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=Argonauts Code",
            "-c",
            "user.email=argonauts@example.invalid",
        ])
        .args(args)
        .current_dir(repo)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn run(spec: &CommandSpec, interrupt_on: &'static [&'static str]) -> (ExitStatus, String, String) {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(if spec.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in &spec.env_remove {
        command.env_remove(name);
    }
    command.envs(spec.env_set.iter().map(|(k, v)| (k, v)));
    let mut child = command.spawn().expect("spawn codex");
    if let Some(input) = &spec.stdin {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(input.as_bytes()).unwrap();
    }
    let stdout = watch(child.stdout.take().unwrap(), child.id(), interrupt_on);
    let stderr = drain(child.stderr.take().unwrap());
    let status = wait(&mut child);
    (status, stdout.join().unwrap(), stderr.join().unwrap())
}

/// Reads stdout whole, and sends SIGINT to `pid` the first time a line holds every marker.
fn watch(
    pipe: impl Read + Send + 'static,
    pid: u32,
    markers: &'static [&'static str],
) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut text = String::new();
        let mut interrupted = markers.is_empty();
        for line in BufReader::new(pipe).lines() {
            let line = line.unwrap();
            if !interrupted && markers.iter().all(|m| line.contains(m)) {
                interrupted = true;
                let status = Command::new("kill")
                    .args(["-INT", &pid.to_string()])
                    .status()
                    .expect("run kill");
                assert!(status.success(), "kill -INT {pid} failed");
            }
            text.push_str(&line);
            text.push('\n');
        }
        text
    })
}

fn drain(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

fn wait(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            return child.wait().unwrap();
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .unwrap()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}

/// Replaces local paths with placeholders and removes anything that looks like a key.
struct Redactor {
    replacements: Vec<(String, String)>,
}

impl Redactor {
    fn new(paths: &[(&Path, &str)]) -> Self {
        let mut replacements = Vec::new();
        for (path, placeholder) in paths {
            let mut forms = vec![path.to_path_buf()];
            if let Ok(canonical) = path.canonicalize() {
                forms.push(canonical);
            }
            if let Ok(rest) = path.strip_prefix("/private") {
                forms.push(Path::new("/").join(rest));
            }
            for form in forms {
                replacements.push((form.display().to_string(), (*placeholder).to_owned()));
            }
        }
        if let Some(home) = std::env::var_os("HOME").filter(|h| h.len() > 1) {
            replacements.push((PathBuf::from(home).display().to_string(), "~".to_owned()));
        }
        for name in PROVIDER_KEY_ENV_VARS {
            if let Some(value) = std::env::var(name).ok().filter(|v| v.len() >= 8) {
                replacements.push((value, "<redacted>".to_owned()));
            }
        }
        // Longest first, so a path is replaced before any of its prefixes.
        replacements.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));
        replacements.dedup();
        Self { replacements }
    }

    fn apply(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for (from, to) in &self.replacements {
            out = out.replace(from.as_str(), to);
        }
        redact_key_like_tokens(&out)
    }
}

fn redact_key_like_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("sk-") {
        out.push_str(&rest[..at]);
        let token_len = rest[at + 3..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(rest.len() - at - 3);
        if token_len >= 20 {
            out.push_str("sk-<redacted>");
        } else {
            out.push_str(&rest[at..at + 3 + token_len]);
        }
        rest = &rest[at + 3 + token_len..];
    }
    out.push_str(rest);
    out
}

#[test]
fn redactor_removes_paths_and_keys() {
    let redactor = Redactor::new(&[(Path::new("/private/tmp/live/repo"), "<workdir>")]);
    let text = r#"{"path":"/private/tmp/live/repo/hello.txt","other":"/tmp/live/repo/x"}"#;
    assert_eq!(
        redactor.apply(text),
        r#"{"path":"<workdir>/hello.txt","other":"<workdir>/x"}"#
    );
    assert_eq!(
        redact_key_like_tokens("key sk-abcdefghijklmnopqrstuvwxyz0123 and sk-short, task-sk-"),
        "key sk-<redacted> and sk-short, task-sk-"
    );
}
