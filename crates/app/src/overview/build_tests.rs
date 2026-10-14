use argonauts_core::RunId;
use argonauts_core::names::WorkerRef;
use argonauts_core::{CardId, Spend};
use time::macros::{date, datetime};

use super::*;

/// Times shown in UTC, whatever zone the machine running the tests is in.
fn utc() -> WallClock {
    WallClock::pinned(datetime!(2026-10-08 12:00 UTC))
}

fn record(at: OffsetDateTime, cli: &str, model: &str, session: u32, tokens: u64) -> Record {
    Record {
        at,
        cli: cli.into(),
        model: model.into(),
        folder: "/work/argonauts-code".into(),
        input: tokens / 10,
        output: tokens / 10,
        cache_read: tokens * 7 / 10,
        cache_write: tokens / 10,
        session,
        subagent: false,
    }
}

fn line(day: Date, agent: &str, model: &str, tokens: u64) -> LedgerLine {
    LedgerLine {
        day,
        repo: "/work/minitorch".into(),
        run: None,
        agent: agent.into(),
        model: Some(model.into()),
        vendor: "google".into(),
        input: tokens,
        cached: tokens / 2,
        output: 0,
        cost: 0.0,
        estimated: false,
        partial: false,
        hour: Some(21),
    }
}

fn names(id: &str) -> String {
    match id {
        "claude-code" => "Claude Code".into(),
        "codex" => "Codex".into(),
        other => other.into(),
    }
}

fn build(
    records: &[Record],
    lines: &[LedgerLine],
    work: &[WorkLine],
    period: OverviewPeriod,
) -> OverviewData {
    let sources = Sources {
        records,
        lines,
        work,
        clock: utc(),
    };
    overview(
        &sources,
        period,
        date!(2026 - 10 - 08),
        ImportStatus::Done,
        YearColors::Cli,
        &names,
    )
}

#[test]
fn imported_clis_count_from_their_records_and_the_rest_from_the_ledger() {
    let records = vec![
        record(
            datetime!(2025-10-09 21:10 UTC),
            "claude-code",
            "claude-opus-5-5",
            1,
            1000,
        ),
        record(
            datetime!(2026-10-08 09:00 UTC),
            "claude-code",
            "claude-opus-5-5",
            2,
            3000,
        ),
        record(datetime!(2026-02-03 10:00 UTC), "codex", "gpt-5.6", 3, 500),
    ];
    let lines = vec![
        // Claude Code also wrote its own file for this, so the ledger's copy isn't counted.
        line(
            date!(2026 - 10 - 08),
            "claude-code",
            "claude-opus-5-5",
            9_999,
        ),
        line(
            date!(2026 - 06 - 02),
            "antigravity-cli",
            "gemini-3.6-pro",
            200,
        ),
    ];
    let data = build(&records, &lines, &[], OverviewPeriod::All);
    assert_eq!(data.total.total(), 1000 + 3000 + 500 + 200);
    let sources: Vec<(&str, bool, u32)> = data
        .sources
        .iter()
        .map(|source| (source.agent.as_str(), source.imported, source.sessions))
        .collect();
    assert_eq!(
        sources,
        vec![
            ("claude-code", true, 2),
            ("codex", true, 1),
            ("antigravity-cli", false, 1)
        ]
    );
    assert_eq!(
        data.found.as_deref(),
        Some("Claude Code since Oct 2025, 2 sessions; Codex since Feb, 1 session")
    );
    assert_eq!(data.models[0].model.as_deref(), Some("claude-opus-5-5"));
    assert!(
        data.models[0].api_value.is_some(),
        "Opus 5.5 has a list price"
    );
    assert_eq!(data.hours[21], 1000 + 200, "the ledger's hour counts too");
    assert_eq!(data.hours[9], 3000);
    let folders: Vec<&str> = data.folders.iter().map(|(f, _)| f.as_str()).collect();
    assert_eq!(folders, vec!["/work/argonauts-code", "/work/minitorch"]);
}

#[test]
fn the_period_keeps_its_days_but_the_year_and_weeks_keep_theirs() {
    let records = vec![
        record(datetime!(2026-09-01 12:00 UTC), "codex", "gpt-5.6", 1, 800),
        record(datetime!(2026-10-07 12:00 UTC), "codex", "gpt-5.6", 2, 100),
        record(datetime!(2026-10-08 12:00 UTC), "codex", "gpt-5.6", 2, 100),
    ];
    let week = build(&records, &[], &[], OverviewPeriod::Week);
    assert_eq!(week.total.total(), 200);
    assert_eq!(week.active_days, 2);
    assert_eq!(week.span_days, 7);
    assert_eq!(week.days.len(), 3, "the year shows every day");
    assert_eq!(week.weeks.len(), 26);
    assert_eq!(week.weeks.iter().map(|w| w.tokens()).sum::<u64>(), 1000);
    assert_eq!((week.streak, week.longest_streak), (2, 2));
    assert_eq!(week.recent, 200);
    let all = build(&records, &[], &[], OverviewPeriod::All);
    assert_eq!(all.span_days, 38, "from the first day with work");
    // Session 2 ran from the 7th to the 8th: a day and its last hour.
    assert_eq!(all.longest_session_secs, 24 * 3600 + 3600);
}

#[test]
fn a_day_takes_the_colour_of_the_cli_that_ran_most() {
    let at = datetime!(2026-10-08 12:00 UTC);
    let records = vec![
        record(at, "claude-code", "claude-sonnet-5-5", 1, 100),
        record(at, "codex", "gpt-5.6", 2, 300),
        record(at, "codex", "gpt-5.6-luna", 3, 200),
    ];
    let data = build(&records, &[], &[], OverviewPeriod::All);
    let day = data.days.last().unwrap();
    assert_eq!(day.agent, "codex");
    assert_eq!(day.model.as_deref(), Some("gpt-5.6"));
    assert_eq!(day.tokens, 600);
}

#[test]
fn what_ran_through_argonauts_comes_from_the_work_ledger() {
    let run = RunId::new("r1").unwrap();
    let work_line = |what: Work| WorkLine {
        day: date!(2026 - 06 - 02),
        repo: "/work".into(),
        run: run.clone(),
        what,
    };
    let worker = WorkerRef::new("codex", Some("gpt-5.6".into()));
    let work = vec![
        work_line(Work::Run {
            by: None,
            took: 60,
            workflow: Some("council".into()),
            goals: 0,
            ultracode: false,
            answered: 2,
        }),
        work_line(Work::Run {
            by: None,
            took: 60,
            workflow: Some("tidy-docs".into()),
            goals: 0,
            ultracode: false,
            answered: 0,
        }),
        work_line(Work::Task {
            task: CardId::new("t1").unwrap(),
            by: worker.clone(),
            kind: None,
            sent_back: 0,
            merged: true,
            reviewed: true,
        }),
        work_line(Work::Task {
            task: CardId::new("t2").unwrap(),
            by: worker.clone(),
            kind: None,
            sent_back: 1,
            merged: true,
            reviewed: true,
        }),
        work_line(Work::Session {
            role: "worker".into(),
            by: worker.clone(),
            task: None,
            kind: None,
            spent: Spend::default(),
            took: 1,
            stood_in_for: Some(WorkerRef::new("claude-code", None)),
            why: "out".into(),
            window: Some("five_hour".into()),
        }),
    ];
    let data = build(&[], &[], &work, OverviewPeriod::All);
    let through = data.argonauts.unwrap();
    assert_eq!(through.workflows, 2);
    assert_eq!(
        through.by_workflow,
        vec![("council".to_owned(), 1), ("tidy-docs".to_owned(), 1)]
    );
    assert_eq!((through.merged, through.first_pass), (2, Some(0.5)));
    assert_eq!((through.fallbacks, through.for_limit), (1, 1));
    assert_eq!(through.answered, 2);
}

#[test]
fn nothing_recorded_is_an_empty_overview() {
    let data = build(&[], &[], &[], OverviewPeriod::All);
    assert_eq!(data.total.total(), 0);
    assert!(data.models.is_empty() && data.found.is_none() && data.argonauts.is_none());
    assert_eq!(data.span_days, 1);
}

/// A session whose subagents ran a smaller model counts once, under the model that did most of
/// its work, so the models' sessions add up to the Sessions tile.
#[test]
fn a_session_over_two_models_counts_once() {
    let at = datetime!(2026-10-08 12:00 UTC);
    let records = vec![
        record(at, "claude-code", "claude-opus-5-5", 1, 9_000),
        record(at, "claude-code", "claude-haiku-4-5", 1, 1_000),
        record(at, "claude-code", "claude-haiku-4-5", 2, 500),
    ];
    let data = build(&records, &[], &[], OverviewPeriod::All);
    let sessions = |model: &str| {
        data.models
            .iter()
            .find(|row| row.model.as_deref() == Some(model))
            .map(|row| row.sessions)
    };
    assert_eq!(sessions("claude-opus-5-5"), Some(1));
    assert_eq!(sessions("claude-haiku-4-5"), Some(1));
    let column: u32 = data.models.iter().map(|row| row.sessions).sum();
    assert_eq!(column, data.sessions());
    assert_eq!(column, 2);
}
