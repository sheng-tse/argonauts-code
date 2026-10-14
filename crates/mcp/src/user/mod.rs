//! `argonauts mcp serve`: start, watch, pause, and guide runs, and ask what's waiting. There is
//! no approve tool, no approval-mode tool, and no take-over or rule-override tool, because the
//! server holds only a `ConductorApi`.

pub mod caps;
pub mod results;
pub mod tools;

use std::sync::Arc;

use anyhow::Context as _;
use argonauts_core::api::ConductorApi;
use argonauts_core::paths::argonauts_home;
use argonauts_core::settings::Span;
use argonauts_core::settings::agents::{McpSettings, Share, StartRuns};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use time::OffsetDateTime;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::OutsideAgentOptions;
use caps::OutsideAgentRuns;

pub struct UserServer {
    api: Arc<dyn ConductorApi>,
    options: OutsideAgentOptions,
    runs: Arc<dyn OutsideAgentRuns>,
}

impl UserServer {
    /// `runs` counts the runs outside agents start each day, the daily ledger outside tests.
    pub fn new(
        api: Arc<dyn ConductorApi>,
        options: OutsideAgentOptions,
        runs: Arc<dyn OutsideAgentRuns>,
    ) -> Self {
        Self { api, options, runs }
    }

    pub fn api(&self) -> &Arc<dyn ConductorApi> {
        &self.api
    }

    /// The options the server started with. Calls use [`UserServer::current_options`].
    pub fn start_options(&self) -> &OutsideAgentOptions {
        &self.options
    }

    /// The options for one call: the stricter of the start values and what the reloader reads
    /// now. A reload that fails keeps the start values. The reloader may read files, so it runs
    /// off the async threads.
    pub async fn current_options(&self) -> OutsideAgentOptions {
        let start = &self.options;
        let Some(reload) = start.reload.clone() else {
            return start.clone();
        };
        let Some(now) = tokio::task::spawn_blocking(move || reload())
            .await
            .ok()
            .flatten()
        else {
            return start.clone();
        };
        OutsideAgentOptions {
            mcp: stricter(&start.mcp, &now.mcp),
            repo_trusted: start.repo_trusted && now.repo_trusted,
            reload: None,
        }
    }

    /// Takes one of today's outside-agent runs under `cap`, or says why not. The ledger locks a
    /// file, so this runs off the async threads.
    pub async fn take_outside_agent_run(&self, cap: u32) -> Result<TakenRun, String> {
        let runs = Arc::clone(&self.runs);
        let taken = OffsetDateTime::now_utc();
        let today = tokio::task::spawn_blocking(move || runs.take(cap, taken))
            .await
            .map_err(|err| format!("counting today's runs failed: {err}"))??;
        Ok(TakenRun { today, taken })
    }

    /// Gives back a run whose start failed, so it doesn't count against today's cap.
    pub async fn give_back_outside_agent_run(&self, run: TakenRun) -> Result<(), String> {
        let runs = Arc::clone(&self.runs);
        tokio::task::spawn_blocking(move || runs.give_back(run.taken))
            .await
            .map_err(|err| format!("giving back today's run failed: {err}"))?
    }
}

/// An outside-agent run taken from today's count.
pub struct TakenRun {
    /// Today's count, this run included.
    pub today: u32,
    taken: OffsetDateTime,
}

/// The tighter side of each key: anything either turns off stays off, the smaller daily cap,
/// the shorter run limit, and status-only sharing if either asks for it.
fn stricter(start: &McpSettings, now: &McpSettings) -> McpSettings {
    let start_runs = if start.start_runs == StartRuns::Off || now.start_runs == StartRuns::Off {
        StartRuns::Off
    } else {
        StartRuns::TrustedRepos
    };
    let share = if start.share == Share::StatusOnly || now.share == Share::StatusOnly {
        Share::StatusOnly
    } else {
        Share::Summaries
    };
    McpSettings {
        status_tool: start.status_tool && now.status_tool,
        start_runs,
        guidance: start.guidance && now.guidance,
        pause: start.pause && now.pause,
        share,
        run_limit: shorter(&start.run_limit, &now.run_limit),
        runs_per_day: start.runs_per_day.min(now.runs_per_day),
    }
}

/// `"none"` means no limit, so any duration is shorter. A span that doesn't parse never wins.
fn shorter(start: &Span, now: &Span) -> Span {
    let now_is_shorter = match (start.parse(), now.parse()) {
        (Ok(Some(start)), Ok(Some(now))) => now < start,
        (Ok(None), Ok(Some(_))) => true,
        _ => false,
    };
    if now_is_shorter { now } else { start }.clone()
}

/// Serves until the client closes the stream. Refuses to start in a repo you haven't trusted.
pub async fn serve_on<R, W>(
    api: Arc<dyn ConductorApi>,
    options: OutsideAgentOptions,
    read: R,
    write: W,
) -> anyhow::Result<()>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    caps::check_can_serve(&options)?;
    let home = argonauts_home().context("argonauts mcp serve needs a home directory")?;
    let server = UserServer::new(api, options, caps::daily_ledger(&home));
    serve_server(server, read, write).await
}

/// [`serve_on`] for a server already built, such as one with its own daily count.
pub async fn serve_server<R, W>(server: UserServer, read: R, write: W) -> anyhow::Result<()>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    caps::check_can_serve(server.start_options())?;
    server.serve((read, write)).await?.waiting().await?;
    Ok(())
}

impl ServerHandler for UserServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("argonauts", env!("CARGO_PKG_VERSION")))
            .with_instructions(tools::INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let options = self.current_options().await;
        if !options.repo_trusted {
            return Err(ErrorData::invalid_request(tools::NOT_TRUSTED, None));
        }
        Ok(crate::tool_list(tools::definitions(&options)))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let client = tools::client_name(&context);
        let arguments = request.arguments.unwrap_or_default();
        Ok(tools::call(self, &request.name, arguments, &client)
            .await
            .into())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use argonauts_core::api::{
        ApiError, DiffScope, DiffView, EventStream, RawView, RunHandle, RunState, RunSummary,
        StartRun, StatusSummary, SteerAck, TaskBrief,
    };
    use argonauts_core::events::approval::{
        ApprovalEvent, ApprovalKind, ApprovalOpened, ApprovalPacket, FreeText,
    };
    use argonauts_core::events::run::{Party, RunLifecycle, RunMode};
    use argonauts_core::events::session::QueuedMessage;
    use argonauts_core::events::task::TaskState;
    use argonauts_core::events::{RunEvent, RunEventKind};
    use argonauts_core::future::BoxFuture;
    use argonauts_core::input::MentionTarget;
    use argonauts_core::names::{ApprovalId, MessageId, ModelRef, SessionKey};
    use argonauts_core::settings::agents::{McpSettings, Share, StartRuns};
    use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
    use argonauts_core::{RunId, Spend, TaskId};
    use rmcp::model::{ClientCapabilities, ClientConfig, ProtocolVersion};
    use rmcp::{ClientLifecycleMode, ClientServiceExt};
    use serde_json::{Value, json};
    use time::OffsetDateTime;
    use tokio::io::{
        AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf,
        WriteHalf,
    };
    use tokio::sync::{mpsc, watch};

    use super::results::AGENT_TEXT_MAX;
    use super::tools::{
        CLIENT_NAME_MAX, LIST_RUNS, MAX_EVENTS, PAUSE_RUN, REQUEST_DECISION, RUN_EVENTS,
        RUN_STATUS, SEND_GUIDANCE, START_RUN,
    };
    use super::*;
    use crate::OutsideAgentSettings;

    const RUN: &str = "run-20260929-041503-3fa2";
    const EVIDENCE: &str = "3fa2c1d0e9ab77aa00112233445566778899aabbccddeeff0011223344556677";
    /// Agent text that may leave only in `agent_text` fields.
    const AGENT_ONLY: &[&str] = &["TITLE-TEXT", "GOAL-TEXT", "TASK-TITLE", "APPROVAL-TITLE"];
    /// Text that never leaves, not even as agent text, besides the secret.
    const NEVER_SENT: &[&str] = &[
        "PLAN-TEXT",
        "NOTE-TEXT",
        "RECENT-TEXT",
        "WAITING-TEXT",
        "WITHDRAW-TEXT",
        "STOPPED-TEXT",
        "PACKET-TEXT",
        "WORKFLOW-TEXT",
    ];

    // Built at runtime so no string in this file looks like a real credential.
    fn secret() -> String {
        format!("sk-ant-{}", "A".repeat(28))
    }

    fn never_sent() -> Vec<String> {
        let mut markers: Vec<String> = NEVER_SENT.iter().map(|&marker| marker.to_owned()).collect();
        markers.push(secret());
        markers
    }

    #[derive(Debug, Clone, PartialEq)]
    enum Call {
        Start(Box<StartRun>),
        Guidance {
            run: RunId,
            text: String,
            client: Option<String>,
        },
        Pause {
            run: RunId,
            now: bool,
            client: Option<String>,
        },
        Read(&'static str),
        Other(&'static str),
    }

    /// Argonauts Code that records every call and serves one run.
    #[derive(Default)]
    struct FakeApi {
        calls: Mutex<Vec<Call>>,
        runs: Vec<RunSummary>,
        status: StatusSummary,
        events: Vec<RunEvent>,
        /// Sent this long after a subscribe, once the history is out.
        live: Option<(Duration, RunEvent)>,
        /// Why every start is refused, when it is.
        start_refusal: Option<&'static str>,
    }

    impl FakeApi {
        fn record(&self, call: Call) {
            self.calls.lock().expect("calls").push(call);
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().expect("calls").clone()
        }

        fn writes(&self) -> Vec<Call> {
            self.calls()
                .into_iter()
                .filter(|call| !matches!(call, Call::Read(_)))
                .collect()
        }

        fn starts(&self) -> Vec<StartRun> {
            self.calls()
                .into_iter()
                .filter_map(|call| match call {
                    Call::Start(start) => Some(*start),
                    _ => None,
                })
                .collect()
        }

        fn known(&self, run: &RunId) -> Result<(), ApiError> {
            if run.as_str() == RUN {
                Ok(())
            } else {
                Err(ApiError::NoRun(run.clone()))
            }
        }

        fn refuse<T: Send + 'static>(
            &self,
            name: &'static str,
        ) -> BoxFuture<'_, Result<T, ApiError>> {
            self.record(Call::Other(name));
            Box::pin(async move { Err(ApiError::Refused(format!("{name} isn't in this fake"))) })
        }
    }

    impl ConductorApi for FakeApi {
        fn start(&self, request: StartRun) -> BoxFuture<'_, Result<RunHandle, ApiError>> {
            self.record(Call::Start(Box::new(request)));
            let refusal = self.start_refusal;
            Box::pin(async move {
                if let Some(reason) = refusal {
                    return Err(ApiError::Refused(reason.to_owned()));
                }
                Ok(RunHandle {
                    run: run_id(),
                    events: "events.jsonl".into(),
                })
            })
        }

        fn subscribe(
            &self,
            run: &RunId,
            from_seq: u64,
        ) -> BoxFuture<'_, Result<EventStream, ApiError>> {
            self.record(Call::Read("subscribe"));
            let known = self.known(run);
            let history: Vec<RunEvent> = self
                .events
                .iter()
                .filter(|event| event.seq >= from_seq)
                .cloned()
                .collect();
            let replay_until = self.events.last().map(|event| event.seq);
            let live = self.live.clone();
            Box::pin(async move {
                known?;
                let (sender, events) = mpsc::channel(history.len() + 2);
                for event in history {
                    sender.try_send(event).expect("room for history");
                }
                let (live_sender, live_flag) = watch::channel(live.is_some());
                if let Some((after, event)) = live {
                    tokio::spawn(async move {
                        tokio::time::sleep(after).await;
                        let _sent = sender.send(event).await;
                        drop(live_sender);
                    });
                }
                Ok(EventStream {
                    events,
                    replay_until,
                    live: live_flag,
                })
            })
        }

        fn steer(
            &self,
            _run: &RunId,
            _session: &SessionKey,
            _text: String,
            _now: bool,
        ) -> BoxFuture<'_, Result<SteerAck, ApiError>> {
            self.refuse("steer")
        }

        fn mention(
            &self,
            _run: &RunId,
            _target: MentionTarget,
            _text: String,
        ) -> BoxFuture<'_, Result<MessageId, ApiError>> {
            self.refuse("mention")
        }

        fn guidance(
            &self,
            run: &RunId,
            text: String,
        ) -> BoxFuture<'_, Result<MessageId, ApiError>> {
            self.record(Call::Guidance {
                run: run.clone(),
                text,
                client: None,
            });
            Box::pin(async move { Ok(MessageId::numbered(1)) })
        }

        fn guidance_from_outside_agent(
            &self,
            run: &RunId,
            text: String,
            client: String,
        ) -> BoxFuture<'_, Result<MessageId, ApiError>> {
            self.record(Call::Guidance {
                run: run.clone(),
                text,
                client: Some(client),
            });
            let known = self.known(run);
            Box::pin(async move { known.map(|()| MessageId::numbered(7)) })
        }

        fn take_back(
            &self,
            _run: &RunId,
            _messages: Vec<MessageId>,
        ) -> BoxFuture<'_, Result<Vec<QueuedMessage>, ApiError>> {
            self.refuse("take_back")
        }

        fn pause(&self, run: &RunId, now: bool) -> BoxFuture<'_, Result<(), ApiError>> {
            self.record(Call::Pause {
                run: run.clone(),
                now,
                client: None,
            });
            Box::pin(async move { Ok(()) })
        }

        fn pause_from_outside_agent(
            &self,
            run: &RunId,
            now: bool,
            client: String,
        ) -> BoxFuture<'_, Result<(), ApiError>> {
            self.record(Call::Pause {
                run: run.clone(),
                now,
                client: Some(client),
            });
            let known = self.known(run);
            Box::pin(async move { known })
        }

        fn cancel_pause(&self, _run: &RunId) -> BoxFuture<'_, Result<(), ApiError>> {
            self.refuse("cancel_pause")
        }

        fn resume(&self, _run: &RunId) -> BoxFuture<'_, Result<(), ApiError>> {
            self.refuse("resume")
        }

        fn stop(&self, _run: &RunId) -> BoxFuture<'_, Result<(), ApiError>> {
            self.refuse("stop")
        }

        fn stop_session(
            &self,
            _run: &RunId,
            _session: &SessionKey,
        ) -> BoxFuture<'_, Result<(), ApiError>> {
            self.refuse("stop_session")
        }

        fn restart_session(
            &self,
            _run: &RunId,
            _session: &SessionKey,
        ) -> BoxFuture<'_, Result<(), ApiError>> {
            self.refuse("restart_session")
        }

        fn reassign(
            &self,
            _run: &RunId,
            _task: &TaskId,
            _to: ModelRef,
        ) -> BoxFuture<'_, Result<(), ApiError>> {
            self.refuse("reassign")
        }

        fn list(&self) -> BoxFuture<'_, Result<Vec<RunSummary>, ApiError>> {
            self.record(Call::Read("list"));
            let runs = self.runs.clone();
            Box::pin(async move { Ok(runs) })
        }

        fn continue_run(
            &self,
            _merged: &RunId,
            _request: StartRun,
        ) -> BoxFuture<'_, Result<RunHandle, ApiError>> {
            self.refuse("continue_run")
        }

        fn status(&self, run: &RunId) -> BoxFuture<'_, Result<StatusSummary, ApiError>> {
            self.record(Call::Read("status"));
            let answer = self.known(run).map(|()| self.status.clone());
            Box::pin(async move { answer })
        }

        fn diff(
            &self,
            _run: &RunId,
            _task: &TaskId,
            _scope: DiffScope,
        ) -> BoxFuture<'_, Result<DiffView, ApiError>> {
            self.refuse("diff")
        }

        fn raw(
            &self,
            _run: &RunId,
            _session: &SessionKey,
        ) -> BoxFuture<'_, Result<RawView, ApiError>> {
            self.refuse("raw")
        }
    }

    /// Today's outside-agent runs, counted in memory instead of `~/.argonauts/daily.json`.
    #[derive(Default)]
    struct FakeRuns {
        taken: AtomicU32,
    }

    impl OutsideAgentRuns for FakeRuns {
        fn take(&self, cap: u32, _now: OffsetDateTime) -> Result<u32, String> {
            let taken = self.taken.load(Ordering::SeqCst);
            if taken >= cap {
                return Err(format!("today's {cap} runs for outside agents are used up"));
            }
            self.taken.store(taken + 1, Ordering::SeqCst);
            Ok(taken + 1)
        }

        fn give_back(&self, _taken: OffsetDateTime) -> Result<(), String> {
            let taken = self.taken.load(Ordering::SeqCst);
            self.taken.store(taken.saturating_sub(1), Ordering::SeqCst);
            Ok(())
        }
    }

    fn run_id() -> RunId {
        RunId::new(RUN).expect("run id")
    }

    fn summary() -> RunSummary {
        RunSummary {
            run: run_id(),
            title: format!(
                "\x1b[31mTITLE-TEXT\x1b[0m {} {}",
                secret(),
                "long ".repeat(200)
            ),
            mode: RunMode::Crew,
            workflow: "WORKFLOW-TEXT".to_owned(),
            state: RunState::Waiting,
            stopped_at: "STOPPED-TEXT".to_owned(),
            updated: OffsetDateTime::UNIX_EPOCH,
            spent: Spend {
                cost_usd: 1.25,
                input_tokens: 1_000,
                output_tokens: 200,
                ..Spend::default()
            },
            sessions_to_resume: 0,
            waiting: vec![ApprovalId::numbered(1)],
            live: false,
            remind: false,
            name: None,
        }
    }

    fn status() -> StatusSummary {
        StatusSummary {
            run: Some(run_id()),
            goal: vec![format!("GOAL-TEXT {}", secret()), "second line".to_owned()],
            plan: Some("PLAN-TEXT".to_owned()),
            tasks: vec![TaskBrief {
                task: TaskId::new("task-001").expect("task id"),
                title: "TASK-TITLE \x1b]0;evil\x07".to_owned(),
                state: TaskState::Running,
                worker: Some(ModelRef::new("claude", None)),
                note: "NOTE-TEXT".to_owned(),
            }],
            recent: vec!["RECENT-TEXT".to_owned()],
            waiting: vec!["WAITING-TEXT".to_owned()],
            ..StatusSummary::default()
        }
    }

    fn event(seq: u64, kind: RunEventKind) -> RunEvent {
        RunEvent {
            seq,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: run_id(),
            task_id: None,
            session: None,
            source: "argonauts-code".to_owned(),
            kind,
        }
    }

    fn pause_event(seq: u64) -> RunEvent {
        event(
            seq,
            RunEventKind::Run(RunLifecycle::PauseRequested {
                by: Party::User,
                now: false,
            }),
        )
    }

    fn opened(number: u32) -> RunEventKind {
        RunEventKind::Approval(ApprovalEvent::Opened(Box::new(ApprovalOpened {
            id: ApprovalId::numbered(number),
            kind: ApprovalKind::Merge,
            title: format!("APPROVAL-TITLE \x1b]0;x\x07 {}", secret()),
            task: None,
            packet: ApprovalPacket::WorkflowPromotion(json!({ "learned_workflow": "PACKET-TEXT" })),
            evidence_sha256: EVIDENCE.to_owned(),
            mode: ApprovalMode::Supervised,
            decide_with: DecideWith::Rules,
            options: Vec::new(),
            free_text: FreeText::None,
            needs_you: true,
        })))
    }

    /// A pause, approval approval-001 still open, and approval-002 opened and withdrawn.
    fn history() -> Vec<RunEvent> {
        vec![
            pause_event(0),
            event(1, opened(1)),
            event(2, opened(2)),
            event(
                3,
                RunEventKind::Approval(ApprovalEvent::Withdrawn {
                    id: ApprovalId::numbered(2),
                    reason: "WITHDRAW-TEXT".to_owned(),
                }),
            ),
        ]
    }

    fn fake() -> FakeApi {
        FakeApi {
            runs: vec![summary()],
            status: status(),
            events: history(),
            ..FakeApi::default()
        }
    }

    fn trusted(mcp: McpSettings) -> OutsideAgentOptions {
        OutsideAgentOptions {
            mcp,
            repo_trusted: true,
            reload: None,
        }
    }

    /// Settings a test changes while the server runs. `None` is a reload that fails.
    type Reloaded = Arc<Mutex<Option<OutsideAgentSettings>>>;

    fn reloading(start: McpSettings) -> (OutsideAgentOptions, Reloaded) {
        let now: Reloaded = Arc::new(Mutex::new(Some(OutsideAgentSettings {
            mcp: start.clone(),
            repo_trusted: true,
        })));
        let read = Arc::clone(&now);
        let options = OutsideAgentOptions {
            reload: Some(Arc::new(move || read.lock().unwrap().clone())),
            ..trusted(start)
        };
        (options, now)
    }

    /// A client speaking raw JSON-RPC lines, as an outside agent's MCP library would.
    struct Wire {
        lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
        write: WriteHalf<DuplexStream>,
        next_id: u64,
        meta: Option<Value>,
        /// Requests and notifications the server sent, such as an elicitation.
        from_server: Vec<Value>,
    }

    impl Wire {
        fn open(api: Arc<FakeApi>, options: OutsideAgentOptions, runs: Arc<FakeRuns>) -> Self {
            let (client, server) = tokio::io::duplex(1 << 20);
            let (server_read, server_write) = tokio::io::split(server);
            let server = UserServer::new(api, options, runs);
            tokio::spawn(serve_server(server, server_read, server_write));
            let (read, write) = tokio::io::split(client);
            Self {
                lines: BufReader::new(read).lines(),
                write,
                next_id: 0,
                meta: None,
                from_server: Vec::new(),
            }
        }

        async fn send(&mut self, message: Value) {
            let mut line = message.to_string();
            line.push('\n');
            self.write.write_all(line.as_bytes()).await.expect("send");
        }

        async fn request(&mut self, method: &str, mut params: Value) -> Value {
            self.next_id += 1;
            let id = self.next_id;
            if let (Some(meta), Some(params)) = (&self.meta, params.as_object_mut()) {
                params.insert("_meta".to_owned(), meta.clone());
            }
            self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
                .await;
            loop {
                let line = tokio::time::timeout(Duration::from_secs(10), self.lines.next_line())
                    .await
                    .expect("an answer in time")
                    .expect("read")
                    .expect("a line");
                let message: Value = serde_json::from_str(&line).expect("json");
                if message.get("id") == Some(&json!(id)) && message.get("method").is_none() {
                    return message;
                }
                self.from_server.push(message);
            }
        }

        async fn handshake(&mut self, version: &str, client: &str) -> Value {
            let answer = self
                .request(
                    "initialize",
                    json!({
                        "protocolVersion": version,
                        "capabilities": {},
                        "clientInfo": { "name": client, "version": "1.0" }
                    }),
                )
                .await;
            self.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
                .await;
            answer
        }

        /// A 2026-07-28 client: no handshake, its version and name on every request.
        fn stateless(&mut self, client: &str) {
            self.meta = Some(json!({
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": { "name": client, "version": "1.0" },
                "io.modelcontextprotocol/clientCapabilities": {}
            }));
        }

        async fn tools(&mut self) -> Vec<Value> {
            let answer = self.request("tools/list", json!({})).await;
            answer["result"]["tools"]
                .as_array()
                .cloned()
                .unwrap_or_else(|| panic!("no tools in {answer}"))
        }

        async fn tool_names(&mut self) -> Vec<String> {
            self.tools()
                .await
                .iter()
                .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
                .collect()
        }

        async fn call(&mut self, name: &str, arguments: Value) -> Value {
            let answer = self
                .request(
                    "tools/call",
                    json!({ "name": name, "arguments": arguments }),
                )
                .await;
            answer
                .get("result")
                .cloned()
                .unwrap_or_else(|| panic!("{name} got no result: {answer}"))
        }
    }

    struct Setup {
        api: Arc<FakeApi>,
        runs: Arc<FakeRuns>,
        wire: Wire,
    }

    async fn connect(api: FakeApi, mcp: McpSettings, client: &str) -> Setup {
        connect_with(api, trusted(mcp), client).await
    }

    async fn connect_with(api: FakeApi, options: OutsideAgentOptions, client: &str) -> Setup {
        let api = Arc::new(api);
        let runs = Arc::new(FakeRuns::default());
        let mut wire = Wire::open(Arc::clone(&api), options, Arc::clone(&runs));
        let answer = wire.handshake("2025-11-25", client).await;
        assert!(answer.get("result").is_some(), "{answer}");
        Setup { api, runs, wire }
    }

    fn is_error(result: &Value) -> bool {
        result["isError"] == json!(true)
    }

    fn text(result: &Value) -> String {
        result["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    /// Every string in `value`, with the key it sits under.
    fn strings<'a>(value: &'a Value, key: &str, found: &mut Vec<(String, &'a str)>) {
        match value {
            Value::String(text) => found.push((key.to_owned(), text)),
            Value::Array(items) => items.iter().for_each(|item| strings(item, key, found)),
            Value::Object(map) => map
                .iter()
                .for_each(|(inner, value)| strings(value, inner, found)),
            _ => {}
        }
    }

    fn agent_texts(result: &Value) -> Vec<String> {
        let mut found = Vec::new();
        strings(&result["structuredContent"], "", &mut found);
        found
            .into_iter()
            .filter(|(key, _)| key == "agent_text")
            .map(|(_, text)| text.to_owned())
            .collect()
    }

    fn assert_code_written(tool: &str, result: &Value) {
        assert!(!is_error(result), "{tool}: {result}");
        let mut found = Vec::new();
        strings(&result["structuredContent"], "", &mut found);
        assert!(!found.is_empty(), "{tool} has no structured result");
        for (key, text) in found {
            for marker in never_sent() {
                assert!(
                    !text.contains(&marker),
                    "{tool} sent {marker} in {key}: {text}"
                );
            }
            if key == "agent_text" {
                assert!(text.chars().count() <= AGENT_TEXT_MAX, "{tool}: {text}");
                assert!(!text.chars().any(char::is_control), "{tool}: {text:?}");
            } else {
                for marker in AGENT_ONLY {
                    assert!(
                        !text.contains(marker),
                        "{tool} put agent text in {key}: {text}"
                    );
                }
            }
        }
    }

    /// Arguments naming every field any tool takes, plus `extra`.
    fn every_argument(extra: Value) -> Value {
        let mut arguments = json!({
            "run": RUN,
            "request": "add a test",
            "mode": "crew",
            "text": "hello",
            "now": true,
            "since": 0,
            "wait_ms": 0
        });
        if let (Some(arguments), Some(extra)) = (arguments.as_object_mut(), extra.as_object()) {
            arguments.extend(extra.clone());
        }
        arguments
    }

    #[tokio::test]
    async fn mcp_has_no_approve_tool() {
        let Setup { mut wire, api, .. } = connect(fake(), McpSettings::default(), "OpenClaw").await;
        let mut names = wire.tool_names().await;
        names.sort();
        let mut expected = vec![
            LIST_RUNS,
            PAUSE_RUN,
            REQUEST_DECISION,
            RUN_EVENTS,
            RUN_STATUS,
            SEND_GUIDANCE,
            START_RUN,
        ];
        expected.sort_unstable();
        assert_eq!(names, expected);
        for name in &names {
            for word in [
                "approv",
                "answer",
                "grant",
                "mode",
                "take_over",
                "reassign",
                "override",
            ] {
                assert!(
                    !name.contains(word),
                    "{name} looks like it decides something"
                );
            }
        }
        for name in [
            "argonauts_approve",
            "argonauts_answer",
            "argonauts_set_approval_mode",
        ] {
            let result = wire.call(name, json!({ "run": RUN })).await;
            assert!(is_error(&result), "{name}: {result}");
        }
        assert!(api.writes().is_empty(), "{:?}", api.calls());
    }

    #[tokio::test]
    async fn mcp_cannot_set_approval_mode() {
        let Setup {
            mut wire,
            api,
            runs,
        } = connect(fake(), McpSettings::default(), "OpenClaw").await;
        let tools = wire.tools().await;
        for tool in &tools {
            let schema = &tool["inputSchema"];
            assert_eq!(schema["additionalProperties"], json!(false), "{tool}");
            let properties = schema["properties"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            for property in properties.keys() {
                assert!(!property.contains("approv"), "{tool}");
            }
            if let Some(modes) = properties.get("mode") {
                assert_eq!(modes["enum"], json!(["captain", "crew"]), "{tool}");
            }
        }
        let asks = [
            json!({ "approval_mode": "auto" }),
            json!({ "approvalMode": "auto" }),
            json!({ "approval_mode": "supervised" }),
            json!({ "approvals": { "mode": "auto" } }),
            json!({ "autonomy": "full" }),
            json!({ "mode": "auto" }),
            json!({ "settings": ["approvals.mode=auto"] }),
            json!({ "settings": ["approvals={mode=\"auto\"}"] }),
        ];
        for tool in &tools {
            let name = tool["name"].as_str().expect("name");
            for ask in &asks {
                let result = wire.call(name, every_argument(ask.clone())).await;
                assert!(is_error(&result), "{name} took {ask}: {result}");
            }
        }
        assert!(api.writes().is_empty(), "{:?}", api.calls());
        assert_eq!(runs.taken.load(Ordering::SeqCst), 0);

        let result = wire
            .call(
                START_RUN,
                json!({ "request": "add a test", "mode": "crew" }),
            )
            .await;
        assert!(!is_error(&result), "{result}");
        let starts = api.starts();
        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].approval_mode, None);
        assert_eq!(starts[0].outside_agent.as_deref(), Some("OpenClaw"));
    }

    #[tokio::test]
    async fn mcp_start_refuses_auto_and_safety_overrides() {
        let Setup {
            mut wire,
            api,
            runs,
        } = connect(fake(), McpSettings::default(), "OpenClaw").await;
        let request = |extra: Value| {
            let mut arguments = json!({ "request": "add a test", "mode": "crew" });
            if let (Some(arguments), Some(extra)) = (arguments.as_object_mut(), extra.as_object()) {
                arguments.extend(extra.clone());
            }
            arguments
        };

        let auto = wire
            .call(START_RUN, request(json!({ "approval_mode": "auto" })))
            .await;
        assert!(is_error(&auto));
        assert!(text(&auto).contains("Argonauts Code app"), "{auto}");
        let auto_mode = wire
            .call(START_RUN, request(json!({ "mode": "auto" })))
            .await;
        assert!(is_error(&auto_mode), "{auto_mode}");

        for (setting, named) in [
            ("approvals.mode=auto", "approvals.mode"),
            ("sandbox.network=true", "sandbox.network"),
            ("mcp.start_runs=trusted-repos", "mcp.start_runs"),
            ("limits.spend_per_run=1000", "limits.spend_per_run"),
            ("agents.claude.binary=/bin/sh", "agents.claude.binary"),
        ] {
            let result = wire
                .call(
                    START_RUN,
                    request(json!({ "settings": ["ui.theme=dark", setting] })),
                )
                .await;
            assert!(is_error(&result), "{setting}: {result}");
            assert!(text(&result).contains(named), "{setting}: {result}");
            assert!(text(&result).contains("safety"), "{setting}: {result}");
        }
        assert!(api.starts().is_empty());
        assert_eq!(
            runs.taken.load(Ordering::SeqCst),
            0,
            "a refusal used up a run"
        );

        let fine = wire
            .call(START_RUN, request(json!({ "settings": ["ui.theme=dark"] })))
            .await;
        assert!(!is_error(&fine), "{fine}");
        let started = &api.starts()[0];
        assert_eq!(started.approval_mode, None);
        assert_eq!(started.outside_agent.as_deref(), Some("OpenClaw"));
        assert_eq!(started.overrides, vec!["ui.theme=dark".to_owned()]);
        assert_eq!(started.mode, RunMode::Crew);
        assert_eq!(fine["structuredContent"]["run"], json!(RUN));
        assert_eq!(
            fine["structuredContent"]["outside_agent_runs_today"],
            json!(1)
        );
    }

    #[tokio::test]
    async fn mcp_start_honors_start_runs_and_the_daily_cap() {
        let off = McpSettings {
            start_runs: StartRuns::Off,
            ..McpSettings::default()
        };
        let Setup { mut wire, api, .. } = connect(fake(), off, "Hermes").await;
        let result = wire
            .call(START_RUN, json!({ "request": "x", "mode": "captain" }))
            .await;
        assert!(is_error(&result));
        assert!(text(&result).contains("mcp.start_runs"), "{result}");
        assert!(api.starts().is_empty());

        let capped = McpSettings {
            runs_per_day: 2,
            ..McpSettings::default()
        };
        let Setup { mut wire, api, .. } = connect(fake(), capped, "Hermes").await;
        for _ in 0..2 {
            let result = wire
                .call(START_RUN, json!({ "request": "x", "mode": "captain" }))
                .await;
            assert!(!is_error(&result), "{result}");
        }
        let third = wire
            .call(START_RUN, json!({ "request": "x", "mode": "captain" }))
            .await;
        assert!(is_error(&third));
        assert!(text(&third).contains("used up"), "{third}");
        assert_eq!(api.starts().len(), 2);

        let empty = wire
            .call(
                START_RUN,
                json!({ "request": " \x1b[2J ", "mode": "captain" }),
            )
            .await;
        assert!(is_error(&empty), "{empty}");
    }

    #[tokio::test]
    async fn mcp_start_argonauts_code_refuses_gives_its_run_back() {
        let refusing = FakeApi {
            start_refusal: Some("the repo is busy"),
            ..fake()
        };
        let capped = McpSettings {
            runs_per_day: 1,
            ..McpSettings::default()
        };
        let Setup {
            mut wire,
            api,
            runs,
        } = connect(refusing, capped, "Hermes").await;
        for _ in 0..3 {
            let result = wire
                .call(START_RUN, json!({ "request": "x", "mode": "captain" }))
                .await;
            assert!(is_error(&result), "{result}");
            assert!(text(&result).contains("the repo is busy"), "{result}");
            assert!(!text(&result).contains("used up"), "{result}");
        }
        assert_eq!(api.starts().len(), 3);
        assert_eq!(
            runs.taken.load(Ordering::SeqCst),
            0,
            "a refused start used up a run"
        );
    }

    #[tokio::test]
    async fn mcp_accepts_handshake_and_stateless_clients() {
        for version in ["2024-11-05", "2025-11-25"] {
            let api = Arc::new(fake());
            let mut wire = Wire::open(
                Arc::clone(&api),
                trusted(McpSettings::default()),
                Arc::new(FakeRuns::default()),
            );
            let answer = wire.handshake(version, "OpenClaw").await;
            assert_eq!(
                answer["result"]["protocolVersion"],
                json!(version),
                "{answer}"
            );
            assert_eq!(answer["result"]["serverInfo"]["name"], json!("argonauts"));
            assert!(answer["result"]["instructions"].is_string(), "{answer}");
            assert_eq!(wire.tool_names().await.len(), 7, "{version}");
            let result = wire.call(LIST_RUNS, json!({})).await;
            assert!(!is_error(&result), "{version}: {result}");
            let guided = wire
                .call(SEND_GUIDANCE, json!({ "run": RUN, "text": "hi" }))
                .await;
            assert!(!is_error(&guided), "{version}: {guided}");
            assert!(api.calls().contains(&Call::Guidance {
                run: run_id(),
                text: "hi".to_owned(),
                client: Some("OpenClaw".to_owned()),
            }));
        }

        // A client that asks for the newest version in a handshake gets the newest one that
        // still has one.
        let mut wire = Wire::open(
            Arc::new(fake()),
            trusted(McpSettings::default()),
            Arc::new(FakeRuns::default()),
        );
        let answer = wire.handshake("2026-07-28", "Hermes").await;
        assert_eq!(
            answer["result"]["protocolVersion"],
            json!("2025-11-25"),
            "{answer}"
        );

        let api = Arc::new(fake());
        let mut wire = Wire::open(
            Arc::clone(&api),
            trusted(McpSettings::default()),
            Arc::new(FakeRuns::default()),
        );
        wire.stateless("Hermes");
        assert_eq!(wire.tool_names().await.len(), 7);
        let result = wire.call(RUN_STATUS, json!({ "run": RUN })).await;
        assert!(!is_error(&result), "{result}");
        let paused = wire.call(PAUSE_RUN, json!({ "run": RUN })).await;
        assert!(!is_error(&paused), "{paused}");
        assert!(api.calls().contains(&Call::Pause {
            run: run_id(),
            now: false,
            client: Some("Hermes".to_owned()),
        }));

        // rmcp's own client in discover mode, as Hermes runs it.
        let api = Arc::new(fake());
        let (client_end, server_end) = tokio::io::duplex(1 << 20);
        let (server_read, server_write) = tokio::io::split(server_end);
        let server = UserServer::new(
            Arc::clone(&api) as Arc<dyn ConductorApi>,
            trusted(McpSettings::default()),
            Arc::new(FakeRuns::default()),
        );
        tokio::spawn(serve_server(server, server_read, server_write));
        let hermes = ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("Hermes Agent", "1.0"),
        );
        let client = hermes
            .serve_with_lifecycle(
                client_end,
                ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                },
            )
            .await
            .expect("discover");
        assert_eq!(
            client.peer_info().map(|info| info.protocol_version.clone()),
            Some(ProtocolVersion::V_2026_07_28)
        );
        assert_eq!(client.list_all_tools().await.expect("tools").len(), 7);
        let arguments = json!({ "run": RUN, "text": "hi" });
        let guided = client
            .call_tool(
                CallToolRequestParams::new(SEND_GUIDANCE)
                    .with_arguments(arguments.as_object().cloned().unwrap_or_default()),
            )
            .await
            .expect("call");
        assert_ne!(guided.is_error, Some(true), "{guided:?}");
        assert!(api.calls().contains(&Call::Guidance {
            run: run_id(),
            text: "hi".to_owned(),
            client: Some("Hermes Agent".to_owned()),
        }));
        client.cancel().await.expect("close");
    }

    #[tokio::test]
    async fn mcp_serve_refuses_untrusted_repo() {
        let api = Arc::new(fake());
        let (mut client, server) = tokio::io::duplex(1 << 16);
        let (read, write) = tokio::io::split(server);
        let err = serve_on(
            Arc::clone(&api) as Arc<dyn ConductorApi>,
            OutsideAgentOptions::default(),
            read,
            write,
        )
        .await
        .expect_err("an untrusted repo is refused");
        assert!(err.to_string().contains("argonauts setup"), "{err}");
        let mut said = String::new();
        client.read_to_string(&mut said).await.expect("read");
        assert!(said.is_empty(), "the server answered: {said}");

        let (_client, server) = tokio::io::duplex(1 << 16);
        let (read, write) = tokio::io::split(server);
        let untrusted = UserServer::new(
            Arc::clone(&api) as Arc<dyn ConductorApi>,
            OutsideAgentOptions {
                repo_trusted: false,
                ..trusted(McpSettings::default())
            },
            Arc::new(FakeRuns::default()),
        );
        assert!(serve_server(untrusted, read, write).await.is_err());
        assert!(api.calls().is_empty());
    }

    #[tokio::test]
    async fn mcp_decision_is_text_with_a_link() {
        let Setup { mut wire, api, .. } = connect(fake(), McpSettings::default(), "OpenClaw").await;
        let result = wire.call(REQUEST_DECISION, json!({ "run": RUN })).await;
        assert!(!is_error(&result), "{result}");
        let said = text(&result);
        assert!(said.contains("approval-001"), "{said}");
        assert!(said.contains("merge"), "{said}");
        assert!(said.contains(&EVIDENCE[..12]), "{said}");
        assert!(!said.contains(&EVIDENCE[..13]), "{said}");
        assert!(said.contains("Argonauts Code app"), "{said}");
        assert!(
            said.contains(&format!("argonauts approve {RUN} approval-001")),
            "{said}"
        );
        assert!(
            !said.contains("approval-002"),
            "a withdrawn approval is listed: {said}"
        );
        let agent_only = AGENT_ONLY.iter().map(|&marker| marker.to_owned());
        for marker in agent_only.chain(never_sent()) {
            assert!(!said.contains(&marker), "{said}");
        }
        let waiting = &result["structuredContent"]["waiting"];
        assert_eq!(waiting.as_array().map(Vec::len), Some(1), "{result}");
        assert_eq!(waiting[0]["approval"], json!("approval-001"));
        assert_eq!(waiting[0]["evidence"], json!(&EVIDENCE[..12]));
        assert_eq!(result["structuredContent"]["needs_you"], json!(true));
        assert_code_written(REQUEST_DECISION, &result);

        assert!(wire.from_server.is_empty(), "{:?}", wire.from_server);
        assert!(api.writes().is_empty(), "{:?}", api.calls());

        let nothing = FakeApi {
            events: vec![pause_event(0)],
            ..fake()
        };
        let Setup { mut wire, .. } = connect(nothing, McpSettings::default(), "OpenClaw").await;
        let result = wire.call(REQUEST_DECISION, json!({ "run": RUN })).await;
        assert!(text(&result).contains("Nothing"), "{result}");
        let unknown = wire
            .call(REQUEST_DECISION, json!({ "run": "run-other" }))
            .await;
        assert!(is_error(&unknown), "{unknown}");
        let bad = wire
            .call(REQUEST_DECISION, json!({ "run": "../../etc" }))
            .await;
        assert!(is_error(&bad), "{bad}");
    }

    #[tokio::test]
    async fn mcp_guidance_and_pause_raise_no_autonomy() {
        let Setup { mut wire, api, .. } = connect(fake(), McpSettings::default(), "OpenClaw").await;
        let guided = wire
            .call(
                SEND_GUIDANCE,
                json!({ "run": RUN, "text": "add tests\x1b[2J" }),
            )
            .await;
        assert!(!is_error(&guided), "{guided}");
        let paused = wire
            .call(PAUSE_RUN, json!({ "run": RUN, "now": true }))
            .await;
        assert!(!is_error(&paused), "{paused}");
        assert_eq!(
            api.writes(),
            vec![
                Call::Guidance {
                    run: run_id(),
                    text: "add tests".to_owned(),
                    client: Some("OpenClaw".to_owned()),
                },
                Call::Pause {
                    run: run_id(),
                    now: true,
                    client: Some("OpenClaw".to_owned()),
                },
            ]
        );

        let named = format!("\x1b[31mOpen\nClaw\x1b[0m{}", "x".repeat(200));
        let Setup { mut wire, api, .. } = connect(fake(), McpSettings::default(), &named).await;
        let guided = wire
            .call(SEND_GUIDANCE, json!({ "run": RUN, "text": "hi" }))
            .await;
        assert!(!is_error(&guided), "{guided}");
        let Some(Call::Guidance {
            client: Some(client),
            ..
        }) = api.writes().first().cloned()
        else {
            panic!("no guidance recorded: {:?}", api.calls());
        };
        assert!(client.starts_with("Open Claw"), "{client:?}");
        assert!(client.chars().count() <= CLIENT_NAME_MAX, "{client:?}");
        assert!(!client.chars().any(char::is_control), "{client:?}");

        let off = McpSettings {
            guidance: false,
            pause: false,
            ..McpSettings::default()
        };
        let Setup { mut wire, api, .. } = connect(fake(), off, "OpenClaw").await;
        let names = wire.tool_names().await;
        assert!(
            !names
                .iter()
                .any(|name| name == SEND_GUIDANCE || name == PAUSE_RUN)
        );
        assert_eq!(names.len(), 5);
        let guided = wire
            .call(SEND_GUIDANCE, json!({ "run": RUN, "text": "hi" }))
            .await;
        assert!(is_error(&guided), "{guided}");
        let paused = wire.call(PAUSE_RUN, json!({ "run": RUN })).await;
        assert!(is_error(&paused), "{paused}");
        assert!(api.writes().is_empty(), "{:?}", api.calls());
    }

    #[tokio::test]
    async fn mcp_outputs_are_code_written_and_capped() {
        let Setup { mut wire, .. } = connect(fake(), McpSettings::default(), "OpenClaw").await;
        let listed = wire.call(LIST_RUNS, json!({})).await;
        assert_code_written(LIST_RUNS, &listed);
        let run = &listed["structuredContent"]["runs"][0];
        assert_eq!(run["run"], json!(RUN));
        assert_eq!(run["state"], json!("waiting"));
        assert_eq!(run["needs_you"], json!(true));
        assert_eq!(run["spent"]["usd"], json!(1.25));
        let title = run["agent_text"].as_str().unwrap_or_default();
        assert!(title.starts_with("TITLE-TEXT [redacted"), "{title}");
        assert_eq!(title.chars().count(), AGENT_TEXT_MAX);

        let status = wire.call(RUN_STATUS, json!({ "run": RUN })).await;
        assert_code_written(RUN_STATUS, &status);
        let fields = &status["structuredContent"];
        assert_eq!(fields["tasks"][0]["task"], json!("task-001"));
        assert_eq!(fields["tasks"][0]["state"], json!("running"));
        assert_eq!(fields["task_counts"]["running"], json!(1));
        assert!(
            fields["agent_text"]
                .as_str()
                .is_some_and(|goal| goal.starts_with("GOAL-TEXT [redacted")),
            "{status}"
        );

        let events = wire.call(RUN_EVENTS, json!({ "run": RUN })).await;
        assert_code_written(RUN_EVENTS, &events);
        let fields = &events["structuredContent"];
        assert_eq!(fields["next_since"], json!(4));
        assert_eq!(fields["events"][0]["type"], json!("run"));
        assert_eq!(fields["events"][0]["event"], json!("pause_requested"));
        assert_eq!(fields["events"][1]["approval"], json!("approval-001"));
        assert_eq!(fields["events"][3]["event"], json!("withdrawn"));

        let many = FakeApi {
            events: (0..(MAX_EVENTS as u64 + 50)).map(pause_event).collect(),
            ..fake()
        };
        let Setup { mut wire, .. } = connect(many, McpSettings::default(), "OpenClaw").await;
        let first = wire.call(RUN_EVENTS, json!({ "run": RUN })).await;
        let fields = &first["structuredContent"];
        assert_eq!(fields["events"].as_array().map(Vec::len), Some(MAX_EVENTS));
        assert_eq!(fields["more"], json!(true));
        let since = fields["next_since"].clone();
        let rest = wire
            .call(RUN_EVENTS, json!({ "run": RUN, "since": since }))
            .await;
        let fields = &rest["structuredContent"];
        assert_eq!(fields["events"].as_array().map(Vec::len), Some(50));
        assert_eq!(fields["more"], json!(false));
        assert_eq!(fields["next_since"], json!(MAX_EVENTS as u64 + 50));
    }

    #[tokio::test]
    async fn run_events_waits_for_the_next_event() {
        let live = FakeApi {
            events: vec![pause_event(0)],
            live: Some((Duration::from_millis(200), pause_event(1))),
            ..fake()
        };
        let Setup { mut wire, .. } = connect(live, McpSettings::default(), "OpenClaw").await;
        let started = std::time::Instant::now();
        let history = wire
            .call(RUN_EVENTS, json!({ "run": RUN, "wait_ms": 20_000 }))
            .await;
        assert_eq!(
            history["structuredContent"]["next_since"],
            json!(1),
            "{history}"
        );
        assert!(started.elapsed() < Duration::from_secs(5), "history waited");

        let next = wire
            .call(
                RUN_EVENTS,
                json!({ "run": RUN, "since": 1, "wait_ms": 20_000 }),
            )
            .await;
        let fields = &next["structuredContent"];
        assert_eq!(fields["events"][0]["seq"], json!(1), "{next}");
        assert_eq!(fields["next_since"], json!(2));
        assert_eq!(fields["live"], json!(true));
        assert!(started.elapsed() < Duration::from_secs(10));

        let quiet = wire
            .call(RUN_EVENTS, json!({ "run": RUN, "since": 5, "wait_ms": 50 }))
            .await;
        assert_eq!(quiet["structuredContent"]["events"], json!([]), "{quiet}");
        assert_eq!(quiet["structuredContent"]["next_since"], json!(5));
    }

    #[tokio::test]
    async fn mcp_share_levels() {
        for share in [Share::StatusOnly, Share::Summaries] {
            let mcp = McpSettings {
                share,
                ..McpSettings::default()
            };
            let Setup { mut wire, .. } = connect(fake(), mcp, "OpenClaw").await;
            let results = [
                (LIST_RUNS, wire.call(LIST_RUNS, json!({})).await),
                (
                    RUN_STATUS,
                    wire.call(RUN_STATUS, json!({ "run": RUN })).await,
                ),
                (
                    RUN_EVENTS,
                    wire.call(RUN_EVENTS, json!({ "run": RUN })).await,
                ),
                (
                    REQUEST_DECISION,
                    wire.call(REQUEST_DECISION, json!({ "run": RUN })).await,
                ),
            ];
            for (tool, result) in &results {
                assert_code_written(tool, result);
                let texts = agent_texts(result);
                match share {
                    Share::StatusOnly => {
                        assert!(texts.is_empty(), "{tool} shared {texts:?}");
                        let whole = result.to_string();
                        for marker in AGENT_ONLY {
                            assert!(!whole.contains(marker), "{tool}: {whole}");
                        }
                    }
                    Share::Summaries => assert!(!texts.is_empty(), "{tool}: {result}"),
                }
            }
            assert_eq!(
                results[3].1["structuredContent"]["waiting"][0]["approval"],
                json!("approval-001")
            );
        }
    }

    #[tokio::test]
    async fn mcp_reloaded_settings_only_tighten() {
        let start = McpSettings {
            runs_per_day: 2,
            ..McpSettings::default()
        };
        let (options, now) = reloading(start.clone());
        let Setup { mut wire, api, .. } = connect_with(fake(), options, "Hermes").await;
        assert_eq!(wire.tool_names().await.len(), 7);

        // Looser settings wait for a restart: the start cap of 2 still holds.
        now.lock().unwrap().as_mut().unwrap().mcp.runs_per_day = 10;
        for _ in 0..2 {
            let result = wire
                .call(START_RUN, json!({ "request": "x", "mode": "captain" }))
                .await;
            assert!(!is_error(&result), "{result}");
            assert_eq!(
                result["structuredContent"]["outside_agent_runs_per_day"],
                json!(2)
            );
        }
        let third = wire
            .call(START_RUN, json!({ "request": "x", "mode": "captain" }))
            .await;
        assert!(text(&third).contains("used up"), "{third}");

        *now.lock().unwrap() = Some(OutsideAgentSettings {
            mcp: McpSettings {
                start_runs: StartRuns::Off,
                guidance: false,
                pause: false,
                share: Share::StatusOnly,
                ..start.clone()
            },
            repo_trusted: true,
        });
        let names = wire.tool_names().await;
        assert!(
            !names
                .iter()
                .any(|name| name == SEND_GUIDANCE || name == PAUSE_RUN)
        );
        let started = wire
            .call(START_RUN, json!({ "request": "x", "mode": "captain" }))
            .await;
        assert!(text(&started).contains("mcp.start_runs"), "{started}");
        let guided = wire
            .call(SEND_GUIDANCE, json!({ "run": RUN, "text": "hi" }))
            .await;
        assert!(text(&guided).contains("mcp.guidance"), "{guided}");
        let paused = wire.call(PAUSE_RUN, json!({ "run": RUN })).await;
        assert!(text(&paused).contains("mcp.pause"), "{paused}");
        let status = wire.call(RUN_STATUS, json!({ "run": RUN })).await;
        assert!(agent_texts(&status).is_empty(), "{status}");

        // A reload that fails keeps the start values, never the looser of anything.
        *now.lock().unwrap() = None;
        assert_eq!(wire.tool_names().await.len(), 7);
        let status = wire.call(RUN_STATUS, json!({ "run": RUN })).await;
        assert!(!agent_texts(&status).is_empty(), "{status}");
        assert_eq!(api.starts().len(), 2);
        assert_eq!(api.writes().len(), 2, "{:?}", api.calls());
    }

    #[tokio::test]
    async fn mcp_refuses_everything_once_trust_is_revoked() {
        let (options, now) = reloading(McpSettings::default());
        let Setup { mut wire, api, .. } = connect_with(fake(), options, "Hermes").await;
        now.lock().unwrap().as_mut().unwrap().repo_trusted = false;

        let listed = wire.request("tools/list", json!({})).await;
        assert!(listed.get("result").is_none(), "{listed}");
        assert!(
            listed["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("argonauts setup"))
        );
        let every_tool = [
            (START_RUN, json!({ "request": "x", "mode": "captain" })),
            (LIST_RUNS, json!({})),
            (RUN_STATUS, json!({ "run": RUN })),
            (RUN_EVENTS, json!({ "run": RUN })),
            (REQUEST_DECISION, json!({ "run": RUN })),
            (SEND_GUIDANCE, json!({ "run": RUN, "text": "hi" })),
            (PAUSE_RUN, json!({ "run": RUN })),
        ];
        for (tool, arguments) in every_tool {
            let result = wire.call(tool, arguments).await;
            assert!(is_error(&result), "{tool}: {result}");
            assert!(text(&result).contains("isn't trusted"), "{tool}: {result}");
        }
        assert!(api.calls().is_empty(), "{:?}", api.calls());

        // Trusting the repo again brings the tools back without a restart.
        now.lock().unwrap().as_mut().unwrap().repo_trusted = true;
        assert_eq!(wire.tool_names().await.len(), 7);
    }

    #[test]
    fn stricter_takes_the_tighter_side_of_every_key() {
        let loose = McpSettings {
            runs_per_day: 9,
            run_limit: Span::new("none"),
            ..McpSettings::default()
        };
        let tight = McpSettings {
            status_tool: false,
            start_runs: StartRuns::Off,
            guidance: false,
            pause: false,
            share: Share::StatusOnly,
            run_limit: Span::new("30m"),
            runs_per_day: 1,
        };
        assert_eq!(stricter(&loose, &tight), tight);
        assert_eq!(stricter(&tight, &loose), tight);
        assert_eq!(stricter(&loose, &loose), loose);

        let limit = |start: &str, now: &str| shorter(&Span::new(start), &Span::new(now)).0;
        assert_eq!(limit("2h", "30m"), "30m");
        assert_eq!(limit("30m", "2h"), "30m");
        assert_eq!(limit("none", "45s"), "45s");
        assert_eq!(limit("45s", "none"), "45s");
        assert_eq!(limit("60m", "soon"), "60m");
    }
}
