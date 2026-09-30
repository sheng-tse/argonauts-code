//! Records live agy output as fixtures. Each ignored test makes one live call, but only when
//! `ARGONAUTS_RECORD` names it and `ARGONAUTS_LIVE_DIR` points at a scratch directory.
//!
//! ```text
//! ARGONAUTS_LIVE_DIR=/tmp/live/antigravity-cli ARGONAUTS_RECORD=record_readonly_ok \
//!   cargo test -p argonauts-adapter-antigravity-cli --test record -- --ignored record_
//! ```

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use argonauts_adapter_antigravity_cli::AntigravityCli;
use argonauts_core::{Adapter, CommandSpec, SessionRequest, SessionRole, report_json_schema};
use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(600);

#[test]
#[ignore = "live call; see the module docs"]
fn record_readonly_ok() {
    record(
        "record_readonly_ok",
        "readonly_ok",
        SessionRole::ReadOnly,
        "Reply with exactly the word: ok",
        None,
    );
}

#[test]
#[ignore = "live call; see the module docs"]
fn record_build_hello() {
    record(
        "record_build_hello",
        "build_hello_schema",
        SessionRole::Build,
        "Create a file named hello.txt containing the word hi. Then reply with: done",
        Some(report_json_schema()),
    );
}

#[test]
#[ignore = "live call; see the module docs"]
fn record_build_shell() {
    record(
        "record_build_shell",
        "build_shell",
        SessionRole::Build,
        "Run `ls` and reply with the number of files",
        None,
    );
}

fn record(test: &str, name: &str, role: SessionRole, prompt: &str, schema: Option<Value>) {
    let Some(live_root) = armed(test) else {
        eprintln!("skipping {test}: set ARGONAUTS_RECORD={test} and ARGONAUTS_LIVE_DIR");
        return;
    };
    let adapter = AntigravityCli;
    let version = cli_version(&adapter);
    let repo = live_root.join(name);
    make_throwaway_repo(&repo);
    let repo = repo.canonicalize().unwrap();

    let (model, effort) = adapter.cheapest_model();
    let log_file = live_root.join("logs").join(format!("{name}.agy.log"));
    let req = SessionRequest {
        workdir: repo.clone(),
        prompt: prompt.to_owned(),
        model: Some(model.to_owned()),
        effort: effort.map(str::to_owned),
        role,
        json_schema: schema,
        extra_args: vec!["--log-file".into(), log_file.display().to_string()],
        ..SessionRequest::default()
    };
    let spec = adapter.command(&req).unwrap();

    let started = Instant::now();
    let (exit_code, stdout, stderr) = run(&spec);
    let duration_ms = started.elapsed().as_millis() as u64;

    // The raw output stays in the live directory. Only scrubbed copies become fixtures.
    write_output(&live_root, name, &stdout, &stderr);
    let scrub = Scrubber::new(&repo, &live_root);
    let out_dir = fixtures_dir().join(&version);
    fs::create_dir_all(&out_dir).unwrap();
    write_output(&out_dir, name, &scrub.apply(&stdout), &scrub.apply(&stderr));

    let args: Vec<String> = spec.args.iter().map(|a| scrub.apply(a)).collect();
    let meta = json!({
        "adapter": adapter.id(),
        "cli_version": version,
        "name": name,
        "role": role,
        "prompt": prompt,
        "program": spec.program,
        "args": args,
        "env_set": spec.env_set,
        "env_removed": spec.env_remove,
        "date": now_rfc3339(),
        "exit_code": exit_code,
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
    eprintln!("{name}: exit {exit_code:?} in {duration_ms} ms");
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

fn run(spec: &CommandSpec) -> (Option<i32>, String, String) {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in &spec.env_set {
        command.env(key, value);
    }
    for key in &spec.env_remove {
        command.env_remove(key);
    }
    let mut child = command.spawn().unwrap();
    let stdout = drain(child.stdout.take().unwrap());
    let stderr = drain(child.stderr.take().unwrap());
    let status = wait(&mut child);
    (status, stdout.join().unwrap(), stderr.join().unwrap())
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
