use argonauts_core::api::StatusSummary;

use super::*;
use crate::overview::sample;
use crate::stability;
use crate::text::{plain_text, spans_width};
use crate::theme::{ColorLevel, Glyphs, Theme};

fn plain() -> Theme {
    let mut theme = Theme::dark().with_level(ColorLevel::None);
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn rows(drawn: &Drawn) -> Vec<String> {
    plain_text(&drawn.lines)
        .lines()
        .map(|row| row.trim_end().to_owned())
        .collect()
}

fn drawn(
    summary: &StatusSummary,
    width: usize,
    card: fn(&StatusCards<'_>, &Kit<'_>, Frame) -> Option<Drawn>,
) -> Vec<String> {
    let theme = plain();
    let kit = Kit::new(&theme);
    let names = sample::names();
    let cards = StatusCards {
        summary,
        names: &names,
        title: Some("export-reports-csv"),
    };
    card(&cards, &kit, Frame::view(width))
        .map(|drawn| rows(&drawn))
        .unwrap_or_default()
}

#[test]
fn the_run_card_counts_its_tasks_by_state_and_lists_each_with_who_has_it() {
    let rows = drawn(&sample::status(), 110, |cards, kit, frame| {
        Some(cards.run(kit, frame, RunShape::WHOLE))
    });
    assert!(
        rows[1].starts_with(" │ ◆ run-03 · export-reports-csv"),
        "{rows:#?}"
    );
    assert!(
        rows[1].ends_with("1 of 4 merged [needs you] │"),
        "{rows:#?}"
    );
    // One bar of the four tasks, then each state that has any with its count.
    assert!(rows[2].contains('█'), "{rows:#?}");
    assert!(
        rows[3].starts_with(" │ ▌ merged 1   ▌ running 1   ▌ waiting 1   ▌ needs you 1  "),
        "{rows:#?}"
    );
    assert!(
        rows[5].starts_with(" │ Goal  Reports export as CSV from the API"),
        "{rows:#?}"
    );
    assert!(rows[6].starts_with(" │ Plan  4 tasks:"), "{rows:#?}");
    for (row, parts) in [
        (
            8,
            [
                "✔ task-001",
                "CSV writer",
                "GPT 5.6",
                "merged · 6m12s · 38k tokens",
            ],
        ),
        (
            9,
            [
                "◐ task-002",
                "Report endpoint",
                "Opus 5.5",
                "running · 14m · 402k tokens",
            ],
        ),
        (
            10,
            [
                "○ task-003",
                "Docs and examples",
                "GPT 5.6",
                "waiting · needs task-002",
            ],
        ),
        (
            11,
            [
                "◆ task-004",
                "Review",
                "Opus 5.5",
                "asks you · which date format to use",
            ],
        ),
    ] {
        for part in parts {
            assert!(rows[row].contains(part), "{part}: {rows:#?}");
        }
    }
    assert_eq!(rows.len(), 13, "{rows:#?}");
}

#[test]
fn spend_says_when_no_limit_is_set_and_meters_one_that_is() {
    let free = drawn(&sample::status(), 60, |cards, kit, frame| {
        cards.spend(kit, frame)
    });
    assert!(free[1].ends_with("$1.20 · no limit set │"), "{free:#?}");
    assert!(free[2].contains("tokens 1.2M"), "{free:#?}");
    assert!(
        free[3].contains("none, so nothing stops this run"),
        "{free:#?}"
    );

    let capped = drawn(&sample::status_with_limit(), 60, |cards, kit, frame| {
        cards.spend(kit, frame)
    });
    assert!(
        capped[1].ends_with("$1.20 of $3.00  [40%] │"),
        "{capped:#?}"
    );
    assert!(capped[2].contains("spend ━"), "{capped:#?}");
    assert!(capped[2].contains(" 40%"), "{capped:#?}");
}

#[test]
fn what_needs_you_has_a_row_each_and_recent_is_one_dim_row() {
    let summary = sample::status();
    let needs = drawn(&summary, 110, |cards, kit, frame| {
        cards.needs_you(kit, frame)
    });
    assert!(needs[1].starts_with(" │ ◆ needs you"), "{needs:#?}");
    assert!(needs[1].ends_with("[1] │"), "{needs:#?}");
    assert!(
        needs[2].contains("◆ task-004 Review asks which date format the CSV uses"),
        "{needs:#?}"
    );

    let theme = plain();
    let kit = Kit::new(&theme);
    let names = sample::names();
    let cards = StatusCards {
        summary: &summary,
        names: &names,
        title: None,
    };
    let recent = rows(&cards.recent(&kit, 110).unwrap());
    assert_eq!(
        recent,
        [" Recent  task-003 moved from agy to Codex 20m ago · task-001 merged 12m ago"]
    );

    let quiet = StatusSummary::default();
    assert!(drawn(&quiet, 110, |cards, kit, frame| cards.needs_you(kit, frame)).is_empty());
    let cards = StatusCards {
        summary: &quiet,
        names: &names,
        title: None,
    };
    assert!(cards.is_empty());
    assert!(cards.recent(&kit, 110).is_none());
}

#[test]
fn a_run_with_no_tasks_yet_shows_its_goal_and_no_bar() {
    let mut summary = sample::status();
    summary.tasks.clear();
    summary.waiting.clear();
    let rows = drawn(&summary, 110, |cards, kit, frame| {
        Some(cards.run(kit, frame, RunShape::WHOLE))
    });
    let header = rows[1].trim_end_matches('│').trim_end();
    assert!(
        header.ends_with("◆ run-03 · export-reports-csv"),
        "no count and no badge: {rows:#?}"
    );
    assert!(!rows.iter().any(|row| row.contains('█')), "{rows:#?}");
    assert!(rows[3].contains("Goal  "), "{rows:#?}");
    assert_eq!(rows.len(), 6, "{rows:#?}");
}

#[test]
fn every_card_keeps_inside_its_width() {
    let theme = plain();
    let kit = Kit::new(&theme);
    let names = sample::names();
    for summary in [sample::status(), sample::status_with_limit()] {
        let cards = StatusCards {
            summary: &summary,
            names: &names,
            title: Some("export-reports-csv"),
        };
        for width in stability::WIDTHS.map(usize::from) {
            let frame = Frame::view(width);
            let all = [
                Some(cards.run(&kit, frame, RunShape::WHOLE)),
                cards.spend(&kit, frame),
                cards.needs_you(&kit, frame),
                cards.recent(&kit, width),
            ];
            for drawn in all.into_iter().flatten() {
                for line in &drawn.lines {
                    assert!(spans_width(&line.spans) <= width, "at {width}: {line}");
                }
            }
        }
    }
}
