//! Who paid for a run's tokens, on a dim line under its end line: the parts that had tokens,
//! one word where a single payer had them all, and nothing for a run all on plans.

use std::path::PathBuf;

use argonauts_core::agents::session_runner::{Access, RunnerKind};
use argonauts_core::app::AppEvent;
use argonauts_core::base::names::{AgentId, RoleName, SessionKey};
use argonauts_core::runs::events::run::{RunLifecycle, RunMode, RunOutcome, RunStarted};
use argonauts_core::runs::events::session::{
    Billing, EndReason, SessionEnd, SessionEvent, SessionStarted,
};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::settings::ui::PaidByPlace;
use argonauts_core::{RunId, Spend};
use time::OffsetDateTime;
use time::macros::datetime;

use super::super::tests::live_rows_tests::Screen;
use crate::testing::pictures::picture_check::assert_matches;

const WIDTH: u16 = 118;

/// One session of a run: its CLI and model, who pays for it, and what it spent.
struct Spent {
    agent: &'static str,
    model: &'static str,
    billing: Option<Billing>,
    tokens: u64,
    cost: Option<f64>,
}

impl Spent {
    fn new(agent: &'static str, model: &'static str, billing: Billing, tokens: u64) -> Self {
        Spent {
            agent,
            model,
            billing: Some(billing),
            tokens,
            cost: None,
        }
    }

    fn costing(self, usd: f64) -> Self {
        Spent {
            cost: Some(usd),
            ..self
        }
    }
}

fn event(run: &RunId, ts: OffsetDateTime, session: Option<u32>, kind: RunEventKind) -> AppEvent {
    AppEvent::Run {
        event: Box::new(RunEvent {
            seq: 0,
            ts,
            run_id: run.clone(),
            task_id: None,
            session: session.map(SessionKey::numbered),
            source: "argonauts-code".into(),
            kind,
        }),
    }
}

fn run_started(title: &str) -> RunLifecycle {
    RunLifecycle::Started(Box::new(RunStarted {
        request: title.to_lowercase(),
        title: title.into(),
        mode: RunMode::Crew,
        forced: false,
        workflow: "orchestrator".into(),
        estimate: None,
        captain_or_crew: None,
        conversation_turn: None,
        continues: None,
        repo_root: PathBuf::from("/Users/you/code/example"),
        base_branch: Some("main".into()),
        base_commit: "8b41d07aa".into(),
        uncommitted: vec![],
        uncommitted_included: false,
        settings_sha256: String::new(),
        settings: serde_json::Value::Null,
        overrides: vec![],
        cli_versions: Default::default(),
        version: "0.2.0".into(),
        outside_agent: None,
        instructions: Vec::new(),
        required_steps: Vec::new(),
        starts_from: None,
    }))
}

fn session_started(spent: &Spent) -> SessionEvent {
    SessionEvent::Started(Box::new(SessionStarted {
        agent_id: AgentId(spent.agent.to_owned()),
        worker: format!("{}/{}", spent.agent, spent.model)
            .parse()
            .expect("a model"),
        provider: spent.agent.to_owned(),
        kind: RunnerKind::Cli,
        role: RoleName::Worker,
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
        billing: spent.billing,
    }))
}

fn session_ended(spent: &Spent) -> SessionEvent {
    SessionEvent::Ended(Box::new(SessionEnd {
        cli_session: None,
        exit_code: Some(0),
        signal: None,
        duration_ms: 60_000,
        reason: EndReason::Completed,
        spend: Spend {
            input_tokens: spent.tokens * 9,
            output_tokens: spent.tokens,
            cost_usd: spent.cost.unwrap_or_default(),
            cost_partial: spent.cost.is_none(),
            ..Spend::default()
        },
        tool_calls: 0,
        result_text: None,
        structured: None,
        last_message: None,
        raw_log: None,
    }))
}

/// A run of `sessions` that merged after `took` seconds.
fn merged_run(screen: &mut Screen, name: &str, title: &str, took: i64, sessions: &[Spent]) {
    let run = RunId::new(name).expect("a run id");
    let start = datetime!(2026-10-20 10:00 UTC);
    let end = start + time::Duration::seconds(took);
    screen.seen(event(
        &run,
        start,
        None,
        RunEventKind::Run(run_started(title)),
    ));
    for (n, spent) in (1..).zip(sessions) {
        let started = RunEventKind::Session(session_started(spent));
        screen.seen(event(&run, start, Some(n), started));
        let ended = RunEventKind::Session(session_ended(spent));
        screen.seen(event(&run, end, Some(n), ended));
    }
    let finished = RunLifecycle::Finished {
        outcome: RunOutcome::Merged,
        detail: None,
    };
    screen.seen(event(&run, end, None, RunEventKind::Run(finished)));
}

/// Four runs as they end: one shared by a plan, a model on this computer and a key; one all
/// on plans; one all on this computer; one all on a key.
fn four_runs(screen: &mut Screen) {
    screen.settings.turn_duration = false;
    merged_run(
        screen,
        "run-20261020-100000-a1b2",
        "Audit the auth routes",
        408,
        &[
            Spent::new("claude-code", "sonnet", Billing::Plan, 1_240_000),
            Spent::new("ollama", "qwen3-coder:30b", Billing::Free, 232_000),
            Spent::new("openrouter", "kimi-k3", Billing::Paid, 58_000).costing(0.31),
        ],
    );
    merged_run(
        screen,
        "run-20261020-100000-c3d4",
        "Add due dates",
        380,
        &[
            Spent::new("claude-code", "opus", Billing::Plan, 1_400_000),
            Spent::new("codex", "gpt-5.6-luna", Billing::Plan, 530_000),
        ],
    );
    merged_run(
        screen,
        "run-20261020-100000-e5f6",
        "Make the rate limiter",
        1_351,
        &[Spent::new(
            "ollama",
            "qwen3-coder:30b",
            Billing::Free,
            112_000,
        )],
    );
    merged_run(
        screen,
        "run-20261020-100000-a7b8",
        "Per key rate limit",
        250,
        &[
            Spent::new("openrouter", "kimi-k3", Billing::Paid, 702_000).costing(1.64),
            Spent::new("openrouter", "glm-5", Billing::Paid, 200_000).costing(0.50),
        ],
    );
}

#[test]
fn a_run_says_who_paid_for_its_tokens_under_its_end_line() {
    let mut screen = Screen::new();
    four_runs(&mut screen);
    let lines = screen.lines(WIDTH);
    assert_matches(
        "end_line_who_paid",
        &lines,
        include_str!("../../../../tests/golden/transcript/end_line_who_paid.txt"),
        include_str!("../../../../tests/golden/transcript/end_line_who_paid.html"),
        &Screen::theme(),
        &[],
    );
}

#[test]
fn the_end_line_keeps_who_paid_off_where_paid_by_leaves_it_out() {
    let mut screen = Screen::new();
    screen.settings.paid_by = vec![PaidByPlace::Usage];
    four_runs(&mut screen);
    let text = screen.text(WIDTH);
    for words in ["plan", "free", "paid"] {
        assert!(!text.contains(words), "{text}");
    }
    assert_eq!(text.matches("Run finished").count(), 4, "{text}");
}

#[test]
fn a_run_with_a_session_that_never_said_who_pays_says_nothing_of_it() {
    let mut screen = Screen::new();
    screen.settings.turn_duration = false;
    let unknown = Spent {
        billing: None,
        ..Spent::new("codex", "gpt-5.6-luna", Billing::Plan, 400_000)
    };
    merged_run(
        &mut screen,
        "run-20261020-100000-c9d0",
        "Port the CSV export",
        302,
        &[
            Spent::new("ollama", "qwen3-coder:30b", Billing::Free, 90_000),
            unknown,
        ],
    );
    let text = screen.text(WIDTH);
    assert!(text.contains("Run finished"), "{text}");
    assert!(!text.contains("free"), "{text}");
}

#[test]
fn paid_tokens_with_no_cost_say_no_amount() {
    let mut screen = Screen::new();
    screen.settings.turn_duration = false;
    merged_run(
        &mut screen,
        "run-20261020-100000-e1f2",
        "Tidy the log lines",
        120,
        &[
            Spent::new("claude-code", "sonnet", Billing::Plan, 150_000),
            Spent::new("openrouter", "kimi-k3", Billing::Paid, 27_000),
        ],
    );
    let rows = screen.rows(WIDTH);
    let paid = rows
        .iter()
        .find(|row| row.contains("paid"))
        .expect("a who-paid line");
    assert_eq!(paid.trim(), "150k plan · 27k paid");
}
