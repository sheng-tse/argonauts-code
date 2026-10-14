//! The session driver: one ACP session, from `initialize` to the end of its last turn. It turns
//! the worker's controls into ACP calls and the agent's updates into events, and answers
//! permission requests while updates stream.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    CancelNotification, ClientCapabilities, CloseSessionRequest, ContentBlock, Implementation,
    InitializeRequest, LoadSessionRequest, McpServer, McpServerStdio, NewSessionRequest,
    PromptRequest, RequestPermissionOutcome, RequestPermissionResponse, ResumeSessionRequest,
    SelectedPermissionOutcome, SetSessionModeRequest,
};
use argonauts_core::driver::{
    DriverControl, DriverEnd, DriverIo, DriverStart, DriverStop, SessionDriver,
};
use argonauts_core::events::session::Delivery;
use argonauts_core::future::BoxFuture;
use argonauts_core::registry::acp::{AcpProfile, ResumeRoute, SteerRoute, UsageSource};
use argonauts_core::session_runner::Access;
use argonauts_core::{AgentEvent, ErrorCategory};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::errors::{ErrorRules, failed, stop_for};
use crate::mapping::{Mapping, end_turn_usage};
use crate::permission::{Answer, Policy, Verdict, is_bypass_mode};
use crate::report::ReportStep;
use crate::rpc::{Closed, INTERNAL_ERROR, Incoming, METHOD_NOT_FOUND, RpcError, Wire};

/// How long a cancelled turn may take to answer before the session ends anyway. The worker's
/// stop plan goes on from there.
const CANCEL_GRACE: Duration = Duration::from_secs(3);

/// How long to wait for stderr lines the worker hasn't passed on yet once stdout closes, so a
/// crash is named by its last words.
const STDERR_SETTLE: Duration = Duration::from_millis(300);

/// How long a stop after the tool, or a steer that has to cancel first, waits for the running
/// tool calls. An agent may never finish a tool it gave up on.
const TOOL_WAIT: Duration = Duration::from_secs(300);

/// A profile's parts the driver needs.
#[derive(Debug)]
pub(crate) struct DriverProfile {
    pub acp: AcpProfile,
    /// An invalid pattern fails every session with its reason.
    pub errors: Result<ErrorRules, String>,
    pub tool_wait: Duration,
}

impl DriverProfile {
    pub fn new(acp: AcpProfile, errors: Result<ErrorRules, String>) -> Self {
        Self {
            acp,
            errors,
            tool_wait: TOOL_WAIT,
        }
    }
}

pub(crate) struct AcpDriver {
    profile: Arc<DriverProfile>,
}

impl AcpDriver {
    pub fn new(profile: Arc<DriverProfile>) -> Self {
        Self { profile }
    }
}

impl SessionDriver for AcpDriver {
    fn run(&self, io: DriverIo, start: DriverStart) -> BoxFuture<'static, DriverEnd> {
        let profile = Arc::clone(&self.profile);
        Box::pin(async move {
            let mut session = Session::new(profile, io, start);
            let stop = match session.drive().await {
                Ok(stop) | Err(stop) => stop,
            };
            session.finish(&stop);
            DriverEnd {
                cli_session: session.session_id.take(),
                stop,
            }
        })
    }
}

/// What the agent's `initialize` answer offers.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Offers {
    pub load: bool,
    pub resume: bool,
    pub close: bool,
}

struct Queued {
    text: String,
    done: oneshot::Sender<Delivery>,
}

/// Why the driver cancelled a turn. A later cause overrides an earlier, weaker one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CancelCause {
    /// `cancel-and-prompt` steering: the message follows as a new prompt.
    Steer,
    StopAfterTool,
    Stop,
}

#[derive(Default)]
struct Turn {
    /// `session/prompt` requests not answered yet. Steering with `input` adds one.
    prompts: Vec<u64>,
    /// The first error, or else the last answer.
    outcome: Option<Result<Value, RpcError>>,
    cancel: Option<CancelCause>,
    cancel_sent: bool,
    stop_after_tool: bool,
    steers: Vec<Queued>,
    /// When the wait for running tools gives up, once something waits on them.
    tool_wait: Option<Instant>,
    tool_wait_over: bool,
}

enum Input {
    Frame(Incoming),
    Closed,
    Control(DriverControl),
    /// Every control sender is gone, which means stop.
    ControlClosed,
    Stderr(String),
    Deadline,
    ToolWaitOver,
}

pub(crate) struct Session {
    pub(crate) profile: Arc<DriverProfile>,
    pub(crate) start: DriverStart,
    pub(crate) wire: Wire,
    events: mpsc::UnboundedSender<AgentEvent>,
    control: mpsc::Receiver<DriverControl>,
    control_open: bool,
    stderr: mpsc::Receiver<String>,
    stderr_open: bool,
    pub(crate) session_id: Option<String>,
    pub(crate) offers: Offers,
    /// The answer to `session/new`, `session/resume`, or `session/load`, for the read-back.
    pub(crate) opened: Value,
    /// The model the agent runs, once read back.
    pub(crate) model: Option<String>,
    pub(crate) mapping: Mapping,
    policy: Policy,
    /// While `session/load` replays history, which is in the raw log and isn't emitted again.
    replaying: bool,
    turn: Turn,
    queued: VecDeque<Queued>,
    /// Controls that arrived before the first prompt went out.
    held: Vec<DriverControl>,
    /// A stderr line that matched a profile pattern, which names the failure if the pipes close.
    stderr_error: Option<(ErrorCategory, String)>,
    /// A check that failed mid-turn, such as a switch into a bypass mode.
    violation: Option<DriverStop>,
    deadline: Option<Instant>,
}

impl Session {
    fn new(profile: Arc<DriverProfile>, io: DriverIo, start: DriverStart) -> Self {
        let cost_from_usage_update = profile.acp.usage.contains(&UsageSource::UsageUpdate);
        let mode = mode_for(&profile.acp, start.access);
        Self {
            wire: Wire::new(io.stdin, io.stdout, io.frames),
            events: io.events,
            control: io.control,
            control_open: true,
            stderr: io.stderr_lines,
            stderr_open: true,
            session_id: None,
            offers: Offers::default(),
            opened: Value::Null,
            model: None,
            mapping: Mapping::new(&start.cwd, cost_from_usage_update),
            policy: Policy::new(&start, mode),
            replaying: false,
            turn: Turn::default(),
            queued: VecDeque::new(),
            held: Vec::new(),
            stderr_error: None,
            violation: None,
            deadline: None,
            profile,
            start,
        }
    }

    async fn drive(&mut self) -> Result<DriverStop, DriverStop> {
        if let Err(reason) = &self.profile.errors {
            return Err(failed(ErrorCategory::Cli, reason.clone()));
        }
        if let Some(mode) = mode_for(&self.profile.acp, self.start.access)
            && is_bypass_mode(&mode)
        {
            return Err(failed(
                ErrorCategory::Permission,
                format!("the profile's mode {mode} skips permission requests"),
            ));
        }
        self.initialize().await?;
        self.open_session().await?;
        self.set_mode().await?;
        self.choose_model().await?;
        self.choose_effort().await?;
        self.emit(AgentEvent::Init {
            session_id: self.session_id.clone(),
            model: self.model.clone(),
            tools: Vec::new(),
            plugins: Vec::new(),
            capabilities: Vec::new(),
            version: None,
        });

        let mut text = self.start.prompt.clone();
        let mut done = Vec::new();
        let mut report_asked = false;
        loop {
            self.run_turn(text, done, Delivery::NextTurn).await?;
            if let Some(next) = self.queued.pop_front() {
                text = next.text;
                done = vec![next.done];
                continue;
            }
            match self.report(report_asked).await? {
                ReportStep::Done => return Ok(DriverStop::EndTurn),
                ReportStep::Ask(prompt) => {
                    report_asked = true;
                    text = prompt;
                    done = Vec::new();
                }
            }
        }
    }

    /// The tools still open, the pending message, and the failure as an event so the journal
    /// shows it.
    fn finish(&mut self, stop: &DriverStop) {
        for event in self.mapping.end_turn() {
            self.emit(event);
        }
        if let DriverStop::Failed { category, message } = stop {
            self.emit(AgentEvent::Error {
                category: *category,
                message: message.clone(),
                retryable: false,
            });
        }
    }

    async fn initialize(&mut self) -> Result<(), DriverStop> {
        // No file system, terminal, or elicitation: the agent uses its own tools inside the
        // sandbox, and states its assumptions in its report instead of asking.
        let request = InitializeRequest::new(ProtocolVersion::V1)
            .client_capabilities(ClientCapabilities::new())
            .client_info(Implementation::new("argonauts", env!("CARGO_PKG_VERSION")));
        let result = self
            .call_or_fail("initialize", &request, ErrorCategory::Cli)
            .await?;
        let version = result.get("protocolVersion").and_then(Value::as_u64);
        if version != Some(1) {
            let spoken =
                version.map_or_else(|| "no version".to_owned(), |v| format!("version {v}"));
            return Err(failed(
                ErrorCategory::Cli,
                format!("the agent speaks ACP {spoken}, and Argonauts Code speaks version 1"),
            ));
        }
        let capabilities = &result["agentCapabilities"];
        let offered = |name: &str| {
            capabilities["sessionCapabilities"]
                .get(name)
                .is_some_and(|value| !value.is_null() && value != &Value::Bool(false))
        };
        self.offers = Offers {
            load: capabilities["loadSession"] == Value::Bool(true),
            resume: offered("resume"),
            close: offered("close"),
        };
        Ok(())
    }

    async fn open_session(&mut self) -> Result<(), DriverStop> {
        let servers = self.mcp_servers();
        let cwd = self.start.cwd.clone();
        let Some(resume) = self.start.resume.clone() else {
            let request = NewSessionRequest::new(cwd).mcp_servers(servers);
            let result = self
                .call_or_fail("session/new", &request, ErrorCategory::Cli)
                .await?;
            let Some(id) = result.get("sessionId").and_then(Value::as_str) else {
                return Err(failed(
                    ErrorCategory::Cli,
                    "the agent's new session has no id",
                ));
            };
            self.session_id = Some(id.to_owned());
            self.opened = result;
            return Ok(());
        };
        let result = match self.profile.acp.resume {
            ResumeRoute::Resume if self.offers.resume => {
                let request = ResumeSessionRequest::new(resume.clone(), cwd).mcp_servers(servers);
                self.call_or_fail("session/resume", &request, ErrorCategory::Cli)
                    .await?
            }
            ResumeRoute::Load if self.offers.load => {
                let request = LoadSessionRequest::new(resume.clone(), cwd).mcp_servers(servers);
                self.replaying = true;
                let result = self
                    .call_or_fail("session/load", &request, ErrorCategory::Cli)
                    .await;
                self.replaying = false;
                result?
            }
            ResumeRoute::None => {
                return Err(failed(
                    ErrorCategory::InvalidRequest,
                    "this agent's profile has no way to resume a session",
                ));
            }
            route => {
                let method = if route == ResumeRoute::Resume {
                    "session/resume"
                } else {
                    "session/load"
                };
                return Err(failed(
                    ErrorCategory::InvalidRequest,
                    format!("the agent doesn't offer {method}, which its profile names"),
                ));
            }
        };
        self.session_id = Some(resume);
        self.opened = result;
        Ok(())
    }

    async fn set_mode(&mut self) -> Result<(), DriverStop> {
        let Some(mode) = mode_for(&self.profile.acp, self.start.access) else {
            return Ok(());
        };
        let request = SetSessionModeRequest::new(self.session_id(), mode.clone());
        match self.call("session/set_mode", to_json(&request)?).await? {
            Ok(_) => Ok(()),
            Err(error) => Err(
                match self
                    .rules()?
                    .rpc_failure(&error, ErrorCategory::InvalidRequest)
                {
                    DriverStop::Failed { category, message } => failed(
                        category,
                        format!("the agent refused the mode {mode}: {message}"),
                    ),
                    other => other,
                },
            ),
        }
    }

    fn mcp_servers(&self) -> Vec<McpServer> {
        if !self.profile.acp.mcp_servers {
            return Vec::new();
        }
        self.start
            .mcp_servers
            .iter()
            .map(|server| {
                McpServer::Stdio(
                    McpServerStdio::new(server.name.clone(), server.command.clone())
                        .args(server.args.clone()),
                )
            })
            .collect()
    }

    /// Runs one turn to its end, plus the prompts that `cancel-and-prompt` steering adds.
    async fn run_turn(
        &mut self,
        mut text: String,
        mut done: Vec<oneshot::Sender<Delivery>>,
        mut delivery: Delivery,
    ) -> Result<(), DriverStop> {
        loop {
            if let Some(stop) = self.violation.take() {
                return Err(stop);
            }
            self.turn = Turn::default();
            self.send_prompt(text, done, delivery).await?;
            for control in std::mem::take(&mut self.held) {
                self.on_control(control).await?;
            }
            self.pump_turn().await?;
            for event in self.mapping.end_turn() {
                self.emit(event);
            }
            self.deadline = None;
            if let Some(stop) = self.violation.take() {
                return Err(stop);
            }
            let turn = std::mem::take(&mut self.turn);
            if turn.cancel == Some(CancelCause::Stop) {
                return Err(DriverStop::Cancelled);
            }
            if let Some(Err(error)) = &turn.outcome {
                return Err(self.rules()?.rpc_failure(error, ErrorCategory::Unknown));
            }
            if turn.cancel == Some(CancelCause::StopAfterTool) {
                self.close_session().await;
                return Err(DriverStop::Cancelled);
            }
            if turn.cancel == Some(CancelCause::Steer) {
                let (texts, dones): (Vec<String>, Vec<_>) = turn
                    .steers
                    .into_iter()
                    .map(|queued| (queued.text, queued.done))
                    .unzip();
                text = texts.join("\n\n");
                done = dones;
                delivery = Delivery::Steer;
                continue;
            }
            let reason = match &turn.outcome {
                Some(Ok(result)) => result.get("stopReason").and_then(Value::as_str),
                _ => None,
            };
            return match reason.map(|reason| (reason, stop_for(reason))) {
                Some((_, Some(DriverStop::EndTurn))) => Ok(()),
                Some((_, Some(stop))) => Err(stop),
                Some((reason, None)) => Err(failed(
                    ErrorCategory::Unknown,
                    format!("the turn ended with an unknown stop reason, {reason}"),
                )),
                None => Err(failed(
                    ErrorCategory::Unknown,
                    "the turn ended with no stop reason",
                )),
            };
        }
    }

    async fn send_prompt(
        &mut self,
        text: String,
        done: Vec<oneshot::Sender<Delivery>>,
        delivery: Delivery,
    ) -> Result<(), DriverStop> {
        let request = PromptRequest::new(self.session_id(), vec![ContentBlock::from(text)]);
        let params = to_json(&request)?;
        let id = self.wire.request("session/prompt", params).await;
        let id = id.map_err(|Closed| self.disconnected_now())?;
        self.turn.prompts.push(id);
        for done in done {
            // Nobody waiting for word of a delivery is fine.
            let _ = done.send(delivery);
        }
        Ok(())
    }

    async fn pump_turn(&mut self) -> Result<(), DriverStop> {
        while !self.turn.prompts.is_empty() {
            match self.next_input().await {
                Input::Frame(incoming) => self.on_incoming(incoming).await?,
                Input::Closed => return Err(self.disconnected().await),
                Input::Control(control) => self.on_control(control).await?,
                Input::ControlClosed => self.stop().await?,
                Input::Stderr(line) => self.on_stderr(line),
                // The agent didn't answer the cancel in time.
                Input::Deadline => self.turn.prompts.clear(),
                Input::ToolWaitOver => self.cancel_after_tool().await?,
            }
        }
        Ok(())
    }

    /// Sends a request outside a turn and waits for its answer, handling whatever else
    /// arrives meanwhile.
    pub(crate) async fn call(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<Result<Value, RpcError>, DriverStop> {
        let id = self.wire.request(method, params).await;
        let id = id.map_err(|Closed| self.disconnected_now())?;
        loop {
            match self.next_input().await {
                Input::Frame(Incoming::Response {
                    id: answered,
                    outcome,
                }) if answered == id => {
                    return Ok(outcome);
                }
                Input::Frame(incoming) => self.on_incoming(incoming).await?,
                Input::Closed => return Err(self.disconnected().await),
                Input::Control(DriverControl::Cancel) | Input::ControlClosed => {
                    return Err(DriverStop::Cancelled);
                }
                Input::Control(control) => self.held.push(control),
                Input::Stderr(line) => self.on_stderr(line),
                // Outside a turn nothing waits on tools.
                Input::ToolWaitOver => {}
                Input::Deadline => {
                    self.deadline = None;
                    return Ok(Err(RpcError {
                        code: 0,
                        message: format!("{method} had no answer in time"),
                        data: None,
                    }));
                }
            }
        }
    }

    /// Like [`Session::call`], with an error answer turned into the session's failure.
    pub(crate) async fn call_or_fail(
        &mut self,
        method: &str,
        request: &impl Serialize,
        fallback: ErrorCategory,
    ) -> Result<Value, DriverStop> {
        let params = to_json(request)?;
        match self.call(method, params).await? {
            Ok(result) => Ok(result),
            Err(error) => Err(self.rules()?.rpc_failure(&error, fallback)),
        }
    }

    async fn next_input(&mut self) -> Input {
        loop {
            let deadline = self.deadline;
            let wake = deadline.unwrap_or_else(Instant::now);
            let tool_wait = self.turn.tool_wait;
            let give_up = tool_wait.unwrap_or_else(Instant::now);
            tokio::select! {
                incoming = self.wire.next() => {
                    return incoming.map_or(Input::Closed, Input::Frame);
                }
                control = self.control.recv(), if self.control_open => match control {
                    Some(control) => return Input::Control(control),
                    None => {
                        self.control_open = false;
                        return Input::ControlClosed;
                    }
                },
                line = self.stderr.recv(), if self.stderr_open => match line {
                    Some(line) => return Input::Stderr(line),
                    None => self.stderr_open = false,
                },
                () = tokio::time::sleep_until(wake), if deadline.is_some() => return Input::Deadline,
                () = tokio::time::sleep_until(give_up), if tool_wait.is_some() => {
                    self.turn.tool_wait = None;
                    self.turn.tool_wait_over = true;
                    return Input::ToolWaitOver;
                }
            }
        }
    }

    async fn on_incoming(&mut self, incoming: Incoming) -> Result<(), DriverStop> {
        match incoming {
            Incoming::Response { id, outcome } => {
                if let Some(index) = self.turn.prompts.iter().position(|prompt| *prompt == id) {
                    self.turn.prompts.remove(index);
                    self.record_answer(outcome);
                }
                Ok(())
            }
            Incoming::Request { id, method, params } => self.on_request(id, &method, params).await,
            Incoming::Notification { method, params } => {
                self.on_notification(&method, params).await
            }
            Incoming::Other(raw) => {
                self.emit(AgentEvent::Unknown { raw });
                Ok(())
            }
        }
    }

    fn record_answer(&mut self, outcome: Result<Value, RpcError>) {
        if let Ok(result) = &outcome
            && self.profile.acp.usage.contains(&UsageSource::EndTurn)
            && let Some(usage) = end_turn_usage(result)
        {
            self.emit(usage);
        }
        if !matches!(self.turn.outcome, Some(Err(_))) {
            self.turn.outcome = Some(outcome);
        }
    }

    async fn on_notification(&mut self, method: &str, params: Value) -> Result<(), DriverStop> {
        if method != "session/update" {
            self.emit(AgentEvent::Unknown {
                raw: json!({ "method": method, "params": params }),
            });
            return Ok(());
        }
        if self.replaying {
            return Ok(());
        }
        let update = params.get("update").cloned().unwrap_or(Value::Null);
        self.check_update(&update).await?;
        for event in self.mapping.update(&update) {
            self.emit(event);
        }
        self.cancel_after_tool().await
    }

    /// Mode and config updates are checked against the role and the model read-back.
    async fn check_update(&mut self, update: &Value) -> Result<(), DriverStop> {
        let violation = match update.get("sessionUpdate").and_then(Value::as_str) {
            Some("current_mode_update") => update
                .get("currentModeId")
                .and_then(Value::as_str)
                .filter(|mode| is_bypass_mode(mode))
                .map(|mode| {
                    failed(
                        ErrorCategory::Permission,
                        format!("the agent switched into {mode}, which skips permission requests"),
                    )
                }),
            Some("config_option_update") => self.model_drift(update),
            _ => None,
        };
        let Some(violation) = violation else {
            return Ok(());
        };
        self.violation.get_or_insert(violation);
        if !self.turn.prompts.is_empty() && !self.turn.cancel_sent {
            self.wire
                .notify(
                    "session/cancel",
                    to_json(&CancelNotification::new(self.session_id()))?,
                )
                .await
                .map_err(|Closed| self.disconnected_now())?;
            self.turn.cancel_sent = true;
        }
        Ok(())
    }

    async fn on_request(
        &mut self,
        id: Value,
        method: &str,
        params: Value,
    ) -> Result<(), DriverStop> {
        if method != "session/request_permission" {
            self.emit(AgentEvent::Unknown {
                raw: json!({ "id": id, "method": method, "params": params }),
            });
            let message = format!("{method} isn't offered by this client");
            let sent = self
                .wire
                .respond_error(id, METHOD_NOT_FOUND, &message)
                .await;
            return sent.map_err(|Closed| self.disconnected_now());
        }
        // A cancelled turn answers every permission request `cancelled`, as ACP requires.
        if self.turn.cancel_sent {
            let answer = RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled);
            let sent = self.wire.respond(id, to_json(&answer)?).await;
            return sent.map_err(|Closed| self.disconnected_now());
        }
        let call = params.get("toolCall").unwrap_or(&Value::Null);
        let mut verdict = self.policy.judge(call);
        // A direct turn that asks you: what the agent asks about and the policy allows is
        // yours to answer, as it would be in the agent's own window. So is a folder outside
        // the project, which the policy alone never allows.
        if let Some(target) = self.start.asking.clone() {
            let outside = crate::permission::outside_folder(call)
                .is_some_and(|folder| self.policy.may_ask_outside(&folder));
            if verdict == Verdict::Allow || outside {
                verdict = crate::permission::ask_you(&target, call).await;
            }
        }
        let title = call
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("a tool call")
            .to_owned();
        let options = params.get("options").unwrap_or(&Value::Null);
        let (sent, rejection) = match crate::permission::answer(options, verdict) {
            Answer::Pick {
                option_id,
                rejection,
            } => {
                let outcome = SelectedPermissionOutcome::new(option_id);
                let answer =
                    RequestPermissionResponse::new(RequestPermissionOutcome::Selected(outcome));
                (self.wire.respond(id, to_json(&answer)?).await, rejection)
            }
            Answer::Refuse(reason) => {
                let message = format!("rejected: {reason}");
                let sent = self.wire.respond_error(id, INTERNAL_ERROR, &message).await;
                (sent, Some(reason))
            }
        };
        sent.map_err(|Closed| self.disconnected_now())?;
        let Some(reason) = rejection else {
            return Ok(());
        };
        self.emit(AgentEvent::Error {
            category: ErrorCategory::Permission,
            message: format!("Rejected {title}: {reason}"),
            retryable: false,
        });
        if let Some(id) = call.get("toolCallId").and_then(Value::as_str) {
            self.mapping.reject(id);
        }
        self.cancel_after_tool().await
    }

    async fn on_control(&mut self, control: DriverControl) -> Result<(), DriverStop> {
        match control {
            DriverControl::Deliver {
                text,
                now: false,
                done,
            } => self.queued.push_back(Queued { text, done }),
            // A stopping turn takes no more messages; dropping `done` says it never went out.
            DriverControl::Deliver { .. }
                if self
                    .turn
                    .cancel
                    .is_some_and(|cause| cause > CancelCause::Steer) => {}
            DriverControl::Deliver {
                text,
                now: true,
                done,
            } => match self.profile.acp.steer {
                SteerRoute::Input => self.send_prompt(text, vec![done], Delivery::Steer).await?,
                SteerRoute::CancelAndPrompt => {
                    self.turn.steers.push(Queued { text, done });
                    self.cancel_after_tool().await?;
                }
            },
            DriverControl::StopAfterTool => {
                self.turn.stop_after_tool = true;
                self.cancel_after_tool().await?;
            }
            DriverControl::Cancel => self.stop().await?,
        }
        Ok(())
    }

    /// Cancels once no tool call is running, or the wait for them is over, for a stop after
    /// the tool or a steer that has to cancel first.
    async fn cancel_after_tool(&mut self) -> Result<(), DriverStop> {
        if self.turn.prompts.is_empty() {
            return Ok(());
        }
        let cause = if self.turn.stop_after_tool {
            CancelCause::StopAfterTool
        } else if !self.turn.steers.is_empty() {
            CancelCause::Steer
        } else {
            return Ok(());
        };
        if self.mapping.tools_running() && !self.turn.tool_wait_over {
            let tool_wait = self.profile.tool_wait;
            self.turn
                .tool_wait
                .get_or_insert_with(|| Instant::now() + tool_wait);
            return Ok(());
        }
        self.turn.tool_wait = None;
        self.cancel(cause).await
    }

    async fn stop(&mut self) -> Result<(), DriverStop> {
        self.cancel(CancelCause::Stop).await?;
        self.deadline
            .get_or_insert_with(|| Instant::now() + CANCEL_GRACE);
        Ok(())
    }

    async fn cancel(&mut self, cause: CancelCause) -> Result<(), DriverStop> {
        self.turn.cancel = self.turn.cancel.max(Some(cause));
        if self.turn.cancel_sent {
            return Ok(());
        }
        let notice = to_json(&CancelNotification::new(self.session_id()))?;
        self.wire
            .notify("session/cancel", notice)
            .await
            .map_err(|Closed| self.disconnected_now())?;
        self.turn.cancel_sent = true;
        Ok(())
    }

    /// `session/close` after a stop after the tool, where the agent offers it. The session ends
    /// either way, so a refusal or silence changes nothing.
    async fn close_session(&mut self) {
        if !self.offers.close {
            return;
        }
        let Ok(params) = to_json(&CloseSessionRequest::new(self.session_id())) else {
            return;
        };
        self.deadline = Some(Instant::now() + CANCEL_GRACE);
        let _ = self.call("session/close", params).await;
        self.deadline = None;
    }

    fn on_stderr(&mut self, line: String) {
        let Ok(rules) = &self.profile.errors else {
            return;
        };
        if let Some((category, retryable)) = rules.classify(&line) {
            self.emit(AgentEvent::Error {
                category,
                message: line.clone(),
                retryable,
            });
            self.stderr_error = Some((category, line));
        }
    }

    /// The end for closed pipes, after the stderr lines still on their way.
    async fn disconnected(&mut self) -> DriverStop {
        let settle = Instant::now() + STDERR_SETTLE;
        while self.stderr_open {
            match tokio::time::timeout_at(settle, self.stderr.recv()).await {
                Ok(Some(line)) => self.on_stderr(line),
                Ok(None) => self.stderr_open = false,
                Err(_) => break,
            }
        }
        self.disconnected_now()
    }

    fn disconnected_now(&self) -> DriverStop {
        match &self.stderr_error {
            Some((category, message)) => failed(*category, message.clone()),
            None => DriverStop::Disconnected,
        }
    }

    pub(crate) fn rules(&self) -> Result<&ErrorRules, DriverStop> {
        self.profile
            .errors
            .as_ref()
            .map_err(|reason| failed(ErrorCategory::Cli, reason.clone()))
    }

    pub(crate) fn session_id(&self) -> String {
        self.session_id.clone().unwrap_or_default()
    }

    pub(crate) fn emit(&self, event: AgentEvent) {
        // The events channel is unbounded, and a closed one means the worker is gone.
        let _ = self.events.send(event);
    }
}

/// The agent's mode for an access. Sessions with no tools use the read-only mode.
fn mode_for(profile: &AcpProfile, access: Access) -> Option<String> {
    match access {
        Access::NoTools | Access::ReadOnly => profile.modes.read_only.clone(),
        Access::Build | Access::Direct => profile.modes.build.clone(),
        Access::Experiment => profile.modes.experiment.clone(),
    }
}

pub(crate) fn to_json(value: &impl Serialize) -> Result<Value, DriverStop> {
    serde_json::to_value(value).map_err(|err| {
        failed(
            ErrorCategory::Cli,
            format!("couldn't encode a request: {err}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::testing;

    fn tool(id: &str, status: &str) -> Value {
        json!({ "sessionUpdate": "tool_call", "toolCallId": id, "title": "Run tests", "kind": "execute", "status": status })
    }

    fn started(id: &'static str) -> impl Fn(&AgentEvent) -> bool {
        move |event| matches!(event, AgentEvent::ToolStarted { id: started, .. } if started == id)
    }

    fn done_tool(id: &str) -> Value {
        json!({ "sessionUpdate": "tool_call_update", "toolCallId": id, "status": "completed" })
    }

    #[tokio::test]
    async fn acp_steer_and_stop_after_tool() {
        // `input` steering: a second prompt during the turn, which the agent folds in.
        let input = AcpProfile {
            steer: SteerRoute::Input,
            ..AcpProfile::default()
        };
        let mut driven = testing::launch(testing::profile(input), testing::start("/w"));
        driven.agent.open(json!({}), json!({})).await;
        let first = driven.agent.expect("session/prompt").await;
        let delivered = driven.deliver("also check the docs", true).await;
        let steer = driven.agent.expect("session/prompt").await;
        assert_eq!(steer["params"]["prompt"][0]["text"], "also check the docs");
        assert_eq!(delivered.await.unwrap(), Delivery::Steer);
        driven
            .agent
            .reply(&first, json!({ "stopReason": "end_turn" }))
            .await;
        driven
            .agent
            .reply(&steer, json!({ "stopReason": "end_turn" }))
            .await;
        assert_eq!(driven.finish().await.0.stop, DriverStop::EndTurn);

        // `cancel-and-prompt`: wait for the running tool, cancel, then prompt with the message.
        let mut driven = testing::launch(
            testing::profile(AcpProfile::default()),
            testing::start("/w"),
        );
        driven.agent.open(json!({}), json!({})).await;
        let first = driven.agent.expect("session/prompt").await;
        driven.agent.update(tool("t1", "in_progress")).await;
        driven.wait_for(started("t1")).await;
        let delivered = driven.deliver("use the new API", true).await;
        let next_turn = driven.deliver("then update the changelog", false).await;
        driven.agent.update(done_tool("t1")).await;
        driven.agent.expect("session/cancel").await;
        driven
            .agent
            .reply(&first, json!({ "stopReason": "cancelled" }))
            .await;
        let steer = driven.agent.expect("session/prompt").await;
        assert_eq!(steer["params"]["prompt"][0]["text"], "use the new API");
        assert_eq!(delivered.await.unwrap(), Delivery::Steer);
        driven
            .agent
            .reply(&steer, json!({ "stopReason": "end_turn" }))
            .await;
        let queued = driven.agent.expect("session/prompt").await;
        assert_eq!(
            queued["params"]["prompt"][0]["text"],
            "then update the changelog"
        );
        assert_eq!(next_turn.await.unwrap(), Delivery::NextTurn);
        driven
            .agent
            .reply(&queued, json!({ "stopReason": "end_turn" }))
            .await;
        assert_eq!(driven.finish().await.0.stop, DriverStop::EndTurn);

        // Stop after the tool: nothing goes out until the tool completes, then cancel and close.
        let mut driven = testing::launch(
            testing::profile(AcpProfile::default()),
            testing::start("/w"),
        );
        let offers_close = json!({ "protocolVersion": 1, "agentCapabilities": { "sessionCapabilities": { "close": {} } } });
        driven.agent.open(offers_close, json!({})).await;
        let prompt = driven.agent.expect("session/prompt").await;
        driven.agent.update(tool("t1", "in_progress")).await;
        driven.wait_for(started("t1")).await;
        driven
            .control
            .send(DriverControl::StopAfterTool)
            .await
            .unwrap();
        driven.agent.update(tool("t2", "pending")).await;
        driven.agent.update(done_tool("t1")).await;
        driven.agent.silent_for(Duration::from_millis(100)).await;
        driven
            .agent
            .update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t2", "status": "failed" }))
            .await;
        driven.agent.expect("session/cancel").await;
        // A permission request after the cancel is answered `cancelled`.
        driven
            .agent
            .ask_permission(
                "p1",
                json!({ "toolCallId": "t3", "title": "Edit", "kind": "edit" }),
            )
            .await;
        let answer = driven.agent.recv().await;
        assert_eq!(answer["id"], "p1");
        assert_eq!(answer["result"]["outcome"]["outcome"], "cancelled");
        driven
            .agent
            .reply(&prompt, json!({ "stopReason": "cancelled" }))
            .await;
        let close = driven.agent.expect("session/close").await;
        driven.agent.reply(&close, json!({})).await;
        let (end, _) = driven.finish().await;
        assert_eq!(end.stop, DriverStop::Cancelled);
        assert_eq!(end.cli_session.as_deref(), Some(testing::SESSION));

        // Cancel stops at once, and a turn that never answers ends after the grace.
        let mut driven = testing::launch(
            testing::profile(AcpProfile::default()),
            testing::start("/w"),
        );
        driven.agent.open(json!({}), json!({})).await;
        driven.agent.expect("session/prompt").await;
        driven.agent.update(tool("t1", "in_progress")).await;
        let dropped = driven.deliver("too late", false).await;
        driven.control.send(DriverControl::Cancel).await.unwrap();
        driven.agent.expect("session/cancel").await;
        let (end, _) = driven.finish().await;
        assert_eq!(end.stop, DriverStop::Cancelled);
        assert!(
            dropped.await.is_err(),
            "a message that never went out drops its sender"
        );

        // Dropping every control sender stops the session too.
        let mut driven = testing::launch(
            testing::profile(AcpProfile::default()),
            testing::start("/w"),
        );
        driven.agent.open(json!({}), json!({})).await;
        let prompt = driven.agent.expect("session/prompt").await;
        let testing::Driven {
            agent,
            control,
            task,
            events,
            ..
        } = driven;
        drop(control);
        let mut agent = agent;
        agent.expect("session/cancel").await;
        agent
            .reply(&prompt, json!({ "stopReason": "cancelled" }))
            .await;
        let (end, _) = testing::finish(task, events).await;
        assert_eq!(end.stop, DriverStop::Cancelled);
    }

    fn tool_failed(id: &'static str) -> impl Fn(&AgentEvent) -> bool {
        move |event| matches!(event, AgentEvent::ToolFinished { id: done, ok: false, .. } if done == id)
    }

    /// A direct turn that asks you: what the policy allows goes to the app, whose answer picks
    /// the option, as OpenCode's own window would have you pick it (checked live).
    #[tokio::test]
    async fn a_direct_turn_asks_you_what_the_agent_asks() {
        use argonauts_core::asking::{AskLine, AskReply, AskTarget};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let app = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = AskTarget {
            endpoint: app.local_addr().unwrap().to_string(),
            token: "t".into(),
        };
        let answers = tokio::spawn(async move {
            let mut asked = Vec::new();
            for reply in [
                AskReply::allow(&json!({})),
                AskReply::deny("not now"),
                AskReply::deny("not there"),
            ] {
                let (stream, _) = app.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut line = String::new();
                BufReader::new(read).read_line(&mut line).await.unwrap();
                asked.push(serde_json::from_str::<AskLine>(&line).unwrap());
                let text = format!("{}\n", serde_json::to_string(&reply).unwrap());
                write.write_all(text.as_bytes()).await.unwrap();
            }
            asked
        });
        let mut start = testing::start("/w");
        start.access = Access::Direct;
        start.scope.writable = vec!["/w".into()];
        start.scope.roots = vec!["/w".into()];
        start.asking = Some(target);
        let mut driven = testing::launch(testing::profile(AcpProfile::default()), start);
        driven.agent.open(json!({}), json!({})).await;
        let _prompt = driven.agent.expect("session/prompt").await;
        let edit = json!({
            "toolCallId": "t1", "title": "hello.txt", "kind": "edit",
            "locations": [{ "path": "/w/hello.txt" }],
            "rawInput": { "filepath": "/w/hello.txt", "diff": "--- a\n+++ b\n-hello\n+hi\n" },
        });
        driven.agent.ask_permission("p1", edit).await;
        let answer = driven.agent.recv().await;
        assert_eq!(answer["result"]["outcome"]["optionId"], "yes");
        let bash = json!({
            "toolCallId": "t2", "title": "echo hi", "kind": "execute",
            "rawInput": { "command": "echo hi" },
        });
        driven.agent.ask_permission("p2", bash).await;
        let answer = driven.agent.recv().await;
        assert_eq!(answer["result"]["outcome"]["optionId"], "no");
        // A folder outside the project, as OpenCode asks for it (checked live): yours alone.
        let outside = json!({
            "toolCallId": "t3", "title": "/", "kind": "other",
            "rawInput": { "filepath": "/", "parentDir": "/" },
        });
        driven.agent.ask_permission("p3", outside).await;
        let answer = driven.agent.recv().await;
        assert_eq!(answer["result"]["outcome"]["optionId"], "no");
        let asked = answers.await.unwrap();
        let tools: Vec<String> = asked
            .iter()
            .map(|line| match &line.ask {
                argonauts_core::asking::Ask::Permission { tool_name, .. } => tool_name.clone(),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(tools, ["Edit", "Bash", "ExternalDirectory"]);
    }

    #[tokio::test]
    async fn a_rejected_tool_doesnt_hold_up_a_stop_after_tool() {
        let mut driven = testing::launch(
            testing::profile(AcpProfile::default()),
            testing::start("/w"),
        );
        driven.agent.open(json!({}), json!({})).await;
        let prompt = driven.agent.expect("session/prompt").await;
        // An edit that names no path is rejected, and the agent never finishes the tool.
        let edit = json!({ "toolCallId": "t1", "title": "Edit", "kind": "edit" });
        driven.agent.update(tool("t1", "pending")).await;
        driven.agent.ask_permission("p1", edit).await;
        let answer = driven.agent.recv().await;
        assert_eq!(answer["result"]["outcome"]["optionId"], "no");
        driven
            .wait_for(|event| {
                matches!(
                    event,
                    AgentEvent::Error {
                        category: ErrorCategory::Permission,
                        ..
                    }
                )
            })
            .await;
        driven
            .control
            .send(DriverControl::StopAfterTool)
            .await
            .unwrap();
        driven.agent.expect("session/cancel").await;
        driven
            .agent
            .reply(&prompt, json!({ "stopReason": "cancelled" }))
            .await;
        let (end, events) = driven.finish().await;
        assert_eq!(end.stop, DriverStop::Cancelled);
        assert!(events.iter().any(tool_failed("t1")), "{events:?}");
    }

    #[tokio::test]
    async fn a_tool_that_never_ends_stops_blocking_after_the_wait() {
        let mut profile = testing::profile(AcpProfile::default());
        profile.tool_wait = Duration::from_millis(300);
        let mut driven = testing::launch(profile, testing::start("/w"));
        driven.agent.open(json!({}), json!({})).await;
        let prompt = driven.agent.expect("session/prompt").await;
        driven.agent.update(tool("t1", "in_progress")).await;
        driven.wait_for(started("t1")).await;
        driven
            .control
            .send(DriverControl::StopAfterTool)
            .await
            .unwrap();
        driven.agent.silent_for(Duration::from_millis(100)).await;
        driven.agent.expect("session/cancel").await;
        driven
            .agent
            .reply(&prompt, json!({ "stopReason": "cancelled" }))
            .await;
        let (end, events) = driven.finish().await;
        assert_eq!(end.stop, DriverStop::Cancelled);
        // The turn's end closes the tool no update finished.
        assert!(events.iter().any(tool_failed("t1")), "{events:?}");
    }

    #[tokio::test]
    async fn a_turn_left_with_an_open_tool_doesnt_block_the_next_steer() {
        let mut driven = testing::launch(
            testing::profile(AcpProfile::default()),
            testing::start("/w"),
        );
        driven.agent.open(json!({}), json!({})).await;
        let first = driven.agent.expect("session/prompt").await;
        driven.agent.update(tool("t1", "in_progress")).await;
        driven.wait_for(started("t1")).await;
        let next_turn = driven.deliver("now the docs", false).await;
        // Lets the driver take the message before the turn ends.
        driven.agent.silent_for(Duration::from_millis(100)).await;
        driven
            .agent
            .reply(&first, json!({ "stopReason": "end_turn" }))
            .await;
        driven.wait_for(tool_failed("t1")).await;
        let second = driven.agent.expect("session/prompt").await;
        assert_eq!(next_turn.await.unwrap(), Delivery::NextTurn);
        // A cancel-and-prompt steer cancels at once, since no tool is open.
        let steer = driven.deliver("use the new API", true).await;
        driven.agent.expect("session/cancel").await;
        driven
            .agent
            .reply(&second, json!({ "stopReason": "cancelled" }))
            .await;
        let third = driven.agent.expect("session/prompt").await;
        assert_eq!(third["params"]["prompt"][0]["text"], "use the new API");
        assert_eq!(steer.await.unwrap(), Delivery::Steer);
        driven
            .agent
            .reply(&third, json!({ "stopReason": "end_turn" }))
            .await;
        assert_eq!(driven.finish().await.0.stop, DriverStop::EndTurn);
    }

    #[tokio::test]
    async fn a_bypass_mode_switch_fails_the_session() {
        let mut driven = testing::launch(
            testing::profile(AcpProfile::default()),
            testing::start("/w"),
        );
        driven.agent.open(json!({}), json!({})).await;
        let prompt = driven.agent.expect("session/prompt").await;
        driven
            .agent
            .update(json!({ "sessionUpdate": "current_mode_update", "currentModeId": "bypassPermissions" }))
            .await;
        driven.agent.expect("session/cancel").await;
        driven
            .agent
            .reply(&prompt, json!({ "stopReason": "cancelled" }))
            .await;
        let (end, events) = driven.finish().await;
        assert!(
            matches!(&end.stop, DriverStop::Failed { category: ErrorCategory::Permission, message } if message.contains("bypassPermissions")),
            "{end:?}"
        );
        assert!(matches!(
            events.last(),
            Some(AgentEvent::Error {
                category: ErrorCategory::Permission,
                ..
            })
        ));
    }
}
