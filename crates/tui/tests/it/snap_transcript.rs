//! What the transcript prints for each kind of event, at 80 columns, and that it holds its
//! shape at every width with awkward text.

use crate::common;
use std::path::PathBuf;
use std::time::Instant;

use argonauts_core::app::{
    AgentReady, AppEvent, ConversationEvent, HeaderAndBottomLine, TurnReply,
};
use argonauts_core::events::agent_status::{AgentStatusEvent, FallbackEvent, FallbackPlan};
use argonauts_core::events::goal::{Criterion, GoalEvent, GoalFile, GoalSource};
use argonauts_core::events::plan::{
    DivideEvent, JudgingResult, MappedPosition, MappedTopic, PlanEvent, TaskGraph,
};
use argonauts_core::events::run::{
    Party, PhaseChange, RunLifecycle, RunMode, RunOutcome, RunPhase, RunStarted,
};
use argonauts_core::events::task::{
    CheckKind, CheckOutcome, MergeEvent, TaskEvent, TaskKind, TaskLimit, TaskSpec, TaskState,
};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::limits::{AgentHealth, HealthState};
use argonauts_core::names::{AgentId, DraftId, ModelRef, TurnId};
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::fallback::{FallbackContext, FallbackMode, FallbackStrategy};
use argonauts_core::settings::protocol::JudgingMethod;
use argonauts_core::settings::ui::{NarratorMode, UiSettings, VerbTheme};
use argonauts_core::steps::understand::Reading;
use argonauts_core::{AgentEvent, DiffStat, PhaseStatus, RunId, Spend, TaskId, Verbosity};
use argonauts_tui::bridge::agents::AgentBlocks;
use argonauts_tui::bridge::transcript::{ConversationTurns, Transcript};
use argonauts_tui::bridge::{Cx, Effects};
use argonauts_tui::cell::ToolKind;
use argonauts_tui::format::WallClock;
use argonauts_tui::model::ViewModel;
use argonauts_tui::stability::{self, WIDTHS, samples};
use argonauts_tui::state::{AppState, Replay, RunState, TaskRow};
use argonauts_tui::theme::Role;
use common::{fg, still, style_of, text, theme};
use ratatui::text::Line;
use time::OffsetDateTime;
use time::macros::datetime;

/// 10:42 AM where the user is, seven hours behind UTC.
const NOW: OffsetDateTime = datetime!(2026-09-30 10:42 -7);

struct TestDriver {
    model: ViewModel,
    state: AppState,
    settings: UiSettings,
    transcript: Transcript,
    agents: AgentBlocks,
}

impl TestDriver {
    fn new() -> Self {
        let mut transcript = Transcript::with_clock(WallClock::pinned(NOW));
        transcript.conversation = ConversationTurns::with_tool_kinds(|_, name, _| match name {
            "Read" => ToolKind::Read,
            "Grep" => ToolKind::Search,
            _ => ToolKind::Other,
        });
        Self {
            model: ViewModel::default(),
            state: AppState::default(),
            settings: UiSettings::default(),
            transcript,
            agents: AgentBlocks::default(),
        }
    }

    /// Folds the replay markers the way `AppState::apply` does, then offers the event.
    fn send(&mut self, event: AppEvent) {
        match &event {
            AppEvent::Attached { summary, .. } => {
                self.state.replay = Replay::Replaying {
                    run: summary.run.clone(),
                };
            }
            AppEvent::ReplayDone { .. } => self.state.replay = Replay::Live,
            _ => {}
        }
        let mut out = Effects::default();
        let mut cx = Cx {
            model: &mut self.model,
            state: &self.state,
            settings: &self.settings,
            now: Instant::now(),
            out: &mut out,
        };
        self.transcript.on_event(&event, &mut cx);
        self.agents.on_event(&event, &mut cx);
    }

    fn send_all(&mut self, events: impl IntoIterator<Item = AppEvent>) {
        for event in events {
            self.send(event);
        }
    }

    fn printed(&mut self, width: u16) -> Vec<Line<'static>> {
        let theme = theme();
        self.model.history.drain_printable(&still(&theme, width))
    }
}

type Scenario = fn(&mut TestDriver, Option<&str>);

fn render(scenario: Scenario, text: Option<&str>, width: u16) -> Vec<Line<'static>> {
    let mut driver = TestDriver::new();
    scenario(&mut driver, text);
    driver.printed(width)
}

/// The snapshot at 80 columns, and the frame check at every width with every sample in place
/// of the scenario's own text.
fn snapshot(name: &str, scenario: Scenario) {
    let lines = render(scenario, None, 80);
    assert!(!lines.is_empty(), "{name} printed nothing");
    insta::assert_snapshot!(name, text(&lines));
    let long = samples::long_word();
    let texts = [
        None,
        Some(samples::CJK),
        Some(samples::EMOJI),
        Some(samples::LONG_PATH),
        Some(samples::EMPTY),
        Some(long.as_str()),
    ];
    for sample in texts {
        for width in WIDTHS {
            stability::assert_lines(&render(scenario, sample, width), width);
        }
    }
}

fn run_id() -> RunId {
    RunId::new("run-01").expect("a valid run id")
}

fn task(n: u32) -> TaskId {
    TaskId::numbered(n)
}

fn worker(driver: &str, model: &str) -> ModelRef {
    ModelRef::new(driver, Some(model.to_owned()))
}

fn at(ts: OffsetDateTime, task_id: Option<TaskId>, kind: RunEventKind) -> AppEvent {
    AppEvent::Run {
        event: Box::new(RunEvent {
            seq: 0,
            ts,
            run_id: run_id(),
            task_id,
            session: None,
            source: "argonauts-code".into(),
            kind,
        }),
    }
}

fn run_event(task_id: Option<TaskId>, kind: RunEventKind) -> AppEvent {
    at(datetime!(2026-09-30 17:00 UTC), task_id, kind)
}

fn conversation(event: ConversationEvent) -> AppEvent {
    AppEvent::Conversation { event }
}

fn turn_started(n: u32, text: &str) -> AppEvent {
    conversation(ConversationEvent::TurnStarted {
        turn: TurnId::numbered(n),
        text: text.into(),
        worker: worker("claude-code", "haiku"),
        read_only: true,
        commit: "8b41d07".into(),
        uncommitted: vec![],
        settings_sha256: None,
        continues: false,
    })
}

fn agent(n: u32, event: AgentEvent) -> AppEvent {
    conversation(ConversationEvent::Agent {
        turn: TurnId::numbered(n),
        event,
    })
}

fn delta(n: u32, text: &str) -> AppEvent {
    agent(
        n,
        AgentEvent::TextDelta {
            text: text.into(),
            parent: None,
        },
    )
}

fn answered(n: u32, markdown: &str, seconds: u64) -> AppEvent {
    conversation(ConversationEvent::TurnFinished {
        turn: TurnId::numbered(n),
        reply: TurnReply::Answer {
            markdown: markdown.into(),
        },
        spend: Spend::default(),
        elapsed_ms: seconds * 1_000,
    })
}

fn check(name: &str, kind: CheckKind, passed: bool, summary: &str) -> RunEventKind {
    RunEventKind::Check(CheckOutcome {
        name: name.into(),
        kind,
        passed,
        command: None,
        exit_code: None,
        duration_ms: 1_200,
        summary: summary.into(),
        output_tail: String::new(),
        violations: vec![],
        sandboxed: true,
        skipped: false,
    })
}

fn checking_done() -> RunEventKind {
    RunEventKind::Task(TaskEvent::State {
        from: Some(TaskState::Checking),
        to: TaskState::InReview,
        reason: String::new(),
    })
}

fn started(mode: RunMode, request: &str) -> RunEventKind {
    RunEventKind::Run(RunLifecycle::Started(Box::new(RunStarted {
        request: request.into(),
        title: request.into(),
        mode,
        forced: false,
        workflow: "council".into(),
        estimate: None,
        captain_or_crew: None,
        conversation_turn: None,
        continues: None,
        repo_root: PathBuf::from("/repo"),
        base_branch: Some("main".into()),
        base_commit: "8b41d07c2e".into(),
        uncommitted: vec!["notes.txt".into(), "scratch.rs".into()],
        uncommitted_included: false,
        settings_sha256: String::new(),
        settings: serde_json::Value::Null,
        overrides: vec![],
        cli_versions: Default::default(),
        version: "0.0.1".into(),
        outside_agent: None,
        instructions: Vec::new(),
        required_steps: Vec::new(),
        starts_from: None,
    })))
}

fn task_row(n: u32, title: &str, worker: ModelRef, diff: Option<DiffStat>) -> TaskRow {
    TaskRow {
        id: task(n),
        title: title.into(),
        state: Some(TaskState::Merged),
        worker: Some(worker),
        try_number: 1,
        diff,
        fallback_for: None,
        debt: vec![],
        checkpoints: 0,
        rotations: 0,
        last_check: None,
        reviews: 0,
        waiting_on_agent: None,
        item: None,
    }
}

#[test]
fn startup_header() {
    snapshot("startup_header", |driver, text| {
        // The controller sends the welcome first and the bottom line with the main agent
        // just after it.
        let status = HeaderAndBottomLine {
            title: "argonauts".into(),
            repo: "~/github/argonauts".into(),
            workflow: "council".into(),
            ultracode: false,
            ultra: None,
            run: None,
            main_agent: worker("claude-code", "haiku-4.5"),
            plan_chips: vec![],
            line: Default::default(),
            spent: None,
            limit: None,
            narrator: NarratorMode::OnDemand,
            approval_mode: ApprovalMode::Supervised,
            awake: None,
        };
        let ready = |id: &str, version: &str| AgentReady {
            id: id.into(),
            version: Some(version.into()),
            ready: true,
            note: None,
        };
        driver.send(AppEvent::Welcome {
            version: "0.0.1".into(),
            repo: text.unwrap_or("~/github/argonauts").into(),
            workflow: "council".into(),
            agents: vec![
                ready("claude-code", "2.1.4"),
                AgentReady {
                    id: "codex".into(),
                    version: None,
                    ready: false,
                    note: Some("logged out · run codex login".into()),
                },
                ready("agy", "1.2.13"),
            ],
            runs_going: 2,
            runs_need_you: 0,
            example_prompt: None,
            main_agent: None,
            effort: None,
        });
        driver.send(AppEvent::Status { status });
    });
}

#[test]
fn turn_end_line() {
    snapshot("turn_end_line", |driver, text| {
        let answer = text.unwrap_or(
            "Hi! I'm the main agent (claude-code/haiku). Ask me about this repo, or describe a change and I'll run it as Captain or with the crew.",
        );
        driver.send_all([turn_started(1, "hi"), answered(1, answer, 3)]);
        driver.settings.verbs = VerbTheme::Plain;
        driver.send_all([turn_started(2, "thanks"), answered(2, "Any time.", 64)]);
        driver.settings.verbs = VerbTheme::Voyage;
        let mut run = RunState::new(run_id());
        run.spent.cost_usd = 0.46;
        let diff = |files, insertions, deletions| DiffStat {
            files_changed: files,
            insertions,
            deletions,
        };
        for (n, (files, added, removed)) in [(2, 40, 6), (3, 55, 10), (1, 20, 2), (1, 5, 0)]
            .into_iter()
            .enumerate()
        {
            let n = u32::try_from(n + 1).expect("four tasks");
            let row = task_row(
                n,
                "a task",
                worker("codex", "gpt-5.6"),
                Some(diff(files, added, removed)),
            );
            run.tasks.insert(task(n), row);
        }
        driver.state.runs.insert(run_id(), run);
        driver.send_all([
            at(
                datetime!(2026-09-30 17:47 UTC),
                None,
                started(RunMode::Crew, "Add a --json flag to export"),
            ),
            run_event(
                None,
                RunEventKind::Merge(MergeEvent::RunMerged {
                    target: "main".into(),
                    sha: "9f2c1ab".into(),
                    fast_forward: true,
                }),
            ),
            run_event(
                None,
                RunEventKind::Phase(PhaseChange {
                    phase: RunPhase::RunReview,
                    status: PhaseStatus::Finished,
                    summary: None,
                }),
            ),
            at(
                datetime!(2026-09-30 17:58:03 UTC),
                None,
                RunEventKind::Run(RunLifecycle::Finished {
                    outcome: RunOutcome::Merged,
                    detail: None,
                }),
            ),
        ]);
    });
    let lines = render(
        |driver, _| driver.send_all([turn_started(1, "hi"), answered(1, "Hi.", 3)]),
        None,
        80,
    );
    let turn_end = style_of(&lines, "Sailed for").expect("the turn end");
    assert_eq!(turn_end.fg, fg(Role::Dim));
}

#[test]
fn setting_changed_line() {
    snapshot("setting_changed_line", |driver, text| {
        let changed = |key: &str, value: &str, scope, previous: Option<&str>, reset| {
            AppEvent::SettingChanged {
                key: key.into(),
                value: value.into(),
                scope,
                previous: previous.map(Into::into),
                reset,
            }
        };
        let value = text.map(|text| format!("\"{text}\""));
        driver.send_all([
            changed(
                "approvals.merge.mode",
                value.as_deref().unwrap_or("\"auto\""),
                SettingScope::Project,
                Some("\"supervised\""),
                false,
            ),
            changed(
                "agents.codex.effort",
                "\"high\"",
                SettingScope::ProjectLocal,
                None,
                false,
            ),
            changed("ui.theme", "\"dark\"", SettingScope::Global, None, true),
            changed("narrator.model", "", SettingScope::Session, None, true),
            changed(
                "crew.main",
                "\"claude-code/claude-opus-5-5\"",
                SettingScope::ProjectLocal,
                Some("\"claude-code\""),
                false,
            ),
            changed(
                "roles.main.effort",
                "\"high\"",
                SettingScope::ProjectLocal,
                None,
                false,
            ),
            changed(
                "roles.reviewer.models",
                "[\"codex/gpt-6-sol\"]",
                SettingScope::Global,
                Some("[\"claude-code/opus\"]"),
                false,
            ),
        ]);
    });
}

const MARKDOWN: &str = "\
# Export formats

The exporter now takes `--json`, and **CSV** stays the default:

1. Parse the flag in `src/cli.rs`.
2. Pass the format to the writer.
   - Each row becomes one JSON object per line.
   - Long exports stream instead of building one array.

## Try it

```sh
argonauts-export --json > rows.jsonl
```
";

#[test]
fn markdown_headings_lists_code() {
    snapshot("markdown_headings_lists_code", |driver, text| {
        let markdown = match text {
            Some(text) => format!("# {text}\n\n- {text}\n\n```\n{text}\n```\n"),
            None => MARKDOWN.to_owned(),
        };
        driver.send_all([
            turn_started(1, "how do I export JSON?"),
            answered(1, &markdown, 12),
        ]);
    });
}

fn read_event(label: &str, who: ModelRef) -> RunEventKind {
    RunEventKind::Goal(GoalEvent::Read {
        label: label.into(),
        worker: who,
        reading: Reading {
            summary: "Export takes --json".into(),
            success_criteria: vec![],
            in_scope: vec![],
            out_of_scope: vec![],
            assumptions: vec![],
            open_questions: vec![],
        },
    })
}

fn sealed(label: u8, who: ModelRef) -> RunEventKind {
    RunEventKind::Plan(PlanEvent::Sealed {
        draft: DraftId::numbered(u32::from(label)),
        label,
        worker: who,
        round: 1,
        summary: "A plan".into(),
        path: PathBuf::from("drafts/plan.md"),
        sha256: String::new(),
    })
}

fn position(label: u8, summary: &str) -> MappedPosition {
    MappedPosition {
        label,
        summary: summary.into(),
        quote: summary.into(),
        quote_verified: true,
    }
}

fn task_spec(n: u32, title: &str) -> TaskSpec {
    TaskSpec {
        id: task(n),
        title: title.into(),
        task: title.into(),
        kind: TaskKind::Build,
        owned_paths: vec![],
        read_only_paths: vec![],
        forbidden_paths: vec![],
        hidden_paths: vec![],
        depends_on: vec![],
        criteria: vec![],
        worker: None,
        reviewers: vec![],
        limit: TaskLimit::default(),
        predictions: None,
        item: None,
    }
}

#[test]
fn finished_step_collapses_with_details() {
    snapshot("finished_step_collapses_with_details", |driver, text| {
        let goal = text.unwrap_or("Add a --json flag to export");
        let claude = worker("claude-code", "opus");
        let codex = worker("codex", "gpt-5.6");
        let agy = worker("agy", "gemini-3.6-pro");
        // A task goes by its label in the conversation, never its id.
        let mut run = RunState::new(run_id());
        for (n, title, item) in [
            (2, "Add the flag", "json-flag"),
            (3, "Write rows", "json-rows"),
        ] {
            let mut row = task_row(n, title, codex.clone(), None);
            row.item = Some(item.into());
            run.tasks.insert(task(n), row);
        }
        driver.state.runs.insert(run_id(), run);
        driver.send_all([
            run_event(None, started(RunMode::Crew, goal)),
            run_event(None, read_event("A", claude.clone())),
            run_event(None, read_event("B", codex.clone())),
            run_event(None, read_event("C", agy.clone())),
            run_event(
                None,
                RunEventKind::Goal(GoalEvent::Approved {
                    file: GoalFile {
                        run: run_id(),
                        version: 1,
                        summary: goal.into(),
                        criteria: vec![Criterion::new("G1", "export --json writes JSON lines")],
                        in_scope: vec![],
                        out_of_scope: vec![],
                        assumptions: vec![],
                        answers: vec!["Per line, not one array".into()],
                        source: GoalSource::Readings,
                        path: PathBuf::from("goal.md"),
                        sha256: String::new(),
                    },
                    by: Party::User,
                }),
            ),
            run_event(None, sealed(1, claude.clone())),
            run_event(None, sealed(2, codex.clone())),
            run_event(None, sealed(3, agy.clone())),
            run_event(
                None,
                RunEventKind::Plan(PlanEvent::Mapped {
                    round: 1,
                    worker: claude,
                    topics: vec![
                        MappedTopic {
                            name: "Row format".into(),
                            disputed: true,
                            positions: vec![
                                position(1, "JSON lines"),
                                position(2, "one array"),
                                position(3, "JSON lines"),
                            ],
                        },
                        MappedTopic {
                            name: text.unwrap_or("Flag name").into(),
                            disputed: true,
                            positions: vec![position(1, "--json"), position(2, "--format json")],
                        },
                        MappedTopic {
                            name: "Tests".into(),
                            disputed: false,
                            positions: vec![position(1, "unit")],
                        },
                    ],
                }),
            ),
            run_event(
                None,
                RunEventKind::Plan(PlanEvent::Judged(JudgingResult {
                    method: JudgingMethod::Cross,
                    plans: vec![],
                    winner: Some(1),
                    margin: 0.8,
                    near_tie: false,
                })),
            ),
            run_event(
                None,
                RunEventKind::Divide(DivideEvent::Graph(TaskGraph {
                    tasks: (1..=5).map(|n| task_spec(n, "a task")).collect(),
                    critical_path: vec![],
                })),
            ),
            run_event(Some(task(2)), check("tests", CheckKind::Command, true, "")),
            run_event(Some(task(2)), check("lint", CheckKind::Command, true, "")),
            run_event(
                Some(task(2)),
                check("link check", CheckKind::Command, true, ""),
            ),
            run_event(
                Some(task(2)),
                check("secrets", CheckKind::SecretScan, true, ""),
            ),
            run_event(Some(task(2)), checking_done()),
            run_event(
                Some(task(3)),
                check("tests", CheckKind::Command, false, "41 passed, 1 failed"),
            ),
            run_event(Some(task(3)), checking_done()),
            run_event(
                Some(task(4)),
                check("protected paths", CheckKind::ProtectedPaths, true, ""),
            ),
            run_event(Some(task(4)), checking_done()),
        ]);
    });
}

fn fallback_plan(n: u32, title: &str, resets_at: Option<OffsetDateTime>) -> FallbackPlan {
    FallbackPlan {
        task: task(n),
        title: title.into(),
        stopped: worker("claude-code", "opus"),
        reason: "usage limit".into(),
        resets_at,
        work: DiffStat {
            files_changed: 2,
            insertions: 30,
            deletions: 4,
        },
        checks_ran: false,
        candidates: vec![],
        mode: FallbackMode::Ask,
        strategy: FallbackStrategy::First,
        race_count: 1,
        also_moving: vec![],
    }
}

fn health(state: HealthState) -> RunEventKind {
    RunEventKind::AgentStatus(AgentStatusEvent::Health(AgentHealth {
        agent_id: AgentId("claude-code".into()),
        state,
        since: datetime!(2026-09-30 17:00 UTC),
        reason: None,
        next_test_at: None,
        disabled: None,
    }))
}

#[test]
fn fallback_and_return_transcript_lines() {
    snapshot("fallback_and_return_transcript_lines", |driver, text| {
        let title = text.unwrap_or("Middleware");
        let codex = worker("codex", "gpt-5.6");
        let mut run = RunState::new(run_id());
        let mut fallback = task_row(2, "Redis store", codex.clone(), None);
        fallback.state = Some(TaskState::Running);
        fallback.fallback_for = Some(worker("claude-code", "opus"));
        // Tasks go by their labels in the conversation; ids stay in commands.
        fallback.item = Some("redis-store".into());
        run.tasks.insert(task(2), fallback);
        let mut limiter = task_row(3, "Middleware", worker("claude-code", "opus"), None);
        limiter.item = Some("limiter".into());
        run.tasks.insert(task(3), limiter);
        driver.state.runs.insert(run_id(), run);
        let resets = Some(datetime!(2026-09-30 22:45 UTC));
        driver.send_all([
            run_event(
                Some(task(2)),
                RunEventKind::Fallback(FallbackEvent::Needed(fallback_plan(
                    2,
                    "Redis store",
                    resets,
                ))),
            ),
            run_event(
                Some(task(2)),
                RunEventKind::Task(TaskEvent::CheckpointTagged {
                    n: 1,
                    commit: "4c1d2e9".into(),
                    from: worker("claude-code", "opus"),
                    to: vec![codex.clone()],
                    reason: "usage limit, resets at 3:45 PM".into(),
                }),
            ),
            run_event(
                Some(task(2)),
                RunEventKind::Fallback(FallbackEvent::FellBack {
                    task: task(2),
                    to: codex.clone(),
                    mode: FallbackMode::Ask,
                    by: Party::User,
                    context: Some(FallbackContext::NoteAndTranscript),
                }),
            ),
            run_event(
                Some(task(3)),
                RunEventKind::Fallback(FallbackEvent::Needed(fallback_plan(3, title, None))),
            ),
            run_event(
                Some(task(3)),
                RunEventKind::Fallback(FallbackEvent::FellBack {
                    task: task(3),
                    to: worker("agy", "gemini-3.6-pro"),
                    mode: FallbackMode::Auto,
                    by: Party::Rules,
                    context: Some(FallbackContext::NoteAndSummary),
                }),
            ),
            run_event(
                Some(task(2)),
                RunEventKind::Task(TaskEvent::Rewound {
                    to: 1,
                    kept_on: "argonauts/run-01/task-002-after-fallback-1".into(),
                }),
            ),
            run_event(None, health(HealthState::Down)),
            run_event(None, health(HealthState::Healthy)),
        ]);
    });
}

#[test]
fn recap_line() {
    snapshot("recap_line", |driver, text| {
        driver.send(AppEvent::Recap {
            text: text
                .unwrap_or(
                    "Export now takes --json, all 4 tasks merged, and the tests pass. Ask for another change, or /resume an older run.",
                )
                .into(),
            model: None,
        });
    });
}

fn tool(n: u32, id: &str, name: &str, path: &str) -> [AppEvent; 2] {
    [
        agent(
            n,
            AgentEvent::ToolStarted {
                id: id.into(),
                name: name.into(),
                input: serde_json::json!({ "file_path": path }),
                parent: None,
            },
        ),
        agent(
            n,
            AgentEvent::ToolFinished {
                id: id.into(),
                ok: true,
                output: Some("ok".into()),
                parent: None,
            },
        ),
    ]
}

#[test]
fn conversation_answer_with_folded_tools() {
    snapshot("conversation_answer_with_folded_tools", |driver, text| {
        driver.settings.verbosity = Verbosity::Normal;
        let answer = text.map_or_else(
            || {
                "Export writes CSV today. The writer lives in `src/export.rs`, and the flag \
                 would go in `src/cli.rs`.\n\nWant me to add `--json`?"
                    .to_owned()
            },
            ToOwned::to_owned,
        );
        driver.send(turn_started(1, "how does export work?"));
        for (id, path) in ["src/cli.rs", "src/export.rs", "src/lib.rs", "Cargo.toml"]
            .into_iter()
            .enumerate()
        {
            driver.send_all(tool(1, &format!("t{id}"), "Read", path));
        }
        // Split mid-sentence, as deltas arrive.
        let middle = (0..=answer.len() / 2)
            .rev()
            .find(|at| answer.is_char_boundary(*at))
            .unwrap_or(0);
        let (first, rest) = answer.split_at(middle);
        driver.send_all([delta(1, first), delta(1, rest), answered(1, &answer, 9)]);
    });
}

#[test]
fn a_call_between_parts_of_the_answer_folds_under_the_first_part() {
    let lines = render(
        |driver, _| {
            driver.settings.verbosity = Verbosity::Normal;
            driver.send_all([
                turn_started(1, "where is the writer?"),
                delta(1, "Let me look.\n"),
            ]);
            driver.send_all(tool(1, "t1", "Grep", "fn write"));
            driver.send_all([
                delta(1, "It's in `src/export.rs`."),
                answered(1, "Let me look.\nIt's in `src/export.rs`.", 2),
            ]);
        },
        None,
        80,
    );
    assert_eq!(
        text(&lines),
        "❯ where is the writer?\n\n● Let me look.\n\n  Searched for 1 pattern (ctrl+o to expand)\n\n● It's in src/export.rs.\n\n✻ Sailed for 2s · done 10:42 AM"
    );
}

#[test]
fn an_answer_split_by_calls_prints_each_part_once() {
    let lines = render(
        |driver, _| {
            driver.settings.verbosity = Verbosity::Normal;
            driver.send_all([
                turn_started(1, "where is the writer?"),
                delta(1, "Let me look."),
            ]);
            driver.send_all(tool(1, "t1", "Grep", "fn write"));
            driver.send(delta(1, "Found the writer. Now the flag."));
            driver.send_all(tool(1, "t2", "Read", "src/cli.rs"));
            driver.send_all([
                delta(1, "Both are in `src`."),
                answered(
                    1,
                    "Let me look.\n\nFound the writer. Now the flag.\n\nBoth are in `src`.",
                    2,
                ),
            ]);
        },
        None,
        80,
    );
    let printed = text(&lines);
    for part in ["Let me look.", "Found the writer.", "Both are in src."] {
        assert_eq!(printed.matches(part).count(), 1, "{part}:\n{printed}");
    }
}

fn call(
    n: u32,
    id: &str,
    name: &str,
    input: serde_json::Value,
    ok: bool,
    output: &str,
) -> [AppEvent; 2] {
    [
        agent(
            n,
            AgentEvent::ToolStarted {
                id: id.into(),
                name: name.into(),
                input,
                parent: None,
            },
        ),
        agent(
            n,
            AgentEvent::ToolFinished {
                id: id.into(),
                ok,
                output: Some(output.into()),
                parent: None,
            },
        ),
    ]
}

/// A turn that reads a file, runs a failing test, and fixes it, with the shared tool table.
fn conversation_with_tools(driver: &mut TestDriver, text: Option<&str>, level: Verbosity) {
    driver.settings.verbosity = level;
    driver.transcript.conversation = ConversationTurns::default();
    let answer = text.unwrap_or("The test expected the old header, so I added the email column.");
    driver.send(turn_started(1, "why does the export test fail?"));
    let read = serde_json::json!({ "file_path": "src/export.rs" });
    driver.send_all(call(1, "t1", "Read", read, true, "1\n2\n3"));
    let test = serde_json::json!({ "command": "cargo test export" });
    let failed =
        "Exit code 101\ntest export_header ... FAILED\nassertion failed: header == \"id,name\"";
    driver.send_all(call(1, "t2", "Bash", test, false, failed));
    let edit = serde_json::json!({
        "file_path": "tests/export.rs",
        "old_string": "\"id,name\"",
        "new_string": "\"id,name,email\"",
    });
    driver.send_all(call(1, "t3", "Edit", edit, true, "ok"));
    driver.send_all([delta(1, answer), answered(1, answer, 12)]);
}

#[test]
fn conversation_tools_at_each_verbosity() {
    snapshot("conversation_tools_full", |driver, text| {
        conversation_with_tools(driver, text, Verbosity::Full);
    });
    snapshot("conversation_tools_normal", |driver, text| {
        conversation_with_tools(driver, text, Verbosity::Normal);
    });
    snapshot("conversation_tools_quiet", |driver, text| {
        conversation_with_tools(driver, text, Verbosity::Quiet);
    });
    let at = |level| {
        let mut driver = TestDriver::new();
        conversation_with_tools(&mut driver, None, level);
        text(&driver.printed(80))
    };
    let full = at(Verbosity::Full);
    for shown in [
        "Read(src/export.rs)",
        "Exit code 101",
        "Update(tests/export.rs)",
    ] {
        assert!(full.contains(shown), "{shown}:\n{full}");
    }
    let normal = at(Verbosity::Normal);
    assert!(!normal.contains("Read(src/export.rs)"), "{normal}");
    assert!(normal.contains("Update(tests/export.rs)"), "{normal}");
    let quiet = at(Verbosity::Quiet);
    assert!(!quiet.contains("export.rs"), "{quiet}");
    assert!(quiet.contains("email column"), "{quiet}");
}

#[test]
fn interrupted_turn_line() {
    snapshot("interrupted_turn_line", |driver, text| {
        driver.send_all([
            turn_started(1, "rewrite the exporter"),
            delta(1, "Let me look at the exporter first.\n"),
            delta(1, text.unwrap_or("It writes each row")),
            conversation(ConversationEvent::TurnCancelled {
                turn: TurnId::numbered(1),
            }),
        ]);
    });
}

/// Claude Code's own screen for the same turn (2.1.284, `ref/claude-code/25-edit-done.txt`).
const CLAUDE_CODE_EDIT_TURN: &str = "\
❯ In greet.txt, change hello to hi.

  Read 1 file

⏺ Update(greet.txt)
  ⎿  Added 1 line, removed 1 line
      1 -hello world
      1 +hi world

⏺ Changed hello to hi in greet.txt.

✻ Worked for 3s · done 1:07 PM";

/// A direct Captain turn as Claude Code ran it in your checkout: your message, its calls drawn as
/// Argonauts Code draws a CLI's, its own answer, and nothing after it but the `✻` line. No run
/// item, task, report, merge, or recap. Shown beside Claude Code's own screen for the turn.
fn direct_edit_turn(driver: &mut TestDriver, text: Option<&str>) {
    driver.settings.verbosity = Verbosity::Normal;
    let answer = text.unwrap_or("Changed hello to hi in greet.txt.");
    driver.send(conversation(ConversationEvent::TurnStarted {
        turn: TurnId::numbered(1),
        text: "In greet.txt, change hello to hi.".into(),
        worker: worker("claude-code", "haiku"),
        read_only: false,
        commit: "8b41d07".into(),
        uncommitted: vec![],
        settings_sha256: None,
        continues: false,
    }));
    let read = serde_json::json!({ "file_path": "greet.txt" });
    driver.send_all(call(1, "t1", "Read", read, true, "1\thello world"));
    let edit = serde_json::json!({
        "file_path": "greet.txt",
        "old_string": "hello world",
        "new_string": "hi world",
    });
    driver.send_all(call(1, "t2", "Edit", edit, true, "ok"));
    driver.send_all([delta(1, answer), answered(1, answer, 3)]);
}

#[test]
fn a_direct_captain_turn_reads_like_claude_codes() {
    snapshot("direct_captain_turn", direct_edit_turn);
    for width in [100, 120] {
        let lines = render(direct_edit_turn, None, width);
        let printed = text(&lines);
        let last = printed.lines().last().unwrap_or_default();
        assert!(last.starts_with("✻ "), "{printed}");
        let answer_at = printed
            .find("● Changed hello to hi")
            .expect("the agent's answer");
        let after: Vec<&str> = printed[answer_at..]
            .lines()
            .skip(1)
            .filter(|line| !line.trim().is_empty())
            .collect();
        assert_eq!(
            after.len(),
            1,
            "only the ✻ line follows the answer:\n{printed}"
        );
        insta::assert_snapshot!(
            format!("direct_captain_turn_beside_claude_code_{width}"),
            format!(
                "Argonauts Code, direct Captain:\n\n{printed}\n\nClaude Code:\n\n{CLAUDE_CODE_EDIT_TURN}"
            )
        );
    }
}

/// The agent ran out mid-turn and the next one carries on under a `↷` line.
#[test]
fn a_direct_turn_fell_back_goes_on_under_one_line() {
    let lines = render(
        |driver, _| {
            driver.settings.verbosity = Verbosity::Normal;
            driver.send(conversation(ConversationEvent::TurnStarted {
                turn: TurnId::numbered(1),
                text: "Fix invoice/money.py".into(),
                worker: worker("claude-code", "haiku"),
                read_only: false,
                commit: "8b41d07".into(),
                uncommitted: vec![],
                settings_sha256: None,
                continues: false,
            }));
            let edit = serde_json::json!({
                "file_path": "invoice/money.py",
                "old_string": "sign = 1",
                "new_string": "negative = True",
            });
            driver.send_all(call(1, "t1", "Edit", edit, true, "ok"));
            driver.send(conversation(ConversationEvent::FellBack {
                turn: TurnId::numbered(1),
                from: worker("claude-code", "haiku"),
                to: worker("codex", "gpt-5.6-luna"),
                why: "hit its usage limit".into(),
                told: "your request, the transcript so far, and that invoice/money.py changed"
                    .into(),
                signed_out: None,
            }));
            let test = serde_json::json!({ "command": "python -m pytest -q" });
            driver.send_all(call(1, "t2", "Bash", test, true, "6 passed in 0.04s"));
            driver.send_all([
                delta(1, "All 6 tests pass."),
                answered(1, "All 6 tests pass.", 32),
            ]);
        },
        None,
        100,
    );
    let printed = text(&lines);
    insta::assert_snapshot!("direct_turn_fell_back_100", printed);
    assert!(
        printed.contains("↷ Claude Code hit its usage limit · Codex continues with"),
        "{printed}"
    );
    assert_eq!(
        printed.matches("❯ Fix invoice/money.py").count(),
        1,
        "{printed}"
    );
}
