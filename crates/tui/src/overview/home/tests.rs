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

/// The Overview's period answers to the keys every tab with periods has, read in one place:
/// `r` steps on, and Shift+R steps back however the terminal sends it, as a capital with the
/// modifier or without, or as the small letter with it. Ctrl+R is not the period's key.
#[test]
fn the_overviews_period_takes_the_keys_every_tab_with_periods_has() {
    use crate::overview::periods;

    let state = state();
    let mut view = tab(sample::overview());
    let asked = |view: &mut OverviewView, code, modifiers| match press(
        view,
        &state,
        KeyEvent::new(code, modifiers),
    ) {
        ViewOutcome::Request(ViewRequest::Overview { period, .. }) => Some(period),
        _ => None,
    };
    let order = OverviewPeriod::ALL;
    let mut at = order[0];
    for (code, modifiers) in [
        (KeyCode::Char('r'), KeyModifiers::NONE),
        (KeyCode::Char('R'), KeyModifiers::SHIFT),
        (KeyCode::Char('R'), KeyModifiers::NONE),
        (KeyCode::Char('r'), KeyModifiers::SHIFT),
    ] {
        let by = periods::step(KeyEvent::new(code, modifiers)).expect("a period key");
        at = periods::cycled(&order, at, by);
        assert_eq!(
            asked(&mut view, code, modifiers),
            Some(at),
            "{code:?} {modifiers:?}"
        );
    }
    assert_eq!(
        asked(&mut view, KeyCode::Char('r'), KeyModifiers::CONTROL),
        None
    );
    // Its key line opens with the words the helper gives every such tab.
    let shown = rows(&mut view, &state, 118);
    let keys = shown
        .iter()
        .rev()
        .find(|row| !row.trim().is_empty())
        .unwrap();
    assert!(keys.trim_start().starts_with(periods::CYCLE_HINT), "{keys}");
}

/// Shift+R goes back through the periods, and the key line starts with the period's key on
/// the tabs and under them.
#[test]
fn shift_r_cycles_back_and_the_key_line_starts_with_the_periods_key() {
    let state = state();
    let mut view = tab(sample::overview());
    for period in [
        OverviewPeriod::Month,
        OverviewPeriod::Week,
        OverviewPeriod::All,
    ] {
        let outcome = key(&mut view, &state, KeyCode::Char('R'));
        assert!(
            matches!(
                outcome,
                ViewOutcome::Request(ViewRequest::Overview { period: asked, import: None })
                    if asked == period
            ),
            "{outcome:?}"
        );
    }
    let keys = |view: &mut OverviewView| rows(view, &state, 118).last().cloned().unwrap();
    assert_eq!(
        keys(&mut view).trim(),
        "r to cycle dates · tab · ←→ tabs · ↓ Overview or Models · ctrl+s to copy · esc closes"
    );
    key(&mut view, &state, KeyCode::Down);
    assert_eq!(
        keys(&mut view).trim(),
        "r to cycle dates · ↑ tabs · tab Overview or Models · ctrl+s to copy · esc closes"
    );
}

#[test]
fn the_sub_tabs_switch_between_overview_and_models() {
    let state = state();
    let mut view = tab(sample::models());
    let shown = rows(&mut view, &state, 118);
    assert!(has(&shown, "favourite    Opus 5.5  46%"), "{shown:#?}");
    assert!(!has(&shown, "Tokens per Day"), "{shown:#?}");
    key(&mut view, &state, KeyCode::Down);
    key(&mut view, &state, KeyCode::Right);
    assert_eq!(view.tab(), Tab::Overview);
    let shown = rows(&mut view, &state, 118);
    assert!(has(&shown, "Tokens per Day"), "{shown:#?}");
    assert!(has(&shown, "★ Favorite: Opus 5.5 · ● Total: 118.4b tokens"));
    assert!(
        has(&shown, "r to cycle dates · ↑ ↓ 1-4 of 7 models"),
        "{shown:#?}"
    );
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
    // The last page ends on the last model, as Claude Code's does, and ↓ stops there.
    key(&mut view, &state, KeyCode::Down);
    key(&mut view, &state, KeyCode::Down);
    let shown = rows(&mut view, &state, 118);
    assert!(has(&shown, "↑ ↓ 4-7 of 7 models"), "{shown:#?}");
    assert!(has(&shown, "Gemini 3.6 Flash"), "{shown:#?}");
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

/// One day of history is `the last day`, where more are counted.
#[test]
fn one_day_of_history_reads_as_the_last_day() {
    let state = state();
    let mut data = sample::overview();
    data.span_days = 1;
    let shown = rows(&mut tab(data), &state, 140);
    let source = shown.iter().find(|row| row.contains("Stats from"));
    let source = source.unwrap_or_else(|| panic!("{shown:#?}"));
    assert!(
        source
            .trim_start()
            .starts_with("Stats from the last day · "),
        "{source}"
    );
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
        "imported: Claude Code since Oct 2025, Codex since Feb · through Argonauts Code: agy since Jun"
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
fn a_narrow_panel_stacks_the_groups_and_the_bars_and_no_row_runs_past_it() {
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
        // Three groups need 110 cells side by side. Under that each stands under the one
        // before it, and each bar takes a row of its own with its legend under it.
        let shown = rows(&mut view, &state, 100);
        let at = |text: &str| shown.iter().position(|row| row.trim() == text);
        let (tokens, days) = (at("Tokens").expect("Tokens"), at("Days").expect("Days"));
        assert!(
            shown[tokens + 1].contains("Argonauts Code  118.4b"),
            "{shown:#?}"
        );
        assert!(
            days > tokens + 1 && shown[days - 1].trim().is_empty(),
            "{shown:#?}"
        );
        // Under one another the three groups' values start in one column.
        assert!(shown[days + 1].contains("active          318       of 365"));
        let kind = shown
            .iter()
            .position(|row| row.contains("by kind"))
            .expect("by kind");
        let cli = shown
            .iter()
            .position(|row| row.contains("by CLI"))
            .expect("by CLI");
        assert_eq!(cli, kind + 2, "{shown:#?}");
        assert!(
            shown[kind + 1].contains("■ cache read 93%  ■ write 6%"),
            "{shown:#?}"
        );
        assert!(
            shown[cli + 1].contains("■ Claude 62%  ■ Codex 28%  ■ agy 10%"),
            "{shown:#?}"
        );
        let bar = |row: usize| {
            shown[row]
                .find('━')
                .map(|at| shown[row][..at].chars().count())
        };
        assert_eq!(bar(kind), bar(cli), "the two bars start in one column");
    }
}

/// A long model's name is cut in its column, and the three groups stay side by side.
#[test]
fn a_long_favourite_is_cut_in_its_column() {
    let state = state();
    let mut data = sample::numbers();
    data.models[0].model = Some("gemini-3.6-flash-low-preview-experimental".to_owned());
    let mut view = tab(data);
    let shown = rows(&mut view, &state, 120);
    let row = shown.iter().find(|row| row.contains("busiest"));
    let row = row.unwrap_or_else(|| panic!("{shown:#?}"));
    assert!(
        row.contains("peak 9pm        favourite    Gemini 3.6 Flash L…"),
        "{row}"
    );
    assert!(row.ends_with('…') && str_width(row) <= 118, "{row}");
}

/// Claude Code's own count stands first where its /stats has kept one, with how far it goes
/// when that isn't up to yesterday. Without it the count here stands alone and says nothing
/// of what it adds.
#[test]
fn claude_codes_own_count_comes_first_and_says_how_far_it_goes() {
    use argonauts_core::overview::OwnCount;
    use time::Duration;

    let state = state();
    let numbers = |data: OverviewData| -> Vec<String> {
        let mut view = tab(data);
        let shown = rows(&mut view, &state, 120);
        let first = shown
            .iter()
            .position(|row| row.trim_start().starts_with("Tokens "));
        shown[first.expect("the groups") + 2..][..3].to_vec()
    };
    let shown = numbers(sample::numbers());
    assert!(
        shown[0].starts_with(" Claude Code     60.6b     as /stats counts "),
        "{shown:#?}"
    );
    assert!(
        shown[1].starts_with(" Argonauts Code  69.1b     + workflow agents "),
        "{shown:#?}"
    );
    assert!(
        shown[2].starts_with(" API value       ~$32,540 "),
        "{shown:#?}"
    );

    let mut stale = sample::numbers();
    stale.claude_stats = Some(OwnCount {
        tokens: 58_100_000_000,
        through: stale.today - Duration::days(9),
    });
    let shown = numbers(stale);
    assert!(
        shown[0].starts_with(" Claude Code     58.1b     /stats to Sep 23 "),
        "{shown:#?}"
    );

    let shown = numbers(sample::overview());
    assert!(
        shown[0].starts_with(" Argonauts Code  118.4b    "),
        "{shown:#?}"
    );
    assert!(!shown[0].contains("workflow agents"), "{shown:#?}");
    assert!(
        shown[1].starts_with(" API value       ~$9,640 "),
        "{shown:#?}"
    );
    assert!(
        shown[2].trim_start().starts_with("busiest"),
        "the third row has no token: {shown:#?}"
    );
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
    assert!(text.contains("Argonauts Code  118.4b"), "{text}");
    assert!(text.contains("favourite       Opus 5.5  46%"), "{text}");
    assert!(
        text.contains("by kind: cache read 93%, write 6%, output <1%, input 1%"),
        "{text}"
    );
    assert!(
        text.contains("by CLI: Claude 62%, Codex 28%, agy 10%"),
        "{text}"
    );
    assert!(!text.contains('━'), "a bar says nothing as text: {text}");
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
