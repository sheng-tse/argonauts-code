use std::sync::Arc;
use std::time::Instant;

use argonauts_core::api::{UsagePeriod, UsageSummary};
use argonauts_core::app::{UiCommand, ViewRequest};
use argonauts_core::crew_stats::CrewStats;
use argonauts_core::crew_stats::StatsRole;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::settings::mods::ModOrigin;
use argonauts_core::settings::mods::view::ModChange;
use argonauts_core::settings::stats::{StatsColumn, StatsGroup, StatsPeriod, StatsSources};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};

use super::Level;
use super::cli::{millions, millions_short, took};
use super::pinned::StatsCard;
use crate::kit::{Form, Frame, Kit};
use crate::overview::{OverviewView, Tab, UsageWatch, sample};
use crate::stability;
use crate::state::AppState;
use crate::text::str_width;
use crate::theme::{ColorLevel, Glyphs, Theme};
use crate::views::{View, ViewCtx, ViewOutcome};

fn plain() -> Theme {
    let mut theme = Theme::dark().with_level(ColorLevel::None);
    theme.glyphs = Glyphs::UNICODE;
    theme
}

/// The CLIs go by their own names here, as the cards head them.
fn state(usage: UsageSummary) -> AppState {
    let mut state = AppState::default();
    state.usage = Some(Arc::new(usage));
    state
}

fn stats_tab() -> OverviewView {
    OverviewView::new(Tab::Stats, UsageWatch::default())
        .at(sample::now())
        .with_period(UsagePeriod::Week)
}

/// The panel's rows on a screen `width` wide.
fn rows(view: &mut OverviewView, state: &AppState, width: u16) -> Vec<String> {
    let theme = plain();
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    let screen = Size::new(width, 80);
    let height = view
        .dock_height(screen, &ctx)
        .unwrap_or_else(|| crate::views::dock::height(80));
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    stability::assert_frames(&buf);
    crate::views::dock::lines(&buf)
        .iter()
        .map(|line| line.to_string().trim_end().to_owned())
        .collect()
}

fn key(view: &mut OverviewView, state: &AppState, code: KeyCode) -> ViewOutcome {
    let theme = plain();
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    view.handle_key(KeyEvent::from(code), &ctx)
}

/// The first row holding `text`.
fn at(rows: &[String], text: &str) -> usize {
    rows.iter()
        .position(|row| row.contains(text))
        .unwrap_or_else(|| panic!("{text}: {rows:#?}"))
}

fn has(rows: &[String], text: &str) -> bool {
    rows.iter().any(|row| row.contains(text))
}

/// Along the sub-tabs to By role, as Tab or the arrows get there, and into its cards.
fn by_role(view: &mut OverviewView, state: &AppState) {
    while view.level() != Level::SubTabs {
        let towards = if view.level() == Level::Tabs {
            KeyCode::Down
        } else {
            KeyCode::Up
        };
        key(view, state, towards);
    }
    key(view, state, KeyCode::Left);
    into_the_cards(view, state);
}

/// ↓ from wherever the keys are, through the sub-tabs, into the cards. A letter that regroups
/// has put them there already.
fn into_the_cards(view: &mut OverviewView, state: &AppState) {
    while view.level() != Level::Cards {
        key(view, state, KeyCode::Down);
    }
}

#[test]
fn by_cli_puts_every_cli_together_then_each_by_itself_then_who_did_what() {
    let state = state(sample::usage_with_stats());
    let mut tab = stats_tab();
    let rows = rows(&mut tab, &state, 120);
    assert!(rows[1].contains("[Stats]"), "{rows:#?}");
    assert!(rows[1].ends_with("Today  Week  Month  All"), "{rows:#?}");
    assert!(
        rows[2].starts_with(" What the crew did: every CLI together, then each one by itself"),
        "{rows:#?}"
    );
    assert!(
        rows[2].ends_with("By CLI    By model    By role"),
        "{rows:#?}"
    );

    // All together: the totals, tokens by CLI on one scale, what they were, how tasks fared.
    let together = at(&rows, "◆ all together");
    assert!(
        rows[together].contains("14 workflows · 63 tasks merged · 5.0M tokens · ~$11.20 · 12h 40m"),
        "{rows:#?}"
    );
    for text in [
        "tokens by CLI · 5.0M",
        "Claude Code 2.9M 58%",
        "Codex 1.55M 31%",
        "Antigravity CLI 0.55M 11%",
        "from cache 3.4M 68%",
        "new input 1.4M 28%",
        "output 0.2M 4%",
        "how the 63 tasks fared in review",
        "passed first time 58",
        "sent back once 4",
        "sent back twice 1",
    ] {
        assert!(has(&rows, text), "{text}: {rows:#?}");
    }
    // The days of this week, from Sunday.
    assert!(
        rows.iter().any(|row| row.contains("S  M  T  W  T  F  S")),
        "{rows:#?}"
    );

    // A card per CLI, side by side, the main one first and filled, each model a bar.
    let cards = at(&rows, "● Claude Code");
    assert!(rows[cards].contains("○ Codex"), "{rows:#?}");
    assert!(rows[cards].contains("○ Antigravity CLI"), "{rows:#?}");
    assert!(rows[cards].contains("2.9M tok"), "{rows:#?}");
    for (text, beside) in [
        ("35 tasks", "Max · ~$7.90"),
        ("28 tasks · 14 reviews", "Plus · $2.75"),
        // A judge's sign-offs count as its CLI's reviews.
        ("63 reviews", "Pro · ~$0.55"),
        ("Opus 5.5", "2.1M · 24"),
        ("GPT 5.6 Luna", "0.35M · 7"),
        ("Gemini 3.6 Flash", "0.12M · 14"),
        ("94% first pass", "89% first pass"),
        ("4m 11s a task", "3m 03s a task"),
    ] {
        let row = at(&rows, text);
        assert!(
            rows[row].contains(beside),
            "{text} beside {beside}: {rows:#?}"
        );
    }
    // The same two rows on every card: its tasks and its reviews, none when it did none.
    let tasks = at(&rows, "│ tasks    ");
    assert!(rows[tasks].contains("94% first pass"), "{rows:#?}");
    assert!(rows[tasks].contains("tasks    none"), "{rows:#?}");
    let reviews = at(&rows, "│ reviews  ");
    assert!(rows[reviews].contains("reviews  none"), "{rows:#?}");
    assert!(rows[reviews].contains("86% accepted"), "{rows:#?}");
    assert!(rows[reviews].contains("92% accepted"), "{rows:#?}");
    assert!(has(&rows, "40s a review"), "{rows:#?}");
    // Each CLI's plan windows, and one that's spent says so.
    let plans = at(&rows, "5h ");
    assert!(rows[plans].contains("71%"), "{rows:#?}");
    assert!(rows[plans].contains("week "), "{rows:#?}");
    assert!(rows[plans].contains(" out "), "{rows:#?}");
    assert!(!rows[plans].contains('…'), "the windows fit: {rows:#?}");

    // Who did what: each role, every model in it, the CLI that did the most first.
    let who = at(&rows, "◆ who did what");
    assert!(cards < who && together < cards, "{rows:#?}");
    for text in [
        "Opus 5.5 14 workflows",
        "Opus 5.5 24   ■ Sonnet 5.5 11   ■ GPT 5.6 21   ■ GPT 5.6 Luna 7   of 63 tasks",
        "Gemini 3.6 Pro 49   ■ GPT 5.6 14   of 63 reviews",
        "Gemini 3.6 Flash 14 sign-offs",
    ] {
        assert!(has(&rows, text), "{text}: {rows:#?}");
    }
    let keys = rows.iter().rev().find(|row| !row.is_empty()).unwrap();
    assert_eq!(
        keys.trim(),
        "r to cycle dates · tab · ← → tabs · ↓ sub-tabs, then cards · u tokens, tasks or spend · enter open · p pin · esc closes"
    );
}

#[test]
fn m_shows_every_model_as_a_card_and_enter_opens_one_a_level_deeper() {
    let state = state(sample::usage_with_stats());
    let mut tab = stats_tab();
    assert!(matches!(
        key(&mut tab, &state, KeyCode::Char('m')),
        ViewOutcome::Consumed
    ));
    let rows = rows(&mut tab, &state, 120);
    assert!(
        rows[2].starts_with(" Every model the crew used: all together, then each one by itself"),
        "{rows:#?}"
    );
    // A letter that regroups puts the keys in the cards, where Tab moves the sub-tabs, so the
    // chosen one stays marked.
    assert!(
        rows[2].ends_with("By CLI   [By model]   By role"),
        "{rows:#?}"
    );
    // Built as By CLI is: all together, a card each, then the same job side by side.
    let parts = [
        at(&rows, "◆ all models  this week"),
        at(&rows, "│ ■ Opus 5.5"),
        at(&rows, "◆ the same job, side by side"),
    ];
    assert!(parts.is_sorted(), "{parts:?}: {rows:#?}");
    for bar in ["tokens by model", "spend by model", "agent time by model"] {
        assert!(has(&rows, bar), "{bar}: {rows:#?}");
    }
    // A card for each model, each CLI's models together, three to a row.
    let cards = at(&rows, "│ ■ Opus 5.5");
    let order = ["■ Opus 5.5", "■ Sonnet 5.5", "■ GPT 5.6 "].map(|name| {
        rows[cards]
            .find(name)
            .unwrap_or_else(|| panic!("{name}: {rows:#?}"))
    });
    assert!(order.is_sorted(), "{order:?}: {rows:#?}");
    for text in [
        "■ orchestrator",
        "■ workers",
        "96% first pass",
        "this week · 4m 40s a task",
        "cache hit",
    ] {
        assert!(has(&rows, text), "{text}: {rows:#?}");
    }
    // Side by side: the workers' four models, and the reviewers' two.
    let same = at(&rows, "◆ the same job, side by side");
    for text in [
        "first pass",
        "time each",
        "cost each",
        "tokens each",
        "accepted",
    ] {
        assert!(
            rows[same..].iter().any(|row| row.contains(text)),
            "{text}: {rows:#?}"
        );
    }

    // Nothing stands opened until Enter opens the card the keys are on, and Enter again
    // closes it.
    assert!(!has(&rows, "Opus 5.5 on Claude Code"), "{rows:#?}");
    key(&mut tab, &state, KeyCode::Enter);
    let rows = self::rows(&mut tab, &state, 120);
    let opened = at(&rows, "■ Opus 5.5 on Claude Code · as orchestrator, worker");
    assert!(
        at(&rows, "│ ■ GPT 5.6 Luna") < opened,
        "under the cards: {rows:#?}"
    );
    key(&mut tab, &state, KeyCode::Enter);
    assert!(!has(
        &self::rows(&mut tab, &state, 120),
        "Opus 5.5 on Claude Code"
    ));

    // ↓ moves to the next card, which Enter opens in the first one's place.
    key(&mut tab, &state, KeyCode::Down);
    key(&mut tab, &state, KeyCode::Down);
    key(&mut tab, &state, KeyCode::Enter);
    let rows = self::rows(&mut tab, &state, 120);
    assert!(!has(&rows, "Opus 5.5 on Claude Code"), "{rows:#?}");
    let opened = at(&rows, "■ GPT 5.6 on Codex · as worker, reviewer");
    assert!(rows[opened].ends_with("1.2M tokens · $2.30 │"), "{rows:#?}");
    // At the left its work, at the right what it used and how it stands beside the others.
    for text in [
        "│ work ",
        "90% first pass · 2 sent back once",
        "86% accepted · 2 sent back",
        "a task · 52s a review",
        "│ cost ",
        "tokens a task",
        "■ cache 0.8M",
        "cache hit",
        "a day, this week",
        "beside the other models",
        "as a reviewer: 52s each vs",
        "fallbacks 2 tasks fell back from Opus 5.5",
        "when Claude's 5-hour window ran out",
    ] {
        assert!(has(&rows[opened..], text), "{text}: {rows:#?}");
    }
    assert!(
        has(
            &rows,
            "r to cycle dates · ↓ more · ↑↓ model · enter opens or closes it · tab By CLI, By model, By role"
        ),
        "{rows:#?}"
    );

    // Esc closes the panel at once, with a card open too.
    assert!(matches!(
        key(&mut tab, &state, KeyCode::Esc),
        ViewOutcome::Close
    ));
}

#[test]
fn enter_on_a_clis_card_opens_it_and_enter_again_closes_it() {
    let state = state(sample::usage_with_stats());
    let mut tab = stats_tab();
    into_the_cards(&mut tab, &state);
    key(&mut tab, &state, KeyCode::Down);
    key(&mut tab, &state, KeyCode::Enter);
    let rows = rows(&mut tab, &state, 120);
    let opened = at(&rows, "● Codex · opened with enter");
    assert!(
        rows[opened].contains("28 tasks · 14 reviews · 1.6M tokens · $2.75"),
        "{rows:#?}"
    );
    assert!(opened < at(&rows, "◆ who did what"), "{rows:#?}");
    key(&mut tab, &state, KeyCode::Enter);
    assert!(!has(
        &self::rows(&mut tab, &state, 120),
        "opened with enter"
    ));
}

#[test]
fn by_role_shows_every_role_as_a_card_with_the_models_that_filled_it() {
    let state = state(sample::usage_with_stats());
    let mut tab = stats_tab();
    by_role(&mut tab, &state);
    let rows = rows(&mut tab, &state, 120);
    assert!(
        rows[2].starts_with(" Every role in the crew: all together, then each one by itself"),
        "{rows:#?}"
    );
    let parts = [
        at(&rows, "◆ all roles  this week"),
        at(&rows, "│ ■ orchestrator"),
        at(&rows, "◆ what each role caught or changed"),
    ];
    assert!(parts.is_sorted(), "{parts:?}: {rows:#?}");
    for bar in ["tokens by role", "spend by role", "agent time by role"] {
        assert!(has(&rows, bar), "{bar}: {rows:#?}");
    }
    // A card for each role, in the order the work goes round the crew.
    let cards = at(&rows, "│ ■ orchestrator");
    let order = ["■ orchestrator", "■ workers", "■ reviewers"].map(|name| {
        rows[cards]
            .find(name)
            .unwrap_or_else(|| panic!("{name}: {rows:#?}"))
    });
    assert!(order.is_sorted(), "{order:?}: {rows:#?}");
    assert!(has(&rows, "│ ■ judge"), "{rows:#?}");
    // Under a role's name what it did and who filled it, then a bar for each model.
    for text in [
        "63 tasks · 4 models",
        "63 reviews · 2 models",
        "■ GPT 5.6 Luna",
        "92% first pass",
        "best first pass: Opus 5.5, 96%",
    ] {
        assert!(has(&rows, text), "{text}: {rows:#?}");
    }
    // What the roles beside the workers settled, a bar and a legend each.
    let caught = at(&rows, "◆ what each role caught or changed");
    assert!(
        rows[caught..]
            .iter()
            .any(|row| row.contains("■ ran as planned 13   ■ replanned 1   of 14 runs")),
        "{rows:#?}"
    );
    // Enter opens the role the keys are on, under the cards.
    key(&mut tab, &state, KeyCode::Down);
    key(&mut tab, &state, KeyCode::Enter);
    let rows = self::rows(&mut tab, &state, 120);
    let opened = at(&rows, "workers · opened with enter");
    assert!(at(&rows, "│ ■ judge") < opened, "{rows:#?}");
}

#[test]
fn runs_filed_before_their_endings_were_kept_say_nothing_of_how_they_ended() {
    let mut usage = sample::usage_with_stats();
    let stats = usage.stats.as_mut().expect("the sample has stats");
    stats.outcomes = Default::default();
    let state = state(usage);
    let mut tab = stats_tab();
    by_role(&mut tab, &state);
    let rows = rows(&mut tab, &state, 120);
    assert!(has(&rows, "│ runs     none"), "{rows:#?}");
    for unknown in ["0% merged", "ran as planned", "never replanned"] {
        assert!(!has(&rows, unknown), "{unknown}: {rows:#?}");
    }
    // The reviewers' send-backs came with no reasons, which the row says for all six.
    assert!(
        has(&rows, "■ no reason kept 6   of 6 sent back"),
        "{rows:#?}"
    );
}

#[test]
fn u_turns_the_bars_to_tasks_then_spend_then_tokens() {
    let state = state(sample::usage_with_stats());
    let mut tab = stats_tab();
    key(&mut tab, &state, KeyCode::Char('u'));
    let rows = rows(&mut tab, &state, 120);
    assert!(has(&rows, "tasks by CLI"), "{rows:#?}");
    assert!(
        has(&rows, "24 · 2.1M"),
        "a model's tasks, then its tokens: {rows:#?}"
    );
    key(&mut tab, &state, KeyCode::Char('u'));
    let rows = self::rows(&mut tab, &state, 120);
    assert!(has(&rows, "spend by CLI"), "{rows:#?}");
    assert!(has(&rows, "$6.40 · 24"), "{rows:#?}");
    key(&mut tab, &state, KeyCode::Char('u'));
    let rows = self::rows(&mut tab, &state, 120);
    assert!(has(&rows, "tokens by CLI"), "{rows:#?}");
    assert!(has(&rows, "2.1M · 24"), "tokens again: {rows:#?}");
}

#[test]
fn the_period_keys_ask_for_its_numbers_and_p_pins_the_card() {
    let state = state(sample::usage_with_stats());
    let mut tab = stats_tab();
    let today = key(&mut tab, &state, KeyCode::Char('1'));
    assert!(
        matches!(
            today,
            ViewOutcome::Request(ViewRequest::Usage {
                period: UsagePeriod::Today
            })
        ),
        "{today:?}"
    );
    let month = key(&mut tab, &state, KeyCode::Char('3'));
    assert!(
        matches!(
            month,
            ViewOutcome::Request(ViewRequest::Usage {
                period: UsagePeriod::Month
            })
        ),
        "{month:?}"
    );
    let pinned = key(&mut tab, &state, KeyCode::Char('p'));
    assert!(
        matches!(pinned, ViewOutcome::Send(UiCommand::Slash { .. })),
        "{pinned:?}"
    );
}

#[test]
fn the_group_and_measure_of_the_stats_file_are_where_the_tab_opens() {
    let mut usage = sample::usage_with_stats();
    let stats = usage.stats.as_mut().unwrap();
    stats.settings.group = StatsGroup::Model;
    let state = state(usage);
    let mut tab = stats_tab();
    let rows = rows(&mut tab, &state, 120);
    assert!(rows[2].contains("Every model the crew used"), "{rows:#?}");
    // A key changes the view, and the file doesn't take it back.
    key(&mut tab, &state, KeyCode::Char('c'));
    let rows = self::rows(&mut tab, &state, 120);
    assert!(has(&rows, "◆ all together"), "{rows:#?}");
}

#[test]
fn narrow_the_cards_stack_and_nothing_runs_past_the_edge() {
    let state = state(sample::usage_with_stats());
    for width in [100, 80, 60] {
        let mut tab = stats_tab();
        for group in ['c', 'm', 'r'] {
            key(&mut tab, &state, KeyCode::Char(group));
            let rows = rows(&mut tab, &state, width);
            for row in &rows {
                assert!(str_width(row) <= usize::from(width), "{width}: {row}");
            }
        }
    }
    let mut tab = stats_tab();
    let rows = rows(&mut tab, &state, 80);
    let claude = at(&rows, "● Claude Code");
    assert!(!rows[claude].contains("● Antigravity CLI"), "{rows:#?}");
}

#[test]
fn before_any_work_the_tab_says_so() {
    let mut tab = stats_tab();
    let reading = state(sample::usage());
    assert!(has(
        &rows(&mut tab, &reading, 120),
        "Reading the crew's stats…"
    ));
    let empty = state(UsageSummary {
        stats: Some(CrewStats::default()),
        ..sample::usage_with_stats()
    });
    assert!(has(
        &rows(&mut tab, &empty, 120),
        "Nothing recorded in this period yet. Stats fill in as the crew works."
    ));
}

/// A CLI whose last plan read failed says so, in the Usage tab's words, rather than that it was
/// never read.
#[test]
fn a_failed_plan_read_says_it_failed() {
    let mut usage = sample::usage_with_stats();
    for plan in &mut usage.plans {
        plan.windows.clear();
        plan.live = None;
        plan.failed = (plan.agent == "codex").then(|| "Codex didn't answer within 15s".to_owned());
    }
    // Narrow, so the cards stack and each line sits under its own CLI.
    let mut tab = stats_tab();
    let rows = rows(&mut tab, &state(usage), 60);
    let (claude, codex) = (at(&rows, "● Claude Code"), at(&rows, "○ Codex"));
    let failed = at(&rows, "Failed to load usage data");
    assert!(claude < codex && codex < failed, "{rows:#?}");
    assert!(
        rows[claude..codex]
            .iter()
            .any(|row| row.contains("no plan reading yet")),
        "{rows:#?}"
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.contains("Failed to load usage data"))
            .count(),
        1,
        "{rows:#?}"
    );
}

fn card_rows(usage: &UsageSummary, width: usize) -> Vec<String> {
    let names = DisplayNames::default();
    let card = StatsCard {
        usage,
        names: &names,
        now: sample::now(),
    };
    let theme = plain();
    let kit = Kit::new(&theme);
    card.draw(&kit, Form::Full, Frame::view(width))
        .lines
        .iter()
        .map(|line| line.to_string().trim_end().to_owned())
        .collect()
}

/// A task nobody reviewed and nobody merged counts in no first-pass share and no square: the
/// card, the tab and every CLI read the same as without it.
#[test]
fn an_unreviewed_unmerged_task_changes_no_count_of_how_tasks_fared() {
    let usage = sample::usage_with_stats();
    let mut more = usage.clone();
    let stats = more.stats.as_mut().expect("the sample has stats");
    for _ in 0..2 {
        stats.tasks.push(argonauts_core::crew_stats::TaskRecord {
            agent: "codex".to_owned(),
            model: Some("gpt-5.6-luna".to_owned()),
            kind: Some("docs".to_owned()),
            sent_back: 0,
            reasons: Vec::new(),
            merged: false,
            reviewed: false,
        });
    }
    assert_eq!(card_rows(&usage, 120), card_rows(&more, 120));
    let mut tab = stats_tab();
    let before = rows(&mut tab, &state(usage), 120);
    let mut tab = stats_tab();
    let after = rows(&mut tab, &state(more), 120);
    let fared = |rows: &[String]| -> Vec<String> {
        rows.iter()
            .filter(|row| row.contains("first pass") || row.contains("fared in review"))
            .cloned()
            .collect()
    };
    assert_eq!(fared(&before), fared(&after));
}

#[test]
fn a_task_sent_back_and_never_merged_still_counts_in_how_tasks_fared() {
    let mut usage = sample::usage_with_stats();
    let stats = usage.stats.as_mut().expect("the sample has stats");
    stats.tasks.push(argonauts_core::crew_stats::TaskRecord {
        agent: "codex".to_owned(),
        model: Some("gpt-5.6-luna".to_owned()),
        kind: Some("build".to_owned()),
        sent_back: 1,
        reasons: Vec::new(),
        merged: false,
        reviewed: true,
    });
    let mut tab = stats_tab();
    let rows = rows(&mut tab, &state(usage), 120);
    assert!(has(&rows, "how the 64 tasks fared in review"), "{rows:#?}");
    assert!(has(&rows, "sent back once 5"), "{rows:#?}");
    assert!(has(&rows, "63 tasks merged"), "{rows:#?}");
}

#[test]
fn the_main_agents_conversations_are_a_roles_work_like_any_other() {
    let mut usage = sample::usage_with_stats();
    let stats = usage.stats.as_mut().expect("the sample has stats");
    stats.work.push(argonauts_core::crew_stats::WorkRow {
        agent: "claude-code".to_owned(),
        model: Some("claude-haiku-4-5".to_owned()),
        role: StatsRole::Main,
        kind: None,
        spent: argonauts_core::Spend {
            input_tokens: 900_000,
            ..Default::default()
        },
        done: 3,
        sent_back: 0,
        took_secs: 0,
        sessions: 0,
        tool_calls: 0,
        turns: 12,
        review: Default::default(),
    });
    let state = state(usage);
    let mut tab = stats_tab();
    key(&mut tab, &state, KeyCode::Char('m'));
    let rows = rows(&mut tab, &state, 120);
    let haiku = at(&rows, "■ Haiku 4.5");
    assert!(
        rows[haiku..].iter().any(|row| row.contains("■ main")),
        "{rows:#?}"
    );
    let mut tab = stats_tab();
    by_role(&mut tab, &state);
    let rows = self::rows(&mut tab, &state, 120);
    for text in [
        "│ ■ main",
        "3 conversations · 1 model",
        "turns    12 in 3 · 4 each",
    ] {
        assert!(has(&rows, text), "{text}: {rows:#?}");
    }
    // A conversation is no task, so the bars that count tasks leave it out.
    let tasks = |usage: UsageSummary| {
        let state = self::state(usage);
        let mut tab = stats_tab();
        key(&mut tab, &state, KeyCode::Char('u'));
        let rows = self::rows(&mut tab, &state, 120);
        rows[at(&rows, "tasks by CLI")].clone()
    };
    let with_main = tasks(state.usage.as_deref().cloned().expect("the numbers"));
    assert_eq!(with_main, tasks(sample::usage_with_stats()));
}

#[test]
fn the_chart_beside_all_together_follows_the_period() {
    let chart = |period: UsagePeriod| -> Vec<String> {
        let mut usage = sample::usage_with_stats();
        usage.period = period;
        let mut tab = stats_tab().with_period(period);
        rows(&mut tab, &state(usage), 120)
    };
    // A month's days a cell each, under them the dates its weeks start on.
    let rows = chart(UsagePeriod::Month);
    assert!(has(&rows, "1      8      15     22     29"), "{rows:#?}");
    assert!(has(&rows, "◆ all together  this month"), "{rows:#?}");
    // Today by itself, under its letter.
    let rows = chart(UsagePeriod::Today);
    assert!(has(&rows, "◆ all together  today"), "{rows:#?}");
    assert!(
        rows.iter()
            .any(|row| row.ends_with("│ M ") || row.contains("│ M                ")),
        "{rows:#?}"
    );
    // This week from Sunday, the days after today empty.
    let rows = chart(UsagePeriod::Week);
    assert!(has(&rows, "this week, Sunday to Saturday"), "{rows:#?}");
    assert!(has(&rows, "S  M  T  W  T  F  S"), "{rows:#?}");
}

#[test]
fn stats_opens_on_every_day_so_far_whichever_period_the_tab_before_showed() {
    let mut usage = sample::usage_with_stats();
    usage.period = UsagePeriod::All;
    let state = state(usage);
    let mut tab = OverviewView::new(Tab::Limits, UsageWatch::default())
        .at(sample::now())
        .with_period(UsagePeriod::Today);
    key(&mut tab, &state, KeyCode::Right);
    assert_eq!(tab.tab(), Tab::Stats);
    let rows = rows(&mut tab, &state, 120);
    assert!(
        has(
            &rows,
            "◆ all together  the crew's work through Argonauts Code"
        ),
        "{rows:#?}"
    );
}

#[test]
fn one_to_four_pick_the_period_from_any_row() {
    let state = state(sample::usage_with_stats());
    let mut tab = stats_tab();
    into_the_cards(&mut tab, &state);
    for (code, period) in [
        ('1', UsagePeriod::Today),
        ('2', UsagePeriod::Week),
        ('3', UsagePeriod::Month),
        ('4', UsagePeriod::All),
    ] {
        let asked = key(&mut tab, &state, KeyCode::Char(code));
        assert!(
            matches!(asked, ViewOutcome::Request(ViewRequest::Usage { period: p }) if p == period),
            "{code}: {asked:?}"
        );
    }
}

#[test]
fn a_week_on_a_clis_card_is_seven_bars_over_the_letters_of_their_days() {
    let state = state(sample::usage_with_stats());
    let mut tab = stats_tab();
    // Three cards to a row: the letters say it's a week, and the pace follows the bars.
    let rows = rows(&mut tab, &state, 120);
    let letters = at(&rows, "│ S M T W T F S ");
    let bars = &rows[letters - 1];
    assert!(bars.contains("▁ ▁"), "a cell between the bars: {rows:#?}");
    assert!(bars.contains(" a task"), "{rows:#?}");
    assert!(!bars.contains("this week"), "{rows:#?}");
    // A blank row between a card's parts, the plan's windows last.
    assert_eq!(rows[letters + 1].trim_matches(['│', ' ']), "", "{rows:#?}");
    assert!(rows[letters + 2].contains("│ 5h "), "{rows:#?}");
    // A card to a row has room to name the period too.
    let rows = self::rows(&mut tab, &state, 60);
    let letters = at(&rows, "│ S M T W T F S ");
    assert!(rows[letters - 1].contains("  this week · "), "{rows:#?}");
}

#[test]
fn the_pinned_card_is_one_row_per_cli_under_the_totals() {
    let usage = sample::usage_with_stats();
    let rows = card_rows(&usage, 120);
    assert!(rows[1].contains("◆ stats this week"), "{rows:#?}");
    assert!(
        rows[1].contains("63 tasks merged · 58 first pass · 5M tokens · ~$11.20 · 12h 40m"),
        "{rows:#?}"
    );
    let claude = at(&rows, "● Claude");
    let codex = at(&rows, "○ Codex");
    let agy = at(&rows, "○ agy");
    assert_eq!((claude + 1, codex + 1), (codex, agy), "{rows:#?}");
    assert!(rows[claude].contains("Opus 5.5") && rows[claude].contains("35 tasks"));
    assert!(rows[agy].contains("63 reviews  5 sent back"), "{rows:#?}");
    assert!(rows[agy].ends_with("out │"), "{rows:#?}");
    // The cells at the right line up in columns.
    let column = |row: &str, text: &str| row.find(text).map(|at| str_width(&row[..at]));
    let tok: Vec<_> = [claude, codex, agy]
        .iter()
        .map(|&row| column(&rows[row], " tok"))
        .collect();
    assert!(
        tok.windows(2).all(|pair| pair[0] == pair[1]),
        "{tok:?}: {rows:#?}"
    );

    // Only the columns the file names.
    let mut usage = sample::usage_with_stats();
    usage.stats.as_mut().unwrap().settings.columns = vec![StatsColumn::Work, StatsColumn::Pace];
    let rows = card_rows(&usage, 120);
    let claude = &rows[at(&rows, "● Claude")];
    assert!(claude.ends_with("4m 11s a task │"), "{rows:#?}");
    assert!(!claude.contains("tok"), "{rows:#?}");
}

#[test]
fn the_tab_writes_numbers_on_one_scale() {
    assert_eq!(millions(5_000_000), "5.0M");
    assert_eq!(millions(1_550_000), "1.55M");
    assert_eq!(millions(800_000), "0.8M");
    assert_eq!(millions(50_000), "0.05M");
    assert_eq!(millions(4_200), "<0.01M", "one unit beside the others");
    assert_eq!(millions(0), "0.0M");
    assert_eq!(millions(99_902_900_000), "99.9b");
    assert_eq!(millions(49_951_970_000), "50.0b");
    assert_eq!(millions_short(27_470_000), "27.5M");
    assert_eq!(millions_short(2_060_000), "2.1M");
    assert_eq!(millions_short(40_000), "<0.1M");
    assert_eq!(millions_short(1_200_000_000), "1.2b");
    assert_eq!(took(185), "3m 05s");
    assert_eq!(took(41), "41s");
    assert_eq!(took(12 * 3600 + 40 * 60), "12h 40m");
}

fn shift(view: &mut OverviewView, state: &AppState, code: KeyCode) -> ViewOutcome {
    let theme = plain();
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    view.handle_key(KeyEvent::new(code, KeyModifiers::SHIFT), &ctx)
}

/// The week's numbers, from a stats file that sets the week, so the page's preview draws
/// them as they are.
fn weekly_file() -> UsageSummary {
    let mut usage = sample::usage_with_stats();
    usage.stats.as_mut().unwrap().settings.period = StatsPeriod::Week;
    usage
}

/// The Stats tab with the stats mod's page open, on its Columns row.
fn on_columns(state: &AppState) -> OverviewView {
    let mut tab = stats_tab();
    key(&mut tab, state, KeyCode::Char(','));
    for _ in 0..4 {
        key(&mut tab, state, KeyCode::Down);
    }
    tab
}

#[test]
fn comma_opens_the_stats_mods_page_over_the_tab_with_its_card_as_the_preview() {
    let state = state(weekly_file());
    let mut tab = stats_tab();
    assert!(matches!(
        key(&mut tab, &state, KeyCode::Char(',')),
        ViewOutcome::Consumed
    ));
    let rows = rows(&mut tab, &state, 120);
    assert!(rows[2].contains("The stats mod"), "{rows:#?}");
    assert!(
        rows[2].ends_with("~/.argonauts/mods/stats.toml"),
        "{rows:#?}"
    );
    // The card above the input, drawn as the page stands, and no card of the tab.
    assert!(has(&rows, "◆ stats this week"), "{rows:#?}");
    assert!(!has(&rows, "◆ all together"), "{rows:#?}");
    let period = &rows[at(&rows, "❯ Period")];
    assert!(period.contains("today  week  month  all"), "{period}");
    assert!(period.ends_with("built-in"), "{period}");
    assert!(has(&rows, "What shows first"), "{rows:#?}");
    assert!(has(&rows, "The card above the input"), "{rows:#?}");
    let columns = &rows[at(&rows, "Columns")];
    assert!(
        columns.contains("☒ work 1st  ☒ review 2nd  ☒ tokens 3rd  ☒ plan 4th  ☐ spend"),
        "{columns}"
    );
    // Lines aren't offered: nothing records them per task yet.
    assert!(!columns.contains("lines"), "{columns}");
    let keys = rows.last().unwrap();
    assert!(keys.contains("shift+← → order"), "{keys}");
    assert!(keys.ends_with("s save to yours · esc back"), "{keys}");

    // Esc goes back to the tab, and another Esc closes the panel.
    key(&mut tab, &state, KeyCode::Esc);
    let rows = self::rows(&mut tab, &state, 120);
    assert!(has(&rows, "◆ all together"), "{rows:#?}");
    assert!(matches!(
        key(&mut tab, &state, KeyCode::Esc),
        ViewOutcome::Close
    ));

    // A file that sets another period has its preview ask for that period's numbers.
    let state = self::state(sample::usage_with_stats());
    let mut tab = stats_tab();
    let all = key(&mut tab, &state, KeyCode::Char(','));
    assert!(
        matches!(
            all,
            ViewOutcome::Request(ViewRequest::Usage {
                period: UsagePeriod::All
            })
        ),
        "{all:?}"
    );
    let rows = self::rows(&mut tab, &state, 120);
    assert!(
        has(&rows, "Reading the numbers for this period…"),
        "{rows:#?}"
    );
    assert!(has(&rows, "❯ Period"), "{rows:#?}");
}

#[test]
fn a_column_ticked_or_moved_shows_in_the_preview_and_s_saves_it_through_the_mod_files() {
    let state = state(weekly_file());
    let mut tab = on_columns(&state);
    let before = rows(&mut tab, &state, 120);
    assert!(
        before[at(&before, "● Claude")].contains("35 tasks"),
        "{before:#?}"
    );
    // Space on work unticks it, and the card loses its tasks.
    key(&mut tab, &state, KeyCode::Char(' '));
    let rows = self::rows(&mut tab, &state, 120);
    assert!(
        !rows[at(&rows, "● Claude")].contains("35 tasks"),
        "{rows:#?}"
    );
    let columns = &rows[at(&rows, "Columns")];
    assert!(columns.contains("☒ review 1st"), "{columns}");
    assert!(columns.contains("☐ work"), "{columns}");
    assert!(has(&rows, "Not saved yet. s saves it"), "{rows:#?}");
    // The keys stayed on work, now fourth; two to the left is tokens, which shift+← moves
    // before review.
    key(&mut tab, &state, KeyCode::Left);
    key(&mut tab, &state, KeyCode::Left);
    shift(&mut tab, &state, KeyCode::Left);
    let rows = self::rows(&mut tab, &state, 120);
    let columns = &rows[at(&rows, "Columns")];
    assert!(columns.contains("☒ tokens 1st  ☒ review 2nd"), "{columns}");
    // Put after tokens, review takes its place among the cells at the right. Without color
    // a passed task's square is `#`.
    let claude = &rows[at(&rows, "● Claude")];
    assert!(
        claude.find(" tok").unwrap() < claude.find('#').unwrap(),
        "{claude}"
    );
    // s saves through the mod files' own save, each key with its value as TOML text, and the
    // page shows it saved at once.
    let saved = key(&mut tab, &state, KeyCode::Char('s'));
    let ViewOutcome::Send(UiCommand::Mod {
        change: ModChange::Save {
            name,
            pairs,
            origin,
        },
    }) = saved
    else {
        panic!("{saved:?}");
    };
    assert_eq!((name.as_str(), origin), ("stats", ModOrigin::Yours));
    let columns = (
        "columns".to_owned(),
        "[\"tokens\", \"review\", \"plan\"]".to_owned(),
    );
    assert!(pairs.contains(&columns), "{pairs:?}");
    let rows = self::rows(&mut tab, &state, 120);
    assert!(!has(&rows, "Not saved yet"), "{rows:#?}");
    // r drops what isn't saved since.
    key(&mut tab, &state, KeyCode::Char(' '));
    key(&mut tab, &state, KeyCode::Char('r'));
    let rows = self::rows(&mut tab, &state, 120);
    assert!(has(&rows, "Back to what the file holds"), "{rows:#?}");
    assert!(!has(&rows, "Not saved yet"), "{rows:#?}");
}

#[test]
fn the_tab_behind_the_page_takes_its_period_grouping_and_measure() {
    let state = state(weekly_file());
    let mut tab = stats_tab();
    key(&mut tab, &state, KeyCode::Char(','));
    // The file says a week, so → asks for the month.
    let month = key(&mut tab, &state, KeyCode::Right);
    assert!(
        matches!(
            month,
            ViewOutcome::Request(ViewRequest::Usage {
                period: UsagePeriod::Month
            })
        ),
        "{month:?}"
    );
    key(&mut tab, &state, KeyCode::Down);
    key(&mut tab, &state, KeyCode::Right);
    let rows = rows(&mut tab, &state, 120);
    assert!(
        rows[at(&rows, "Grouped")].contains("by CLI  by model"),
        "{rows:#?}"
    );
    key(&mut tab, &state, KeyCode::Esc);
    let rows = self::rows(&mut tab, &state, 120);
    assert!(rows[2].contains("Every model the crew used"), "{rows:#?}");
}

#[test]
fn each_row_says_which_file_set_it_and_the_page_saves_where_the_file_in_use_is() {
    let mut usage = sample::usage_with_stats();
    usage.stats.as_mut().unwrap().sources = StatsSources {
        yours: vec!["measure".to_owned()],
        project: vec!["period".to_owned()],
    };
    let state = state(usage);
    let mut tab = stats_tab();
    key(&mut tab, &state, KeyCode::Char(','));
    let rows = rows(&mut tab, &state, 120);
    assert!(rows[at(&rows, "Period")].ends_with("project"), "{rows:#?}");
    assert!(
        rows[at(&rows, "Bars measure")].ends_with("yours"),
        "{rows:#?}"
    );
    assert!(
        rows[at(&rows, "Grouped")].ends_with("built-in"),
        "{rows:#?}"
    );
    assert!(
        rows[2].ends_with(" .argonauts/mods/stats.toml"),
        "{rows:#?}"
    );
    assert!(
        rows.last().unwrap().contains("s save to the project"),
        "{rows:#?}"
    );
    // With nothing changed there's nothing to save, and r asks before it deletes a file.
    key(&mut tab, &state, KeyCode::Char('s'));
    let rows = self::rows(&mut tab, &state, 120);
    assert!(has(&rows, "Nothing to save"), "{rows:#?}");
    key(&mut tab, &state, KeyCode::Char('r'));
    let rows = self::rows(&mut tab, &state, 120);
    assert!(
        has(&rows, "r again deletes .argonauts/mods/stats.toml"),
        "{rows:#?}"
    );
    // Any other key and what was said goes.
    key(&mut tab, &state, KeyCode::Down);
    let rows = self::rows(&mut tab, &state, 120);
    assert!(!has(&rows, "r again deletes"), "{rows:#?}");
}

#[test]
fn the_page_reads_the_file_again_when_nothing_on_it_is_unsaved() {
    let mut tab = stats_tab();
    let before = state(weekly_file());
    key(&mut tab, &before, KeyCode::Char(','));
    // The file now says the measure is spend, and yours says so.
    let mut usage = weekly_file();
    let stats = usage.stats.as_mut().unwrap();
    stats.settings.measure = argonauts_core::settings::stats::StatsMeasure::Spend;
    stats.sources.yours = vec!["measure".to_owned()];
    let after = state(usage);
    let rows = rows(&mut tab, &after, 120);
    let measure = &rows[at(&rows, "Bars measure")];
    assert!(measure.ends_with("yours"), "{measure}");
    assert!(!has(&rows, "Not saved yet"), "{rows:#?}");
    // Something unsaved stays as it is.
    key(&mut tab, &after, KeyCode::Down);
    key(&mut tab, &after, KeyCode::Right);
    let rows = self::rows(&mut tab, &before, 120);
    assert!(has(&rows, "Not saved yet"), "{rows:#?}");
}

#[test]
fn the_page_asked_for_before_the_numbers_opens_when_they_come() {
    let mut tab = OverviewView::new(Tab::Stats, UsageWatch::default())
        .at(sample::now())
        .with_period(UsagePeriod::Week)
        .with_stats_page();
    let empty = AppState::default();
    let rows = rows(&mut tab, &empty, 120);
    assert!(has(&rows, "Reading the stats mod's file…"), "{rows:#?}");
    let state = state(sample::usage_with_stats());
    let rows = self::rows(&mut tab, &state, 120);
    assert!(has(&rows, "❯ Period"), "{rows:#?}");

    // Esc while it waits gives up on it.
    let mut waiting = OverviewView::new(Tab::Stats, UsageWatch::default()).with_stats_page();
    assert!(matches!(
        key(&mut waiting, &empty, KeyCode::Esc),
        ViewOutcome::Consumed
    ));
    assert!(matches!(
        key(&mut waiting, &empty, KeyCode::Esc),
        ViewOutcome::Close
    ));
}

#[test]
fn narrow_the_columns_go_on_under_their_first_choice() {
    let state = state(sample::usage_with_stats());
    let mut tab = on_columns(&state);
    let rows = rows(&mut tab, &state, 70);
    for row in &rows {
        assert!(str_width(row) <= 70, "{row}");
    }
    let first = at(&rows, "Columns");
    assert!(rows[first].ends_with("built-in"), "{rows:#?}");
    assert!(rows[first + 1].contains("☐"), "{rows:#?}");
    let indent = |row: &str| row.len() - row.trim_start().len();
    let choices = &rows[first][..rows[first].find("☒").unwrap()];
    assert_eq!(indent(&rows[first + 1]), str_width(choices), "{rows:#?}");
}

#[test]
fn a_stats_file_that_doesnt_read_is_named_on_the_tab_and_on_its_page() {
    let mut usage = sample::usage_with_stats();
    usage.stats.as_mut().unwrap().problems =
        vec!["~/.argonauts/mods/stats.toml: unknown variant `fortnight`".to_owned()];
    let state = state(usage);
    let mut tab = stats_tab();
    let shown = rows(&mut tab, &state, 120);
    let named = at(
        &shown,
        "~/.argonauts/mods/stats.toml: unknown variant `fortnight`",
    );
    assert!(
        shown[named].ends_with("left out until it reads"),
        "{shown:#?}"
    );
    // Above the cards, so it's the first thing read.
    assert!(named < at(&shown, "all together"), "{shown:#?}");

    key(&mut tab, &state, KeyCode::Char(','));
    let page = rows(&mut tab, &state, 120);
    assert!(has(&page, "unknown variant `fortnight`"), "{page:#?}");
    assert!(has(&page, "❯ Period"), "{page:#?}");
}

#[test]
fn down_goes_from_the_tabs_to_the_sub_tabs_where_the_arrows_regroup_at_once() {
    let state = state(sample::usage_with_stats());
    let mut tab = stats_tab();
    // Opened, the keys are on the tabs: no row is picked yet.
    let rows = self::rows(&mut tab, &state, 120);
    assert!(!has(&rows, "❯"), "{rows:#?}");
    key(&mut tab, &state, KeyCode::Down);
    assert!(matches!(
        key(&mut tab, &state, KeyCode::Right),
        ViewOutcome::Consumed
    ));
    let rows = self::rows(&mut tab, &state, 120);
    assert_eq!(tab.tab(), Tab::Stats, "→ on the sub-tabs stays on Stats");
    assert!(
        rows[2].ends_with(" By CLI   [By model]   By role"),
        "{rows:#?}"
    );
    assert!(
        has(&rows, "◆ all models"),
        "the cards changed at once: {rows:#?}"
    );
    // Left of the first sub-tab is the last.
    key(&mut tab, &state, KeyCode::Left);
    key(&mut tab, &state, KeyCode::Left);
    let rows = self::rows(&mut tab, &state, 120);
    assert!(
        rows[2].ends_with(" By CLI    By model   [By role]"),
        "{rows:#?}"
    );
    // Into the cards, and ↑ on the first goes back to the sub-tabs, then the tabs.
    key(&mut tab, &state, KeyCode::Down);
    assert_eq!(tab.level(), Level::Cards);
    key(&mut tab, &state, KeyCode::Up);
    assert_eq!(tab.level(), Level::SubTabs);
    key(&mut tab, &state, KeyCode::Up);
    key(&mut tab, &state, KeyCode::Left);
    assert_eq!(tab.tab(), Tab::Limits, "← on the tabs changes the tab");
}

/// A model that spent in a role and did none of its work isn't on that role's row of who did
/// what, and a role nobody did any of says so.
#[test]
fn who_did_what_names_only_the_models_that_did_the_work() {
    use argonauts_core::crew_stats::WorkRow;
    let mut usage = sample::usage_with_stats();
    let stats = usage.stats.as_mut().unwrap();
    let spent = stats.work[0].spent;
    let idle = [
        ("claude-sonnet-5-5", StatsRole::Judge),
        ("claude-opus-5-5", StatsRole::Merger),
    ];
    for (model, role) in idle {
        stats.work.push(WorkRow {
            agent: "claude-code".to_owned(),
            model: Some(model.to_owned()),
            role,
            kind: None,
            spent,
            done: 0,
            sent_back: 0,
            took_secs: 0,
            sessions: 1,
            tool_calls: 0,
            turns: 0,
            review: Default::default(),
        });
    }
    let state = state(usage);
    let shown = rows(&mut stats_tab(), &state, 120);
    let who = &shown[at(&shown, "who did what")..];
    let judge = at(who, "■ Gemini 3.6 Flash 14 sign-offs");
    assert!(!who[judge].contains("Sonnet 5.5"), "{who:#?}");
    let merger = at(who, " merger ");
    assert!(who[merger + 1].contains("no merges recorded"), "{who:#?}");
    assert!(!who.iter().any(|row| row.contains(" 0 ")), "{who:#?}");
}

/// Records that spell one model three ways, as the alias `opus`, as `claude-opus-5-5` and with
/// a date, draw By CLI, By model, a model opened and By role just as records that spell it one
/// way do.
#[test]
fn a_model_spelled_three_ways_is_one_model_in_every_grouping() {
    use argonauts_core::model_catalog::CatalogEntry;
    use argonauts_core::names::ModelRef;

    let mut names = sample::names();
    let mut opus = CatalogEntry::new(ModelRef::new("claude-code", Some("claude-opus-5-5".into())));
    opus.name = Some("Opus 5.5".to_owned());
    opus.aliases = Some(vec!["opus".to_owned()]);
    names.catalog.models.push(opus);
    let once = sample::usage_with_stats();
    let mut spelled = once.clone();
    let stats = spelled.stats.as_mut().unwrap();
    let mut ways = ["opus", "claude-opus-5-5-20261001", "claude-opus-5-5"]
        .into_iter()
        .cycle();
    let mut respell = |model: &mut Option<String>| {
        if model.as_deref() == Some("claude-opus-5-5") {
            *model = ways.next().map(str::to_owned);
        }
    };
    stats
        .work
        .iter_mut()
        .for_each(|row| respell(&mut row.model));
    stats
        .tasks
        .iter_mut()
        .for_each(|task| respell(&mut task.model));
    stats
        .days
        .iter_mut()
        .for_each(|part| respell(&mut part.model));
    assert!(
        stats
            .work
            .iter()
            .any(|row| row.model.as_deref() == Some("opus"))
    );

    let screens = |usage: UsageSummary| -> Vec<Vec<String>> {
        let mut state = state(usage);
        state.names = names.clone();
        let mut tab = stats_tab();
        let by_cli = rows(&mut tab, &state, 120);
        key(&mut tab, &state, KeyCode::Char('m'));
        let by_model = rows(&mut tab, &state, 120);
        key(&mut tab, &state, KeyCode::Enter);
        let opened = rows(&mut tab, &state, 120);
        while tab.level() != Level::SubTabs {
            key(&mut tab, &state, KeyCode::Up);
        }
        key(&mut tab, &state, KeyCode::Right);
        into_the_cards(&mut tab, &state);
        let by_role = rows(&mut tab, &state, 120);
        assert!(has(&by_role, "◆ all roles"), "{by_role:#?}");
        vec![by_cli, by_model, opened, by_role]
    };
    let (want, got) = (screens(once), screens(spelled));
    for (want, got) in want.iter().zip(&got) {
        assert_eq!(got, want);
    }
}

/// By role and a role opened list only the models that did the role's work, while the role's
/// tokens still hold what a model that did none of it spent there.
#[test]
fn a_role_lists_only_the_models_that_did_its_work() {
    use argonauts_core::crew_stats::WorkRow;
    let once = sample::usage_with_stats();
    let mut idle = once.clone();
    let stats = idle.stats.as_mut().unwrap();
    let spent = argonauts_core::Spend {
        input_tokens: 2_000_000,
        ..Default::default()
    };
    stats.work.push(WorkRow {
        agent: "claude-code".to_owned(),
        model: Some("claude-haiku-5-5".to_owned()),
        role: StatsRole::Reviewer,
        kind: None,
        spent,
        done: 0,
        sent_back: 0,
        took_secs: 0,
        sessions: 1,
        tool_calls: 0,
        turns: 0,
        review: Default::default(),
    });
    let reviewers = |usage: UsageSummary| {
        let state = state(usage);
        let mut tab = stats_tab();
        by_role(&mut tab, &state);
        let cards = rows(&mut tab, &state, 120);
        // The reviewers' card comes after the workers'.
        key(&mut tab, &state, KeyCode::Down);
        key(&mut tab, &state, KeyCode::Down);
        key(&mut tab, &state, KeyCode::Enter);
        let opened = rows(&mut tab, &state, 120);
        (cards, opened)
    };
    let (cards, opened) = reviewers(idle);
    assert!(has(&cards, "63 reviews · 2 models"), "{cards:#?}");
    assert!(!cards.iter().any(|row| row.contains("aiku")), "{cards:#?}");
    let opened = &opened[at(&opened, "reviewers · opened with enter")..];
    assert!(
        !opened.iter().any(|row| row.contains("aiku")),
        "{opened:#?}"
    );
    let tokens = |rows: &[String]| {
        let row = &rows[at(rows, "reviewers · opened with enter")];
        let after = &row[row.find("sent back").expect("the opened row's counts")..];
        after
            .split_whitespace()
            .nth(3)
            .unwrap_or_default()
            .to_owned()
    };
    let (_, without) = reviewers(once);
    assert_ne!(tokens(opened), tokens(&without), "{opened:#?}");
}
