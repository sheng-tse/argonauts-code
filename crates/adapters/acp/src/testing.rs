//! A scripted agent on in-memory pipes, for driving the session driver in tests.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use argonauts_core::AgentEvent;
use argonauts_core::driver::{
    DriverControl, DriverEnd, DriverIo, DriverStart, RawFrame, SessionDriver,
};
use argonauts_core::events::session::Delivery;
use argonauts_core::registry::acp::AcpProfile;
use argonauts_core::worker::Access;
use serde_json::{Value, json};
use tokio::io::{
    AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::driver::{AcpDriver, DriverProfile};
use crate::errors::ErrorRules;

pub(crate) const SESSION: &str = "s-1";
const PATIENCE: Duration = Duration::from_secs(10);

pub(crate) fn profile(acp: AcpProfile) -> DriverProfile {
    DriverProfile::new(acp, Ok(ErrorRules::default()))
}

pub(crate) fn profile_with_rules(rules: ErrorRules) -> DriverProfile {
    DriverProfile::new(AcpProfile::default(), Ok(rules))
}

pub(crate) fn start(cwd: impl AsRef<Path>) -> DriverStart {
    DriverStart {
        cwd: cwd.as_ref().to_owned(),
        prompt: "Fix the bug.".into(),
        model: None,
        effort: None,
        access: Access::Build,
        resume: None,
        mcp_servers: Vec::new(),
        scope: Default::default(),
        json_schema: None,
        report_file: None,
        asking: None,
    }
}

pub(crate) struct Driven {
    pub events: mpsc::UnboundedReceiver<AgentEvent>,
    pub frames: mpsc::UnboundedReceiver<RawFrame>,
    pub control: mpsc::Sender<DriverControl>,
    pub stderr: mpsc::Sender<String>,
    pub agent: Agent,
    pub task: JoinHandle<DriverEnd>,
}

pub(crate) fn launch(profile: DriverProfile, start: DriverStart) -> Driven {
    let (client, agent) = tokio::io::duplex(1 << 20);
    let (client_read, client_write) = tokio::io::split(client);
    let (agent_read, agent_write) = tokio::io::split(agent);
    let (events_sender, events) = mpsc::unbounded_channel();
    let (frames_sender, frames) = mpsc::unbounded_channel();
    let (control, control_receiver) = mpsc::channel(16);
    let (stderr, stderr_lines) = mpsc::channel(16);
    let io = DriverIo {
        stdin: Box::new(client_write),
        stdout: Box::new(client_read),
        stderr_lines,
        events: events_sender,
        control: control_receiver,
        frames: frames_sender,
    };
    let driver = AcpDriver::new(Arc::new(profile));
    let task = tokio::spawn(driver.run(io, start));
    Driven {
        events,
        frames,
        control,
        stderr,
        agent: Agent {
            lines: BufReader::new(agent_read).lines(),
            writer: Some(agent_write),
            seen: Vec::new(),
        },
        task,
    }
}

impl Driven {
    pub async fn deliver(&self, text: &str, now: bool) -> oneshot::Receiver<Delivery> {
        let (done, delivered) = oneshot::channel();
        self.control
            .send(DriverControl::Deliver {
                text: text.to_owned(),
                now,
                done,
            })
            .await
            .unwrap();
        delivered
    }

    /// Waits until the driver has emitted a matching event, so a control sent next can't
    /// overtake the frames before it.
    pub async fn wait_for(&mut self, wanted: impl Fn(&AgentEvent) -> bool) {
        loop {
            let event = tokio::time::timeout(PATIENCE, self.events.recv())
                .await
                .expect("the driver emitted nothing")
                .expect("the driver ended");
            if wanted(&event) {
                return;
            }
        }
    }

    pub async fn finish(self) -> (DriverEnd, Vec<AgentEvent>) {
        let Driven {
            events,
            task,
            agent,
            control,
            ..
        } = self;
        let ended = finish(task, events).await;
        drop((agent, control));
        ended
    }
}

/// Waits for the driver to end, then takes every event it sent.
pub(crate) async fn finish(
    task: JoinHandle<DriverEnd>,
    mut events: mpsc::UnboundedReceiver<AgentEvent>,
) -> (DriverEnd, Vec<AgentEvent>) {
    let end = tokio::time::timeout(PATIENCE, task)
        .await
        .expect("the driver didn't end")
        .unwrap();
    let mut seen = Vec::new();
    while let Ok(event) = events.try_recv() {
        seen.push(event);
    }
    (end, seen)
}

pub(crate) struct Agent {
    lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
    writer: Option<WriteHalf<DuplexStream>>,
    seen: Vec<Value>,
}

impl Agent {
    /// The next frame the driver sent.
    pub async fn recv(&mut self) -> Value {
        let line = tokio::time::timeout(PATIENCE, self.lines.next_line())
            .await
            .expect("the driver sent nothing")
            .unwrap()
            .expect("the driver closed its pipe");
        let frame: Value = serde_json::from_str(&line).unwrap();
        self.seen.push(frame.clone());
        frame
    }

    pub async fn expect(&mut self, method: &str) -> Value {
        let frame = self.recv().await;
        assert_eq!(frame["method"], method, "{frame}");
        frame
    }

    /// Asserts the driver sends nothing for a while. Closing its pipe is fine.
    pub async fn silent_for(&mut self, quiet: Duration) {
        if let Ok(Ok(Some(line))) = tokio::time::timeout(quiet, self.lines.next_line()).await {
            panic!("the driver sent {line}");
        }
    }

    pub async fn send(&mut self, frame: Value) {
        let writer = self.writer.as_mut().expect("the agent's pipe is closed");
        let mut line = frame.to_string().into_bytes();
        line.push(b'\n');
        writer.write_all(&line).await.unwrap();
        writer.flush().await.unwrap();
    }

    pub async fn reply(&mut self, request: &Value, result: Value) {
        self.send(json!({ "jsonrpc": "2.0", "id": request["id"], "result": result }))
            .await;
    }

    pub async fn fail(&mut self, request: &Value, code: i64, message: &str) {
        self.send(json!({ "jsonrpc": "2.0", "id": request["id"], "error": { "code": code, "message": message } }))
            .await;
    }

    pub async fn update(&mut self, update: Value) {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": { "sessionId": SESSION, "update": update },
        }))
        .await;
    }

    pub async fn say(&mut self, text: &str) {
        self.update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": text } }))
            .await;
    }

    /// A permission request with one option of each kind, ids unlike their kinds.
    pub async fn ask_permission(&mut self, id: &str, tool_call: Value) {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/request_permission",
            "params": {
                "sessionId": SESSION,
                "toolCall": tool_call,
                "options": [
                    { "optionId": "yes", "name": "Allow", "kind": "allow_once" },
                    { "optionId": "always", "name": "Always allow", "kind": "allow_always" },
                    { "optionId": "no", "name": "Reject", "kind": "reject_once" },
                    { "optionId": "never", "name": "Always reject", "kind": "reject_always" },
                ],
            },
        }))
        .await;
    }

    /// Answers `initialize` and `session/new`. An empty `initialize` answer means ACP v1 with
    /// no extra capabilities.
    pub async fn open(&mut self, initialize: Value, new_session: Value) {
        let request = self.expect("initialize").await;
        let answer = if initialize == json!({}) {
            json!({ "protocolVersion": 1, "agentCapabilities": {} })
        } else {
            initialize
        };
        self.reply(&request, answer).await;
        let request = self.expect("session/new").await;
        let mut answer = new_session;
        answer["sessionId"] = SESSION.into();
        self.reply(&request, answer).await;
    }

    /// A frame the driver already sent, by method.
    pub fn sent_before_prompt(&self, method: &str) -> Value {
        self.seen
            .iter()
            .find(|frame| frame["method"] == method)
            .cloned()
            .unwrap_or_else(|| panic!("the driver never sent {method}"))
    }

    /// Closes the agent's stdout, as a crash would.
    pub async fn close(&mut self) {
        if let Some(mut writer) = self.writer.take() {
            writer.shutdown().await.unwrap();
        }
    }
}
