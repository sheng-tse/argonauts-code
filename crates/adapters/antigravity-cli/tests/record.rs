//! Records live agy output as fixtures. Each ignored test makes one live call, but only when
//! `ARGONAUTS_RECORD` names it and `ARGONAUTS_LIVE_DIR` points at a scratch directory.
//!
//! ```text
//! ARGONAUTS_LIVE_DIR=/tmp/live/antigravity-cli ARGONAUTS_RECORD=record_stdin_steer \
//!   cargo test -p argonauts-adapter-antigravity-cli --test record -- --ignored record_
//! ```

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use argonauts_adapter_antigravity_cli::AntigravityCli;
use argonauts_core::settings::sessions::NativeFanout;
use argonauts_core::{Adapter, CommandSpec, SessionRequest, SessionRole, report_json_schema};
use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(600);

#[test]
#[ignore = "live call; see the module docs"]
fn record_readonly_ok() {
    record(Recording::new(
        "record_readonly_ok",
        "readonly_ok",
        SessionRole::ReadOnly,
        "Reply with exactly the word: ok",
    ));
}

#[test]
#[ignore = "live call; see the module docs"]
fn record_build_hello() {
    record(Recording {
        schema: Some(report_json_schema()),
        ..Recording::new(
            "record_build_hello",
            "build_hello_schema",
            SessionRole::Build,
            "Create a file named hello.txt containing the word hi. Then reply with: done",
        )
    });
}

#[test]
#[ignore = "live call; see the module docs"]
fn record_build_shell() {
    record(Recording::new(
        "record_build_shell",
        "build_shell",
        SessionRole::Build,
        "Run `ls` and reply with the number of files",
    ));
}

/// The prompt and a second message mid-run, both over stream-json stdin, in a read-only
/// session with the restricted agent and a per-run project.
#[test]
#[ignore = "live call; see the module docs"]
fn record_stdin_steer() {
    record(Recording {
        follow_up: Some("Also tell me how many lines notes.txt has."),
        ..Recording::new(
            "record_stdin_steer",
            "stdin_steer",
            SessionRole::ReadOnly,
            "Read README.md and reply with its first line.",
        )
    });
}

#[test]
#[ignore = "live call; see the module docs"]
fn record_build_subagent() {
    record(Recording {
        native_fanout: Some(NativeFanout::Limit { max: 2 }),
        ..Recording::new(
            "record_build_subagent",
            "build_subagent",
            SessionRole::Build,
            "Use the invoke_subagent tool to start one research subagent that reads notes.txt \
             and reports its first line. Then reply with that line.",
        )
    });
}

/// A read-only session asked to write, to check that the restricted agent has no write tool.
#[test]
#[ignore = "live call; see the module docs"]
fn record_readonly_write_refused() {
    record(Recording::new(
        "record_readonly_write_refused",
        "readonly_write_refused",
        SessionRole::ReadOnly,
        "Create a file named probe.txt containing the word hi. If you have no tool that can \
         write files, reply with exactly: no write tool",
    ));
}

struct Recording {
    test: &'static str,
    name: &'static str,
    role: SessionRole,
    prompt: &'static str,
    schema: Option<Value>,
    /// Sent on stdin once the first turn's first step arrives, which keeps stdin open.
    follow_up: Option<&'static str>,
    native_fanout: Option<NativeFanout>,
}

impl Recording {
    fn new(
        test: &'static str,
        name: &'static str,
        role: SessionRole,
        prompt: &'static str,
    ) -> Self {
        Self {
            test,
            name,
            role,
            prompt,
            schema: None,
            follow_up: None,
            native_fanout: None,
        }
    }
}

fn record(recording: Recording) {
    let Recording {
        test,
        name,
        role,
        prompt,
        ..
    } = recording;
    let Some(live_root) = armed(test) else {
        eprintln!("skipping {test}: set ARGONAUTS_RECORD={test} and ARGONAUTS_LIVE_DIR");
        return;
    };
    let adapter = AntigravityCli;
    let version = cli_version(&adapter);
    let repo = live_root.join(name);
    make_throwaway_repo(&repo);
    let repo = repo.canonicalize().unwrap();
    let scratch = live_root.join("scratch").join(name);
    fs::create_dir_all(&scratch).unwrap();
    let scratch = scratch.canonicalize().unwrap();

    let (model, effort) = adapter.cheapest_model();
    let log_file = live_root.join("logs").join(format!("{name}.agy.log"));
    let req = SessionRequest {
        workdir: repo.clone(),
        prompt: prompt.to_owned(),
        model: Some(model.to_owned()),
        effort: effort.map(str::to_owned),
        role,
        json_schema: recording.schema.clone(),
        keep_input_open: recording.follow_up.is_some(),
        project: Some(format!("argonauts-live-{name}")),
        native_fanout: recording.native_fanout,
        scratch_dir: Some(scratch.clone()),
        extra_args: vec!["--log-file".into(), log_file.display().to_string()],
        ..SessionRequest::default()
    };
    let spec = adapter.command(&req).unwrap();
    for file in &spec.files {
        fs::create_dir_all(file.path.parent().unwrap()).unwrap();
        fs::write(&file.path, &file.contents).unwrap();
    }

    let follow_up = recording.follow_up.and_then(|m| adapter.input_line(m));
    let started = Instant::now();
    let run = run(&spec, follow_up.as_deref());
    let duration_ms = started.elapsed().as_millis() as u64;

    // The raw output stays in the live directory. Only scrubbed copies become fixtures.
    write_output(&live_root, name, &run.stdout, &run.stderr);
    let scrub = Scrubber::new(&repo, &live_root);
    let out_dir = fixtures_dir().join(&version);
    fs::create_dir_all(&out_dir).unwrap();
    write_output(
        &out_dir,
        name,
        &scrub.apply(&run.stdout),
        &scrub.apply(&run.stderr),
    );

    let args: Vec<String> = spec.args.iter().map(|a| scrub.apply(a)).collect();
    let files: Vec<Value> = spec
        .files
        .iter()
        .map(
            |f| json!({"path": scrub.apply(&f.path.display().to_string()), "contents": f.contents}),
        )
        .collect();
    let stdin: Vec<Value> = run
        .stdin
        .iter()
        .map(|(after_ms, line)| json!({"after_ms": after_ms, "line": scrub.apply(line.trim_end())}))
        .collect();
    let meta = json!({
        "adapter": adapter.id(),
        "cli_version": version,
        "name": name,
        "role": role,
        "prompt": prompt,
        "program": spec.program,
        "args": args,
        "stdin": stdin,
        "keep_stdin_open": spec.keep_stdin_open,
        "files": files,
        "env_set": spec.env_set,
        "env_removed": spec.env_remove,
        "date": now_rfc3339(),
        "exit_code": run.exit_code,
        "duration_ms": duration_ms,
        "files_after": list_files(&repo),
        "sanitized": "absolute paths replaced with <workdir> and <live>, the home directory with ~, and email addresses with <email>",
    });
    fs::write(
        out_dir.join(format!("{name}.meta.json")),
        serde_json::to_string_pretty(&meta).unwrap() + "\n",
    )
    .unwrap();

    report_log_lines(&log_file);
    eprintln!("{name}: exit {:?} in {duration_ms} ms", run.exit_code);
}

fn write_output(dir: &Path, name: &str, stdout: &str, stderr: &str) {
    fs::write(dir.join(format!("{name}.stdout.jsonl")), stdout).unwrap();
    fs::write(dir.join(format!("{name}.stderr.txt")), stderr).unwrap();
}

fn armed(test: &str) -> Option<PathBuf> {
    let wanted = std::env::var("ARGONAUTS_RECORD").ok()?;
    if wanted != test {
        return None;
    }
    let root = PathBuf::from(std::env::var("ARGONAUTS_LIVE_DIR").ok()?);
    assert!(root.is_absolute(), "ARGONAUTS_LIVE_DIR must be absolute");
    fs::create_dir_all(&root).unwrap();
    Some(root)
}

fn cli_version(adapter: &AntigravityCli) -> String {
    let output = Command::new(adapter.binary())
        .args(adapter.version_args())
        .output()
        .unwrap();
    adapter
        .parse_version(&String::from_utf8_lossy(&output.stdout))
        .expect("agy --version printed no version")
}

fn make_throwaway_repo(repo: &Path) {
    assert!(
        !repo.exists(),
        "{} already exists; remove it to record again",
        repo.display()
    );
    fs::create_dir_all(repo).unwrap();
    fs::write(
        repo.join("README.md"),
        "Throwaway repo for recording agy output.\n",
    )
    .unwrap();
    fs::write(repo.join("notes.txt"), "Nothing to see here.\n").unwrap();
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
            ])
            .args([
                "-c",
                "user.name=argonauts",
                "-c",
                "user.email=argonauts@example.invalid",
            ])
            .args(args)
            .current_dir(repo)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "init"]);
}

struct Run {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    /// Each stdin line with the milliseconds since spawn when it was written.
    stdin: Vec<(u64, String)>,
}

/// Writes the spec's stdin, then the follow-up once the first step arrives. With stdin kept
/// open, it closes stdin after one `result` per message sent, which ends the session.
fn run(spec: &CommandSpec, follow_up: Option<&str>) -> Run {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in &spec.env_set {
        command.env(key, value);
    }
    for key in &spec.env_remove {
        command.env_remove(key);
    }
    let started = Instant::now();
    let mut child = command.spawn().unwrap();
    let mut stdin_log = Vec::new();
    let mut stdin = child.stdin.take();
    if let (Some(pipe), Some(first)) = (stdin.as_mut(), spec.stdin.as_deref()) {
        send(pipe, first, started, &mut stdin_log);
    }
    if !spec.keep_stdin_open {
        stdin = None;
    }

    let (lines_tx, lines_rx) = mpsc::channel::<String>();
    let stdout_pipe = child.stdout.take().unwrap();
    let stdout = thread::spawn(move || {
        let mut all = String::new();
        for line in BufReader::new(stdout_pipe).lines() {
            let Ok(line) = line else { break };
            all.push_str(&line);
            all.push('\n');
            let _ = lines_tx.send(line);
        }
        all
    });
    let stderr = drain(child.stderr.take().unwrap());

    let mut pending = follow_up;
    let mut results = 0;
    let expected_results = stdin_log.len() + usize::from(pending.is_some());
    while stdin.is_some() && started.elapsed() < TIMEOUT {
        let Ok(line) = lines_rx.recv_timeout(Duration::from_millis(200)) else {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            continue;
        };
        if line.contains(r#""event":"step_update""#)
            && let (Some(message), Some(pipe)) = (pending.take(), stdin.as_mut())
        {
            send(pipe, message, started, &mut stdin_log);
        }
        if line.contains(r#""event":"result""#) {
            results += 1;
            if results >= expected_results && pending.is_none() {
                stdin = None;
            }
        }
    }
    drop(stdin);
    let exit_code = wait(&mut child);
    Run {
        exit_code,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
        stdin: stdin_log,
    }
}

fn send(pipe: &mut ChildStdin, text: &str, started: Instant, log: &mut Vec<(u64, String)>) {
    pipe.write_all(text.as_bytes()).unwrap();
    pipe.flush().unwrap();
    log.push((started.elapsed().as_millis() as u64, text.to_owned()));
}

fn drain(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

fn wait(child: &mut Child) -> Option<i32> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status.code();
        }
        if started.elapsed() > TIMEOUT {
            child.kill().unwrap();
            return child.wait().unwrap().code();
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn list_files(repo: &Path) -> Vec<String> {
    let mut files: Vec<String> = fs::read_dir(repo)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name != ".git")
        .collect();
    files.sort();
    files
}

/// Prints only the log lines that say how agy treated our flags and environment.
fn report_log_lines(log_file: &Path) {
    let Ok(log) = fs::read_to_string(log_file) else {
        eprintln!("no agy log at {}", log_file.display());
        return;
    };
    let keywords = [
        "Auto-update",
        "auto-update",
        "Print mode",
        "warning",
        "sandbox",
        "--mode",
    ];
    for line in log
        .lines()
        .filter(|l| keywords.iter().any(|k| l.contains(k)))
    {
        eprintln!("log: {}", line.chars().take(240).collect::<String>());
    }
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}

/// Replaces machine-specific paths and email addresses in recorded text.
struct Scrubber {
    replacements: Vec<(String, String)>,
}

impl Scrubber {
    fn new(repo: &Path, live_root: &Path) -> Self {
        let mut replacements = Vec::new();
        for (path, placeholder) in [(repo, "<workdir>"), (live_root, "<live>")] {
            let shown = path.display().to_string();
            if let Some(short) = shown.strip_prefix("/private") {
                replacements.push((shown.clone(), placeholder.to_owned()));
                replacements.push((short.to_owned(), placeholder.to_owned()));
            } else {
                replacements.push((shown, placeholder.to_owned()));
            }
        }
        if let Some(home) = std::env::var_os("HOME") {
            replacements.push((home.to_string_lossy().into_owned(), "~".to_owned()));
        }
        Self { replacements }
    }

    fn apply(&self, text: &str) -> String {
        let replaced = self
            .replacements
            .iter()
            .fold(text.to_owned(), |acc, (from, to)| acc.replace(from, to));
        scrub_emails(&replaced)
    }
}

fn scrub_emails(text: &str) -> String {
    let is_local = |c: char| c.is_ascii_alphanumeric() || "._%+-".contains(c);
    let is_domain = |c: char| c.is_ascii_alphanumeric() || ".-".contains(c);
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '@' {
            let local_start = out
                .char_indices()
                .rev()
                .take_while(|(_, c)| is_local(*c))
                .last()
                .map(|(at, _)| at);
            let domain_len = chars[i + 1..].iter().take_while(|c| is_domain(**c)).count();
            let domain: String = chars[i + 1..i + 1 + domain_len].iter().collect();
            if let (Some(start), true) = (local_start, domain.contains('.')) {
                out.truncate(start);
                out.push_str("<email>");
                i += 1 + domain_len;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

#[test]
fn scrubber_hides_paths_and_emails() {
    let scrub = Scrubber {
        replacements: vec![("/private/tmp/live/x".into(), "<workdir>".into())],
    };
    assert_eq!(
        scrub.apply(r#"{"cwd":"/private/tmp/live/x","who":"a.b@example.com says hi"}"#),
        r#"{"cwd":"<workdir>","who":"<email> says hi"}"#
    );
    assert_eq!(scrub_emails("plugin@marketplace"), "plugin@marketplace");
}
