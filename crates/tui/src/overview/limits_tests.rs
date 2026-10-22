use argonauts_core::RunId;
use argonauts_core::names::{RoleName, SessionKey};

use super::*;
use crate::overview::sample;
use crate::state::{SessionState, TaskRow};
use crate::text::plain_text;
use crate::theme::{ColorLevel, Glyphs, Theme};

fn plain() -> Theme {
    let mut theme = Theme::dark().with_level(ColorLevel::None);
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn rows(drawn: Option<Drawn>) -> Vec<String> {
    let drawn = drawn.unwrap_or_default();
    plain_text(&drawn.lines)
        .lines()
        .map(|row| row.trim_end().to_owned())
        .collect()
}

fn task(n: u32, title: &str, state: TaskState, usd: f64, tokens: u64) -> TaskSpend {
    TaskSpend {
        task: TaskId::numbered(n),
        title: title.to_owned(),
        state,
        usd,
        tokens,
    }
}

fn four_tasks() -> Vec<TaskSpend> {
    vec![
        task(1, "CSV writer", TaskState::Merged, 0.31, 38_000),
        task(2, "Report endpoint", TaskState::Running, 0.62, 402_000),
        task(3, "Docs and examples", TaskState::Waiting, 0.09, 9_000),
        task(4, "Review", TaskState::Blocked, 0.18, 21_000),
    ]
}

fn tasks_card(task_limit: Option<LimitAmount>, width: usize) -> Vec<String> {
    let theme = plain();
    let tasks = four_tasks();
    let cards = LimitsCards {
        tasks: &tasks,
        task_limit,
        limits: &[],
    };
    rows(cards.tasks(&Kit::new(&theme), Frame::view(width)))
}

fn list(limits: &[SettingRow], width: usize, picked: usize) -> Vec<String> {
    let theme = plain();
    let cards = LimitsCards {
        tasks: &[],
        task_limit: None,
        limits,
    };
    rows(cards.list(&Kit::new(&theme), width, picked, "❯"))
}

#[test]
fn with_no_limit_each_task_shows_its_share_of_what_the_run_spent() {
    let rows = tasks_card(None, 100);
    assert!(rows[1].starts_with(" │ ◆ tasks"), "{rows:#?}");
    assert!(
        rows[1].ends_with("$1.20 · 4 tasks · no limit set │"),
        "{rows:#?}"
    );
    assert!(
        rows[2].starts_with(" │ ✔ task-001  CSV writer"),
        "{rows:#?}"
    );
    assert!(rows[2].ends_with("$0.31  26% │"), "{rows:#?}");
    assert!(
        rows[3].starts_with(" │ ◐ task-002  Report endpoint"),
        "{rows:#?}"
    );
    assert!(rows[3].ends_with("$0.62  52% │"), "{rows:#?}");
    // The costly task's meter is the longest.
    let filled = |row: &str| row.chars().filter(|c| matches!(c, '━' | '╸')).count();
    assert!(filled(&rows[3]) > filled(&rows[2]), "{rows:#?}");
    assert!(filled(&rows[2]) > filled(&rows[4]), "{rows:#?}");
}

#[test]
fn with_a_limit_each_task_fills_toward_it() {
    let limit = LimitAmount {
        unit: LimitUnit::Usd,
        amount: 1.0,
    };
    let rows = tasks_card(Some(limit), 100);
    assert!(
        rows[1].ends_with("$1.20 · 4 tasks · $1.00 a task │"),
        "{rows:#?}"
    );
    assert!(rows[2].ends_with("$0.31  31% │"), "{rows:#?}");
    assert!(rows[3].ends_with("$0.62  62% │"), "{rows:#?}");

    let tokens = LimitAmount {
        unit: LimitUnit::Tokens,
        amount: 300_000.0,
    };
    let rows = tasks_card(Some(tokens), 100);
    assert!(rows[1].contains("300k tokens a task"), "{rows:#?}");
    assert!(
        rows[3].ends_with("$0.62 134% │"),
        "past its limit: {rows:#?}"
    );

    // Time isn't counted by task here, so the card says nothing it can't show.
    let minutes = LimitAmount {
        unit: LimitUnit::Minutes,
        amount: 30.0,
    };
    assert!(tasks_card(Some(minutes), 100)[1].contains("no limit set"));
}

#[test]
fn a_narrow_card_gives_up_the_meter_and_keeps_the_numbers() {
    for width in [40, 50, 60, 80, 120, 200] {
        let rows = tasks_card(None, width);
        assert!(rows[3].contains("$0.62"), "{width}: {rows:#?}");
        assert!(rows[3].contains("52%"), "{width}: {rows:#?}");
        for row in &rows {
            assert!(crate::text::str_width(row) <= width, "{width}: {rows:#?}");
        }
    }
    assert!(!tasks_card(None, 40)[3].contains('━'));
}

#[test]
fn the_list_says_each_limit_in_words_and_where_it_came_from() {
    let rows = list(&sample::limits(), 110, 0);
    assert!(
        rows[0].starts_with(" Limits · none set, so nothing stops a run"),
        "{rows:#?}"
    );
    assert!(rows[0].ends_with("enter to change"), "{rows:#?}");
    let cells = |row: &str| -> Vec<String> {
        row.split("  ")
            .map(str::trim)
            .filter(|cell| !cell.is_empty())
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(
        cells(&rows[1]),
        [
            "❯ Spend for a run",
            "not set",
            "default",
            "limits.spend_per_run"
        ]
    );
    assert_eq!(
        cells(&rows[7]),
        [
            "When a limit is reached",
            "fallback",
            "default",
            "limits.when_reached"
        ]
    );
    assert_eq!(
        cells(&rows[8]),
        [
            "Wrap up from",
            "85% of a limit",
            "default",
            "limits.soft_stop"
        ]
    );

    let set = sample::with_limit(sample::limits(), "limits.spend_per_run", "3.0", "project");
    let set = sample::with_limit(set, "limits.plan_usage", "0.8", "global");
    let set = sample::with_limit(set, "limits.run_tokens", "2000000", "project-local");
    let rows = list(&set, 110, 2);
    assert!(
        rows[0].contains("at a limit the task falls back, or the run stops"),
        "{rows:#?}"
    );
    assert_eq!(
        cells(&rows[1]),
        [
            "Spend for a run",
            "$3.00",
            "this project",
            "limits.spend_per_run"
        ]
    );
    assert_eq!(
        cells(&rows[3]),
        [
            "❯ Plan usage",
            "80% of a window",
            "everywhere",
            "limits.plan_usage"
        ]
    );
    assert_eq!(
        cells(&rows[5]),
        [
            "Tokens for a run",
            "2M tokens",
            "just you",
            "limits.run_tokens"
        ]
    );
}

#[test]
fn a_long_list_scrolls_under_the_picked_limit_and_a_narrow_one_drops_the_keys() {
    let limits = sample::limits();
    let top = list(&limits, 110, 0);
    assert_eq!(top.len(), 1 + 8 + 1, "{top:#?}");
    assert!(top.last().unwrap().contains("1 of 10 · ↑↓  shows the rest"));
    assert!(!top.iter().any(|row| row.contains("Share for review")));
    let end = list(&limits, 110, 9);
    assert!(end[8].contains("❯ Share for review"), "{end:#?}");
    assert!(end[8].contains("15% of the run's"), "{end:#?}");
    assert!(!end.iter().any(|row| row.contains("Spend for a run")));

    let narrow = list(&limits, 70, 0);
    assert!(
        !narrow.iter().any(|row| row.contains("limits.")),
        "{narrow:#?}"
    );
    assert!(narrow[1].contains("Spend for a run"), "{narrow:#?}");
    for width in [30, 40, 60, 80, 200] {
        for row in list(&limits, width, 3) {
            assert!(crate::text::str_width(&row) <= width, "{width}: {row}");
        }
    }
    assert!(list(&[], 110, 0).is_empty());
}

#[test]
fn a_runs_tasks_add_up_what_their_sessions_spent() {
    let mut run = RunState::new(RunId::new("run-03").expect("a run id"));
    for (n, title) in [(1, "CSV writer"), (2, "Report endpoint")] {
        let task = TaskRow::new(TaskId::numbered(n), title);
        run.tasks.insert(task.id.clone(), task);
    }
    // Two sessions worked on the first task, one on no task at all.
    for (n, task, usd, tokens) in [
        (1, Some(1), 0.20, 10_000),
        (2, Some(1), 0.11, 5_000),
        (3, None, 0.50, 1_000),
    ] {
        let key = SessionKey::numbered(n);
        let worker = "codex/gpt-5.6".parse().expect("a model");
        let mut session = SessionState::new(
            key.clone(),
            worker,
            String::new(),
            RoleName::Worker,
            sample::now(),
        );
        session.task = task.map(TaskId::numbered);
        session.cost_usd = Some(usd);
        session.tokens_in = tokens;
        run.sessions.insert(key, session);
    }
    let tasks = tasks_of(&run);
    assert_eq!(tasks.len(), 2);
    assert!((tasks[0].usd - 0.31).abs() < 1e-9, "{tasks:?}");
    assert_eq!(tasks[0].tokens, 15_000);
    assert_eq!(tasks[1].usd, 0.0);
    assert_eq!(tasks[1].state, TaskState::Waiting);
}
