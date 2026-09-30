//! Scripted run events with a fixed clock, shared by the model, view, and plain tests.

use std::path::PathBuf;

use argonauts_core::{
    AgentEvent, BudgetAction, BudgetDimension, BudgetLimit, BudgetStatus, CardId, Checkpoint,
    CheckpointKind, CommitInfo, Config, DecidedBy, DecisionRecord, DiffStat, GateResult, MergeCard,
    MergeDecision, Phase, PhaseEvent, PhaseStatus, Report, ReportStatus, RunEvent, RunEventKind,
    RunFinished, RunId, RunOutcome, RunStarted, SessionEnded, SessionRole, SessionStarted, Spend,
    StopCause, TodoItem, TodoStatus, WorkLimits, WorkUsed,
};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;

pub(crate) const WORKTREE: &str =
    "/home/you/.argonauts/worktrees/api/run-20260929-101500-a1b2/card-001";

pub(crate) struct Script {
    pub events: Vec<RunEvent>,
    run_id: RunId,
    clock: OffsetDateTime,
}

impl Script {
    pub(crate) fn new() -> Self {
        Self {
            events: Vec::new(),
            run_id: RunId::new("run-20260929-101500-a1b2").unwrap(),
            clock: datetime!(2026-09-29 10:15:00 UTC),
        }
    }

    pub(crate) fn now(&self) -> OffsetDateTime {
        self.clock
    }

    pub(crate) fn wait_ms(&mut self, ms: i64) {
        self.clock += time::Duration::milliseconds(ms);
    }

    fn push(&mut self, source: &str, card: bool, kind: RunEventKind) {
        self.events.push(RunEvent {
            ts: self.clock,
            run_id: self.run_id.clone(),
            card_id: card.then(|| CardId::numbered(1)),
            source: source.to_owned(),
            kind,
        });
    }

    pub(crate) fn run_started(&mut self, task: &str) {
        self.push(
            "conductor",
            false,
            RunEventKind::RunStarted(RunStarted {
                task: task.to_owned(),
                mode: "solo".into(),
                agent: "claude-code".into(),
                model: Some("haiku".into()),
                effort: Some("low".into()),
                repo_root: PathBuf::from("/home/you/code/api"),
                base_branch: Some("main".into()),
                base_commit: "3f9c2e1d8a7b6c5d4e3f2a1b0c9d8e7f6a5b4c3d".into(),
                config_hash: "0".repeat(64),
                config: Config::default().to_json(),
                version: "0.0.1".into(),
            }),
        );
    }

    pub(crate) fn phase(&mut self, phase: Phase, status: PhaseStatus, detail: Option<&str>) {
        self.push(
            "conductor",
            true,
            RunEventKind::Phase(PhaseEvent {
                phase,
                status,
                detail: detail.map(str::to_owned),
            }),
        );
    }

    pub(crate) fn session_started(&mut self) {
        self.session_started_as("claude-code");
    }

    pub(crate) fn session_started_as(&mut self, agent: &str) {
        self.push(
            agent,
            true,
            RunEventKind::SessionStarted(SessionStarted {
                agent: agent.into(),
                model: Some("haiku".into()),
                effort: Some("low".into()),
                role: SessionRole::Build,
                program: "claude".into(),
                args: vec!["-p".into(), "--model".into(), "haiku".into()],
                cwd: PathBuf::from(WORKTREE),
                prompt: "Create a file named hello.txt containing the word hi".into(),
                goal_hash: Some("9f3c".into()),
            }),
        );
    }

    pub(crate) fn agent(&mut self, event: AgentEvent) {
        self.push("claude-code", true, RunEventKind::Agent(event));
    }

    pub(crate) fn tool(&mut self, id: &str, name: &str, input: Value) {
        self.agent(AgentEvent::ToolStarted {
            id: id.into(),
            name: name.into(),
            input,
            parent: None,
        });
    }

    pub(crate) fn finish_tool(&mut self, id: &str, ok: bool, output: Option<&str>) {
        self.agent(AgentEvent::ToolFinished {
            id: id.into(),
            ok,
            output: output.map(str::to_owned),
            parent: None,
        });
    }

    pub(crate) fn text(&mut self, text: &str) {
        self.agent(AgentEvent::TextDelta {
            text: text.into(),
            parent: None,
        });
    }

    pub(crate) fn message(&mut self, text: &str) {
        self.agent(AgentEvent::Message {
            text: text.into(),
            parent: None,
        });
    }

    pub(crate) fn usage(&mut self, input: u64, cached: u64, output: u64, cost: Option<f64>) {
        self.agent(AgentEvent::Usage {
            input_tokens: input,
            output_tokens: output,
            cached_tokens: cached,
            cost_usd: cost,
            cumulative: true,
        });
    }

    pub(crate) fn session_ended(&mut self, exit_code: Option<i32>, stopped_by: Option<StopCause>) {
        self.push(
            "conductor",
            true,
            RunEventKind::SessionEnded(SessionEnded {
                exit_code,
                signal: None,
                duration_ms: 38_400,
                stopped_by,
            }),
        );
    }

    pub(crate) fn report(&mut self) {
        self.push("conductor", true, RunEventKind::Report(report()));
    }

    pub(crate) fn commit(&mut self) {
        self.push(
            "conductor",
            true,
            RunEventKind::Commit(CommitInfo {
                branch: branch(),
                sha: Some("9a8b7c6d5e4f".into()),
                message: "argonauts: card-001 Create a file named hello.txt".into(),
                stat: stat(),
                by: None,
            }),
        );
    }

    pub(crate) fn gate(&mut self, command: &[&str], passed: bool, output: &str) {
        self.push(
            "conductor",
            true,
            RunEventKind::GateResult(gate(command, passed, output)),
        );
    }

    pub(crate) fn checkpoint(&mut self) {
        self.push(
            "conductor",
            true,
            RunEventKind::Checkpoint(Checkpoint::Merge(merge_card())),
        );
    }

    pub(crate) fn decision(&mut self, decision: MergeDecision, by: DecidedBy) {
        self.push(
            "conductor",
            true,
            RunEventKind::Decision(DecisionRecord {
                checkpoint: CheckpointKind::Merge,
                decision,
                by,
                commit: Some("3f2a9c1d0b7e".into()),
            }),
        );
    }

    pub(crate) fn budget(&mut self, level: BudgetAction, fraction: f64) {
        self.push(
            "conductor",
            true,
            RunEventKind::Budget(BudgetStatus {
                level,
                limit: BudgetLimit::Usd(3.0),
                spend: Spend {
                    cost_usd: 3.0 * fraction,
                    ..Spend::default()
                },
                fraction_used: Some(fraction),
                work: WorkUsed::default(),
                work_limits: WorkLimits::default(),
                binding: Some(BudgetDimension::Spend),
            }),
        );
    }

    pub(crate) fn push_note(&mut self, text: &str) {
        self.push(
            "conductor",
            false,
            RunEventKind::Note(argonauts_core::Note {
                level: argonauts_core::NoteLevel::Info,
                text: text.to_owned(),
            }),
        );
    }

    pub(crate) fn finished(&mut self, outcome: RunOutcome, detail: Option<&str>) {
        self.push(
            "conductor",
            false,
            RunEventKind::RunFinished(RunFinished {
                outcome,
                detail: detail.map(str::to_owned),
            }),
        );
    }

    pub(crate) fn solo_until_working() -> Self {
        let mut s = Self::new();
        s.run_started("Create a file named hello.txt containing the word hi");
        s.phase(Phase::Intake, PhaseStatus::Started, None);
        s.phase(Phase::Intake, PhaseStatus::Finished, None);
        s.phase(Phase::Worktree, PhaseStatus::Started, None);
        s.wait_ms(300);
        s.phase(
            Phase::Worktree,
            PhaseStatus::Finished,
            Some("argonauts/run-20260929-101500-a1b2/card-001"),
        );
        s.phase(Phase::Agent, PhaseStatus::Started, None);
        s.session_started();
        s.agent(AgentEvent::Init {
            session_id: Some("5e1c".into()),
            model: Some("claude-haiku-4-5".into()),
            tools: vec!["Read".into(), "Write".into()],
        });
        s.wait_ms(2_000);
        s.message("I'll look at the repo first.");
        s.tool(
            "t1",
            "Read",
            json!({"file_path": format!("{WORKTREE}/README.md")}),
        );
        s.finish_tool("t1", true, Some("# api"));
        s.tool("t2", "Glob", json!({"pattern": "*.txt"}));
        s.finish_tool("t2", true, Some(""));
        s.wait_ms(4_000);
        s.tool(
            "t3",
            "Write",
            json!({"file_path": format!("{WORKTREE}/hello.txt"), "content": "hi\n"}),
        );
        s.finish_tool("t3", true, Some("File created"));
        s.agent(AgentEvent::Todo {
            items: vec![
                TodoItem {
                    text: "create hello.txt".into(),
                    status: TodoStatus::Completed,
                },
                TodoItem {
                    text: "check the file".into(),
                    status: TodoStatus::InProgress,
                },
                TodoItem {
                    text: "report".into(),
                    status: TodoStatus::Pending,
                },
            ],
        });
        s.usage(12_000, 3_000, 400, Some(0.0123));
        s.tool("t4", "Bash", json!({"command": "cat hello.txt"}));
        s.text("The file is in place. Checking");
        s.wait_ms(6_200);
        s
    }

    /// A whole solo run that ends with the branch left by config.
    pub(crate) fn solo_finished() -> Self {
        let mut s = Self::solo_until_working();
        s.finish_tool("t4", true, Some("hi"));
        s.message("Created hello.txt containing hi.");
        s.agent(AgentEvent::Result {
            text: Some("Created hello.txt containing hi.".into()),
            structured: Some(json!({"status": "done"})),
            is_error: false,
        });
        s.wait_ms(26_000);
        s.session_ended(Some(0), None);
        s.phase(Phase::Agent, PhaseStatus::Finished, None);
        s.report();
        s.phase(Phase::Commit, PhaseStatus::Started, None);
        s.commit();
        s.phase(Phase::Commit, PhaseStatus::Finished, None);
        s.phase(Phase::Gates, PhaseStatus::Started, None);
        s.gate(&["test", "-f", "hello.txt"], true, "");
        s.phase(Phase::Gates, PhaseStatus::Finished, None);
        s.phase(Phase::Decision, PhaseStatus::Started, None);
        s.checkpoint();
        s
    }
}

pub(crate) fn solo_run_events() -> Vec<RunEvent> {
    let mut s = Script::solo_finished();
    s.decision(MergeDecision::Leave, DecidedBy::Config);
    s.phase(Phase::Decision, PhaseStatus::Finished, None);
    s.finished(RunOutcome::BranchLeft, Some("merge.mode is leave"));
    s.events
}

pub(crate) fn branch() -> String {
    "argonauts/run-20260929-101500-a1b2/card-001".into()
}

pub(crate) fn stat() -> DiffStat {
    DiffStat {
        files_changed: 1,
        insertions: 1,
        deletions: 0,
    }
}

pub(crate) fn report() -> Report {
    Report {
        status: ReportStatus::Done,
        summary: "Created hello.txt containing the word hi.".into(),
        files_changed: vec!["hello.txt".into()],
        tests_run: vec!["test -f hello.txt: passed".into()],
        concerns: Vec::new(),
    }
}

pub(crate) fn gate(command: &[&str], passed: bool, output: &str) -> GateResult {
    GateResult {
        command: command.iter().map(|c| (*c).to_owned()).collect(),
        passed,
        exit_code: Some(if passed { 0 } else { 1 }),
        duration_ms: 40,
        output: output.to_owned(),
    }
}

pub(crate) fn merge_card() -> MergeCard {
    MergeCard {
        branch: branch(),
        target_branch: Some("main".into()),
        stat: stat(),
        gates: vec![gate(&["test", "-f", "hello.txt"], true, "")],
        report: Some(report()),
        budget: Some(BudgetStatus {
            level: BudgetAction::Continue,
            limit: BudgetLimit::Usd(3.0),
            spend: Spend {
                input_tokens: 12_000,
                cached_tokens: 3_000,
                output_tokens: 400,
                cost_usd: 0.0123,
                ..Spend::default()
            },
            fraction_used: Some(0.0041),
            work: WorkUsed {
                minutes: 0.4,
                tool_calls: 2,
            },
            work_limits: WorkLimits {
                max_minutes: Some(60.0),
                max_tool_calls: Some(300),
            },
            binding: Some(BudgetDimension::Spend),
        }),
        stopped_by: None,
    }
}

pub(crate) const DIFF: &str = "diff --git a/hello.txt b/hello.txt\nnew file mode 100644\n--- /dev/null\n+++ b/hello.txt\n@@ -0,0 +1 @@\n+hi\n";
