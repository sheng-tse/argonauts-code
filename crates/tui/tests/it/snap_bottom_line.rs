//! The bottom line at every width in `WIDTHS`, in the healthy state and
//! with one CLI near its limit and one out. The rows are drawn without color, so a bar reads
//! as `█` and `░`, and once more in color at 120 columns. Where a CLI's name has no room, two
//! letters stand for it beside its dot.

use crate::common;
use argonauts_core::settings::bottom_line::{BottomLineSettings, LineForm};
use argonauts_tui::app::model::{
    ApprovalMode, BottomLine, BottomRow, CliState, CliUse, LinePart, WindowUse,
};
use argonauts_tui::base::theme::{ColorLevel, Role};
use argonauts_tui::bottom::ModeState;
use argonauts_tui::bridge::bottom_line::{Sample, SampleCli, preview};
use common::{row_text, still, styled, theme, theme_at};
use ratatui::text::Line;

const WIDTHS: [u16; 8] = [200, 150, 120, 100, 80, 60, 40, 20];

fn cli(name: &str, windows: &[(&str, f64)]) -> CliUse {
    let mut cli = CliUse::new(name, Role::for_cli(name));
    let id = match name {
        "Claude Code" => "claude-code",
        "Codex" => "codex",
        _ => "antigravity-cli",
    };
    cli.logo = Some(argonauts_core::look::logos::logo_for(id, None));
    cli.windows = windows
        .iter()
        .map(|(label, used)| WindowUse::new(*label, *used))
        .collect();
    cli
}

/// Claude in use at 1% and 56%, Codex at 11%, agy at 2%.
fn healthy() -> Vec<CliUse> {
    let mut claude = cli("Claude Code", &[("5h", 0.01), ("7d", 0.56)]);
    claude.main = true;
    vec![
        claude,
        cli("Codex", &[("7d", 0.11)]),
        cli("agy", &[("5h", 0.02)]),
    ]
}

/// A pressed moment: Claude near its five-hour limit, Codex fine, agy out until Tuesday.
fn pressed() -> Vec<CliUse> {
    let mut clis = healthy();
    clis[0].windows[0] = WindowUse::new("5h", 0.93);
    clis[0].windows[0].resets = Some("4:40pm".into());
    clis[2].windows.clear();
    clis[2].state = CliState::Out {
        until: Some("Tue 3:45pm".into()),
    };
    clis
}

fn line(form: LineForm, mut clis: Vec<CliUse>) -> BottomLine {
    // Gauges are one cell a window, so the sync gives every CLI both of its own.
    if form == LineForm::Gauges && clis[1].windows.len() == 1 {
        clis[1].windows.insert(0, WindowUse::new("5h", 0.01));
    }
    BottomLine {
        form,
        right: vec![LinePart::PlanUsage(clis)],
        ..BottomLine::default()
    }
}

fn row(line: &BottomLine, theme: &argonauts_tui::base::theme::Theme, width: u16) -> Line<'static> {
    let mode = BottomRow {
        approval: ApprovalMode::Auto,
        ..BottomRow::default()
    };
    let state = ModeState {
        line: Some(line),
        ..ModeState::default()
    };
    mode.line(&still(theme, width), state)
}

/// Every width, one row each, with the terminal's edge marked.
fn at_every_width(line: &BottomLine) -> String {
    let theme = theme_at(ColorLevel::None);
    WIDTHS
        .iter()
        .map(|&width| {
            let text = row_text(&row(line, &theme, width));
            let pad = usize::from(width).saturating_sub(text.chars().count());
            format!("{width:>3} columns\n{text}{}▕", " ".repeat(pad))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn fit_at_every_width_healthy() {
    let line = line(LineForm::Fit, healthy());
    insta::assert_snapshot!("fit_healthy", at_every_width(&line));
}

#[test]
fn fit_at_every_width_near_a_limit_and_out() {
    let line = line(LineForm::Fit, pressed());
    insta::assert_snapshot!("fit_near_a_limit_and_out", at_every_width(&line));
}

#[test]
fn the_pinned_forms_at_every_width() {
    for (form, name) in [
        (LineForm::Bars, "bars"),
        (LineForm::Compact, "compact"),
        (LineForm::Gauges, "gauges"),
        (LineForm::Text, "text"),
    ] {
        let both = format!(
            "healthy\n{}\n\nnear a limit and out\n{}",
            at_every_width(&line(form, healthy())),
            at_every_width(&line(form, pressed()))
        );
        insta::assert_snapshot!(format!("form_{name}"), both);
    }
}

/// One color per CLI, amber from 90% and red when out, at 120 columns.
#[test]
fn the_colors_at_120_columns() {
    let theme = theme();
    let rows: Vec<Line<'static>> = [healthy(), pressed()]
        .into_iter()
        .map(|clis| row(&line(LineForm::Fit, clis), &theme, 120))
        .collect();
    insta::assert_snapshot!("colors_120", styled(&rows));
}

/// What `/mods bottom-line` previews: the page's own sample, the branch after the mode and
/// the spend last, drawn by the line's drawer from the few numbers a page has. It gives up
/// what the live line gives up, at the same widths.
#[test]
fn the_page_s_preview_at_every_width() {
    let cli = |label: &str, id: &str, five_hours: f64, seven_days: f64| SampleCli {
        label: label.to_owned(),
        id: id.to_owned(),
        main: id == "claude-code",
        five_hours: Some(five_hours),
        seven_days: Some(seven_days),
    };
    let healthy = Sample {
        clis: vec![
            cli("Claude Code", "claude-code", 0.01, 0.56),
            cli("Codex", "codex", 0.01, 0.11),
            cli("agy", "antigravity-cli", 0.02, 0.01),
        ],
        ..Sample::default()
    };
    let mut pressed = healthy.clone();
    pressed.clis[0].five_hours = Some(0.93);
    pressed.clis[2].five_hours = Some(1.0);
    let picture = BottomLineSettings::from_toml(
        "left = [\"mode\", \"branch\"]\nright = [\"plan-usage\", \"spend\"]\n\
         [plan-usage]\nshow = \"all\"",
    )
    .unwrap();
    let theme = theme_at(ColorLevel::None);
    let rows = |sample: &Sample| -> String {
        WIDTHS
            .iter()
            .map(|&width| {
                let spans = preview(&theme, &picture, sample, usize::from(width));
                let text = row_text(&Line::from(spans));
                let pad = usize::from(width).saturating_sub(text.chars().count());
                format!("{width:>3} columns\n{text}{}▕", " ".repeat(pad))
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let both = format!(
        "healthy\n{}\n\nnear a limit and out\n{}",
        rows(&healthy),
        rows(&pressed)
    );
    insta::assert_snapshot!("preview", both);
}
