//! Made-up run logs, written event by event, each a minute after the one before unless told.

use std::path::Path;

use argonauts_core::events::agent_status::FallbackEvent;
use argonauts_core::events::council::{
    CouncilEvent, CouncilEventKind, CouncilMessage, MessageKind, Seat, SeatRole, Seating, Speaker,
};
use argonauts_core::events::run::{Party, ResumeCause, RunLifecycle, RunOutcome, RunResumed};
use argonauts_core::events::session::{
    Billing, CliAgent, CliWorkflowRun, EndReason, SessionEnd, SessionEvent, SessionStarted,
    SubagentStatus,
};
use argonauts_core::events::task::{
    CheckKind, CheckOutcome, ReviewEvent, TaskEvent, TaskKind, TaskLimit, TaskSpec, TaskState,
    TryCause,
};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{
    AgentId, MessageId, ModelRef, RoleName, SeatId, SessionKey, WorkflowId,
};
use argonauts_core::paths::EVENT_LOG_FILE;
use argonauts_core::session_runner::{Access, RunnerKind};
use argonauts_core::settings::fallback::FallbackMode;
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::steps::judgment::{ReviewOutput, ReviewVerdict, SettleOutput};
use argonauts_core::workflow::StepRef;
use argonauts_core::{AgentEvent, RunId, Spend, TaskId};
use time::{Duration, OffsetDateTime};

pub(super) const PROJECT: &str = "harbor-0123456789ab";

pub(super) struct Log {
    run: RunId,
    at: OffsetDateTime,
    events: Vec<RunEvent>,
}

pub(super) fn task_id(id: &str) -> TaskId {
    TaskId::new(id).unwrap()
}

pub(super) fn model(worker: &str) -> ModelRef {
    worker.parse().unwrap()
}

fn talk() -> StepRef {
    StepRef {
        workflow: WorkflowId::numbered(1),
        phase: 0,
        path: vec![0],
    }
}

impl Log {
    pub fn new(run: &str, at: OffsetDateTime) -> Self {
        Self {
            run: RunId::new(run).unwrap(),
            at,
            events: Vec::new(),
        }
    }

    pub fn run(&self) -> &str {
        self.run.as_str()
    }

    pub fn events(&self) -> &[RunEvent] {
        &self.events
    }

    /// Moves the clock on before the next event.
    pub fn later(&mut self, minutes: i64) -> &mut Self {
        self.at += Duration::minutes(minutes);
        self
    }

    pub fn at(&self) -> OffsetDateTime {
        self.at
    }

    fn push(&mut self, task: Option<&str>, session: Option<u32>, kind: RunEventKind) -> &mut Self {
        self.at += Duration::minutes(1);
        self.events.push(RunEvent {
            seq: self.events.len() as u64,
            ts: self.at,
            run_id: self.run.clone(),
            task_id: task.map(task_id),
            session: session.map(SessionKey::numbered),
            source: "argonauts-code".to_owned(),
            kind,
        });
        self
    }

    /// A session of `worker`, such as `claude/opus`, whose model `maker` trained.
    pub fn session(
        &mut self,
        n: u32,
        task: Option<&str>,
        role: RoleName,
        worker: &str,
        maker: &str,
    ) -> &mut Self {
        self.started(n, task, role, worker, maker, None, None)
    }

    pub fn local_session(&mut self, n: u32, task: &str, worker: &str, maker: &str) -> &mut Self {
        self.started(
            n,
            Some(task),
            RoleName::Worker,
            worker,
            maker,
            None,
            Some(Billing::Free),
        )
    }

    pub fn seat_session(&mut self, n: u32, seat: u32, worker: &str, maker: &str) -> &mut Self {
        let seat = Some(SeatId::numbered(seat));
        self.started(n, None, RoleName::Consultant, worker, maker, seat, None)
    }

    #[allow(clippy::too_many_arguments)]
    fn started(
        &mut self,
        n: u32,
        task: Option<&str>,
        role: RoleName,
        worker: &str,
        maker: &str,
        seat: Option<SeatId>,
        billing: Option<Billing>,
    ) -> &mut Self {
        let worker = model(worker);
        let started = SessionStarted {
            agent_id: AgentId(worker.agent.clone()),
            worker,
            provider: maker.to_owned(),
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
            seat,
            billing,
        };
        let kind = RunEventKind::Session(SessionEvent::Started(Box::new(started)));
        self.push(task, Some(n), kind)
    }

    /// One tool call of session `n`.
    pub fn acts(&mut self, n: u32, calls: u32) -> &mut Self {
        for call in 0..calls {
            let kind = RunEventKind::Agent(AgentEvent::ToolStarted {
                id: format!("call-{n}-{call}"),
                name: "Read".to_owned(),
                input: serde_json::json!({"file_path": "src/lib.rs"}),
                parent: None,
            });
            self.push(None, Some(n), kind);
        }
        self
    }

    /// Session `n` keeps working, a call every half hour, for `hours`.
    pub fn works(&mut self, n: u32, hours: i64) -> &mut Self {
        for _ in 0..hours * 2 {
            self.later(29).acts(n, 1);
        }
        self
    }

    pub fn ended(&mut self, n: u32, task: &str, reason: EndReason) -> &mut Self {
        let end = SessionEnd {
            cli_session: None,
            exit_code: None,
            signal: None,
            duration_ms: 0,
            reason,
            spend: Spend::default(),
            tool_calls: 0,
            result_text: None,
            structured: None,
            last_message: None,
            raw_log: None,
        };
        let kind = RunEventKind::Session(SessionEvent::Ended(Box::new(end)));
        self.push(Some(task), Some(n), kind)
    }

    pub fn task(&mut self, id: &str, worker: &str) -> &mut Self {
        self.task_of(id, worker, TaskKind::Build)
    }

    pub fn task_of(&mut self, id: &str, worker: &str, kind: TaskKind) -> &mut Self {
        let spec = TaskSpec {
            id: task_id(id),
            title: "Add a made-up feature".to_owned(),
            task: "Add a made-up feature".to_owned(),
            kind,
            owned_paths: Vec::new(),
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: Vec::new(),
            worker: Some(model(worker)),
            reviewers: Vec::new(),
            limit: TaskLimit::default(),
            predictions: None,
            item: None,
            way: None,
        };
        self.push(
            Some(id),
            None,
            RunEventKind::Task(TaskEvent::Created(Box::new(spec))),
        )
    }

    pub fn try_(&mut self, id: &str, n: u32, cause: TryCause, worker: &str) -> &mut Self {
        let kind = RunEventKind::Task(TaskEvent::Try {
            try_number: n,
            worker: model(worker),
            cause,
            told: Vec::new(),
        });
        self.push(Some(id), None, kind)
    }

    pub fn check(&mut self, id: &str, kind: CheckKind, passed: bool) -> &mut Self {
        self.check_of(Some(id), kind, passed)
    }

    /// The whole suite, in the run's final check.
    pub fn final_check(&mut self, passed: bool) -> &mut Self {
        self.check_of(None, CheckKind::FullSuite, passed)
    }

    fn check_of(&mut self, task: Option<&str>, kind: CheckKind, passed: bool) -> &mut Self {
        let check = CheckOutcome {
            name: "checks".to_owned(),
            kind,
            passed,
            command: None,
            exit_code: Some(if passed { 0 } else { 1 }),
            duration_ms: 1,
            summary: String::new(),
            output_tail: String::new(),
            violations: Vec::new(),
            sandboxed: true,
            skipped: false,
        };
        self.push(task, None, RunEventKind::Check(check))
    }

    pub fn review(
        &mut self,
        id: &str,
        n: u32,
        reviewer: &str,
        verdict: ReviewVerdict,
    ) -> &mut Self {
        let kind = RunEventKind::Review(ReviewEvent::Reviewed {
            reviewer: model(reviewer),
            blind: true,
            round: 1,
            review: ReviewOutput {
                verdict,
                summary: "Reads well.".to_owned(),
                comments: Vec::new(),
                tamper_suspected: false,
            },
            picked_by: None,
        });
        self.push(Some(id), Some(n), kind)
    }

    pub fn settle(&mut self, id: &str, judge: &str, verdict: ReviewVerdict) -> &mut Self {
        let kind = RunEventKind::Review(ReviewEvent::Settled {
            judge: model(judge),
            ruling: SettleOutput {
                verdict,
                reasons: Vec::new(),
                required_changes: Vec::new(),
            },
            picked_by: None,
        });
        self.push(Some(id), None, kind)
    }

    pub fn tests_written(&mut self, id: &str, reviewer: &str, for_task: &str) -> &mut Self {
        let kind = RunEventKind::Review(ReviewEvent::TestsWritten {
            reviewer: model(reviewer),
            for_task: task_id(for_task),
            files: vec!["tests/made_up.rs".to_owned()],
        });
        self.push(Some(id), None, kind)
    }

    pub fn merged(&mut self, id: &str) -> &mut Self {
        let kind = RunEventKind::Task(TaskEvent::State {
            from: Some(TaskState::Merging),
            to: TaskState::Merged,
            reason: "merged".to_owned(),
        });
        self.push(Some(id), None, kind)
    }

    /// A task built on its first try, checked, and merged.
    pub fn clean_task(&mut self, id: &str, worker: &str) -> &mut Self {
        self.task(id, worker)
            .try_(id, 1, TryCause::First, worker)
            .check(id, CheckKind::Command, true)
            .merged(id)
    }

    pub fn fell_back(&mut self, id: &str, to: &str) -> &mut Self {
        let kind = RunEventKind::Fallback(FallbackEvent::FellBack {
            task: task_id(id),
            to: model(to),
            mode: FallbackMode::Auto,
            by: Party::Rules,
            context: None,
        });
        self.push(Some(id), None, kind)
    }

    pub fn finished(&mut self, outcome: RunOutcome) -> &mut Self {
        let kind = RunEventKind::Run(RunLifecycle::Finished {
            outcome,
            detail: None,
        });
        self.push(None, None, kind)
    }

    pub fn resumed(&mut self, cause: ResumeCause) -> &mut Self {
        let kind = RunEventKind::Run(RunLifecycle::Resumed(RunResumed {
            cause,
            settings_changed: false,
            cli_versions_changed: Vec::new(),
            main_moved_by: None,
            sessions: Vec::new(),
        }));
        self.push(None, None, kind)
    }

    /// Something from you: Enter, before the run's safe point.
    pub fn you(&mut self) -> &mut Self {
        self.push(None, None, RunEventKind::Run(RunLifecycle::PauseCancelled))
    }

    pub fn seated(&mut self, seats: &[&str]) -> &mut Self {
        let seats = (1u32..)
            .zip(seats)
            .map(|(n, worker)| Seat {
                id: SeatId::numbered(n),
                model: model(worker),
                role: SeatRole::default(),
                number: None,
            })
            .collect();
        self.council(CouncilEventKind::Seated(Seating { seats, notes: None }))
    }

    pub fn seat_writes(&mut self, seat: u32, kind: MessageKind) -> &mut Self {
        let message = CouncilMessage {
            id: MessageId::numbered(self.events.len() as u32),
            from: Speaker::Seat(SeatId::numbered(seat)),
            kind,
            text: Vec::new(),
            reply_to: None,
            mentions: Vec::new(),
            stances: Vec::new(),
            hidden: 0,
            hidden_names: Vec::new(),
            guesses_hidden: 0,
            rewritten: false,
            declined: false,
            started_before: None,
        };
        self.council(CouncilEventKind::Message(Box::new(message)))
    }

    pub fn agreed(&mut self) -> &mut Self {
        self.council(CouncilEventKind::Agreed { points: 3 })
    }

    fn council(&mut self, kind: CouncilEventKind) -> &mut Self {
        let event = CouncilEvent { talk: talk(), kind };
        self.push(None, None, RunEventKind::Council(event))
    }

    /// Session `n`'s CLI runs a workflow of its own with `agents` agents of `model`.
    pub fn cli_workflow(&mut self, n: u32, agents: u32, model: &str) -> &mut Self {
        let agents = (1..=agents)
            .map(|index| CliAgent {
                index,
                label: format!("part:{index}"),
                model: Some(model.to_owned()),
                tool_calls: 2,
                try_number: 1,
                ..CliAgent::default()
            })
            .collect();
        let workflow = CliWorkflowRun {
            id: "made-up-workflow".to_owned(),
            call: None,
            name: "made-up-workflow".to_owned(),
            description: String::new(),
            phases: Vec::new(),
            agents,
            status: SubagentStatus::Running,
            tokens: 0,
            tool_calls: 0,
            duration_ms: 0,
            summary: None,
            started_at_ms: None,
        };
        let kind = RunEventKind::Agent(AgentEvent::Workflow {
            workflow: Box::new(workflow),
        });
        self.push(None, Some(n), kind)
    }

    /// Writes the log where the app keeps run `run` of the made-up project.
    pub fn write(&self, home: &Path) {
        let dir = home
            .join("state")
            .join(PROJECT)
            .join("runs")
            .join(self.run());
        std::fs::create_dir_all(&dir).unwrap();
        let mut text = String::new();
        for event in &self.events {
            text.push_str(&serde_json::to_string(event).unwrap());
            text.push('\n');
        }
        std::fs::write(dir.join(EVENT_LOG_FILE), text).unwrap();
    }
}
