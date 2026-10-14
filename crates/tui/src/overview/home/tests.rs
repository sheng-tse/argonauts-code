use std::time::Instant;

use argonauts_core::app::{ViewData, ViewRequest};
use argonauts_core::overview::{ImportStatus, OverviewData, OverviewPeriod};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};

use super::sample;
use crate::overview::{OverviewView, Tab, UsageWatch};
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

fn state() -> AppState {
    let mut state = AppState::default();
    state.names = crate::overview::sample::names();
    state
}

fn tab(data: OverviewData) -> OverviewView {
    let mut view =
        OverviewView::new(Tab::Overview, UsageWatch::default()).at(crate::overview::sample::now());
    assert!(view.accept(&ViewData::Overview {
        overview: Box::new(data)
    }));
    view
}

fn rows_in(view: &mut OverviewView, state: &AppState, width: u16, theme: &Theme) -> Vec<String> {
    let ctx = ViewCtx {
        theme,
        state,
        now: Instant::now(),
    };
    let height = view.dock_height(Size::new(width, 120), &ctx).unwrap_or(40);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    stability::assert_frames(&buf);
    crate::views::dock::lines(&buf)
        .iter()
        .map(|line| line.to_string().trim_end().to_owned())
        .collect()
}

fn rows(view: &mut OverviewView, state: &AppState, width: u16) -> Vec<String> {
    rows_in(view, state, width, &plain())
}

/// The panel's rows on a screen `width` by `height`.
fn rows_on(view: &mut OverviewView, state: &AppState, width: u16, height: u16) -> Vec<String> {
    let theme = plain();
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    let height = view
        .dock_height(Size::new(width, height), &ctx)
        .unwrap_or(height);
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
    press(view, state, KeyEvent::from(code))
}

fn press(view: &mut OverviewView, state: &AppState, key: KeyEvent) -> ViewOutcome {
    let theme = plain();
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    view.handle_key(key, &ctx)
}

fn has(rows: &[String], text: &str) -> bool {
    rows.iter().any(|row| row.contains(text))
}

#[test]
fn the_header_names_every_cli_and_where_its_history_came_from() {
    let state = state();
    let mut view = tab(sample::overview());
    let rows = rows(&mut view, &state, 160);
    assert!(rows[1].ends_with("every CLI on this computer"), "{rows:#?}");
    assert!(
        rows[2].starts_with("  All time   Last 30 days   Last 7 days"),
        "{rows:#?}"
    );
    assert!(
        rows[2].ends_with(
            "imported · Claude Code since Oct 2025 · Codex since Feb · agy through Argonauts since Jun"
        ),
        "{rows:#?}"
    );
    // Narrow, the note gives way with … and the periods stay whole.
    let rows = self::rows(&mut view, &state, 70);
    assert!(rows[2].contains("Last 7 days"), "{rows:#?}");
    assert!(rows[2].ends_with('…'), "{rows:#?}");
}

#[test]
fn six_tiles_hold_the_numbers_people_ask_for_first() {
    let state = state();
    let mut view = tab(sample::overview());
    let rows = rows(&mut view, &state, 200);
    for text in [
        "Total tokens",
        "118.4b",
        "↑ 12% vs the 30 days before",
        "API value",
        "~$9,640",
        "at list prices",
        "Sessions",
        "1,086",
        "812 · 221 · 53 by CLI",
        "Active days",
        "318",
        "of 365",
        "Streak",
        "18 days",
        "longest 64",
        "Favourite",
        "Opus 5.5",
        "46% of tokens",
    ] {
        assert!(has(&rows, text), "{text}: {rows:#?}");
    }
    let tiles = rows
        .iter()
        .position(|row| row.contains("Total tokens"))
        .unwrap();
    assert!(rows[tiles].contains("Favourite"), "six to a row: {rows:#?}");
    // Six still fit a little narrower, the trend said more briefly.
    let rows = self::rows(&mut view, &state, 160);
    let tiles = rows
        .iter()
        .position(|row| row.contains("Total tokens"))
        .unwrap();
    assert!(rows[tiles].contains("Favourite"), "{rows:#?}");
    assert!(has(&rows, "↑ 12% vs before"), "{rows:#?}");
    // On a narrower screen they take two rows of three.
    let rows = self::rows(&mut view, &state, 100);
    let tiles = rows
        .iter()
        .position(|row| row.contains("Total tokens"))
        .unwrap();
    assert!(rows[tiles].contains("Sessions") && !rows[tiles].contains("Active days"));
}

#[test]
fn every_part_of_the_picture_is_drawn() {
    let state = state();
    let mut view = tab(sample::overview());
    let rows = rows(&mut view, &state, 140);
    for text in [
        "Mon",
        "Fri",
        "each day in its main CLI's colour, darker for less · busiest",
        "by hour of day · busiest 9pm",
        "12am",
        "by CLI",
        "◆ tokens a week, by CLI",
        "last 26 weeks",
        "◆ models",
        "sorted by tokens · enter opens one in Stats",
        "GPT 5.6 Luna",
        "◆ where it went",
        "folders, on this computer only",
        "argonauts-code",
        "9 others",
        "◆ through Argonauts",
        "14 · council 8 · orchestrator 5 · yours 1",
        "63 · 92% passed review first time",
        "4 · 3 for a usage limit",
        "11 questions on autopilot",
        "◆ worth knowing",
        "97% of your input came from cache",
        "64% of tokens ran in subagents",
        "Codex went from 14% of your tokens in March to 34% in September",
        "Your longest session ran 57 days · probably left open in a terminal",
        "Your input and output are",
    ] {
        assert!(has(&rows, text), "{text}: {rows:#?}");
    }
    let last = rows.last().unwrap();
    assert!(
        last.starts_with(" ←→ tabs · ↓ periods · ↓ cards · enter open"),
        "{last}"
    );
}

#[test]
fn the_weekly_bars_fill_the_card_at_any_width() {
    let state = state();
    for width in [80u16, 101, 133, 180] {
        let mut view = tab(sample::overview());
        let rows = rows(&mut view, &state, width);
        let head = rows
            .iter()
            .position(|row| row.contains("tokens a week, by CLI"))
            .unwrap();
        // The tallest week reaches the right edge of the card's inside.
        let chart = &rows[head + 1..head + 7];
        let reach = chart
            .iter()
            .map(|row| str_width(row.trim_end_matches(['│', ' '])))
            .max()
            .unwrap();
        assert!(
            reach >= usize::from(width) - 4,
            "{width}: reach {reach}: {chart:#?}"
        );
        for row in &rows {
            assert!(str_width(row) <= usize::from(width), "{width}: {row}");
        }
    }
}

/// The last date under the bars ends at the card's edge rather than being cut, and with no
/// history the card says so instead of drawing a scale over nothing.
#[test]
fn the_weekly_dates_stay_whole_and_no_history_says_so() {
    let state = state();
    for width in [80u16, 101] {
        let mut view = tab(sample::overview());
        let rows = rows(&mut view, &state, width);
        let head = rows
            .iter()
            .position(|row| row.contains("tokens a week, by CLI"))
            .unwrap();
        let dates = &rows[head + 7];
        assert!(!dates.contains('…'), "{width}: {dates}");
    }
    let mut data = sample::overview();
    for week in &mut data.weeks {
        week.by_cli.clear();
    }
    let mut view = tab(data);
    let rows = rows(&mut view, &state, 100);
    assert!(has(&rows, "No tokens in the last 26 weeks"), "{rows:#?}");
}

#[test]
fn the_first_time_it_asks_once_and_the_answer_goes_to_the_app() {
    let state = state();
    let mut data = sample::overview();
    data.import = ImportStatus::Unasked;
    let mut view = tab(data);
    let rows = rows(&mut view, &state, 120);
    assert!(has(&rows, "◆ your history"), "{rows:#?}");
    assert!(has(&rows, "❯ 1. Yes, import it"), "{rows:#?}");
    assert!(
        has(&rows, "What was said is skipped as each line is read"),
        "{rows:#?}"
    );
    key(&mut view, &state, KeyCode::Down);
    assert!(has(&self::rows(&mut view, &state, 120), "❯ 2. Not now"));
    key(&mut view, &state, KeyCode::Up);
    let outcome = key(&mut view, &state, KeyCode::Enter);
    assert!(
        matches!(
            outcome,
            ViewOutcome::Request(ViewRequest::Overview {
                period: OverviewPeriod::All,
                import: Some(true),
            })
        ),
        "{outcome:?}"
    );
    let rows = self::rows(&mut view, &state, 120);
    assert!(!has(&rows, "◆ your history"), "asked once: {rows:#?}");
    assert!(has(&rows, "importing your CLIs' history"), "{rows:#?}");
}

#[test]
fn the_import_says_what_it_found_when_it_finishes() {
    let state = state();
    let mut running = sample::overview();
    running.import = ImportStatus::Running { done: 3, of: 9 };
    let mut view = tab(running);
    assert!(has(
        &rows(&mut view, &state, 140),
        "importing your CLIs' history · 3 of 9 files"
    ));
    let mut done = sample::overview();
    done.found = Some("Claude Code since Oct 2025, 812 sessions".into());
    view.accept(&ViewData::Overview {
        overview: Box::new(done),
    });
    let rows = rows(&mut view, &state, 140);
    assert!(
        rows[2].ends_with("imported Claude Code since Oct 2025, 812 sessions"),
        "{rows:#?}"
    );
}

#[test]
fn down_reaches_the_periods_where_the_arrows_ask_for_another() {
    let state = state();
    let mut view = tab(sample::overview());
    key(&mut view, &state, KeyCode::Down);
    let outcome = key(&mut view, &state, KeyCode::Left);
    assert!(
        matches!(
            outcome,
            ViewOutcome::Request(ViewRequest::Overview {
                period: OverviewPeriod::Week,
                import: None,
            })
        ),
        "← from All time wraps to the last period: {outcome:?}"
    );
    assert_eq!(view.tab(), Tab::Overview);
    let rows = rows(&mut view, &state, 120);
    assert!(rows[2].contains("[Last 7 days]"), "{rows:#?}");
}

#[test]
fn enter_on_a_model_opens_it_in_stats() {
    let state = state();
    let mut view = tab(sample::overview());
    for code in [KeyCode::Down, KeyCode::Down, KeyCode::Down] {
        key(&mut view, &state, code);
    }
    let rows = rows(&mut view, &state, 140);
    assert!(
        rows.iter().any(|row| row.contains("❯ ▌ GPT 5.6 ")),
        "{rows:#?}"
    );
    let outcome = key(&mut view, &state, KeyCode::Enter);
    assert_eq!(view.tab(), Tab::Stats);
    assert!(matches!(
        outcome,
        ViewOutcome::Request(ViewRequest::Usage { .. })
    ));
}

/// A model the crew never ran, whose history came from the import, opens Stats with a line
/// that says so, gone at the next key.
#[test]
fn enter_on_a_model_the_crew_never_ran_says_why_stats_has_no_row() {
    let mut state = state();
    // The numbers for the period the Overview asks Stats for, every day so far.
    let mut usage = crate::overview::sample::usage_with_stats();
    usage.period = argonauts_core::api::UsagePeriod::All;
    state.usage = Some(std::sync::Arc::new(usage));
    let mut data = sample::overview();
    data.models
        .retain(|model| model.model.as_deref() == Some("claude-opus-5"));
    let mut view = tab(data);
    for _ in 0..6 {
        if rows(&mut view, &state, 140)
            .iter()
            .any(|row| row.contains("❯ ▌ Opus 5"))
        {
            break;
        }
        key(&mut view, &state, KeyCode::Down);
    }
    key(&mut view, &state, KeyCode::Enter);
    assert_eq!(view.tab(), Tab::Stats);
    let said = "Opus 5 on Claude Code did no crew work";
    let shown = rows(&mut view, &state, 140);
    assert!(has(&shown, said), "{shown:#?}");
    key(&mut view, &state, KeyCode::Char('s'));
    assert!(!has(&rows(&mut view, &state, 140), said));
}

/// Every block under the models table is a stop: ↓ past the last model reaches "where it
/// went", "worth knowing" and the comparison in turn, the tab scrolling with them, and the
/// keys line says when more is below.
#[test]
fn the_keys_reach_every_block_of_the_overview() {
    let state = state();
    let mut view = tab(sample::overview());
    let first = rows_on(&mut view, &state, 120, 40);
    assert!(has(&first, "↓ more"), "{first:#?}");
    let mut seen = Vec::new();
    for _ in 0..30 {
        key(&mut view, &state, KeyCode::Down);
        seen.extend(rows_on(&mut view, &state, 120, 40));
    }
    assert!(has(&seen, "worth knowing"), "{seen:#?}");
    let last = rows_on(&mut view, &state, 120, 40);
    assert!(!has(&last, "↓ more"), "{last:#?}");
    // A page up goes back the way it came.
    key(&mut view, &state, KeyCode::PageUp);
    let back = rows_on(&mut view, &state, 120, 40);
    assert_ne!(back, last);
}

#[test]
fn ctrl_s_copies_a_summary_without_the_folders() {
    let state = state();
    let mut view = tab(sample::overview());
    let outcome = press(
        &mut view,
        &state,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
    );
    let ViewOutcome::Copy(text) = outcome else {
        panic!("{outcome:?}");
    };
    assert!(text.contains("118.4b tokens"), "{text}");
    assert!(text.contains("Opus 5.5 on Claude Code"), "{text}");
    assert!(!text.contains("argonauts-code"), "{text}");
}

#[test]
fn colour_draws_every_day_in_its_clis_ramp() {
    let state = state();
    let theme = Theme::dark().with_level(ColorLevel::TrueColor);
    let mut view = tab(sample::overview());
    let rows = rows_in(&mut view, &state, 140, &theme);
    assert!(has(&rows, "■"), "{rows:#?}");
}

/// The Overview and Stats sit in the panel's frame and move as its other tabs do: ↓ to the
/// sub-tabs, where ←→ change what shows at once, ↓ into the cards, ↑ back out a row at a time,
/// and ←→ on the tabs to the next tab, wrapping at both ends. Without colour, brackets mark
/// the one row with the keys, where colour would fill it.
#[test]
fn the_overview_and_stats_move_as_every_tab_of_the_panel_does() {
    use crate::overview::view::Level;

    let mut state = state();
    state.usage = Some(std::sync::Arc::new(
        crate::overview::sample::usage_with_stats(),
    ));
    let mut view = tab(sample::overview());
    let marked = |rows: &[String]| (rows[1].contains('['), rows[2].contains('['));
    let shown = rows(&mut view, &state, 140);
    assert_eq!(marked(&shown), (true, false), "{shown:#?}");

    key(&mut view, &state, KeyCode::Down);
    assert_eq!(view.level(), Level::SubTabs);
    let shown = rows(&mut view, &state, 140);
    assert_eq!(marked(&shown), (false, true), "{shown:#?}");
    let outcome = key(&mut view, &state, KeyCode::Right);
    assert!(
        matches!(
            outcome,
            ViewOutcome::Request(ViewRequest::Overview {
                period: OverviewPeriod::Month,
                import: None,
            })
        ),
        "→ on the periods asks for the next at once: {outcome:?}"
    );
    key(&mut view, &state, KeyCode::Down);
    assert_eq!(view.level(), Level::Cards);
    let shown = rows(&mut view, &state, 140);
    assert_eq!(marked(&shown), (false, false), "{shown:#?}");
    key(&mut view, &state, KeyCode::Up);
    assert_eq!(view.level(), Level::SubTabs, "↑ on the first row");
    key(&mut view, &state, KeyCode::Up);
    assert_eq!(view.level(), Level::Tabs);

    key(&mut view, &state, KeyCode::Left);
    assert_eq!(
        view.tab(),
        Tab::Stats,
        "← on the first tab goes to the last"
    );
    key(&mut view, &state, KeyCode::Down);
    assert_eq!(view.level(), Level::SubTabs);
    key(&mut view, &state, KeyCode::Right);
    let shown = rows(&mut view, &state, 140);
    assert!(shown[2].contains("[By model]"), "{shown:#?}");
    assert_eq!(view.tab(), Tab::Stats, "→ on the sub-tabs stays on the tab");
    key(&mut view, &state, KeyCode::Down);
    assert_eq!(view.level(), Level::Cards);
    key(&mut view, &state, KeyCode::Up);
    key(&mut view, &state, KeyCode::Up);
    assert_eq!(view.level(), Level::Tabs);
    key(&mut view, &state, KeyCode::Right);
    assert_eq!(
        view.tab(),
        Tab::Overview,
        "→ on the last tab goes to the first"
    );
}
