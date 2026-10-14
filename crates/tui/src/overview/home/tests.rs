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

mod golden;

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
    let lines = crate::views::dock::lines(&buf);
    // The Models chart draws its curves with the corners boxes use, so only the other screens
    // are checked for whole frames.
    if !lines
        .iter()
        .any(|line| line.to_string().contains("Tokens per Day"))
    {
        stability::assert_frames(&buf);
    }
    lines
        .iter()
        .map(|line| line.to_string().trim_end().to_owned())
        .collect()
}

fn rows(view: &mut OverviewView, state: &AppState, width: u16) -> Vec<String> {
    rows_on(view, state, width, 120)
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
fn r_cycles_the_periods_as_claude_codes_stats_does() {
    let state = state();
    let mut view = tab(sample::overview());
    for period in [
        OverviewPeriod::Week,
        OverviewPeriod::Month,
        OverviewPeriod::All,
    ] {
        let outcome = key(&mut view, &state, KeyCode::Char('r'));
        assert!(
            matches!(
                outcome,
                ViewOutcome::Request(ViewRequest::Overview { period: asked, import: None })
                    if asked == period
            ),
            "{outcome:?}"
        );
    }
    let shown = rows(&mut view, &state, 118);
    assert!(
        has(&shown, "All time · Last 7 days · Last 30 days"),
        "{shown:#?}"
    );
}

#[test]
fn the_sub_tabs_switch_between_overview_and_models() {
    let state = state();
    let mut view = tab(sample::models());
    let shown = rows(&mut view, &state, 118);
    assert!(has(&shown, "Favorite model: Opus 5.5"), "{shown:#?}");
    assert!(!has(&shown, "Tokens per Day"), "{shown:#?}");
    key(&mut view, &state, KeyCode::Down);
    key(&mut view, &state, KeyCode::Right);
    assert_eq!(view.tab(), Tab::Overview);
    let shown = rows(&mut view, &state, 118);
    assert!(has(&shown, "Tokens per Day"), "{shown:#?}");
    assert!(has(&shown, "★ Favorite: Opus 5.5 · ● Total: 118.4b tokens"));
    assert!(has(&shown, "↑ ↓ 1-4 of 7 models"), "{shown:#?}");
}

#[test]
fn the_models_page_moves_a_row_at_a_time_and_enter_opens_one_in_stats() {
    let state = state();
    let mut view = tab(sample::models());
    key(&mut view, &state, KeyCode::Down);
    key(&mut view, &state, KeyCode::Right);
    key(&mut view, &state, KeyCode::Down);
    key(&mut view, &state, KeyCode::Down);
    let shown = rows(&mut view, &state, 118);
    assert!(has(&shown, "↑ ↓ 3-6 of 7 models"), "{shown:#?}");
    assert!(has(&shown, "Sonnet 5.5 (13.0%)"), "{shown:#?}");
    assert!(!has(&shown, "Opus 5.5 (46.0%)"), "{shown:#?}");
    key(&mut view, &state, KeyCode::Down);
    key(&mut view, &state, KeyCode::Down);
    let shown = rows(&mut view, &state, 118);
    assert!(has(&shown, "↑ ↓ 5-7 of 7 models"), "{shown:#?}");
    key(&mut view, &state, KeyCode::Up);
    key(&mut view, &state, KeyCode::Up);
    assert!(has(&rows(&mut view, &state, 118), "↑ ↓ 1-4 of 7 models"));
    let outcome = key(&mut view, &state, KeyCode::Enter);
    assert_eq!(view.tab(), Tab::Stats);
    assert!(matches!(
        outcome,
        ViewOutcome::Request(ViewRequest::Usage { .. })
    ));
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
        "Stats from the last 365 days · importing your CLIs' history · 3 of 9 files"
    ));
    let mut done = sample::overview();
    done.found = Some("Claude Code since Oct 2025, 812 sessions".into());
    view.accept(&ViewData::Overview {
        overview: Box::new(done),
    });
    let rows = rows(&mut view, &state, 140);
    assert!(
        has(
            &rows,
            "Stats from the last 365 days · imported Claude Code since Oct 2025, 812 sessions"
        ),
        "{rows:#?}"
    );
}

#[test]
fn what_ran_only_through_argonauts_says_so() {
    let state = state();
    let mut data = sample::overview();
    data.sources[2].imported = false;
    let mut view = tab(data);
    assert!(has(
        &rows(&mut view, &state, 140),
        "imported: Claude Code since Oct 2025, Codex since Feb · through Argonauts: agy since Jun"
    ));
}

#[test]
fn nothing_yet_says_so_on_both_sub_tabs() {
    let state = state();
    let mut data = OverviewData::empty(OverviewPeriod::Week, sample::today());
    data.import = ImportStatus::Declined;
    let mut view = tab(data);
    key(&mut view, &state, KeyCode::Char('r'));
    let mut data = OverviewData::empty(OverviewPeriod::Week, sample::today());
    data.import = ImportStatus::Declined;
    view.accept(&ViewData::Overview {
        overview: Box::new(data),
    });
    let said = "Nothing in this period yet. i imports what your CLIs recorded.";
    assert!(has(&rows(&mut view, &state, 118), said));
    key(&mut view, &state, KeyCode::Down);
    key(&mut view, &state, KeyCode::Right);
    assert!(has(&rows(&mut view, &state, 118), said));
}

#[test]
fn a_narrow_panel_stacks_the_pairs_and_no_row_runs_past_it() {
    let state = state();
    for data in [sample::overview(), sample::models()] {
        let mut view = tab(data);
        for width in [40u16, 59, 80, 118, 160] {
            for sub in 0..2 {
                if sub == 1 {
                    key(&mut view, &state, KeyCode::Down);
                    key(&mut view, &state, KeyCode::Right);
                }
                let shown = rows(&mut view, &state, width);
                for row in &shown {
                    assert!(str_width(row) <= usize::from(width), "{width}: {row:?}");
                }
                if sub == 1 {
                    key(&mut view, &state, KeyCode::Right);
                    key(&mut view, &state, KeyCode::Up);
                }
            }
        }
        let shown = rows(&mut view, &state, 50);
        assert!(has(&shown, "Total tokens: 118.4b"), "{shown:#?}");
        assert!(
            !shown
                .iter()
                .any(|row| row.contains("Favorite model:") && row.contains("Total")),
            "{shown:#?}"
        );
    }
}

#[test]
fn ctrl_s_copies_the_sub_tab_without_the_folders() {
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
    assert!(text.contains("Total tokens: 118.4b"), "{text}");
    assert!(text.contains("Favorite model: Opus 5.5"), "{text}");
    assert!(!text.contains("argonauts-code"), "{text}");
}

/// The Overview sits in the panel's frame and moves as its other tabs do: ↓ to the sub-tabs,
/// where ←→ switch Overview and Models, ↓ into the rows, ↑ back out a row at a time, and ←→ on
/// the tabs to the next tab, wrapping at both ends.
#[test]
fn the_overview_moves_as_every_tab_of_the_panel_does() {
    use crate::overview::view::Level;

    let state = state();
    let mut view = tab(sample::models());
    key(&mut view, &state, KeyCode::Down);
    assert_eq!(view.level(), Level::SubTabs);
    // A blank row parts the tabs from the sub-tabs.
    let shown = rows(&mut view, &state, 118);
    assert!(shown[2].trim().is_empty(), "{shown:#?}");
    assert!(shown[3].contains("[Overview]"), "{shown:#?}");
    key(&mut view, &state, KeyCode::Right);
    let shown = rows(&mut view, &state, 118);
    assert!(shown[3].contains("[Models]"), "{shown:#?}");
    key(&mut view, &state, KeyCode::Down);
    assert_eq!(view.level(), Level::Cards);
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
}
