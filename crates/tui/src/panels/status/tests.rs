use argonauts_core::runs::api::StatusSummary;

use super::*;
use crate::base::stability;
use crate::base::text::{plain_text, spans_width};
use crate::base::theme::{ColorLevel, Glyphs, Theme};
use crate::testing::sample;

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
    // One bar of the four tasks, then a row for each state that has any: its count and its
    // share of them, in one column whatever the counts.
    assert!(rows[2].contains('█'), "{rows:#?}");
    for (row, state) in [
        (3, "merged"),
        (4, "running"),
        (5, "waiting"),
        (6, "needs you"),
    ] {
        let expected = format!(" │ ██  {state:<16}{:>10}  {:>4}", 1, "25%");
        assert!(rows[row].starts_with(&expected), "{state}: {rows:#?}");
        assert!(rows[row].ends_with(" │"), "{state}: {rows:#?}");
    }
    assert!(
        rows[8].starts_with(" │ Goal  Reports export as CSV from the API"),
        "{rows:#?}"
    );
    assert!(rows[9].starts_with(" │ Plan  4 tasks:"), "{rows:#?}");
    for (row, parts) in [
        (
            11,
            [
                "✔ task-001",
                "CSV writer",
                "GPT 5.6",
                "merged · 6m12s · 38k tokens",
            ],
        ),
        (
            12,
            [
                "◐ task-002",
                "Report endpoint",
                "Opus 5.5",
                "running · 14m · 402k tokens",
            ],
        ),
        (
            13,
            [
                "○ task-003",
                "Docs and examples",
                "GPT 5.6",
                "waiting · needs task-002",
            ],
        ),
        (
            14,
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
    assert_eq!(rows.len(), 16, "{rows:#?}");
}

/// Above the input the run's states share one row, so the card keeps its rows for the tasks.
#[test]
fn the_pinned_run_card_keeps_its_states_on_one_row() {
    let rows = drawn(&sample::status(), 110, |cards, kit, frame| {
        Some(cards.pinned(kit, StatusCard::Run, Form::Full, frame))
    });
    assert!(
        rows[3].starts_with(" │ ▌ merged 1   ▌ running 1   ▌ waiting 1   ▌ needs you 1  "),
        "{rows:#?}"
    );
    assert!(rows[4].contains("task-001    CSV writer"), "{rows:#?}");
}

/// A state with too few of the tasks to make one percent still says it has some.
#[test]
fn a_state_with_a_sliver_of_the_tasks_reads_under_one_percent() {
    let mut summary = sample::status();
    let (merged, running) = (summary.tasks[0].clone(), summary.tasks[1].clone());
    summary.tasks = std::iter::repeat_n(merged, 200).chain([running]).collect();
    let rows = drawn(&summary, 110, |cards, kit, frame| {
        Some(cards.run(kit, frame, RunShape::WHOLE))
    });
    assert!(
        rows[3].starts_with(" │ ██  merged                 200  100%"),
        "{rows:#?}"
    );
    assert!(
        rows[4].starts_with(" │ ██  running                  1   <1%"),
        "{rows:#?}"
    );
}

#[test]
fn spend_says_when_no_limit_is_set_and_meters_one_that_is() {
    let free = drawn(&sample::status(), 60, |cards, kit, frame| {
        cards.spend(kit, frame)
    });
    assert!(free[1].ends_with("$1.20 · no limit set │"), "{free:#?}");
    assert!(free[2].contains("tokens   1.2M"), "{free:#?}");
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
    assert!(capped[2].contains("spend   ━"), "{capped:#?}");
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

/// Where the terminal draws logos, each task's row has the logo of who has it before the name,
/// and the column widens to hold it.
#[test]
fn the_run_card_puts_each_workers_logo_before_its_name() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    use crate::base::kit::logo::golden::{count, pictured, with_stand_ins};
    use crate::testing::pictures::golden::{Picture, Region, assert_matches, keep, theme};

    let theme = pictured(&theme());
    let kit = Kit::new(&theme);
    let names = sample::names();
    let summary = sample::status();
    let cards = StatusCards {
        summary: &summary,
        names: &names,
        title: Some("export-reports-csv"),
    };
    let drawn = cards.run(&kit, Frame::view(110), RunShape::WHOLE);
    let area = Rect::new(0, 0, 110, drawn.lines.len() as u16);
    let mut buf = Buffer::empty(area);
    for (y, line) in drawn.lines.iter().enumerate() {
        buf.set_line(0, y as u16, line, area.width);
    }
    assert_eq!(count(&buf), 4);
    let shown = with_stand_ins(&buf);
    keep("run_card_logos", &shown);
    let picture = Picture::load("run_card_logos");
    assert_matches(&picture, &shown, &Region::whole(&picture, (0, 0)), &theme);
}
