//! Replays a scripted solo run on the main screen, or as plain lines with `--plain`.
//! Run with `cargo run -p argonauts-tui --example demo [-- --plain] [--full|--quiet]`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use argonauts_core::{
    AgentEvent, BudgetAction, BudgetDimension, BudgetLimit, BudgetStatus, CardId, Checkpoint,
    CheckpointKind, CommitInfo, Config, DecidedBy, DecisionRecord, DiffStat, GateResult, MergeCard,
    MergeDecision, Phase, PhaseEvent, PhaseStatus, Report, ReportStatus, RunEvent, RunEventKind,
    RunFinished, RunId, RunOutcome, RunStarted, SessionEnded, SessionRole, SessionStarted, Spend,
    TodoItem, TodoStatus, UiConfig, Verbosity, WorkLimits, WorkUsed,
};
use argonauts_tui::{UiEvent, UiOptions, ask_merge, run_plain_stdio, run_terminal, wants_plain};
use serde_json::{Value, json};
use tokio::sync::mpsc;

const STEP: Duration = Duration::from_millis(250);
const BRANCH: &str = "argonauts/run-demo/card-001";
const DIFF: &str =
    "diff --git a/hello.txt b/hello.txt\n--- /dev/null\n+++ b/hello.txt\n@@ -0,0 +1 @@\n+hi\n";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let verbosity = if args.iter().any(|a| a == "--full") {
        Verbosity::Full
    } else if args.iter().any(|a| a == "--quiet") {
        Verbosity::Quiet
    } else {
        Verbosity::Normal
    };
    let options = UiOptions::from_config(&UiConfig {
        verbosity,
        ..UiConfig::default()
    });
    let plain = wants_plain(args.iter().any(|a| a == "--plain"));

    let (events, rx) = mpsc::unbounded_channel();
    let (commands, mut commands_rx) = mpsc::unbounded_channel();
    let ui = if plain {
        tokio::spawn(run_plain_stdio(rx, options))
    } else {
        tokio::spawn(run_terminal(rx, commands, options))
    };
    let conductor = tokio::spawn(async move {
        tokio::select! {
            () = play(&events) => {}
            _ = commands_rx.recv() => {
                send(&events, None, RunEventKind::RunFinished(RunFinished {
                    outcome: RunOutcome::Cancelled,
                    detail: Some("stopped from the screen".into()),
                }));
            }
        }
    });
    conductor.await?;
    ui.await??;
    Ok(())
}

fn run_id() -> RunId {
    RunId::new("run-demo").unwrap_or_else(|_| RunId::generate())
}

fn send(events: &mpsc::UnboundedSender<UiEvent>, card: Option<CardId>, kind: RunEventKind) {
    let source = match kind {
        RunEventKind::Agent(_) => "claude-code",
        _ => "conductor",
    };
    let _ = events.send(RunEvent::now(run_id(), card, source, kind).into());
}

struct Demo<'a> {
    events: &'a mpsc::UnboundedSender<UiEvent>,
    card: Option<CardId>,
}

impl Demo<'_> {
    fn send(&self, kind: RunEventKind) {
        send(self.events, self.card.clone(), kind);
    }

    async fn step(&self, kind: RunEventKind) {
        self.send(kind);
        tokio::time::sleep(STEP).await;
    }
}

async fn play(events: &mpsc::UnboundedSender<UiEvent>) {
    let demo = Demo {
        events,
        card: Some(CardId::numbered(1)),
    };
    let worktree = PathBuf::from("/tmp/argonauts-demo/worktrees/card-001");

    demo.step(run_started()).await;
    demo.step(phase(Phase::Worktree, PhaseStatus::Started))
        .await;
    demo.step(phase(Phase::Worktree, PhaseStatus::Finished))
        .await;
    demo.step(session_started(&worktree)).await;
    agent_session(&demo, &worktree).await;

    demo.step(RunEventKind::Report(report())).await;
    demo.step(RunEventKind::Commit(CommitInfo {
        branch: BRANCH.into(),
        sha: Some("9a8b7c6d".into()),
        message: "argonauts: card-001".into(),
        stat: stat(),
        by: None,
    }))
    .await;
    demo.step(phase(Phase::Gates, PhaseStatus::Started)).await;
    demo.step(RunEventKind::GateResult(gate())).await;
    demo.step(phase(Phase::Gates, PhaseStatus::Finished)).await;

    decide_the_card(&demo).await;
    tokio::time::sleep(STEP * 2).await;
}

async fn agent_session(demo: &Demo<'_>, worktree: &Path) {
    for word in "I'll check the repo, then write the file. ".split_inclusive(' ') {
        demo.send(RunEventKind::Agent(AgentEvent::TextDelta {
            text: word.into(),
            parent: None,
        }));
        tokio::time::sleep(STEP / 4).await;
    }
    let readme = json!({"file_path": worktree.join("README.md")});
    demo.step(tool_started("t1", "Read", readme)).await;
    demo.step(tool_done("t1")).await;
    let hello = json!({"file_path": worktree.join("hello.txt"), "content": "hi\n"});
    demo.step(tool_started("t2", "Write", hello)).await;
    demo.step(tool_done("t2")).await;
    demo.step(RunEventKind::Agent(AgentEvent::Todo {
        items: vec![
            TodoItem {
                text: "write hello.txt".into(),
                status: TodoStatus::Completed,
            },
            TodoItem {
                text: "report".into(),
                status: TodoStatus::InProgress,
            },
        ],
    }))
    .await;
    demo.step(RunEventKind::Agent(AgentEvent::Usage {
        input_tokens: 12_000,
        output_tokens: 400,
        cached_tokens: 3_000,
        cost_usd: Some(0.0123),
        cumulative: true,
    }))
    .await;
    demo.step(RunEventKind::Agent(AgentEvent::Message {
        text: "Created hello.txt containing hi.".into(),
        parent: None,
    }))
    .await;
    demo.step(RunEventKind::SessionEnded(SessionEnded {
        exit_code: Some(0),
        signal: None,
        duration_ms: 4_200,
        stopped_by: None,
    }))
    .await;
}

async fn decide_the_card(demo: &Demo<'_>) {
    let card = merge_card();
    demo.send(RunEventKind::Checkpoint(Checkpoint::Merge(card.clone())));
    let decision = ask_merge(demo.events, CardId::numbered(1), card, Some(DIFF.into())).await;
    let outcome = match decision {
        MergeDecision::Approve => RunOutcome::Merged,
        _ => RunOutcome::BranchLeft,
    };
    demo.send(RunEventKind::Decision(DecisionRecord {
        checkpoint: CheckpointKind::Merge,
        decision,
        by: DecidedBy::User,
        commit: None,
    }));
    send(
        demo.events,
        None,
        RunEventKind::RunFinished(RunFinished {
            outcome,
            detail: None,
        }),
    );
}

fn run_started() -> RunEventKind {
    RunEventKind::RunStarted(RunStarted {
        task: "Create a file named hello.txt containing the word hi".into(),
        mode: "solo".into(),
        agent: "claude-code".into(),
        model: Some("haiku".into()),
        effort: Some("low".into()),
        repo_root: PathBuf::from("/tmp/argonauts-demo/repo"),
        base_branch: Some("main".into()),
        base_commit: "3f9c2e1d8a7b".into(),
        config_hash: "0".repeat(64),
        config: Config::default().to_json(),
        version: "0.0.1".into(),
    })
}

fn session_started(worktree: &Path) -> RunEventKind {
    RunEventKind::SessionStarted(SessionStarted {
        agent: "claude-code".into(),
        model: Some("haiku".into()),
        effort: Some("low".into()),
        role: SessionRole::Build,
        program: "claude".into(),
        args: vec!["-p".into()],
        cwd: worktree.to_path_buf(),
        prompt: "demo".into(),
        goal_hash: Some("demo-goal".into()),
    })
}

fn phase(phase: Phase, status: PhaseStatus) -> RunEventKind {
    RunEventKind::Phase(PhaseEvent {
        phase,
        status,
        detail: None,
    })
}

fn tool_started(id: &str, name: &str, input: Value) -> RunEventKind {
    RunEventKind::Agent(AgentEvent::ToolStarted {
        id: id.into(),
        name: name.into(),
        input,
        parent: None,
    })
}

fn tool_done(id: &str) -> RunEventKind {
    RunEventKind::Agent(AgentEvent::ToolFinished {
        id: id.into(),
        ok: true,
        output: Some("ok".into()),
        parent: None,
    })
}

fn report() -> Report {
    Report {
        status: ReportStatus::Done,
        summary: "Created hello.txt containing the word hi.".into(),
        files_changed: vec!["hello.txt".into()],
        tests_run: Vec::new(),
        concerns: Vec::new(),
    }
}

fn stat() -> DiffStat {
    DiffStat {
        files_changed: 1,
        insertions: 1,
        deletions: 0,
    }
}

fn gate() -> GateResult {
    GateResult {
        command: vec!["test".into(), "-f".into(), "hello.txt".into()],
        passed: true,
        exit_code: Some(0),
        duration_ms: 12,
        output: String::new(),
    }
}

fn merge_card() -> MergeCard {
    MergeCard {
        branch: BRANCH.into(),
        target_branch: Some("main".into()),
        stat: stat(),
        gates: vec![gate()],
        report: Some(report()),
        budget: Some(BudgetStatus {
            level: BudgetAction::Continue,
            limit: BudgetLimit::Usd(3.0),
            spend: Spend {
                input_tokens: 12_000,
                output_tokens: 400,
                cached_tokens: 3_000,
                cost_usd: 0.0123,
                ..Spend::default()
            },
            fraction_used: Some(0.004),
            work: WorkUsed {
                minutes: 0.3,
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
