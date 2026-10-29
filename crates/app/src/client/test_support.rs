//! Events, run directories, a home short enough for socket paths, and a fake run host that
//! answers on a run's socket, for the client's and the views' tests.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use argonauts_core::api::StatusSummary;
use argonauts_core::control::{ControlReply, ControlRequest, Frame};
use argonauts_core::events::approval::{
    AnswerAction, ApprovalDecided, ApprovalEvent, ApprovalKind, ApprovalOpened, ApprovalPacket,
    FreeText, PlanPacket,
};
use argonauts_core::events::plan::{LimitPlan, TaskGraph};
use argonauts_core::events::run::{Party, RunLifecycle, RunMode, RunStarted};
use argonauts_core::events::session::{
    Delivery, EndReason, InputPart, InputPartKind, LaunchRecord, PromptVia, QueuedMessage,
    SessionEnd, SessionEvent, SessionInput, SessionStarted,
};
use argonauts_core::events::task::{TaskEvent, TaskKind, TaskLimit, TaskSpec};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::input::MentionTarget;
use argonauts_core::names::{AgentId, ApprovalId, MessageId, ModelRef, RoleName, SessionKey};
use argonauts_core::session_runner::{Access, RunnerKind};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::asking::WhenDeclined;
use argonauts_core::settings::limit::{LimitAmount, LimitUnit};
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::{AgentEvent, Paths, RunId, Spend, TaskId};
use time::OffsetDateTime;
use tokio::io::BufReader;
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;

use super::Launcher;
use crate::env::AppEnv;
use crate::wire::{read_frame, write_frame};

pub(crate) const FAKE_PID: u32 = 4242;

pub(crate) fn run_id() -> RunId {
    RunId::new("run-20260930-101500-00ab").unwrap()
}

pub(crate) fn base_ts() -> OffsetDateTime {
    time::macros::datetime!(2026-09-30 10:15:00 UTC)
}

/// `seq` seconds after [`base_ts`].
pub(crate) fn event(seq: u64, kind: RunEventKind) -> RunEvent {
    event_at(seq, base_ts() + time::Duration::seconds(seq as i64), kind)
}

pub(crate) fn event_at(seq: u64, ts: OffsetDateTime, kind: RunEventKind) -> RunEvent {
    RunEvent {
        seq,
        ts,
        run_id: run_id(),
        task_id: None,
        session: None,
        source: "test".to_owned(),
        kind,
    }
}

pub(crate) fn session_event(
    seq: u64,
    session: &SessionKey,
    task: Option<&TaskId>,
    kind: RunEventKind,
) -> RunEvent {
    RunEvent {
        session: Some(session.clone()),
        task_id: task.cloned(),
        ..event(seq, kind)
    }
}

pub(crate) fn task_event(seq: u64, task: &TaskId, kind: TaskEvent) -> RunEvent {
    RunEvent {
        task_id: Some(task.clone()),
        ..event(seq, RunEventKind::Task(kind))
    }
}

pub(crate) fn agent(seq: u64, session: &SessionKey, kind: AgentEvent) -> RunEvent {
    session_event(seq, session, None, RunEventKind::Agent(kind))
}

pub(crate) fn usage(seq: u64, session: &SessionKey, input: u64, output: u64, usd: f64) -> RunEvent {
    agent(
        seq,
        session,
        AgentEvent::Usage {
            input_tokens: input,
            output_tokens: output,
            cached_tokens: 0,
            cost_usd: Some(usd),
            cumulative: true,
        },
    )
}

pub(crate) fn session_started(
    seq: u64,
    session: &SessionKey,
    task: Option<&TaskId>,
    role: RoleName,
    worker: &str,
) -> RunEvent {
    let started = SessionStarted {
        worker: ModelRef::new(worker, None),
        agent_id: AgentId(worker.to_owned()),
        provider: worker.to_owned(),
        kind: RunnerKind::Cli,
        role,
        step: None,
        access: Access::Build,
        lifetime: SessionLifetime::Task,
        resumed_from: None,
        follow_key: None,
        workflow_step: None,
        label: None,
        asked: None,
        asked_by: None,
        asked_why: None,
        forked: false,
        seat: None,
        billing: None,
    };
    session_event(
        seq,
        session,
        task,
        RunEventKind::Session(SessionEvent::Started(Box::new(started))),
    )
}

pub(crate) fn input(seq: u64, session: &SessionKey, text: &str) -> RunEvent {
    let input = SessionInput {
        launch: LaunchRecord {
            program: "claude".to_owned(),
            args: vec!["<prompt>".to_owned()],
            cwd: PathBuf::from("/tmp"),
            env_set: Vec::new(),
            env_removed: Vec::new(),
            prompt_via: PromptVia::Argument,
            sandbox: None,
        },
        parts: vec![InputPart {
            kind: InputPartKind::Prompt,
            label: "prompt".to_owned(),
            text: text.to_owned(),
            approx_tokens: 1,
            source: None,
        }],
        queued: Vec::new(),
    };
    session_event(
        seq,
        session,
        None,
        RunEventKind::Session(SessionEvent::Input(Box::new(input))),
    )
}

pub(crate) fn session_ended(
    seq: u64,
    session: &SessionKey,
    reason: EndReason,
    duration_ms: u64,
) -> RunEvent {
    let end = SessionEnd {
        cli_session: None,
        exit_code: Some(0),
        signal: None,
        duration_ms,
        reason,
        spend: Spend::default(),
        tool_calls: 0,
        result_text: Some("done".to_owned()),
        structured: None,
        last_message: None,
        raw_log: None,
    };
    session_event(
        seq,
        session,
        None,
        RunEventKind::Session(SessionEvent::Ended(Box::new(end))),
    )
}

pub(crate) fn run_started(seq: u64, request: &str, outside_agent: Option<&str>) -> RunEvent {
    let started = RunStarted {
        request: request.to_owned(),
        title: request.to_owned(),
        mode: RunMode::Captain,
        forced: false,
        workflow: "captain".to_owned(),
        estimate: None,
        captain_or_crew: None,
        conversation_turn: None,
        continues: None,
        repo_root: PathBuf::from("/code/app"),
        base_branch: Some("main".to_owned()),
        base_commit: "abc123".to_owned(),
        uncommitted: Vec::new(),
        uncommitted_included: false,
        settings_sha256: String::new(),
        settings: serde_json::json!({}),
        overrides: Vec::new(),
        cli_versions: BTreeMap::new(),
        version: "0.0.1".to_owned(),
        outside_agent: outside_agent.map(str::to_owned),
        instructions: Vec::new(),
        required_steps: Vec::new(),
        starts_from: None,
    };
    event(
        seq,
        RunEventKind::Run(RunLifecycle::Started(Box::new(started))),
    )
}

pub(crate) fn queued(seq: u64, text: &str, from: Party) -> RunEvent {
    let message = QueuedMessage {
        id: MessageId::numbered(u32::try_from(seq).unwrap()),
        from,
        to: MentionTarget::All,
        text: text.to_owned(),
        queued_at: base_ts(),
        delivery: Delivery::NextTurn,
    };
    event(seq, RunEventKind::Session(SessionEvent::Queued(message)))
}

pub(crate) fn task_spec(n: usize) -> TaskSpec {
    TaskSpec {
        id: TaskId::new(format!("task-{n:03}")).unwrap(),
        title: format!("Task {n}"),
        task: String::new(),
        kind: TaskKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: Vec::new(),
        worker: None,
        reviewers: Vec::new(),
        limit: TaskLimit::default(),
        predictions: None,
        item: None,
        way: None,
    }
}

pub(crate) fn opened(
    seq: u64,
    id: &ApprovalId,
    kind: ApprovalKind,
    task: Option<&TaskId>,
    packet: ApprovalPacket,
) -> RunEvent {
    let opened = ApprovalOpened {
        id: id.clone(),
        kind,
        title: "approval".to_owned(),
        task: task.cloned(),
        packet,
        evidence_sha256: "evidence".to_owned(),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        when_declined: WhenDeclined::Decide,
        options: Vec::new(),
        free_text: FreeText::None,
        needs_you: true,
    };
    event(
        seq,
        RunEventKind::Approval(ApprovalEvent::Opened(Box::new(opened))),
    )
}

pub(crate) fn plan_approval(seq: u64, id: &ApprovalId, tasks: usize) -> RunEvent {
    let packet = PlanPacket {
        summary: "the plan".to_owned(),
        judging: None,
        near_tie: false,
        decisions: Vec::new(),
        graph: TaskGraph {
            tasks: (1..=tasks).map(task_spec).collect(),
            critical_path: Vec::new(),
        },
        acceptance_tests: Vec::new(),
        limit: LimitPlan {
            lines: Vec::new(),
            total: LimitAmount {
                unit: LimitUnit::Usd,
                amount: 10.0,
            },
            estimate_low: 1.0,
            estimate_high: 2.0,
        },
        new_dependencies: Vec::new(),
        flags: Vec::new(),
        workflow: None,
        seats: Vec::new(),
        objections: Vec::new(),
        offer: None,
    };
    opened(
        seq,
        id,
        ApprovalKind::Plan,
        None,
        ApprovalPacket::Plan(Box::new(packet)),
    )
}

pub(crate) fn approval_decided(
    seq: u64,
    id: &ApprovalId,
    action: AnswerAction,
    by: Party,
    text: Option<&str>,
) -> RunEvent {
    let decided = ApprovalDecided {
        id: id.clone(),
        action,
        text: text.map(str::to_owned),
        by,
        evidence_sha256: "evidence".to_owned(),
        auto_preview: None,
        tasks: Vec::new(),
    };
    event(seq, RunEventKind::Approval(ApprovalEvent::Decided(decided)))
}

pub(crate) fn write_journal(path: &Path, events: &[RunEvent]) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    for event in events {
        let mut line = serde_json::to_vec(event).unwrap();
        line.push(b'\n');
        file.write_all(&line).unwrap();
    }
}

/// A repo and an Argonauts Code home under `/tmp`, since a socket path must stay under about 100
/// bytes and the system temp directory is long on macOS.
pub(crate) struct TestHome {
    _dir: tempfile::TempDir,
    pub repo: PathBuf,
    pub paths: Paths,
}

impl TestHome {
    pub(crate) fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("ag")
            .tempdir_in("/tmp")
            .unwrap();
        let repo = dir.path().join("r");
        std::fs::create_dir_all(&repo).unwrap();
        let paths = Paths::new(dir.path().join("h"));
        Self {
            _dir: dir,
            repo,
            paths,
        }
    }

    pub(crate) fn env(&self, interactive: bool) -> Arc<AppEnv> {
        let mut env = AppEnv::for_repo(self.paths.clone(), &self.repo, interactive).unwrap();
        env.agent_session = false;
        Arc::new(env)
    }

    /// A run directory holding `events`.
    pub(crate) fn run_with(&self, env: &AppEnv, run: &RunId, events: &[RunEvent]) -> PathBuf {
        let run_dir = env.run_dir(run);
        std::fs::create_dir_all(&run_dir).unwrap();
        write_journal(
            &run_dir.join(argonauts_core::layout::run_files::EVENTS),
            events,
        );
        run_dir
    }

    pub(crate) fn write_global_config(&self, text: &str) {
        std::fs::create_dir_all(self.paths.home()).unwrap();
        std::fs::write(self.paths.global_config(), text).unwrap();
    }
}

/// Stands in for `argonauts host`: launching binds the run's socket and answers there.
#[derive(Default)]
pub(crate) struct FakeRunHost {
    pub launches: Mutex<Vec<RunId>>,
    pub script: Arc<Script>,
    servers: Mutex<Vec<ServerHandle>>,
}

impl FakeRunHost {
    pub(crate) fn launches(&self) -> Vec<RunId> {
        self.launches.lock().unwrap().clone()
    }
}

impl Launcher for FakeRunHost {
    fn launch(&self, _repo_root: &Path, run: &RunId, run_dir: &Path) -> anyhow::Result<()> {
        self.launches.lock().unwrap().push(run.clone());
        let server = serve(run_dir, run.clone(), Arc::clone(&self.script));
        self.servers.lock().unwrap().push(server);
        Ok(())
    }
}

/// What a fake run host sends and what it was asked.
#[derive(Default)]
pub(crate) struct Script {
    /// Sent in full on every `Subscribe`, whatever it asks for, so the client must dedupe.
    pub events: Mutex<Vec<RunEvent>>,
    pub requests: Mutex<Vec<ControlRequest>>,
    /// Messages the run host still holds, which `TakeBack` returns.
    pub queued: Mutex<Vec<QueuedMessage>>,
}

pub(crate) struct ServerHandle {
    shutdown: CancellationToken,
    socket: PathBuf,
}

impl ServerHandle {
    /// Closes every connection and removes the socket, as an exiting run host does.
    pub(crate) fn stop(&self) {
        self.shutdown.cancel();
        let _ = std::fs::remove_file(&self.socket);
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(crate) fn serve(run_dir: &Path, run: RunId, script: Arc<Script>) -> ServerHandle {
    let socket = crate::run_files::socket_path(run_dir);
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).unwrap();
    let shutdown = CancellationToken::new();
    let stop = shutdown.clone();
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                () = stop.cancelled() => return,
                accepted = listener.accept() => accepted,
            };
            let Ok((stream, _)) = accepted else { return };
            let (run, script, stop) = (run.clone(), Arc::clone(&script), stop.clone());
            tokio::spawn(async move {
                tokio::select! {
                    () = stop.cancelled() => {}
                    () = answer(stream, run, script) => {}
                }
            });
        }
    });
    ServerHandle { shutdown, socket }
}

async fn answer(stream: tokio::net::UnixStream, run: RunId, script: Arc<Script>) {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    while let Ok(Some(frame)) = read_frame::<_, ControlRequest>(&mut reader).await {
        let reply = |body| Frame { id: frame.id, body };
        let body = match frame.body {
            ControlRequest::Hello { .. } => ControlReply::Welcome {
                run: run.clone(),
                pid: FAKE_PID,
                version: "test".to_owned(),
            },
            ControlRequest::Subscribe { .. } => {
                let events = script.events.lock().unwrap().clone();
                for event in events {
                    let body = ControlReply::Event {
                        event: Box::new(event),
                    };
                    if write_frame(&mut writer, &reply(body)).await.is_err() {
                        return;
                    }
                }
                std::future::pending::<()>().await;
                return;
            }
            request => {
                script.requests.lock().unwrap().push(request.clone());
                match request {
                    ControlRequest::Guidance { .. } | ControlRequest::Mention { .. } => {
                        ControlReply::Queued {
                            id: MessageId::numbered(1),
                        }
                    }
                    ControlRequest::TakeBack { messages } => {
                        let mut queued = script.queued.lock().unwrap();
                        let (back, kept) = queued
                            .drain(..)
                            .partition(|message| messages.contains(&message.id));
                        *queued = kept;
                        ControlReply::TakenBack { messages: back }
                    }
                    ControlRequest::Status => ControlReply::Status {
                        summary: Box::new(StatusSummary {
                            run: Some(run.clone()),
                            recent: vec!["from the run host".to_owned()],
                            ..StatusSummary::default()
                        }),
                    },
                    _ => ControlReply::Ok,
                }
            }
        };
        if write_frame(&mut writer, &reply(body)).await.is_err() {
            return;
        }
    }
}
