use argonauts_core::RunId;
use argonauts_core::names::ModelRef;
use argonauts_core::{Spend, TaskId};
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
        whole_day: false,
    }
}

fn line(day: Date, agent: &str, model: &str, tokens: u64) -> LedgerLine {
    LedgerLine {
        day,
        repo: "/work/minitorch".into(),
        run: None,
        agent: agent.into(),
        model: Some(model.into()),
        agent_id: "google".into(),
        input: tokens,
        cached: tokens / 2,
        output: 0,
        cost: 0.0,
        estimated: false,
        partial: false,
        hour: Some(21),
        conversation: None,
        took: 0,
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
    build_named(records, lines, work, period, &DisplayNames::default())
}

/// [`build`], telling models apart by `names`.
fn build_named(
    records: &[Record],
    lines: &[LedgerLine],
    work: &[WorkLine],
    period: OverviewPeriod,
    display: &DisplayNames,
) -> OverviewData {
    let prices = PriceList::default();
    let sources = Sources {
        records,
        lines,
        work,
        clock: utc(),
        prices: &prices,
        names: display,
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

/// A model's color goes by its tokens in all, so the list is the same whatever the period.
#[test]
fn every_model_is_listed_with_its_tokens_in_all_whatever_the_period() {
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
            "claude-haiku-5-5",
            2,
            3000,
        ),
    ];
    let lines = vec![
        // Claude Code's records hold this work already, so the ledger's line adds only the
        // name it gave the model.
        line(date!(2026 - 10 - 08), "claude-code", "haiku", 9_999),
        line(
            date!(2026 - 06 - 02),
            "antigravity-cli",
            "gemini-3.6-pro",
            200,
        ),
    ];
    let all = build(&records, &lines, &[], OverviewPeriod::All);
    let listed: Vec<(&str, Option<&str>, u64)> = all
        .all_time_models
        .iter()
        .map(|model| (model.agent.as_str(), model.model.as_deref(), model.tokens))
        .collect();
    assert_eq!(
        listed,
        [
            ("claude-code", Some("claude-haiku-5-5"), 3000),
            ("claude-code", Some("claude-opus-5-5"), 1000),
            ("antigravity-cli", Some("gemini-3.6-pro"), 200),
            ("claude-code", Some("haiku"), 0),
        ]
    );
    let week = build(&records, &lines, &[], OverviewPeriod::Week);
    assert_eq!(week.models.len(), 1, "only Haiku 5.5 ran this week");
    assert_eq!(week.all_time_models, all.all_time_models);
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
    let worker = ModelRef::new("codex", Some("gpt-5.6".into()));
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
            task: TaskId::new("t1").unwrap(),
            by: worker.clone(),
            kind: None,
            sent_back: 0,
            merged: true,
            reviewed: true,
            checks_first: None,
            checks_last: None,
        }),
        work_line(Work::Task {
            task: TaskId::new("t2").unwrap(),
            by: worker.clone(),
            kind: None,
            sent_back: 1,
            merged: true,
            reviewed: true,
            checks_first: None,
            checks_last: None,
        }),
        work_line(Work::Session {
            role: "worker".into(),
            by: worker.clone(),
            task: None,
            kind: None,
            spent: Spend::default(),
            took: 1,
            tools: 0,
            fallback_for: Some(ModelRef::new("claude-code", None)),
            why: "out".into(),
            window: Some("five_hour".into()),
            cause: None,
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

#[test]
fn the_models_chart_gets_each_days_tokens_of_the_top_three_models() {
    let lines = vec![
        line(date!(2026 - 10 - 01), "codex", "gpt-5.6", 900),
        line(date!(2026 - 10 - 01), "claude-code", "claude-opus-5-5", 400),
        line(
            date!(2026 - 10 - 02),
            "claude-code",
            "claude-opus-5-5",
            2_000,
        ),
        line(
            date!(2026 - 10 - 02),
            "antigravity-cli",
            "gemini-3.6-pro",
            100,
        ),
        line(
            date!(2026 - 10 - 03),
            "antigravity-cli",
            "gemini-3.6-flash",
            50,
        ),
        line(
            date!(2026 - 10 - 03),
            "claude-code",
            "claude-sonnet-5-5",
            300,
        ),
    ];
    let data = build(&[], &lines, &[], OverviewPeriod::All);
    let top: Vec<&str> = data
        .models
        .iter()
        .take(3)
        .filter_map(|model| model.model.as_deref())
        .collect();
    assert_eq!(top, ["claude-opus-5-5", "gpt-5.6", "claude-sonnet-5-5"]);
    let days: Vec<(Date, Vec<u64>)> = data
        .model_days
        .iter()
        .map(|day| (day.day, day.tokens.clone()))
        .collect();
    assert_eq!(
        days,
        [
            (date!(2026 - 10 - 01), vec![400, 900, 0]),
            (date!(2026 - 10 - 02), vec![2_000, 0, 0]),
            (date!(2026 - 10 - 03), vec![0, 0, 300]),
        ]
    );
    assert_eq!(data.peak_day, Some(date!(2026 - 10 - 02)));
    // The last seven days end on the 8th, so the 1st is left out.
    let week = build(&[], &lines, &[], OverviewPeriod::Week);
    assert_eq!(week.model_days.len(), 2);
}

#[test]
fn every_model_has_an_api_value_even_one_no_table_names() {
    let lines = vec![
        line(
            date!(2026 - 10 - 01),
            "opencode",
            "moonshot/kimi-k3",
            1_000_000,
        ),
        line(date!(2026 - 10 - 01), "codex", "gpt-5.6", 1_000_000),
        line(
            date!(2026 - 10 - 02),
            "claude-code",
            "claude-opus-5-5",
            1_000_000,
        ),
    ];
    let data = build(&[], &lines, &[], OverviewPeriod::All);
    assert_eq!(data.models.len(), 3);
    for model in &data.models {
        let value = model.api_value.unwrap_or_default();
        assert!(value > 0.0, "{model:?}");
    }
    assert!(!data.api_partial);
    let sum: f64 = data.models.iter().filter_map(|model| model.api_value).sum();
    assert!((data.api_value - sum).abs() < 1e-9);
}

/// The Stats tab's summary counts what ran through Argonauts Code in its own period.
#[test]
fn the_stats_summary_counts_what_ran_through_argonauts_in_its_period() {
    let home = tempfile::tempdir().unwrap();
    let paths = argonauts_core::Paths::new(home.path());
    let ledger = crate::usage_ledger::UsageLedger::new(&paths);
    let run = |day: Date| WorkLine {
        day,
        repo: "/work".into(),
        run: RunId::new("r1").unwrap(),
        what: Work::Run {
            by: None,
            took: 60,
            workflow: Some("council".into()),
            goals: 0,
            ultracode: false,
            answered: 1,
        },
    };
    crate::work_ledger::WorkLedger::beside(&paths.usage_dir())
        .append(&[run(date!(2026 - 09 - 01)), run(date!(2026 - 10 - 07))])
        .unwrap();
    let today = date!(2026 - 10 - 08);
    let week_starts = argonauts_core::settings::periods::WeekStart::Sunday;
    let summary = |period| {
        crate::usage_ledger::summary(&ledger, (period, week_starts), today, &[], Vec::new())
    };
    let all = summary(argonauts_core::api::UsagePeriod::All)
        .argonauts
        .expect("all of it");
    assert_eq!((all.workflows, all.answered), (2, 2));
    let week = summary(argonauts_core::api::UsagePeriod::Week)
        .argonauts
        .expect("this week's");
    assert_eq!(week.workflows, 1);
    assert_eq!(week.by_workflow, [("council".to_owned(), 1)]);
    assert!(
        summary(argonauts_core::api::UsagePeriod::Today)
            .argonauts
            .is_none()
    );
}

/// The ledger spells one model three ways, as the alias `haiku`, as `claude-haiku-5-5` and
/// with a date: the Overview lists it once, with all its tokens and sessions, under the
/// catalog's spelling, and its days and chart count it as one.
#[test]
fn a_model_spelled_three_ways_is_one_model() {
    use argonauts_core::model_catalog::CatalogEntry;

    let mut display = DisplayNames::default();
    let mut haiku = CatalogEntry::new(ModelRef::new(
        "claude-code",
        Some("claude-haiku-5-5".into()),
    ));
    haiku.name = Some("Haiku 5.5".to_owned());
    haiku.aliases = Some(vec!["haiku".to_owned()]);
    display.catalog.models.push(haiku);
    let day = date!(2026 - 10 - 07);
    let lines = [
        line(day, "claude-code", "haiku", 1_000),
        line(day, "claude-code", "claude-haiku-5-5", 2_000),
        line(day, "claude-code", "claude-haiku-5-5-20261001", 4_000),
        line(day, "codex", "gpt-5.6", 500),
    ];
    let data = build_named(&[], &lines, &[], OverviewPeriod::All, &display);
    let models: Vec<(&str, Option<&str>, u64)> = data
        .models
        .iter()
        .map(|model| {
            (
                model.agent.as_str(),
                model.model.as_deref(),
                model.tokens.total(),
            )
        })
        .collect();
    assert_eq!(
        models,
        [
            ("claude-code", Some("claude-haiku-5-5"), 7_000),
            ("codex", Some("gpt-5.6"), 500)
        ]
    );
    let year = data.days.iter().find(|part| part.day == day).unwrap();
    assert_eq!(year.model.as_deref(), Some("claude-haiku-5-5"));
    let chart = data.model_days.iter().find(|part| part.day == day).unwrap();
    assert_eq!(chart.tokens, [7_000, 500]);
}
