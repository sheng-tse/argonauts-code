//! The fake ACP agent, driven over pipes as the ACP worker will drive it. No model, no network.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use argonauts_testkit::fake_clis::acp::WRONG_PICK_EXIT;
use argonauts_testkit::fake_clis::scenario::{LogLine, NO_RULE_EXIT};
use argonauts_testkit::{FakeClis, TempRepo};
use serde_json::{Value, json};

const FAKE: &str = env!("CARGO_BIN_EXE_argonauts-fake-cli");
const PATIENCE: Duration = Duration::from_secs(10);

fn install(repo: &TempRepo, scenario: &str) -> FakeClis {
    let path = repo.scratch().join("scenario.toml");
    std::fs::write(&path, scenario).unwrap();
    FakeClis::install_with(Path::new(FAKE), repo.scratch(), &path).unwrap()
}

/// The client side of one ACP session with the fake.
struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    frames: Receiver<Value>,
}

impl Client {
    fn start(clis: &FakeClis, repo: &TempRepo, program: &str) -> Self {
        let mut child = Command::new(clis.bin_dir().join(program))
            .arg("acp")
            .current_dir(repo.root())
            .envs(clis.envs())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, frames) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let frame = serde_json::from_str(&line.unwrap()).unwrap();
                if sender.send(frame).is_err() {
                    break;
                }
            }
        });
        Self {
            stdin: child.stdin.take(),
            child,
            frames,
        }
    }

    fn send(&mut self, frame: Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{frame}").unwrap();
        stdin.flush().unwrap();
    }

    fn request(&mut self, id: u64, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
    }

    fn next(&self) -> Value {
        self.frames
            .recv_timeout(PATIENCE)
            .expect("a frame from the fake agent")
    }

    /// The reply to request `id`, with every notification before it.
    fn reply(&self, id: u64) -> (Value, Vec<Value>) {
        let mut notes = Vec::new();
        loop {
            let frame = self.next();
            if frame["id"] == id && frame.get("method").is_none() {
                return (frame, notes);
            }
            notes.push(frame);
        }
    }

    fn open_session(&mut self, mcp_servers: Value) -> String {
        self.request(
            1,
            "initialize",
            json!({ "protocolVersion": 1, "clientCapabilities": {} }),
        );
        let (init, _) = self.reply(1);
        assert_eq!(init["result"]["protocolVersion"], 1, "{init}");
        self.request(
            2,
            "session/new",
            json!({ "cwd": "/work", "mcpServers": mcp_servers }),
        );
        let (new, _) = self.reply(2);
        new["result"]["sessionId"].as_str().unwrap().to_owned()
    }

    fn prompt(&mut self, id: u64, session: &str, text: &str) {
        self.request(
            id,
            "session/prompt",
            json!({ "sessionId": session, "prompt": [{ "type": "text", "text": text }] }),
        );
    }

    /// Closes stdin, as a client does when it's done, and returns the exit code.
    fn finish(mut self) -> i32 {
        drop(self.stdin.take());
        let deadline = argonauts_testkit::pty::deadline::after(PATIENCE);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.code().unwrap_or(-1);
            }
            if Instant::now() > deadline {
                self.child.kill().unwrap();
                panic!("the fake agent didn't exit after its stdin closed");
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

fn frames(clis: &FakeClis, from_agent: bool) -> Vec<Value> {
    clis.log()
        .unwrap()
        .into_iter()
        .filter_map(|line| match line {
            LogLine::AcpFrame {
                from_agent: sent_by_agent,
                frame,
                ..
            } if sent_by_agent == from_agent => Some(frame),
            _ => None,
        })
        .collect()
}

fn wrong_picks(clis: &FakeClis) -> Vec<(String, String)> {
    clis.log()
        .unwrap()
        .into_iter()
        .filter_map(|line| match line {
            LogLine::WrongPick {
                expected, picked, ..
            } => Some((expected, picked)),
            _ => None,
        })
        .collect()
}

fn text_chunk(text: &str) -> String {
    format!(
        "{{ sessionUpdate = \"agent_message_chunk\", content = {{ type = \"text\", text = \"{text}\" }} }}"
    )
}

#[test]
fn a_session_plays_its_updates_and_ends_the_turn() {
    let repo = TempRepo::new().unwrap();
    std::fs::write(
        repo.scratch().join("updates.jsonl"),
        concat!(
            "{\"sessionUpdate\":\"plan\",\"entries\":[]}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"s-1\",",
            "\"update\":{\"sessionUpdate\":\"usage_update\",\"used\":100,\"size\":1000}}}\n",
        ),
    )
    .unwrap();
    let scenario = format!(
        r#"
[[acp]]
argv0 = "hermes"
session_id = "s-1"

[[acp.turn]]
result = {{ usage = {{ inputTokens = 12, outputTokens = 3 }} }}
[[acp.turn.step]]
update = {}
[[acp.turn.step]]
update_file = "updates.jsonl"
"#,
        text_chunk("Hello")
    );
    let clis = install(&repo, &scenario);
    let mut client = Client::start(&clis, &repo, "hermes");
    let session = client.open_session(json!([]));
    assert_eq!(session, "s-1");

    client.prompt(3, &session, "say hello");
    let (end, updates) = client.reply(3);
    let kinds: Vec<&str> = updates
        .iter()
        .map(|note| note["params"]["update"]["sessionUpdate"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["agent_message_chunk", "plan", "usage_update"]);
    assert!(
        updates
            .iter()
            .all(|note| note["params"]["sessionId"] == "s-1")
    );
    assert_eq!(updates[0]["params"]["update"]["content"]["text"], "Hello");
    assert_eq!(end["result"]["stopReason"], "end_turn");
    assert_eq!(end["result"]["usage"]["inputTokens"], 12);

    client.prompt(4, &session, "anything else?");
    let (end, updates) = client.reply(4);
    assert!(updates.is_empty());
    assert_eq!(end["result"]["stopReason"], "end_turn");

    client.request(5, "session/set_model", json!({ "sessionId": session }));
    let (unknown, _) = client.reply(5);
    assert_eq!(unknown["error"]["code"], -32601);

    assert_eq!(client.finish(), 0);
    let received = frames(&clis, false);
    let methods: Vec<&str> = received
        .iter()
        .map(|frame| frame["method"].as_str().unwrap())
        .collect();
    assert_eq!(
        methods,
        [
            "initialize",
            "session/new",
            "session/prompt",
            "session/prompt",
            "session/set_model"
        ]
    );
    assert_eq!(frames(&clis, true).len(), 2 + 4 + 1 + 1);
}

#[test]
fn permission_answers_are_checked_and_a_wrong_pick_fails_the_fake() {
    let repo = TempRepo::new().unwrap();
    let clis = install(
        &repo,
        r#"
[[acp]]
argv0 = "goose"

[[acp.turn]]
[[acp.turn.step]]
permission = { tool_call = { toolCallId = "edit-1", title = "Edit a.txt", kind = "edit" }, expect = "allow-once" }
[[acp.turn.step]]
write = { path = "a.txt", content = "edited\n" }

[[acp.turn]]
[[acp.turn.step]]
permission = { tool_call = { toolCallId = "edit-2", title = "Edit .hermes/config.yaml", kind = "edit" }, expect = "reject-once" }
"#,
    );
    let mut client = Client::start(&clis, &repo, "goose");
    let session = client.open_session(json!([]));

    client.prompt(3, &session, "edit a.txt");
    let ask = client.next();
    assert_eq!(ask["method"], "session/request_permission");
    assert_eq!(ask["params"]["toolCall"]["toolCallId"], "edit-1");
    let options: Vec<(&str, &str)> = ask["params"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|option| {
            (
                option["optionId"].as_str().unwrap(),
                option["kind"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        options,
        [
            ("allow-once", "allow_once"),
            ("allow-always", "allow_always"),
            ("reject-once", "reject_once"),
            ("reject-always", "reject_always"),
        ]
    );
    client.send(json!({
        "jsonrpc": "2.0",
        "id": ask["id"],
        "result": { "outcome": { "outcome": "selected", "optionId": "allow-once" } },
    }));
    let (end, _) = client.reply(3);
    assert_eq!(end["result"]["stopReason"], "end_turn");
    assert_eq!(
        std::fs::read_to_string(repo.root().join("a.txt")).unwrap(),
        "edited\n"
    );
    assert!(wrong_picks(&clis).is_empty());

    client.prompt(4, &session, "edit the hermes config");
    let ask = client.next();
    assert_eq!(ask["params"]["toolCall"]["toolCallId"], "edit-2");
    client.send(json!({
        "jsonrpc": "2.0",
        "id": ask["id"],
        "result": { "outcome": { "outcome": "selected", "optionId": "allow-once" } },
    }));
    let (end, _) = client.reply(4);
    assert_eq!(end["result"]["stopReason"], "end_turn");

    assert_eq!(client.finish(), WRONG_PICK_EXIT);
    assert_eq!(
        wrong_picks(&clis),
        [("reject-once".to_owned(), "allow-once".to_owned())]
    );
}

#[test]
fn a_cancel_stops_the_updates_and_ends_the_turn_cancelled() {
    let repo = TempRepo::new().unwrap();
    let scenario = format!(
        r#"
[[acp]]
argv0 = "openclaw"

[[acp.turn]]
[[acp.turn.step]]
update = {working}
[[acp.turn.step]]
sleep_ms = 30000
[[acp.turn.step]]
update = {never}

[[acp.turn]]
[[acp.turn.step]]
permission = {{ tool_call = {{ toolCallId = "run-1", title = "Run the tests", kind = "execute" }}, expect = "allow-once" }}
[[acp.turn.step]]
update = {never}
"#,
        working = text_chunk("working"),
        never = text_chunk("never sent"),
    );
    let clis = install(&repo, &scenario);
    let mut client = Client::start(&clis, &repo, "openclaw");
    let session = client.open_session(json!([]));
    let cancel = json!({
        "jsonrpc": "2.0",
        "method": "session/cancel",
        "params": { "sessionId": session },
    });

    let started = Instant::now();
    client.prompt(3, &session, "work slowly");
    let first = client.next();
    assert_eq!(first["params"]["update"]["content"]["text"], "working");
    client.send(cancel.clone());
    let (end, updates) = client.reply(3);
    assert!(updates.is_empty(), "{updates:?}");
    assert_eq!(end["result"]["stopReason"], "cancelled");
    assert!(started.elapsed() < PATIENCE);

    // After a cancel the client answers every pending permission request `cancelled`.
    client.prompt(4, &session, "run the tests");
    let ask = client.next();
    assert_eq!(ask["method"], "session/request_permission");
    client.send(cancel);
    client.send(json!({
        "jsonrpc": "2.0",
        "id": ask["id"],
        "result": { "outcome": { "outcome": "cancelled" } },
    }));
    let (end, updates) = client.reply(4);
    assert!(updates.is_empty(), "{updates:?}");
    assert_eq!(end["result"]["stopReason"], "cancelled");

    assert_eq!(client.finish(), 0);
    assert!(wrong_picks(&clis).is_empty());
    let sent = frames(&clis, true);
    assert!(
        !sent
            .iter()
            .any(|frame| frame.to_string().contains("never sent"))
    );
}

#[test]
fn scripted_errors_and_resume() {
    let repo = TempRepo::new().unwrap();
    let clis = install(
        &repo,
        r#"
[[acp]]
argv0 = "hermes"

[acp.reply.initialize]
result = { protocolVersion = 1, agentCapabilities = { loadSession = true }, authMethods = [{ id = "login", name = "Log in" }] }

[acp.reply."session/new"]
error = { code = -32000, message = "Authentication required" }

[[acp.turn]]
stop_reason = "max_turn_requests"
"#,
    );
    let mut client = Client::start(&clis, &repo, "hermes");
    client.request(1, "initialize", json!({ "protocolVersion": 1 }));
    let (init, _) = client.reply(1);
    assert_eq!(init["result"]["authMethods"][0]["id"], "login");

    client.request(
        2,
        "session/new",
        json!({ "cwd": "/work", "mcpServers": [] }),
    );
    let (new, _) = client.reply(2);
    assert_eq!(new["error"]["code"], -32000);
    assert_eq!(new["error"]["message"], "Authentication required");

    client.prompt(3, "old-1", "no session yet");
    let (refused, _) = client.reply(3);
    assert_eq!(refused["error"]["code"], -32602);

    client.request(
        4,
        "session/resume",
        json!({ "sessionId": "old-1", "cwd": "/work", "mcpServers": [] }),
    );
    let (resumed, _) = client.reply(4);
    assert_eq!(resumed["result"], json!({}));
    client.prompt(5, "old-1", "carry on");
    let (end, _) = client.reply(5);
    assert_eq!(end["result"]["stopReason"], "max_turn_requests");

    assert_eq!(client.finish(), 0);
}

#[test]
fn a_report_step_calls_the_report_tool_of_the_named_mcp_server() {
    let repo = TempRepo::new().unwrap();
    let server_log = repo.scratch().join("mcp-server.log");
    let server = repo.scratch().join("mcp-server.sh");
    std::fs::write(
        &server,
        r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$REPORT_LOG"
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"0"}}}' ;;
    *'"method":"tools/call"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"saved"}],"isError":false}}' ;;
  esac
done
"#,
    )
    .unwrap();
    std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o755)).unwrap();
    let clis = install(
        &repo,
        r#"
[[acp]]
argv0 = "hermes"

[[acp.turn]]
[[acp.turn.step]]
report = { status = "done", summary = "All good" }
"#,
    );
    let mut client = Client::start(&clis, &repo, "hermes");
    let session = client.open_session(json!([{
        "name": "argonauts",
        "command": server,
        "args": [],
        "env": [{ "name": "REPORT_LOG", "value": server_log }],
    }]));
    client.prompt(3, &session, "report");
    let (end, _) = client.reply(3);
    assert_eq!(end["result"]["stopReason"], "end_turn");
    assert_eq!(client.finish(), 0);

    let calls: Vec<(String, Value, Value)> = clis
        .log()
        .unwrap()
        .into_iter()
        .filter_map(|line| match line {
            LogLine::ReportCall {
                server,
                report,
                reply,
                ..
            } => Some((server, report, reply)),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "argonauts");
    assert_eq!(calls[0].1["summary"], "All good");
    assert_eq!(calls[0].2["content"][0]["text"], "saved");

    let seen: Vec<Value> = std::fs::read_to_string(&server_log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let methods: Vec<&str> = seen
        .iter()
        .map(|frame| frame["method"].as_str().unwrap())
        .collect();
    assert_eq!(
        methods,
        ["initialize", "notifications/initialized", "tools/call"]
    );
    assert_eq!(seen[2]["params"]["name"], "argonauts_report");
    assert_eq!(seen[2]["params"]["arguments"]["status"], "done");
}

#[test]
fn message_sends_are_recorded_and_succeed() {
    let repo = TempRepo::new().unwrap();
    let clis = install(&repo, "");
    let run = |program: &str, args: &[&str]| {
        Command::new(clis.bin_dir().join(program))
            .args(args)
            .envs(clis.envs())
            .stdin(Stdio::null())
            .output()
            .unwrap()
            .status
            .code()
    };
    assert_eq!(
        run("hermes", &["send", "--to", "me", "A run needs you"]),
        Some(0)
    );
    assert_eq!(
        run(
            "openclaw",
            &[
                "message",
                "send",
                "--target",
                "me",
                "--message",
                "A run needs you"
            ]
        ),
        Some(0)
    );
    assert_eq!(run("openclaw", &["message", "read"]), Some(NO_RULE_EXIT));

    assert_eq!(
        clis.message_sends().unwrap(),
        [
            vec!["hermes", "send", "--to", "me", "A run needs you"],
            vec![
                "openclaw",
                "message",
                "send",
                "--target",
                "me",
                "--message",
                "A run needs you"
            ],
        ]
    );
}
