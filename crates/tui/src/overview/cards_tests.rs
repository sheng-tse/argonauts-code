use argonauts_core::api::{PlanUsage, UsageSummary};

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

fn rows(usage: &UsageSummary, card: UsageCard, form: Form, width: usize) -> Vec<String> {
    let theme = plain();
    let kit = Kit::new(&theme);
    let names = sample::names();
    let cards = UsageCards {
        usage,
        names: &names,
        now: sample::now(),
        left: false,
    };
    let drawn = cards.draw(&kit, card, form, Frame::view(width));
    plain_text(&drawn.lines)
        .lines()
        .map(|row| row.trim_end().to_owned())
        .collect()
}

#[test]
fn plan_usage_has_a_row_for_each_cli_with_both_windows() {
    let rows = rows(&sample::usage(), UsageCard::PlanUsage, Form::Compact, 110);
    assert_eq!(rows.len(), 7, "{rows:#?}");
    assert!(rows[1].starts_with(" │ ◆ plan usage"), "{rows:#?}");
    assert!(
        rows[1].ends_with("no limit set · highest Codex 5h  [92%] │"),
        "{rows:#?}"
    );
    let claude = &rows[2];
    assert!(claude.starts_with(" │ Claude Code Max"), "{claude}");
    for part in [
        "5h",
        " 71%",
        "resets 4:40pm",
        "7d",
        " 38%",
        "resets Tue 10pm",
    ] {
        assert!(claude.contains(part), "{part}: {claude}");
    }
    let codex = &rows[3];
    for part in [
        "Codex Plus",
        " 92%",
        "resets 2:15pm",
        " 41%",
        "resets Fri 9am",
    ] {
        assert!(codex.contains(part), "{part}: {codex}");
    }
    assert!(
        rows[4].contains(
            "Antigravity CLI Pro  out until Tue 3:45pm · its tasks moved to GPT 5.6 on Codex"
        ),
        "{rows:#?}"
    );
    assert!(
        rows[5].contains("OpenRouter pays per token, so there is no plan to track"),
        "{rows:#?}"
    );
    // Both CLIs' meters start in the same column, and so do their numbers.
    let at = |row: &str, what: &str| row.find(what).map(|byte| row[..byte].chars().count());
    assert_eq!(at(claude, "5h"), at(codex, "5h"));
    assert_eq!(at(claude, "7d"), at(codex, "7d"));
}

#[test]
fn in_color_a_windows_meter_is_half_blocks_and_only_its_number_warns() {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    let kit = Kit::new(&theme);
    let (usage, names) = (sample::usage(), sample::names());
    let cards = UsageCards {
        usage: &usage,
        names: &names,
        now: sample::now(),
        left: false,
    };
    let drawn = cards.draw(&kit, UsageCard::PlanUsage, Form::Compact, Frame::view(110));
    let codex = &drawn.lines[3].spans;
    let text: String = codex.iter().map(|span| span.content.as_ref()).collect();
    assert!(text.contains("▄▄▄") && !text.contains('━'), "{text}");
    let style_of = |what: &str| {
        let span = codex.iter().find(|span| span.content.contains(what));
        span.unwrap_or_else(|| panic!("{what}: {text}")).style
    };
    // At 92% the number is amber. When the window resets stays as quiet as on any row.
    assert_eq!(style_of(" 92%"), kit.bold(Level::Warn.strong_role()));
    assert_eq!(style_of("resets 2:15pm"), kit.dim("").style);
    assert_eq!(style_of("resets Fri 9am"), kit.dim("").style);
}

#[test]
fn in_full_each_cli_says_its_plan_whether_its_in_use_and_how_old_the_reading_is() {
    let rows = rows(&sample::usage(), UsageCard::PlanUsage, Form::Full, 110);
    assert!(
        rows.iter()
            .any(|row| row.contains("● Claude Code Max · in use · read 1m ago")),
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.contains("Codex Plus · read 3m ago")),
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .filter(|row| row.contains("5-hour and 7-day"))
            .count()
            == 2,
        "{rows:#?}"
    );
}

#[test]
fn a_narrow_card_keeps_each_clis_fullest_window() {
    let rows = rows(&sample::usage(), UsageCard::PlanUsage, Form::Compact, 50);
    let codex = rows.iter().find(|row| row.contains("Codex Plus")).unwrap();
    assert!(codex.contains("5h") && codex.contains("92%"), "{codex}");
    assert!(!codex.contains("7d"), "{codex}");
}

#[test]
fn what_is_left_reads_as_the_rest_of_the_window() {
    let theme = plain();
    let kit = Kit::new(&theme);
    let (usage, names) = (sample::usage(), sample::names());
    let cards = UsageCards {
        usage: &usage,
        names: &names,
        now: sample::now(),
        left: true,
    };
    let drawn = cards.draw(&kit, UsageCard::PlanUsage, Form::Compact, Frame::view(110));
    let text = plain_text(&drawn.lines);
    assert!(text.contains(" 29%") && text.contains("  8%"), "{text}");
}

#[test]
fn spend_names_who_spent_what_and_tokens_what_they_were() {
    let usage = sample::usage();
    let spend = rows(&usage, UsageCard::Spend, Form::Full, 110);
    assert!(
        spend[1].ends_with("~$1.86 today · no limit set · 3 runs and this chat │"),
        "{spend:#?}"
    );
    let legend = spend[3..].join("\n");
    for part in [
        "on Claude Code",
        "~$1.12",
        "60%",
        "$0.62",
        "33%",
        "on agy",
        "~$0.12",
    ] {
        assert!(legend.contains(part), "{part}: {legend}");
    }
    let tokens = rows(&usage, UsageCard::Tokens, Form::Full, 60);
    assert!(tokens[1].ends_with("1.4M today │"), "{tokens:#?}");
    let legend = tokens[3..].join("\n");
    for part in [
        "from cache",
        "950k",
        "68%",
        "new input",
        "392k",
        "28%",
        "output",
        "58k",
    ] {
        assert!(legend.contains(part), "{part}: {legend}");
    }
}

#[test]
fn a_spend_limit_is_what_the_bar_counts_toward() {
    let usage = UsageSummary {
        spend_limit: Some(5.0),
        ..sample::usage()
    };
    let spend = rows(&usage, UsageCard::Spend, Form::Compact, 80);
    assert!(
        spend[1].ends_with("~$1.86 of $5.00 today  [37%] │"),
        "{spend:#?}"
    );
    let bar = &spend[2];
    let filled = bar.chars().filter(|c| *c == '█').count();
    let cells = bar.chars().filter(|c| matches!(c, '█' | '░')).count();
    assert!(filled * 100 / cells.max(1) < 45, "{bar}");
}

#[test]
fn a_cli_at_your_plan_limit_says_so_and_who_takes_its_tasks() {
    let mut usage = UsageSummary {
        plan_limit: Some(0.8),
        ..sample::usage()
    };
    let codex = usage
        .plans
        .iter_mut()
        .find(|plan| plan.agent == "codex")
        .expect("codex");
    codex.moved_to = Some("Sonnet 5.5 on Claude Code".to_owned());
    let rows = rows(&usage, UsageCard::PlanUsage, Form::Compact, 160);
    assert!(
        rows[1].contains("your limit 80% · highest Codex 5h"),
        "{rows:#?}"
    );
    // Claude Code is under the limit, so both of its windows show as ever.
    assert!(
        rows[2].contains(" 71%") && rows[2].contains(" 38%"),
        "{rows:#?}"
    );
    let codex = &rows[3];
    assert!(codex.contains(" 92%"), "{codex}");
    assert!(
        codex.contains("limit reached · Sonnet 5.5 on Claude Code takes its tasks"),
        "{codex}"
    );
    assert!(
        !codex.contains(" 41%"),
        "the other window gives way: {codex}"
    );

    // Where the words don't fit beside the window they go under it, whole.
    let narrow = self::rows(&usage, UsageCard::PlanUsage, Form::Compact, 100);
    assert!(narrow[3].contains(" 92%"), "{narrow:#?}");
    assert!(
        narrow[4].contains("limit reached · Sonnet 5.5 on Claude Code takes its tasks"),
        "{narrow:#?}"
    );

    // Nobody to take over, the limit still shows as reached.
    usage.plans[1].moved_to = None;
    let rows = self::rows(&usage, UsageCard::PlanUsage, Form::Compact, 160);
    assert!(
        rows[3]
            .trim_end_matches('│')
            .trim_end()
            .ends_with("limit reached"),
        "{rows:#?}"
    );
}

#[test]
fn history_charts_fourteen_days_and_workflows_list_the_periods_runs() {
    let usage = sample::usage();
    let history = rows(&usage, UsageCard::History, Form::Full, 60);
    assert!(history[1].ends_with("in 14 days │"), "{history:#?}");
    assert!(
        history
            .iter()
            .any(|row| row.contains("M T W T F S S M T W T F S S") || row.contains("T W T F S S M")),
        "{history:#?}"
    );
    let workflows = rows(&usage, UsageCard::Workflows, Form::Full, 110);
    assert!(
        workflows[1].ends_with("today · 3 · ~$1.65 │"),
        "{workflows:#?}"
    );
    for (row, parts) in [
        (
            2,
            [
                "◐ export-reports-csv",
                "running",
                "1/4 tasks",
                "~$1.20     51m │",
            ],
        ),
        (
            3,
            [
                "✔ fix-flaky-export-test",
                "merged",
                "2/2 tasks",
                "~$0.39  12m40s │",
            ],
        ),
        (
            4,
            [
                "■ streaming-writer",
                "stopped",
                "0/1 tasks",
                "~$0.06   3m10s │",
            ],
        ),
    ] {
        for part in parts {
            assert!(workflows[row].contains(part), "{part}: {workflows:#?}");
        }
    }
    // The meter is the share of its tasks a run has merged, as wide as the card leaves it.
    let filled = |row: &str| row.matches('━').count();
    let cells = |row: &str| row.matches(['━', '╸', '─']).count();
    assert_eq!(cells(&workflows[2]), cells(&workflows[3]));
    assert!(cells(&workflows[2]) > 40, "{workflows:#?}");
    let quarter = filled(&workflows[2]) as f64 / cells(&workflows[2]) as f64;
    assert!((quarter - 0.25).abs() < 0.03, "{workflows:#?}");
    assert_eq!(filled(&workflows[3]), cells(&workflows[3]));
    assert_eq!(filled(&workflows[4]), 0);
}

#[test]
fn a_workflow_with_no_tasks_or_start_shows_only_what_is_known() {
    let mut usage = sample::usage();
    usage.progress.clear();
    let workflows = rows(&usage, UsageCard::Workflows, Form::Full, 110);
    assert!(
        workflows[2].ends_with("running  ~$1.20 │") || workflows[2].ends_with("~$1.20 │"),
        "{workflows:#?}"
    );
    assert!(!workflows[2].contains("tasks"), "{workflows:#?}");
    assert!(!workflows[2].contains('━'), "{workflows:#?}");
}

#[test]
fn the_workflows_card_lists_the_newest_and_counts_the_rest() {
    let mut usage = sample::usage();
    let first = usage.runs[0].clone();
    usage.runs = (0..11).map(|_| first.clone()).collect();
    let workflows = rows(&usage, UsageCard::Workflows, Form::Full, 110);
    assert_eq!(workflows.len(), 2 + 8 + 1 + 1, "{workflows:#?}");
    assert!(
        workflows[10].contains("and 3 more · /workflows lists them"),
        "{workflows:#?}"
    );
}

#[test]
fn every_card_keeps_inside_its_width_in_every_form() {
    let theme = plain();
    let kit = Kit::new(&theme);
    let (usage, names) = (sample::usage(), sample::names());
    let cards = UsageCards {
        usage: &usage,
        names: &names,
        now: sample::now(),
        left: false,
    };
    for width in stability::WIDTHS.map(usize::from) {
        for card in UsageCard::ALL {
            for form in [Form::Full, Form::Compact, Form::Line] {
                let drawn = cards.draw(&kit, card, form, Frame::band(width));
                for line in &drawn.lines {
                    assert!(
                        spans_width(&line.spans) <= width,
                        "{card:?} {form:?} at {width}: {line}"
                    );
                }
            }
        }
    }
}

#[test]
fn in_full_the_cli_in_use_keeps_its_mark_before_any_reading() {
    let usage = UsageSummary {
        plans: vec![
            PlanUsage {
                agent: "claude-code".to_owned(),
                name: "Claude Code".to_owned(),
                in_use: true,
                ..PlanUsage::default()
            },
            PlanUsage {
                agent: "codex".to_owned(),
                name: "Codex".to_owned(),
                ..PlanUsage::default()
            },
        ],
        ..UsageSummary::default()
    };
    let rows = rows(&usage, UsageCard::PlanUsage, Form::Full, 110);
    assert!(
        rows.iter()
            .any(|row| row.contains("● Claude Code · in use · no reading yet")),
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.contains("│ Codex  no reading yet")),
        "{rows:#?}"
    );
}

#[test]
fn before_any_reading_the_card_says_so() {
    let usage = UsageSummary::default();
    let rows = rows(&usage, UsageCard::PlanUsage, Form::Compact, 80);
    assert!(rows[1].ends_with("no reading yet │"), "{rows:#?}");
    assert!(
        rows[2].contains("No CLI in the crew reports a plan yet"),
        "{rows:#?}"
    );
}
