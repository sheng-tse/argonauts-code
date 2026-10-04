//! Events, run directories, a home short enough for socket paths, and a fake supervisor that
//! answers on a run's socket, for the client's and the views' tests.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use argonauts_core::api::StatusSummary;
use argonauts_core::control::{ControlReply, ControlRequest, Frame};
use argonauts_core::events::card::{CardBudget, CardEvent, CardKind, CardSpec};
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointDecided, CheckpointEvent, CheckpointKind, CheckpointOpened,
    CheckpointPacket, FreeText, PlanPacket,
};
use argonauts_core::events::plan::{BudgetPlan, TaskGraph};
use argonauts_core::events::run::{Party, RunLifecycle, RunMode, RunStarted};
use argonauts_core::events::session::{
    Delivery, EndReason, InputPart, InputPartKind, LaunchRecord, PromptVia, QueuedMessage,
    SessionEnd, SessionEvent, SessionInput, SessionStarted,
};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::input::MentionTarget;
use argonauts_core::names::{CheckpointId, MessageId, RoleName, SessionKey, VendorId, WorkerRef};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::budget::{BudgetAmount, BudgetUnit};
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::worker::{Access, WorkerKind};
use argonauts_core::{AgentEvent, CardId, Paths, RunId, Spend};
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
        card_id: None,
        session: None,
        source: "test".to_owned(),
        kind,
    }
}

pub(crate) fn session_event(
    seq: u64,
    session: &SessionKey,
    card: Option<&CardId>,
    kind: RunEventKind,
) -> RunEvent {
    RunEvent {
        session: Some(session.clone()),
        card_id: card.cloned(),
        ..event(seq, kind)
    }
}

pub(crate) fn card_event(seq: u64, card: &CardId, kind: CardEvent) -> RunEvent {
    RunEvent {
        card_id: Some(card.clone()),
        ..event(seq, RunEventKind::Card(kind))
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
    card: Option<&CardId>,
    role: RoleName,
    worker: &str,
) -> RunEvent {
    let started = SessionStarted {
        worker: WorkerRef::new(worker, None),
        vendor: VendorId(worker.to_owned()),
        family: worker.to_owned(),
        kind: WorkerKind::Harness,
        role,
        step: None,
        access: Access::Build,
        lifetime: SessionLifetime::Card,
        resumed_from: None,
        follow_key: None,
        workflow_step: None,
        label: None,
    };
    session_event(
        seq,
        session,
        card,
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
        vendor_session: None,
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

pub(crate) fn run_started(seq: u64, request: &str, front_end: Option<&str>) -> RunEvent {
    let started = RunStarted {
        request: request.to_owned(),
        title: request.to_owned(),
        mode: RunMode::Solo,
        forced: false,
        preset: "solo".to_owned(),
        estimate: None,
        triage: None,
        chat_turn: None,
        continues: None,
        repo_root: PathBuf::from("/code/app"),
        base_branch: Some("main".to_owned()),
        base_commit: "abc123".to_owned(),
        uncommitted: Vec::new(),
        uncommitted_included: false,
        settings_sha256: String::new(),
        settings: serde_json::json!({}),
        overrides: Vec::new(),
        harness_versions: BTreeMap::new(),
        version: "0.0.1".to_owned(),
        front_end: front_end.map(str::to_owned),
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

pub(crate) fn card_spec(n: usize) -> CardSpec {
    CardSpec {
        id: CardId::new(format!("card-{n:03}")).unwrap(),
        title: format!("Card {n}"),
        task: String::new(),
        kind: CardKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: Vec::new(),
        owner: None,
        reviewers: Vec::new(),
        budget: CardBudget::default(),
        predictions: None,
        item: None,
    }
}

pub(crate) fn opened(
    seq: u64,
    id: &CheckpointId,
    kind: CheckpointKind,
    card: Option<&CardId>,
    packet: CheckpointPacket,
) -> RunEvent {
    let opened = CheckpointOpened {
        id: id.clone(),
        kind,
        title: "checkpoint".to_owned(),
        card: card.cloned(),
        packet,
        evidence_sha256: "evidence".to_owned(),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        options: Vec::new(),
        free_text: FreeText::None,
        needs_you: true,
    };
    event(
        seq,
        RunEventKind::Checkpoint(CheckpointEvent::Opened(Box::new(opened))),
    )
}

pub(crate) fn plan_checkpoint(seq: u64, id: &CheckpointId, cards: usize) -> RunEvent {
    let packet = PlanPacket {
        summary: "the plan".to_owned(),
        judging: None,
        near_tie: false,
        decisions: Vec::new(),
        graph: TaskGraph {
            cards: (1..=cards).map(card_spec).collect(),
            critical_path: Vec::new(),
        },
        acceptance_tests: Vec::new(),
        budget: BudgetPlan {
            lines: Vec::new(),
            total: BudgetAmount {
                unit: BudgetUnit::Usd,
                amount: 10.0,
            },
            estimate_low: 1.0,
            estimate_high: 2.0,
        },
        new_dependencies: Vec::new(),
        flags: Vec::new(),
        workflow: None,
    };
    opened(
        seq,
        id,
        CheckpointKind::Plan,
        None,
        CheckpointPacket::Plan(Box::new(packet)),
    )
}

pub(crate) fn checkpoint_decided(
    seq: u64,
    id: &CheckpointId,
    action: AnswerAction,
    by: Party,
    text: Option<&str>,
) -> RunEvent {
    let decided = CheckpointDecided {
        id: id.clone(),
        action,
        text: text.map(str::to_owned),
        by,
        evidence_sha256: "evidence".to_owned(),
        shadow: None,
        cards: Vec::new(),
    };
    event(
        seq,
        RunEventKind::Checkpoint(CheckpointEvent::Decided(decided)),
    )
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

/// A repo and an Argonauts home under `/tmp`, since a socket path must stay under about 100
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

/// Stands in for `argonauts supervise`: launching binds the run's socket and answers there.
#[derive(Default)]
pub(crate) struct FakeSupervisor {
    pub launches: Mutex<Vec<RunId>>,
    pub script: Arc<Script>,
    servers: Mutex<Vec<ServerHandle>>,
}

impl FakeSupervisor {
    pub(crate) fn launches(&self) -> Vec<RunId> {
        self.launches.lock().unwrap().clone()
    }
}

impl Launcher for FakeSupervisor {
    fn launch(&self, _repo_root: &Path, run: &RunId, run_dir: &Path) -> anyhow::Result<()> {
        self.launches.lock().unwrap().push(run.clone());
        let server = serve(run_dir, run.clone(), Arc::clone(&self.script));
        self.servers.lock().unwrap().push(server);
        Ok(())
    }
}

/// What a fake supervisor sends and what it was asked.
#[derive(Default)]
pub(crate) struct Script {
    /// Sent in full on every `Subscribe`, whatever it asks for, so the client must dedupe.
    pub events: Mutex<Vec<RunEvent>>,
    pub requests: Mutex<Vec<ControlRequest>>,
    /// Messages the supervisor still holds, which `TakeBack` returns.
    pub queued: Mutex<Vec<QueuedMessage>>,
}

pub(crate) struct ServerHandle {
    shutdown: CancellationToken,
    socket: PathBuf,
}

impl ServerHandle {
    /// Closes every connection and removes the socket, as an exiting supervisor does.
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
                            recent: vec!["from the supervisor".to_owned()],
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
