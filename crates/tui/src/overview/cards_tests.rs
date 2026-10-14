use argonauts_core::api::{PlanUsage, UsageSummary};

use super::*;
use crate::kit::Level;
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
fn plan_usage_has_a_row_for_each_window_grouped_by_cli() {
    let rows = rows(&sample::usage(), UsageCard::PlanUsage, Form::Full, 110);
    assert_eq!(rows.len(), 13, "{rows:#?}");
    assert!(rows[1].starts_with(" │ ◆ plan usage"), "{rows:#?}");
    assert!(
        rows[1].ends_with("no limit set · highest Codex 5-hour [92%] │"),
        "{rows:#?}"
    );
    let claude = &rows[3];
    assert!(
        claude.starts_with(" │ Claude Code      Max    5-hour   ━"),
        "{claude}"
    );
    assert!(
        claude.contains(" 71%   resets in 3h 40m · today 4:40pm"),
        "{claude}"
    );
    let week = &rows[4];
    assert!(
        week.starts_with(" │                         week     ━"),
        "{week}"
    );
    assert!(
        week.contains(" 38%   resets in 1d 9h · Tue Oct 6, 10:00pm"),
        "{week}"
    );
    let codex = &rows[6];
    assert!(
        codex.starts_with(" │ Codex            Plus   5-hour"),
        "{codex}"
    );
    assert!(codex.contains(" 92%   resets in 1h 15m"), "{codex}");
    assert!(rows[7].contains(" 41%   resets in 3d 20h"), "{rows:#?}");
    assert!(
        rows[9].contains(
            "│ Antigravity CLI  Pro    out until Tue 3:45pm · its tasks moved to GPT 5.6 on Codex"
        ),
        "{rows:#?}"
    );
    assert!(
        rows[11].contains("│ OpenRouter              pays per token, so there is no plan to track"),
        "{rows:#?}"
    );
    // Every row's bar, share and reset words start in the same column.
    let at = |row: &str, what: &str| row.find(what).map(|byte| row[..byte].chars().count());
    for row in [&rows[4], &rows[6], &rows[7]] {
        assert_eq!(at(claude, "━"), at(row, "━"), "{rows:#?}");
        assert_eq!(at(claude, "%"), at(row, "%"), "{rows:#?}");
        assert_eq!(at(claude, "resets"), at(row, "resets"), "{rows:#?}");
    }
}

#[test]
fn in_color_a_windows_bar_is_thin_and_only_its_share_warns() {
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
    let style_in = |row: usize, what: &str| {
        let spans = &drawn.lines[row].spans;
        let text: String = spans.iter().map(|span| span.content.as_ref()).collect();
        let span = spans.iter().find(|span| span.content.contains(what));
        span.unwrap_or_else(|| panic!("{what}: {text}")).style
    };
    // Claude Code's bar in its own colour, the bottom line's.
    assert_eq!(style_in(2, "━").fg, theme.color(Role::CliClaude));
    // At 92% the share is amber. When the window resets stays as quiet as on any row.
    assert_eq!(style_in(4, "92%"), kit.bold(Level::Warn.strong_role()));
    assert_eq!(style_in(4, "resets in 1h 15m"), kit.dim("").style);
    assert_eq!(style_in(5, "resets in 3d 20h"), kit.dim("").style);
}

#[test]
fn compact_the_card_draws_the_same_rows_without_blank_rows_between_clis() {
    let full = rows(&sample::usage(), UsageCard::PlanUsage, Form::Full, 110);
    let compact = rows(&sample::usage(), UsageCard::PlanUsage, Form::Compact, 110);
    let blank = |row: &String| row.trim_matches(|c| c == ' ' || c == '│').is_empty();
    let full: Vec<&String> = full.iter().filter(|row| !blank(row)).collect();
    assert_eq!(full, compact.iter().collect::<Vec<_>>());
}

#[test]
fn a_narrow_card_gives_up_the_reset_words_then_the_plan_and_keeps_every_window() {
    let rows = rows(&sample::usage(), UsageCard::PlanUsage, Form::Compact, 60);
    let at = |text: &str| rows.iter().position(|row| row.contains(text));
    let codex = at("│ Codex").expect("Codex");
    assert!(
        rows[codex].contains("5-hour") && rows[codex].contains("92%"),
        "{rows:#?}"
    );
    assert!(
        rows[codex + 1].contains("week") && rows[codex + 1].contains("41%"),
        "{rows:#?}"
    );
    assert!(at("resets").is_none(), "{rows:#?}");
    let narrowest = self::rows(&sample::usage(), UsageCard::PlanUsage, Form::Compact, 45);
    assert!(
        narrowest
            .iter()
            .any(|row| row.contains("│ Claude      5-hour")),
        "then the plan goes and the names shorten: {narrowest:#?}"
    );
    assert!(!narrowest.iter().any(|row| row.contains("Plus")));
    let wider = self::rows(&sample::usage(), UsageCard::PlanUsage, Form::Compact, 90);
    assert!(
        wider
            .iter()
            .any(|row| row.contains("92%   resets in 1h 15m ") && !row.contains("today")),
        "the reset words shorten before they go: {wider:#?}"
    );
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
    assert!(spend[1].starts_with(" │ ◆ spend  today "), "{spend:#?}");
    assert!(spend[1].ends_with("~$1.86 · no limit set │"), "{spend:#?}");
    let models = spend[5..].join("\n");
    for part in [
        "■ Opus 5.5            Claude Code   ~$1.12   60%",
        "■ GPT 5.6             Codex         $0.62    33%",
        "■ Gemini 3.6 Flash    agy           ~$0.12   6%",
    ] {
        assert!(models.contains(part), "{part}: {models}");
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
        rows[1].contains("your limit 80% · highest Codex 5-hour"),
        "{rows:#?}"
    );
    // Claude Code is under the limit, so it says nothing more.
    assert!(!rows[2..4].iter().any(|row| row.contains("limit reached")));
    assert!(
        rows[4].contains(" 92%") && rows[5].contains(" 41%"),
        "{rows:#?}"
    );
    assert!(
        rows[6].contains("limit reached · Sonnet 5.5 on Claude Code takes its tasks"),
        "{rows:#?}"
    );

    // With no fallback, the limit still shows as reached.
    usage.plans[1].moved_to = None;
    let rows = self::rows(&usage, UsageCard::PlanUsage, Form::Compact, 160);
    assert!(
        rows[6]
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
fn a_cli_with_no_reading_says_so_in_its_row() {
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
    let rows = rows(&usage, UsageCard::PlanUsage, Form::Compact, 110);
    assert!(
        rows.iter()
            .any(|row| row.contains("│ Claude Code  no reading yet")),
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.contains("│ Codex        no reading yet")),
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
