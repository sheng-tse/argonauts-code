//! What `/templates` prints, drawn from its picture's example names at 120 columns.

use std::time::Instant;

use argonauts_core::app::AppEvent;
use argonauts_core::app::template_list::{ChangedIn, TemplateEntry, TemplateList};
use ratatui::text::Line;

use super::live_rows_tests::Screen;
use crate::base::draw::render::{Clock, RenderCtx};
use crate::base::text::plain_text;
use crate::cells::cell::{HistoryCell as _, TemplateFolder, TemplateRows};
use crate::testing::pictures::picture_check::assert_matches;

const WIDTH: u16 = 120;

/// The picture's example: Council in use, a workflow of your own, and the reviewers' prompts
/// changed in this folder.
fn list() -> TemplateList {
    let entry = |name: &str| TemplateEntry::built_in(name);
    TemplateList {
        folder: "~/.argonauts/templates/".to_owned(),
        workflows: vec![
            TemplateEntry {
                in_use: true,
                ..entry("Council")
            },
            entry("Orchestrator"),
            entry("Maker-checker"),
            entry("Best-of-n"),
            TemplateEntry {
                changed: Some(ChangedIn::Yours),
                ..entry("Spec-first")
            },
        ],
        prompts: vec![
            entry("main"),
            entry("orchestrator"),
            entry("workers"),
            TemplateEntry {
                changed: Some(ChangedIn::Here),
                ..entry("reviewers")
            },
            entry("judge"),
        ],
    }
}

#[test]
fn templates_lists_the_workflows_and_the_prompts_and_shows_the_folder() {
    let theme = Screen::theme();
    let mut screen = Screen::new();
    screen.app(AppEvent::Templates { list: list() });
    let printed = screen.lines(WIDTH);
    let rows = plain_text(&printed);
    let rows: Vec<&str> = rows.lines().collect();
    assert_eq!(rows[0], "❯ /templates", "{rows:#?}");
    assert_eq!(rows[4], "", "{rows:#?}");
    assert_eq!(rows.len(), 11, "{rows:#?}");
    // The picture draws the folder above the command. The screen prints it under the rows,
    // which hang from the command that asked for them: the same rows, the two blocks in the
    // other order.
    let (command_and_rows, folder) = printed.split_at(4);
    let mut lines = folder[1..].to_vec();
    lines.push(Line::default());
    lines.extend_from_slice(command_and_rows);
    assert_matches(
        "templates",
        &lines,
        include_str!("../../../../tests/golden/transcript/templates.txt"),
        include_str!("../../../../tests/golden/transcript/templates.html"),
        &theme,
        &[],
    );
}

#[test]
fn the_folder_s_rows_wrap_on_a_narrow_screen() {
    let theme = Screen::theme();
    let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
    let rows = plain_text(&TemplateFolder::new("~/.argonauts/templates/").lines(&ctx));
    let rows: Vec<&str> = rows.lines().map(str::trim_end).collect();
    assert_eq!(
        rows[..4],
        [
            "~/.argonauts/templates/        reference copies, refreshed when Argonauts Code",
            "                               updates; edit a copy, not these",
            "├─ README.md          what is here, and where a copy goes to take effect",
            "├─ workflows/         Council, Orchestrator, Maker-checker, Best-of-n, … +",
        ],
        "{rows:#?}"
    );
    assert_eq!(rows[4], "                      README.md", "{rows:#?}");
    assert!(
        rows.iter().all(|row| !row.ends_with('…')),
        "nothing is cut: {rows:#?}"
    );
}

#[test]
fn a_long_row_wraps_between_names_and_keeps_their_column() {
    let theme = Screen::theme();
    let ctx = RenderCtx::new(48, &theme, Clock::still(Instant::now()));
    let rows = plain_text(&TemplateRows::new(list()).lines(&ctx));
    let rows: Vec<&str> = rows.lines().collect();
    assert_eq!(
        rows[0].trim_end(),
        "  workflows   Council (in use) · Orchestrator ·"
    );
    assert_eq!(
        rows[1].trim_end(),
        "              Maker-checker · Best-of-n ·"
    );
    assert_eq!(rows[2].trim_end(), "              Spec-first (yours)");
}
