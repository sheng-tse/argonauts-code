//! A report's rows that start with a CLI's name, such as `/update`'s, with logos.

use std::time::Instant;

use argonauts_core::app::AppEvent;
use argonauts_core::settings::ui::UiSettings;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;

use super::super::Transcript;
use crate::bridge::{Cx, Effects};
use crate::kit::logo::golden::{count, pictured, with_stand_ins};
use crate::model::ViewModel;
use crate::overview::golden::{Picture, Region, assert_matches, keep, theme};
use crate::render::{Clock, RenderCtx};
use crate::state::AppState;
use crate::text::plain_text;
use crate::theme::Theme;

const WIDTH: u16 = 118;

const ROWS: [&str; 6] = [
    "Argonauts Code   0.1.0     0.1.1 downloaded · takes over at the next start · /release-notes",
    "Claude Code      2.1.304   up to date",
    "Codex            0.163.0   up to date",
    "agy              1.4.3     up to date",
    "OpenCode         2.0.32    up to date · also through ACP",
    "Codex updates on its own schedule.",
];

/// The `/update` report as the screen prints it in `theme`.
fn printed(theme: &Theme) -> Vec<Line<'static>> {
    let mut model = ViewModel::default();
    let state = AppState::default();
    let settings = UiSettings::default();
    let mut out = Effects::default();
    let mut cx = Cx {
        model: &mut model,
        state: &state,
        settings: &settings,
        now: Instant::now(),
        out: &mut out,
    };
    let report = AppEvent::Report {
        title: "Update".to_owned(),
        lines: ROWS.iter().map(|row| (*row).to_owned()).collect(),
    };
    Transcript::default().on_event(&report, &mut cx);
    let ctx = RenderCtx::new(WIDTH, theme, Clock::still(Instant::now()));
    model.history.drain_printable(&ctx)
}

/// Where no logo draws, the rows read as the controller wrote them.
#[test]
fn without_logos_the_rows_read_as_written() {
    let text = plain_text(&printed(&theme()));
    for row in ROWS {
        assert!(text.contains(row), "{row:?} in {text}");
    }
}

/// Where the terminal draws logos, each CLI's row has its logo before the name, which gives up
/// two blanks so the version column stays. Argonauts Code's own row and a sentence that only
/// starts with a CLI's name have none.
#[test]
fn an_update_report_puts_each_clis_logo_before_its_name() {
    let theme = pictured(&theme());
    let lines = printed(&theme);
    let mut buf = Buffer::empty(Rect::new(0, 0, WIDTH, lines.len() as u16));
    for (y, line) in lines.iter().enumerate() {
        buf.set_line(0, y as u16, line, WIDTH);
    }
    assert_eq!(count(&buf), 4);
    let shown = with_stand_ins(&buf);
    keep("update_logos", &shown);
    let picture = Picture::screen("views", "update_logos");
    assert_matches(&picture, &shown, &Region::whole(&picture, (0, 0)), &theme);
}

/// A row the doctor writes, the CLI's name and its version with one blank between, has its
/// logo too; a sentence that starts with the name has none.
#[test]
fn a_doctor_row_with_the_clis_version_has_its_logo() {
    let names = argonauts_core::display_names::DisplayNames::default();
    let logo = |line: &str| super::detail_logo(0, line, &names).and_then(|found| found.logo);
    assert_eq!(
        logo("Claude Code 2.1.303 · signed in · plan usage read"),
        Some(argonauts_core::logos::logo_for("claude-code", None))
    );
    assert_eq!(
        logo("Codex   0.163.0   up to date").map(|logo| logo.mark),
        Some(Some(argonauts_core::logos::LogoMark::Codex))
    );
    assert_eq!(logo("Claude Code updates on its own schedule."), None);
}
