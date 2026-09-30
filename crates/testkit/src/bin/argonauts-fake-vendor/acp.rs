//! The fake vendor as an ACP agent: newline-delimited JSON-RPC 2.0 on stdio, played from an
//! `[[acp]]` rule, with every frame logged both ways.

use std::collections::VecDeque;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use argonauts_testkit::acp::{
    AcpError, AcpPermission, AcpRule, AcpStep, AcpTurn, DEFAULT_SESSION_ID, PROTOCOL_VERSION,
    WRONG_PICK_EXIT,
};
use argonauts_testkit::scenario::{LogLine, NO_RULE_EXIT};
use serde_json::{Value, json};

use crate::{log, log_invocation, report, write_inside_cwd};

const PARSE_ERROR: i64 = -32700;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

pub fn serve(rules: &[AcpRule], base: &Path, argv0: &str, args: &[String]) -> anyhow::Result<i32> {
    let matched = rules
        .iter()
        .enumerate()
        .find(|(_, rule)| matches(rule, argv0, args));
    log_invocation(argv0, args, None, matched.map(|(index, _)| index));
    let Some((_, rule)) = matched else {
        eprintln!(
            "argonauts-fake-vendor: no [[acp]] rule matched {argv0} {}",
            args.join(" ")
        );
        return Ok(NO_RULE_EXIT);
    };
    let code = Agent::new(rule, base, argv0).run()?;
    log(&LogLine::Exit {
        pid: std::process::id(),
        code,
    });
    Ok(code)
}

fn matches(rule: &AcpRule, argv0: &str, args: &[String]) -> bool {
    rule.argv0.as_deref().is_none_or(|want| want == argv0)
        && rule.args_contain.iter().all(|want| args.contains(want))
}

enum Flow {
    Continue,
    Exit(i32),
}

struct Agent<'a> {
    rule: &'a AcpRule,
    base: &'a Path,
    name: &'a str,
    incoming: Receiver<Value>,
    /// Requests that arrived mid-turn, answered once it ends.
    deferred: VecDeque<Value>,
    session: Option<String>,
    mcp_servers: Vec<Value>,
    turns_played: usize,
    next_request: u64,
    cancelled: bool,
    /// The client closed our stdin.
    closed: bool,
    wrong_picks: usize,
}

impl<'a> Agent<'a> {
    fn new(rule: &'a AcpRule, base: &'a Path, name: &'a str) -> Self {
        Self {
            rule,
            base,
            name,
            incoming: read_frames(),
            deferred: VecDeque::new(),
            session: None,
            mcp_servers: Vec::new(),
            turns_played: 0,
            next_request: 0,
            cancelled: false,
            closed: false,
            wrong_picks: 0,
        }
    }

    fn run(mut self) -> anyhow::Result<i32> {
        loop {
            let frame = match self.deferred.pop_front() {
                Some(frame) => frame,
                None => match self.incoming.recv() {
                    Ok(frame) => frame,
                    Err(_) => return Ok(self.exit_code(0)),
                },
            };
            if let Flow::Exit(code) = self.handle(frame)? {
                return Ok(self.exit_code(code));
            }
        }
    }

    fn exit_code(&self, code: i32) -> i32 {
        if code == 0 && self.wrong_picks > 0 {
            WRONG_PICK_EXIT
        } else {
            code
        }
    }

    fn handle(&mut self, frame: Value) -> anyhow::Result<Flow> {
        if frame.is_string() {
            self.send_error(&Value::Null, PARSE_ERROR, "not a JSON frame")?;
            return Ok(Flow::Continue);
        }
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            return Ok(Flow::Continue);
        };
        // Notifications outside a turn, such as a late `session/cancel`, need nothing.
        let Some(id) = frame.get("id") else {
            return Ok(Flow::Continue);
        };
        let params = frame.get("params").unwrap_or(&Value::Null);
        if method == "session/prompt" {
            return self.prompt(id, params);
        }
        self.answer(method, id, params)?;
        Ok(Flow::Continue)
    }

    fn answer(&mut self, method: &str, id: &Value, params: &Value) -> anyhow::Result<()> {
        let reply = self.rule.replies.get(method);
        if let Some(error) = reply.and_then(|reply| reply.error.as_ref()) {
            return self.send_rpc_error(id, error);
        }
        let result = match reply.and_then(|reply| reply.result.clone()) {
            Some(result) => result,
            None => match self.default_result(method) {
                Some(result) => result,
                None => {
                    let message = format!("method not found: {method}");
                    return self.send_error(id, METHOD_NOT_FOUND, &message);
                }
            },
        };
        let result = match method {
            "session/new" => {
                let result = with_session_id(result, &self.new_session_id());
                self.session = result["sessionId"].as_str().map(str::to_owned);
                self.mcp_servers = mcp_servers(params);
                result
            }
            "session/load" | "session/resume" => {
                self.session = params["sessionId"].as_str().map(str::to_owned);
                self.mcp_servers = mcp_servers(params);
                result
            }
            _ => result,
        };
        for update in reply
            .map(|reply| reply.updates.as_slice())
            .unwrap_or_default()
        {
            self.send_update(update)?;
        }
        self.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }

    fn default_result(&self, method: &str) -> Option<Value> {
        Some(match method {
            "initialize" => json!({
                "protocolVersion": PROTOCOL_VERSION,
                "agentCapabilities": {
                    "loadSession": false,
                    "promptCapabilities": {
                        "image": false,
                        "audio": false,
                        "embeddedContext": false,
                    },
                    "mcpCapabilities": { "http": false, "sse": false },
                },
                "authMethods": [],
                "agentInfo": { "name": self.name, "version": env!("CARGO_PKG_VERSION") },
            }),
            "session/new" => json!({ "sessionId": self.new_session_id() }),
            "session/load" => Value::Null,
            "authenticate" | "session/resume" | "session/set_mode" => json!({}),
            _ => return None,
        })
    }

    fn new_session_id(&self) -> String {
        self.rule
            .session_id
            .clone()
            .unwrap_or_else(|| DEFAULT_SESSION_ID.to_owned())
    }

    fn prompt(&mut self, id: &Value, params: &Value) -> anyhow::Result<Flow> {
        let asked = params["sessionId"].as_str();
        if asked.is_none() || asked != self.session.as_deref() {
            let message = format!("unknown session {}", params["sessionId"]);
            self.send_error(id, INVALID_PARAMS, &message)?;
            return Ok(Flow::Continue);
        }
        let turn = self.rule.turns.get(self.turns_played);
        self.turns_played += 1;
        self.cancelled = false;
        for step in turn.map(|turn| turn.steps.as_slice()).unwrap_or_default() {
            self.poll();
            if self.interrupted() {
                break;
            }
            if let Flow::Exit(code) = self.play(step)? {
                return Ok(Flow::Exit(code));
            }
        }
        self.poll();
        if self.closed {
            return Ok(Flow::Exit(0));
        }
        if !self.cancelled
            && let Some(error) = turn.and_then(|turn| turn.error.as_ref())
        {
            self.send_rpc_error(id, error)?;
            return Ok(Flow::Continue);
        }
        let stop_reason = if self.cancelled {
            "cancelled"
        } else {
            turn.map_or("end_turn", AcpTurn::stop_reason)
        };
        let mut result = turn
            .and_then(|turn| turn.result.clone())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        result["stopReason"] = stop_reason.into();
        self.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }))?;
        Ok(Flow::Continue)
    }

    fn play(&mut self, step: &AcpStep) -> anyhow::Result<Flow> {
        match step {
            AcpStep::Update(update) => self.send_update(update)?,
            AcpStep::UpdateFile(path) => self.send_update_file(path)?,
            AcpStep::SleepMs(ms) => self.wait(Some(Duration::from_millis(*ms))),
            AcpStep::Permission(permission) => self.ask(permission)?,
            AcpStep::Report(report) => self.report(report),
            AcpStep::Write { path, content } => write_inside_cwd(path, content)?,
            AcpStep::Stderr(line) => eprintln!("{line}"),
            AcpStep::Exit(code) => return Ok(Flow::Exit(*code)),
            AcpStep::Hang(_) => self.wait(None),
        }
        Ok(Flow::Continue)
    }

    fn interrupted(&self) -> bool {
        self.cancelled || self.closed
    }

    /// Takes what arrived mid-turn without waiting.
    fn poll(&mut self) {
        loop {
            match self.incoming.try_recv() {
                Ok(frame) => {
                    self.during_turn(frame);
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.closed = true;
                    return;
                }
            }
        }
    }

    /// Waits until `limit` passes, the turn is cancelled, or the client goes away.
    fn wait(&mut self, limit: Option<Duration>) {
        let deadline = limit.map(|limit| Instant::now() + limit);
        while !self.interrupted() {
            let next = match deadline {
                Some(deadline) => self
                    .incoming
                    .recv_timeout(deadline.saturating_duration_since(Instant::now())),
                None => self
                    .incoming
                    .recv()
                    .map_err(|_| RecvTimeoutError::Disconnected),
            };
            match next {
                Ok(frame) => {
                    self.during_turn(frame);
                }
                Err(RecvTimeoutError::Timeout) => return,
                Err(RecvTimeoutError::Disconnected) => self.closed = true,
            }
        }
    }

    /// A cancel marks the turn, and requests wait for it to end. A response goes back to the
    /// caller, which drops it unless it's waiting for that one.
    fn during_turn(&mut self, frame: Value) -> Option<Value> {
        match frame.get("method").and_then(Value::as_str) {
            Some("session/cancel") => self.cancelled = true,
            Some(_) if frame.get("id").is_some() => self.deferred.push_back(frame),
            Some(_) => {}
            None if frame.is_object() => return Some(frame),
            None => self.deferred.push_back(frame),
        }
        None
    }

    fn ask(&mut self, permission: &AcpPermission) -> anyhow::Result<()> {
        let id = self.next_request;
        self.next_request += 1;
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/request_permission",
            "params": {
                "sessionId": self.session,
                "toolCall": permission.tool_call,
                "options": permission.options(),
            },
        }))?;
        let answer = loop {
            let Ok(frame) = self.incoming.recv() else {
                self.closed = true;
                return Ok(());
            };
            if let Some(response) = self.during_turn(frame)
                && response["id"] == id
            {
                break response;
            }
        };
        let picked = picked_option(&answer);
        // Once the turn is cancelled, the client must answer every pending request `cancelled`.
        let expected = if self.cancelled {
            Some("cancelled")
        } else {
            permission.expect.as_deref()
        };
        if let Some(expected) = expected
            && picked != expected
        {
            eprintln!("argonauts-fake-vendor: expected the pick {expected:?}, got {picked:?}");
            log(&LogLine::WrongPick {
                pid: std::process::id(),
                tool_call_id: permission.tool_call["toolCallId"].clone(),
                expected: expected.to_owned(),
                picked,
            });
            self.wrong_picks += 1;
        }
        Ok(())
    }

    fn report(&mut self, report: &Value) {
        let server = self
            .mcp_servers
            .iter()
            .find(|server| server.get("type").is_none_or(|kind| kind == "stdio"));
        let (name, reply) = match server {
            None => (
                String::new(),
                json!({ "error": "the client named no stdio MCP server" }),
            ),
            Some(server) => (
                server["name"].as_str().unwrap_or_default().to_owned(),
                report::call(server, report)
                    .unwrap_or_else(|err| json!({ "error": format!("{err:#}") })),
            ),
        };
        if let Some(error) = reply.get("error") {
            eprintln!("argonauts-fake-vendor: the report call failed: {error}");
        }
        log(&LogLine::ReportCall {
            pid: std::process::id(),
            server: name,
            report: report.clone(),
            reply,
        });
    }

    fn send_update(&self, update: &Value) -> anyhow::Result<()> {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": { "sessionId": self.session, "update": update },
        }))
    }

    fn send_update_file(&mut self, path: &str) -> anyhow::Result<()> {
        let path = self.base.join(path);
        let text = fs::read_to_string(&path)
            .with_context(|| format!("reading the updates in {}", path.display()))?;
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            self.poll();
            if self.interrupted() {
                break;
            }
            let frame: Value = serde_json::from_str(line)
                .with_context(|| format!("parsing a line of {}", path.display()))?;
            if frame.get("method").is_some() {
                self.send(frame)?;
            } else {
                self.send_update(&frame)?;
            }
        }
        Ok(())
    }

    fn send_error(&self, id: &Value, code: i64, message: &str) -> anyhow::Result<()> {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        }))
    }

    fn send_rpc_error(&self, id: &Value, error: &AcpError) -> anyhow::Result<()> {
        let mut body = json!({ "code": error.code, "message": error.message });
        if let Some(data) = &error.data {
            body["data"] = data.clone();
        }
        self.send(json!({ "jsonrpc": "2.0", "id": id, "error": body }))
    }

    /// Logged before it's written, so a client that reads the frame can already find it logged.
    fn send(&self, frame: Value) -> anyhow::Result<()> {
        let line = serde_json::to_string(&frame).context("encoding a frame")?;
        log(&LogLine::AcpFrame {
            pid: std::process::id(),
            from_agent: true,
            frame,
        });
        let mut out = io::stdout().lock();
        writeln!(out, "{line}")?;
        out.flush()?;
        Ok(())
    }
}

/// Reads stdin on its own thread, so a cancel arrives while the agent is busy mid-turn.
fn read_frames() -> Receiver<Value> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            let frame = serde_json::from_str(&line).unwrap_or(Value::String(line));
            log(&LogLine::AcpFrame {
                pid: std::process::id(),
                from_agent: false,
                frame: frame.clone(),
            });
            if sender.send(frame).is_err() {
                break;
            }
        }
    });
    receiver
}

fn with_session_id(result: Value, session_id: &str) -> Value {
    let mut result = if result.is_object() {
        result
    } else {
        json!({})
    };
    if result.get("sessionId").is_none() {
        result["sessionId"] = session_id.into();
    }
    result
}

fn mcp_servers(params: &Value) -> Vec<Value> {
    params["mcpServers"].as_array().cloned().unwrap_or_default()
}

/// The option id, `cancelled`, or the whole answer when it's neither.
fn picked_option(answer: &Value) -> String {
    let outcome = &answer["result"]["outcome"];
    match (outcome["outcome"].as_str(), outcome["optionId"].as_str()) {
        (Some("selected"), Some(option)) => option.to_owned(),
        (Some("cancelled"), _) => "cancelled".to_owned(),
        _ => answer.to_string(),
    }
}
