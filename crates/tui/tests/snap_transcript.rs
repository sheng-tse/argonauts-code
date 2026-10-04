//! What the transcript prints for each kind of event, at 80 columns, and that it holds its
//! shape at every width with awkward text.

mod common;

use std::path::PathBuf;
use std::time::Instant;

use argonauts_core::api::{RunState as RunStatus, RunSummary};
use argonauts_core::app::{AgentReady, AppEvent, ChatEvent, ReplayTurn, StatusLine, TurnReply};
use argonauts_core::events::card::{
    CardBudget, CardEvent, CardKind, CardSpec, CardState, GateKind, GateOutcome, IntegrationEvent,
};
use argonauts_core::events::goal::{Criterion, GoalEvent, GoalFile, GoalSource};
use argonauts_core::events::plan::{
    DivideEvent, JudgingResult, MappedPosition, MappedTopic, PlanEvent, TaskGraph,
};
use argonauts_core::events::run::{
    Party, PhaseChange, RunLifecycle, RunMode, RunOutcome, RunPhase, RunStarted,
};
use argonauts_core::events::session::{EndReason, SessionEnd, SessionEvent, SessionStarted};
use argonauts_core::events::vendor::{HandoffEvent, HandoffPlan, VendorEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::limits::{HealthState, VendorHealth};
use argonauts_core::names::{DraftId, RoleName, SessionKey, TurnId, VendorId, WorkerRef};
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::handoff::{HandoffContext, HandoffMode, HandoffStrategy};
use argonauts_core::settings::protocol::JudgingMethod;
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::settings::ui::{NarratorMode, UiSettings, VerbTheme};
use argonauts_core::steps::goal::Restatement;
use argonauts_core::worker::{Access, WorkerKind};
use argonauts_core::{AgentEvent, CardId, DiffStat, PhaseStatus, RunId, Spend, Verbosity};
use argonauts_tui::bridge::agents::AgentBlocks;
use argonauts_tui::bridge::transcript::{ChatTurns, Transcript};
use argonauts_tui::bridge::{Cx, Effects};
use argonauts_tui::cell::ToolKind;
use argonauts_tui::format::WallClock;
use argonauts_tui::model::ViewModel;
use argonauts_tui::stability::{self, WIDTHS, samples};
use argonauts_tui::state::{AppState, CardRow, Replay, RunState};
use argonauts_tui::theme::Role;
use common::{fg, still, style_of, text, theme};
use ratatui::text::Line;
use time::OffsetDateTime;
use time::macros::datetime;

/// 10:42 AM where the user is, seven hours behind UTC.
const NOW: OffsetDateTime = datetime!(2026-09-30 10:42 -7);

struct Harness {
    model: ViewModel,
    state: AppState,
    settings: UiSettings,
    transcript: Transcript,
    agents: AgentBlocks,
}

impl Harness {
    fn new() -> Self {
        let mut transcript = Transcript::with_clock(WallClock::pinned(NOW));
        transcript.chat = ChatTurns::with_tool_kinds(|_, name| match name {
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

type Scenario = fn(&mut Harness, Option<&str>);

fn render(scenario: Scenario, text: Option<&str>, width: u16) -> Vec<Line<'static>> {
    let mut harness = Harness::new();
    scenario(&mut harness, text);
    harness.printed(width)
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

fn card(n: u32) -> CardId {
    CardId::numbered(n)
}

fn worker(harness: &str, model: &str) -> WorkerRef {
    WorkerRef::new(harness, Some(model.to_owned()))
}

fn at(ts: OffsetDateTime, card_id: Option<CardId>, kind: RunEventKind) -> AppEvent {
    AppEvent::Run {
        event: Box::new(RunEvent {
            seq: 0,
            ts,
            run_id: run_id(),
            card_id,
            session: None,
            source: "conductor".into(),
            kind,
        }),
    }
}

fn run_event(card_id: Option<CardId>, kind: RunEventKind) -> AppEvent {
    at(datetime!(2026-09-30 17:00 UTC), card_id, kind)
}

fn chat(event: ChatEvent) -> AppEvent {
    AppEvent::Chat { event }
}

fn turn_started(n: u32, text: &str) -> AppEvent {
    chat(ChatEvent::TurnStarted {
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
    chat(ChatEvent::Agent {
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
    chat(ChatEvent::TurnFinished {
        turn: TurnId::numbered(n),
        reply: TurnReply::Answer {
            markdown: markdown.into(),
        },
        spend: Spend::default(),
        elapsed_ms: seconds * 1_000,
    })
}

fn gate(name: &str, kind: GateKind, passed: bool, summary: &str) -> RunEventKind {
    RunEventKind::Gate(GateOutcome {
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

fn gating_done() -> RunEventKind {
    RunEventKind::Card(CardEvent::State {
        from: Some(CardState::Gating),
        to: CardState::InReview,
        reason: String::new(),
    })
}

fn started(mode: RunMode, request: &str) -> RunEventKind {
    RunEventKind::Run(RunLifecycle::Started(Box::new(RunStarted {
        request: request.into(),
        title: request.into(),
        mode,
        forced: false,
        preset: "peer-crew".into(),
        estimate: None,
        triage: None,
        chat_turn: None,
        continues: None,
        repo_root: PathBuf::from("/repo"),
        base_branch: Some("main".into()),
        base_commit: "8b41d07c2e".into(),
        uncommitted: vec!["notes.txt".into(), "scratch.rs".into()],
        uncommitted_included: false,
        settings_sha256: String::new(),
        settings: serde_json::Value::Null,
        overrides: vec![],
        harness_versions: Default::default(),
        version: "0.0.1".into(),
        front_end: None,
        instructions: Vec::new(),
    })))
}

fn card_row(n: u32, title: &str, owner: WorkerRef, diff: Option<DiffStat>) -> CardRow {
    CardRow {
        id: card(n),
        title: title.into(),
        state: Some(CardState::Integrated),
        owner: Some(owner),
        attempt: 1,
        diff,
        standing_in_for: None,
        debt: vec![],
        checkpoints: 0,
        rotations: 0,
        last_gate: None,
        reviews: 0,
        waiting_on_vendor: None,
        item: None,
    }
}

#[test]
fn startup_header() {
    snapshot("startup_header", |harness, text| {
        // The controller sends the welcome first and the status line with the default agent
        // just after it.
        let status = StatusLine {
            title: "argonauts".into(),
            repo: "~/github/argonauts".into(),
            preset: "peer-crew".into(),
            all_hands: false,
            run: None,
            default_agent: worker("claude-code", "haiku-4.5"),
            quotas: vec![],
            spent: None,
            limit: None,
            narrator: NarratorMode::OnDemand,
            approval_mode: ApprovalMode::Supervised,
        };
        let ready = |id: &str, version: &str| AgentReady {
            id: id.into(),
            version: Some(version.into()),
            ready: true,
            note: None,
        };
        harness.send(AppEvent::Welcome {
            version: "0.0.1".into(),
            repo: text.unwrap_or("~/github/argonauts").into(),
            preset: "peer-crew".into(),
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
            example_prompt: None,
            default_agent: None,
            effort: None,
        });
        harness.send(AppEvent::Status { status });
    });
}

#[test]
fn turn_end_line() {
    snapshot("turn_end_line", |harness, text| {
        let answer = text.unwrap_or(
            "Hi! I'm the default agent (claude-code/haiku). Ask me about this repo, or describe a change and I'll run it solo or with the crew.",
        );
        harness.send_all([turn_started(1, "hi"), answered(1, answer, 3)]);
        harness.settings.verbs = VerbTheme::Plain;
        harness.send_all([turn_started(2, "thanks"), answered(2, "Any time.", 64)]);
        harness.settings.verbs = VerbTheme::Voyage;
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
            let n = u32::try_from(n + 1).expect("four cards");
            let row = card_row(
                n,
                "a card",
                worker("codex", "gpt-5.6"),
                Some(diff(files, added, removed)),
            );
            run.cards.insert(card(n), row);
        }
        harness.state.runs.insert(run_id(), run);
        harness.send_all([
            at(
                datetime!(2026-09-30 17:47 UTC),
                None,
                started(RunMode::Crew, "Add a --json flag to export"),
            ),
            run_event(
                None,
                RunEventKind::Integration(IntegrationEvent::Landed {
                    target: "main".into(),
                    sha: "9f2c1ab".into(),
                    fast_forward: true,
                }),
            ),
            run_event(
                None,
                RunEventKind::Phase(PhaseChange {
                    phase: RunPhase::Retro,
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
        |harness, _| harness.send_all([turn_started(1, "hi"), answered(1, "Hi.", 3)]),
        None,
        80,
    );
    let turn_end = style_of(&lines, "Sailed for").expect("the turn end");
    assert_eq!(turn_end.fg, fg(Role::Dim));
}

#[test]
fn setting_changed_line() {
    snapshot("setting_changed_line", |harness, text| {
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
        harness.send_all([
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
                "default_agent",
                "\"claude-code/claude-opus-5-5\"",
                SettingScope::ProjectLocal,
                Some("\"claude-code\""),
                false,
            ),
            changed(
                "roles.default.effort",
                "\"high\"",
                SettingScope::ProjectLocal,
                None,
                false,
            ),
            changed(
                "roles.reviewer.workers",
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
    snapshot("markdown_headings_lists_code", |harness, text| {
        let markdown = match text {
            Some(text) => format!("# {text}\n\n- {text}\n\n```\n{text}\n```\n"),
            None => MARKDOWN.to_owned(),
        };
        harness.send_all([
            turn_started(1, "how do I export JSON?"),
            answered(1, &markdown, 12),
        ]);
    });
}

fn restated(label: &str, who: WorkerRef) -> RunEventKind {
    RunEventKind::Goal(GoalEvent::Restated {
        label: label.into(),
        worker: who,
        restatement: Restatement {
            summary: "Export takes --json".into(),
            success_criteria: vec![],
            in_scope: vec![],
            out_of_scope: vec![],
            assumptions: vec![],
            open_questions: vec![],
        },
    })
}

fn sealed(label: u8, who: WorkerRef) -> RunEventKind {
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

fn card_spec(n: u32, title: &str) -> CardSpec {
    CardSpec {
        id: card(n),
        title: title.into(),
        task: title.into(),
        kind: CardKind::Build,
        owned_paths: vec![],
        read_only_paths: vec![],
        forbidden_paths: vec![],
        hidden_paths: vec![],
        depends_on: vec![],
        criteria: vec![],
        owner: None,
        reviewers: vec![],
        budget: CardBudget::default(),
        predictions: None,
        item: None,
    }
}

#[test]
fn finished_step_collapses_with_details() {
    snapshot("finished_step_collapses_with_details", |harness, text| {
        let goal = text.unwrap_or("Add a --json flag to export");
        let claude = worker("claude-code", "opus");
        let codex = worker("codex", "gpt-5.6");
        let agy = worker("agy", "gemini-3.6-pro");
        // A card goes by its label in the chat, never its id.
        let mut run = RunState::new(run_id());
        for (n, title, item) in [
            (2, "Add the flag", "json-flag"),
            (3, "Write rows", "json-rows"),
        ] {
            let mut row = card_row(n, title, codex.clone(), None);
            row.item = Some(item.into());
            run.cards.insert(card(n), row);
        }
        harness.state.runs.insert(run_id(), run);
        harness.send_all([
            run_event(None, started(RunMode::Crew, goal)),
            run_event(None, restated("A", claude.clone())),
            run_event(None, restated("B", codex.clone())),
            run_event(None, restated("C", agy.clone())),
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
                        source: GoalSource::Restatements,
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
                    cards: (1..=5).map(|n| card_spec(n, "a card")).collect(),
                    critical_path: vec![],
                })),
            ),
            run_event(Some(card(2)), gate("tests", GateKind::Command, true, "")),
            run_event(Some(card(2)), gate("lint", GateKind::Command, true, "")),
            run_event(
                Some(card(2)),
                gate("link check", GateKind::Command, true, ""),
            ),
            run_event(
                Some(card(2)),
                gate("secrets", GateKind::SecretScan, true, ""),
            ),
            run_event(Some(card(2)), gating_done()),
            run_event(
                Some(card(3)),
                gate("tests", GateKind::Command, false, "41 passed, 1 failed"),
            ),
            run_event(Some(card(3)), gating_done()),
            run_event(
                Some(card(4)),
                gate("protected paths", GateKind::ProtectedPaths, true, ""),
            ),
            run_event(Some(card(4)), gating_done()),
        ]);
    });
}

fn handoff_plan(n: u32, title: &str, resets_at: Option<OffsetDateTime>) -> HandoffPlan {
    HandoffPlan {
        card: card(n),
        title: title.into(),
        stopped: worker("claude-code", "opus"),
        reason: "usage limit".into(),
        resets_at,
        work: DiffStat {
            files_changed: 2,
            insertions: 30,
            deletions: 4,
        },
        gates_ran: false,
        candidates: vec![],
        mode: HandoffMode::Ask,
        strategy: HandoffStrategy::First,
        race_count: 1,
        also_moving: vec![],
    }
}

fn health(state: HealthState) -> RunEventKind {
    RunEventKind::Vendor(VendorEvent::Health(VendorHealth {
        vendor: VendorId("claude-code".into()),
        state,
        since: datetime!(2026-09-30 17:00 UTC),
        reason: None,
        next_test_at: None,
        disabled: None,
    }))
}

#[test]
fn handoff_and_return_transcript_lines() {
    snapshot("handoff_and_return_transcript_lines", |harness, text| {
        let title = text.unwrap_or("Middleware");
        let codex = worker("codex", "gpt-5.6");
        let mut run = RunState::new(run_id());
        let mut stand_in = card_row(2, "Redis store", codex.clone(), None);
        stand_in.state = Some(CardState::Running);
        stand_in.standing_in_for = Some(worker("claude-code", "opus"));
        // Cards go by their labels in the chat; ids stay in commands.
        stand_in.item = Some("redis-store".into());
        run.cards.insert(card(2), stand_in);
        let mut limiter = card_row(3, "Middleware", worker("claude-code", "opus"), None);
        limiter.item = Some("limiter".into());
        run.cards.insert(card(3), limiter);
        harness.state.runs.insert(run_id(), run);
        let resets = Some(datetime!(2026-09-30 22:45 UTC));
        harness.send_all([
            run_event(
                Some(card(2)),
                RunEventKind::Handoff(HandoffEvent::Needed(handoff_plan(2, "Redis store", resets))),
            ),
            run_event(
                Some(card(2)),
                RunEventKind::Card(CardEvent::CheckpointTagged {
                    n: 1,
                    commit: "4c1d2e9".into(),
                    from: worker("claude-code", "opus"),
                    to: vec![codex.clone()],
                    reason: "usage limit, resets at 3:45 PM".into(),
                }),
            ),
            run_event(
                Some(card(2)),
                RunEventKind::Handoff(HandoffEvent::HandedOff {
                    card: card(2),
                    to: codex.clone(),
                    mode: HandoffMode::Ask,
                    by: Party::User,
                    context: Some(HandoffContext::NoteAndTranscript),
                }),
            ),
            run_event(
                Some(card(3)),
                RunEventKind::Handoff(HandoffEvent::Needed(handoff_plan(3, title, None))),
            ),
            run_event(
                Some(card(3)),
                RunEventKind::Handoff(HandoffEvent::HandedOff {
                    card: card(3),
                    to: worker("agy", "gemini-3.6-pro"),
                    mode: HandoffMode::Auto,
                    by: Party::Rules,
                    context: Some(HandoffContext::NoteAndSummary),
                }),
            ),
            run_event(
                Some(card(2)),
                RunEventKind::Card(CardEvent::Reverted {
                    to: 1,
                    kept_on: "argonauts/run-01/card-002-after-handoff-1".into(),
                }),
            ),
            run_event(None, health(HealthState::Down)),
            run_event(None, health(HealthState::Healthy)),
        ]);
    });
}

#[test]
fn recap_line() {
    snapshot("recap_line", |harness, text| {
        harness.send(AppEvent::Recap {
            text: text
                .unwrap_or(
                    "Export now takes --json, all 4 cards merged, and the tests pass. Ask for another change, or /resume an older run.",
                )
                .into(),
            model: None,
        });
    });
}

fn session(ts: OffsetDateTime, key: &str, kind: SessionEvent) -> AppEvent {
    let AppEvent::Run { mut event } = at(ts, Some(card(1)), RunEventKind::Session(kind)) else {
        unreachable!("at builds run events");
    };
    event.session = SessionKey::parse(key);
    AppEvent::Run { event }
}

/// A session that started and finished in the replayed history.
fn replayed_session(key: &str) -> [AppEvent; 2] {
    let started = SessionStarted {
        vendor: VendorId("claude-code".into()),
        worker: worker("claude-code", "opus"),
        family: "test".into(),
        kind: WorkerKind::Harness,
        role: RoleName::Owner,
        step: None,
        access: Access::Build,
        lifetime: SessionLifetime::Card,
        resumed_from: None,
        follow_key: None,
        workflow_step: None,
        label: None,
    };
    let ended = SessionEnd {
        vendor_session: None,
        exit_code: Some(0),
        signal: None,
        duration_ms: 60_000,
        reason: EndReason::Completed,
        spend: Spend::default(),
        tool_calls: 0,
        result_text: Some("done".into()),
        structured: None,
        last_message: None,
        raw_log: None,
    };
    [
        session(
            datetime!(2026-09-30 16:00 UTC),
            key,
            SessionEvent::Started(Box::new(started)),
        ),
        session(
            datetime!(2026-09-30 16:01 UTC),
            key,
            SessionEvent::Ended(Box::new(ended)),
        ),
    ]
}

#[test]
fn condensed_replay_layout() {
    snapshot("condensed_replay_layout", resumed_run);
    // Sessions that finished before are in the Ctrl+O history, not above the run's header.
    let printed = text(&render(resumed_run, None, 80));
    assert!(
        printed.starts_with("● Add a --json flag to export\n"),
        "{printed}"
    );
}

fn resumed_run(harness: &mut Harness, text: Option<&str>) {
    let title = text.unwrap_or("Add a --json flag to export");
    harness.send(AppEvent::Attached {
        summary: RunSummary {
            run: run_id(),
            title: title.into(),
            mode: RunMode::Crew,
            preset: "peer-crew".into(),
            state: RunStatus::Parked,
            stopped_at: "plan checkpoint, waiting for you".into(),
            updated: datetime!(2026-09-30 17:00 UTC),
            spent: Spend {
                cost_usd: 1.24,
                ..Spend::default()
            },
            sessions_to_resume: 2,
            waiting: vec![],
            live: false,
            remind: false,
        },
        started_here: false,
    });
    harness.send_all(replayed_session("s-001"));
    harness.send_all(replayed_session("s-002"));
    for n in 0..50 {
        harness.send(run_event(
            Some(card(1)),
            gate("tests", GateKind::Command, true, &format!("run {n}")),
        ));
        harness.send(run_event(Some(card(1)), gating_done()));
    }
    harness.send_all([
        AppEvent::CondensedReplay {
            run: run_id(),
            turns: vec![
                ReplayTurn {
                    from: Party::User,
                    text: title.into(),
                },
                ReplayTurn {
                    from: Party::Agent {
                        session: argonauts_core::names::SessionKey::parse("chat-1")
                            .expect("a valid session key"),
                        worker: worker("claude-code", "opus"),
                    },
                    text: "That touches the CLI and the writer, so I'll run it with the crew."
                        .into(),
                },
            ],
            milestones: vec![
                "Triage: crew · 7 files, tests exist".into(),
                "Goal approved · 3 criteria".into(),
                "Plan 1 won · 5 cards".into(),
            ],
            recap: "The plan waits for your approval at the plan checkpoint.".into(),
            recap_model: None,
        },
        AppEvent::ReplayDone {
            run: run_id(),
            seq: 100,
        },
    ]);
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
fn chat_answer_with_folded_tools() {
    snapshot("chat_answer_with_folded_tools", |harness, text| {
        harness.settings.verbosity = Verbosity::Normal;
        let answer = text.map_or_else(
            || {
                "Export writes CSV today. The writer lives in `src/export.rs`, and the flag \
                 would go in `src/cli.rs`.\n\nWant me to add `--json`?"
                    .to_owned()
            },
            ToOwned::to_owned,
        );
        harness.send(turn_started(1, "how does export work?"));
        for (id, path) in ["src/cli.rs", "src/export.rs", "src/lib.rs", "Cargo.toml"]
            .into_iter()
            .enumerate()
        {
            harness.send_all(tool(1, &format!("t{id}"), "Read", path));
        }
        // Split mid-sentence, as deltas arrive.
        let middle = (0..=answer.len() / 2)
            .rev()
            .find(|at| answer.is_char_boundary(*at))
            .unwrap_or(0);
        let (first, rest) = answer.split_at(middle);
        harness.send_all([delta(1, first), delta(1, rest), answered(1, &answer, 9)]);
    });
}

#[test]
fn a_call_between_parts_of_the_answer_folds_under_the_first_part() {
    let lines = render(
        |harness, _| {
            harness.settings.verbosity = Verbosity::Normal;
            harness.send_all([
                turn_started(1, "where is the writer?"),
                delta(1, "Let me look.\n"),
            ]);
            harness.send_all(tool(1, "t1", "Grep", "fn write"));
            harness.send_all([
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
        |harness, _| {
            harness.settings.verbosity = Verbosity::Normal;
            harness.send_all([
                turn_started(1, "where is the writer?"),
                delta(1, "Let me look."),
            ]);
            harness.send_all(tool(1, "t1", "Grep", "fn write"));
            harness.send(delta(1, "Found the writer. Now the flag."));
            harness.send_all(tool(1, "t2", "Read", "src/cli.rs"));
            harness.send_all([
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
fn chat_with_tools(harness: &mut Harness, text: Option<&str>, level: Verbosity) {
    harness.settings.verbosity = level;
    harness.transcript.chat = ChatTurns::default();
    let answer = text.unwrap_or("The test expected the old header, so I added the email column.");
    harness.send(turn_started(1, "why does the export test fail?"));
    let read = serde_json::json!({ "file_path": "src/export.rs" });
    harness.send_all(call(1, "t1", "Read", read, true, "1\n2\n3"));
    let test = serde_json::json!({ "command": "cargo test export" });
    let failed =
        "Exit code 101\ntest export_header ... FAILED\nassertion failed: header == \"id,name\"";
    harness.send_all(call(1, "t2", "Bash", test, false, failed));
    let edit = serde_json::json!({
        "file_path": "tests/export.rs",
        "old_string": "\"id,name\"",
        "new_string": "\"id,name,email\"",
    });
    harness.send_all(call(1, "t3", "Edit", edit, true, "ok"));
    harness.send_all([delta(1, answer), answered(1, answer, 12)]);
}

#[test]
fn chat_tools_at_each_verbosity() {
    snapshot("chat_tools_full", |harness, text| {
        chat_with_tools(harness, text, Verbosity::Full);
    });
    snapshot("chat_tools_normal", |harness, text| {
        chat_with_tools(harness, text, Verbosity::Normal);
    });
    snapshot("chat_tools_quiet", |harness, text| {
        chat_with_tools(harness, text, Verbosity::Quiet);
    });
    let at = |level| {
        let mut harness = Harness::new();
        chat_with_tools(&mut harness, None, level);
        text(&harness.printed(80))
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
    snapshot("interrupted_turn_line", |harness, text| {
        harness.send_all([
            turn_started(1, "rewrite the exporter"),
            delta(1, "Let me look at the exporter first.\n"),
            delta(1, text.unwrap_or("It writes each row")),
            chat(ChatEvent::TurnCancelled {
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

/// A direct solo turn as Claude Code ran it in your checkout: your message, its calls drawn as
/// Argonauts draws a CLI's, its own answer, and nothing after it but the `✻` line. No Triage
/// item, card, report, merge, or recap. Shown beside Claude Code's own screen for the turn.
fn direct_edit_turn(harness: &mut Harness, text: Option<&str>) {
    harness.settings.verbosity = Verbosity::Normal;
    let answer = text.unwrap_or("Changed hello to hi in greet.txt.");
    harness.send(chat(ChatEvent::TurnStarted {
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
    harness.send_all(call(1, "t1", "Read", read, true, "1\thello world"));
    let edit = serde_json::json!({
        "file_path": "greet.txt",
        "old_string": "hello world",
        "new_string": "hi world",
    });
    harness.send_all(call(1, "t2", "Edit", edit, true, "ok"));
    harness.send_all([delta(1, answer), answered(1, answer, 3)]);
}

#[test]
fn a_direct_solo_turn_reads_like_claude_codes() {
    snapshot("direct_solo_turn", direct_edit_turn);
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
            format!("direct_solo_turn_beside_claude_code_{width}"),
            format!(
                "Argonauts, direct solo:\n\n{printed}\n\nClaude Code:\n\n{CLAUDE_CODE_EDIT_TURN}"
            )
        );
    }
}

/// The agent ran out mid-turn and the next one carries on under a `↷` line.
#[test]
fn a_direct_turn_handed_off_goes_on_under_one_line() {
    let lines = render(
        |harness, _| {
            harness.settings.verbosity = Verbosity::Normal;
            harness.send(chat(ChatEvent::TurnStarted {
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
            harness.send_all(call(1, "t1", "Edit", edit, true, "ok"));
            harness.send(chat(ChatEvent::HandedOff {
                turn: TurnId::numbered(1),
                from: worker("claude-code", "haiku"),
                to: worker("codex", "gpt-5.6-luna"),
                why: "hit its usage limit".into(),
                told: "your request, the transcript so far, and that invoice/money.py changed"
                    .into(),
            }));
            let test = serde_json::json!({ "command": "python -m pytest -q" });
            harness.send_all(call(1, "t2", "Bash", test, true, "6 passed in 0.04s"));
            harness.send_all([
                delta(1, "All 6 tests pass."),
                answered(1, "All 6 tests pass.", 32),
            ]);
        },
        None,
        100,
    );
    let printed = text(&lines);
    insta::assert_snapshot!("direct_turn_handed_off_100", printed);
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
