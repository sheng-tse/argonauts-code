//! Recorded agent replies to the conversation's own steps, one `Estimate` and one resumed
//! `ConversationAnswer` per agent, replayed through each adapter's parser and the conversation's
//! reading.
//!
//! `record_conversation_fixtures` makes the live calls again. It is ignored, and runs only with
//! `ARGONAUTS_RECORD=record_conversation_fixtures` and `ARGONAUTS_LIVE_DIR` naming an absolute
//! scratch folder outside this repository; `ARGONAUTS_AGENTS` picks adapters, such as `codex`.

use std::collections::BTreeMap;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use argonauts_core::runs::event::AgentEvent;
use argonauts_core::runs::events::session::EndReason;
use argonauts_core::settings::Settings;
use argonauts_core::workflows::steps::intake::{
    ConversationAnswer, ConversationAnswerInput, Estimate, IntakeInput,
};
use argonauts_core::workflows::steps::{
    StepError, StepOutcome, decode, last_json_object, output_schema,
};
use argonauts_core::{Adapter, CommandSpec, SessionRequest, SessionRole};
use argonauts_testkit::engine::runner::session_end;
use regex::Regex;
use serde_json::{Value, json};

use super::captain_or_crew::{Reading, read_reply};
use crate::agents::adapters::builtin_adapters;

const AGENTS: &[&str] = &["claude-code", "codex", "antigravity-cli"];
const REQUEST: &str = "How should we add rate limiting to the server in server.py?";
const FOLLOW_UP: &str = "In one sentence, what did I just ask you about?";
const TIMEOUT: Duration = Duration::from_secs(300);
/// Stands in for the scratch folder the calls ran in.
const LIVE: &str = "/tmp/argonauts-live/conversation";
/// Parts of Claude Code's init line that describe the recording machine, not the session.
const LOCAL_SETUP: &[&str] = &[
    "slash_commands",
    "terminal_slash_commands",
    "skills",
    "agents",
    "plugins",
    "mcp_servers",
    "memory_paths",
];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn adapter(id: &str) -> Arc<dyn Adapter> {
    builtin_adapters()
        .into_iter()
        .find(|adapter| adapter.id() == id)
        .unwrap_or_else(|| panic!("no adapter {id}"))
}

struct Recorded {
    events: Vec<AgentEvent>,
    meta: Value,
}

/// `None` when that agent wasn't recorded.
fn replay(agent_id: &str, name: &str) -> Option<Recorded> {
    let base = fixtures().join(agent_id).join(name);
    let read = |ext: &str| fs::read_to_string(base.with_extension(ext)).ok();
    let stdout = read("stdout.jsonl")?;
    let stderr = read("stderr.txt").unwrap_or_default();
    let meta: Value = serde_json::from_str(&read("meta.json")?).expect("meta json");
    Some(Recorded {
        events: parse(agent_id, &stdout, &stderr, meta["exit_code"].as_i64()),
        meta,
    })
}

fn parse(agent_id: &str, stdout: &str, stderr: &str, exit_code: Option<i64>) -> Vec<AgentEvent> {
    let mut parser = adapter(agent_id).parser();
    let mut events = Vec::new();
    for line in stdout.lines() {
        events.extend(parser.parse_stdout_line(line));
    }
    for line in stderr.lines() {
        events.extend(parser.parse_stderr_line(line));
    }
    events.extend(parser.finish(exit_code.and_then(|code| i32::try_from(code).ok())));
    events
}

/// What the step runner would take as the output: the structured result, else the last JSON
/// object in the final text.
fn output(events: &[AgentEvent]) -> Option<StepOutcome> {
    let (text, structured) = events.iter().rev().find_map(|event| match event {
        AgentEvent::Result {
            text, structured, ..
        } => Some((text.clone(), structured.clone())),
        _ => None,
    })?;
    let value = structured
        .clone()
        .filter(Value::is_object)
        .or_else(|| text.as_deref().and_then(last_json_object))?;
    Some(StepOutcome {
        value,
        end: session_end(EndReason::Completed, text.as_deref(), structured),
        tries: 1,
    })
}

fn session_id(events: &[AgentEvent]) -> Option<String> {
    events.iter().find_map(|event| match event {
        AgentEvent::Init { session_id, .. } => session_id.clone(),
        _ => None,
    })
}

/// What each agent's recorded estimate turn did. `Silent` records an agent quirk, with why.
enum EstimateTurn {
    Answered,
    Silent(&'static str),
}

const RECORDED: &[(&str, EstimateTurn)] = &[
    ("claude-code", EstimateTurn::Answered),
    ("codex", EstimateTurn::Answered),
    (
        "antigravity-cli",
        EstimateTurn::Silent(
            "agy 1.2.13 in headless mode denies a shell command it wasn't allowed and then ends \
             the turn with no output",
        ),
    ),
];

#[test]
fn recorded_estimates_read_as_a_branch() {
    for (agent_id, expected) in RECORDED {
        let estimate =
            replay(agent_id, "estimate").unwrap_or_else(|| panic!("{agent_id}: no fixture"));
        assert!(
            session_id(&estimate.events).is_some(),
            "{agent_id}: no session id"
        );
        let reading = match output(&estimate.events) {
            Some(outcome) => read_reply(&Ok(outcome), &Settings::default()),
            None => read_reply(
                &Err(StepError::SchemaMismatch {
                    raw: None,
                    errors: vec!["the answer held no JSON object".to_owned()],
                }),
                &Settings::default(),
            ),
        };
        match (expected, reading) {
            (EstimateTurn::Answered, Ok(reading)) => {
                let markdown = match &reading {
                    Reading::Answer { markdown }
                    | Reading::Change { markdown, .. }
                    | Reading::Unclear { markdown, .. } => markdown,
                };
                assert!(!markdown.trim().is_empty(), "{agent_id}: {reading:?}");
            }
            // The conversation shows it as a failed turn rather than an empty answer.
            (EstimateTurn::Silent(why), Err(err)) => {
                assert_eq!(
                    super::turn::describe(&err),
                    "it ended without an answer",
                    "{why}"
                );
            }
            (_, reading) => panic!("{agent_id}: {reading:?}"),
        }
    }
}

#[test]
fn recorded_resumed_turns_remember_the_first() {
    for (agent_id, _) in RECORDED {
        let estimate =
            replay(agent_id, "estimate").unwrap_or_else(|| panic!("{agent_id}: no fixture"));
        let resumed =
            replay(agent_id, "resumed").unwrap_or_else(|| panic!("{agent_id}: no resumed"));
        let first = session_id(&estimate.events);
        assert_eq!(
            resumed.meta["resumed_from"].as_str(),
            first.as_deref(),
            "{agent_id}"
        );
        let args = resumed.meta["args"].to_string();
        assert!(
            first.is_some_and(|id| args.contains(&id)),
            "{agent_id}: the resume id isn't among {args}"
        );
        let outcome = output(&resumed.events)
            .unwrap_or_else(|| panic!("{agent_id}: no output in {:#?}", resumed.events));
        let answer = decode::<ConversationAnswer>(&outcome)
            .unwrap_or_else(|err| panic!("{agent_id}: {err}"));
        assert!(
            answer.answer_markdown.to_lowercase().contains("rate limit"),
            "{agent_id} doesn't remember the first turn: {}",
            answer.answer_markdown
        );
    }
}

#[test]
#[ignore = "live calls that spend agent plan usage"]
fn record_conversation_fixtures() {
    if env::var("ARGONAUTS_RECORD").ok().as_deref() != Some("record_conversation_fixtures") {
        eprintln!("set ARGONAUTS_RECORD=record_conversation_fixtures and ARGONAUTS_LIVE_DIR");
        return;
    }
    let live = PathBuf::from(env::var("ARGONAUTS_LIVE_DIR").expect("ARGONAUTS_LIVE_DIR"));
    assert!(live.is_absolute(), "ARGONAUTS_LIVE_DIR must be absolute");
    fs::create_dir_all(&live).expect("the live folder");
    let live = live.canonicalize().expect("the live folder");
    let ours = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repo");
    assert!(
        !live.starts_with(&ours),
        "live calls run outside the repository"
    );
    let wanted = env::var("ARGONAUTS_AGENTS").unwrap_or_else(|_| AGENTS.join(","));
    for agent_id in wanted.split(',').map(str::trim).filter(|v| !v.is_empty()) {
        record_agent(agent_id, &live);
    }
}

fn record_agent(agent_id: &str, live: &Path) {
    let adapter = adapter(agent_id);
    let repo = throwaway_repo(live, agent_id);
    let scratch = live.join(format!("{agent_id}-scratch"));
    fs::create_dir_all(&scratch).expect("scratch");
    let (model, effort) = adapter.cheapest_model();
    let request = |prompt: String, schema: Value, resume: Option<String>| SessionRequest {
        workdir: repo.clone(),
        prompt,
        model: Some(model.to_owned()),
        effort: effort.map(str::to_owned),
        role: SessionRole::ReadOnly,
        json_schema: Some(schema),
        resume_session: resume,
        scratch_dir: Some(scratch.clone()),
        ..SessionRequest::default()
    };
    let scrub = Scrubber::new(live);

    let estimate = request(
        crate::prompts::estimate(
            &IntakeInput {
                request: REQUEST.to_owned(),
                uncommitted_note: None,
            },
            true,
        ),
        output_schema::<Estimate>(),
        None,
    );
    let first = call(adapter.as_ref(), &estimate);
    save(agent_id, "estimate", &first, None, &scrub);
    let events = parse(agent_id, &first.stdout, &first.stderr, first.exit_code);
    let Some(id) = session_id(&events) else {
        panic!(
            "{agent_id} gave no session id to resume; see {}",
            live.display()
        );
    };

    let follow_up = request(
        crate::prompts::conversation_answer(&ConversationAnswerInput {
            question: FOLLOW_UP.to_owned(),
            run_status: None,
        }),
        output_schema::<ConversationAnswer>(),
        Some(id.clone()),
    );
    let second = call(adapter.as_ref(), &follow_up);
    save(agent_id, "resumed", &second, Some(&id), &scrub);
}

struct Call {
    spec: CommandSpec,
    stdout: String,
    stderr: String,
    exit_code: Option<i64>,
    duration_ms: u64,
}

fn call(adapter: &dyn Adapter, request: &SessionRequest) -> Call {
    let spec = adapter.command(request).expect("a command");
    for file in &spec.files {
        if let Some(dir) = file.path.parent() {
            fs::create_dir_all(dir).expect("the file's folder");
        }
        fs::write(&file.path, &file.contents).expect("a file the command reads");
    }
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
    let mut child = command.spawn().expect("the agent CLI starts");
    let mut stdin = child.stdin.take().expect("stdin");
    stdin
        .write_all(spec.stdin.as_deref().unwrap_or_default().as_bytes())
        .expect("the prompt");
    drop(stdin);
    let stdout = drain(child.stdout.take().expect("stdout"));
    let stderr = drain(child.stderr.take().expect("stderr"));
    let status = loop {
        if let Some(status) = child.try_wait().expect("waiting") {
            break status;
        }
        if started.elapsed() > TIMEOUT {
            let _ = child.kill();
            break child.wait().expect("the killed CLI");
        }
        thread::sleep(Duration::from_millis(100));
    };
    Call {
        spec,
        stdout: stdout.join().expect("stdout"),
        stderr: stderr.join().expect("stderr"),
        exit_code: status.code().map(i64::from),
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    }
}

fn drain(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut text = String::new();
        let _ = pipe.read_to_string(&mut text);
        text
    })
}

fn save(agent_id: &str, name: &str, call: &Call, resumed_from: Option<&str>, scrub: &Scrubber) {
    let dir = fixtures().join(agent_id);
    fs::create_dir_all(&dir).expect("the fixture folder");
    let base = dir.join(name);
    let stdout = scrub.lines(&call.stdout);
    fs::write(
        base.with_extension("stdout.jsonl"),
        stdout.join("\n") + "\n",
    )
    .expect("stdout");
    fs::write(base.with_extension("stderr.txt"), scrub.text(&call.stderr)).expect("stderr");
    let meta = json!({
        "adapter": agent_id,
        "program": call.spec.program,
        "args": call.spec.args.iter().map(|arg| scrub.text(arg)).collect::<Vec<_>>(),
        "exit_code": call.exit_code,
        "duration_ms": call.duration_ms,
        "resumed_from": resumed_from,
    });
    let meta = serde_json::to_string_pretty(&meta).expect("meta") + "\n";
    fs::write(base.with_extension("meta.json"), meta).expect("meta");
    println!(
        "{agent_id} {name}: exit {:?} in {} ms",
        call.exit_code, call.duration_ms
    );
}

fn throwaway_repo(live: &Path, agent_id: &str) -> PathBuf {
    let repo = live.join(format!("{agent_id}-repo"));
    if repo.exists() {
        fs::remove_dir_all(&repo).expect("an old throwaway repo");
    }
    fs::create_dir_all(&repo).expect("the throwaway repo");
    fs::write(repo.join("README.md"), "# A tiny server\n").expect("readme");
    fs::write(
        repo.join("server.py"),
        "from http.server import BaseHTTPRequestHandler, HTTPServer\n\n\
         class Handler(BaseHTTPRequestHandler):\n    def do_GET(self):\n        \
         self.send_response(200)\n        self.end_headers()\n        \
         self.wfile.write(b\"ok\")\n\n\
         HTTPServer((\"127.0.0.1\", 8000), Handler).serve_forever()\n",
    )
    .expect("server");
    for args in [
        &["init", "--quiet"][..],
        &["add", "."][..],
        &["commit", "--quiet", "-m", "a tiny server"][..],
    ] {
        let status = Command::new("git")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "user.name=Argonauts Code",
            ])
            .args(["-c", "user.email=argonauts@example.invalid"])
            .args(args)
            .current_dir(&repo)
            .stdout(Stdio::null())
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }
    repo
}

/// Recordings keep no local paths, addresses, or machine setup.
struct Scrubber {
    replacements: Vec<(String, String)>,
    email: Regex,
}

impl Scrubber {
    fn new(live: &Path) -> Self {
        let mut replacements = vec![(live.display().to_string(), LIVE.to_owned())];
        if let Some(home) = env::var_os("HOME") {
            replacements.push((PathBuf::from(home).display().to_string(), "~".to_owned()));
        }
        Self {
            replacements,
            email: Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").expect("regex"),
        }
    }

    fn text(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for (from, to) in &self.replacements {
            text = text.replace(from, to);
        }
        self.email
            .replace_all(&text, "you@example.invalid")
            .into_owned()
    }

    fn lines(&self, stdout: &str) -> Vec<String> {
        let mut lines: Vec<Result<Value, String>> = stdout
            .lines()
            .map(|line| serde_json::from_str(line).map_err(|_| line.to_owned()))
            .collect();
        self.rejoin_split_deltas(&mut lines);
        lines
            .into_iter()
            .map(|line| match line {
                Ok(mut value) => {
                    if let Some(object) = value.as_object_mut() {
                        for key in LOCAL_SETUP {
                            object.remove(*key);
                        }
                    }
                    self.text(&value.to_string())
                }
                Err(line) => self.text(&line),
            })
            .collect()
    }

    /// Claude Code streams a path in small pieces, so no single piece matches a path to
    /// replace. A block whose whole text needs scrubbing gets it in its first piece, and the
    /// rest go empty, which the parser joins to the same text.
    fn rejoin_split_deltas(&self, lines: &mut [Result<Value, String>]) {
        let mut blocks: BTreeMap<(u32, u64, &'static str), Vec<usize>> = BTreeMap::new();
        let mut message = 0;
        for (at, line) in lines.iter().enumerate() {
            let Ok(value) = line else { continue };
            let event = &value["event"];
            match event["type"].as_str() {
                Some("message_start") => message += 1,
                Some("content_block_delta") => {
                    let field = match event["delta"]["type"].as_str() {
                        Some("input_json_delta") => "partial_json",
                        Some("text_delta") => "text",
                        Some("thinking_delta") => "thinking",
                        _ => continue,
                    };
                    let index = event["index"].as_u64().unwrap_or(0);
                    blocks.entry((message, index, field)).or_default().push(at);
                }
                _ => {}
            }
        }
        for ((_, _, field), pieces) in blocks {
            let piece = |line: &Result<Value, String>| {
                line.as_ref()
                    .ok()
                    .and_then(|value| value["event"]["delta"][field].as_str().map(str::to_owned))
            };
            let whole: String = pieces.iter().filter_map(|&at| piece(&lines[at])).collect();
            let clean = self.text(&whole);
            if clean == whole {
                continue;
            }
            for (n, &at) in pieces.iter().enumerate() {
                if let Ok(value) = &mut lines[at] {
                    let text = if n == 0 { clean.clone() } else { String::new() };
                    value["event"]["delta"][field] = Value::String(text);
                }
            }
        }
    }
}
