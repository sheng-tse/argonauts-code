//! The line as a settings page previews it: the parts its settings name, laid out by the
//! live line's own drawer, so the preview gives things up at the widths the line does. It
//! draws from a handful of numbers in place of the app's state. A part with no number yet
//! shows a sample one, or its name, so a part you tick always shows where it will sit.

use std::time::Instant;

use argonauts_core::settings::bottom_line::{
    ARTIFACT, BRANCH, BottomLineSettings, CONTEXT, LineForm, MODE, PLAN_USAGE, PlanUsageAs,
    PlanUsageLine, SPEND, STATS, TOKENS,
};
use ratatui::text::Span;

use super::{shown_windows, shows, with_mode};
use crate::bottom::{
    AddedPart, ApprovalMode, BottomLine, CliState, CliUse, LinePart, ModeLine, ModeState, PartBody,
    WindowUse,
};
use crate::cell::Tone;
use crate::render::{Clock, RenderCtx};
use crate::text::sanitize_line;
use crate::theme::{Role, Theme};

/// What the app knows now of what the line shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sample {
    /// The approval mode as the line names it, such as `auto mode on`.
    pub mode: String,
    pub branch: Option<String>,
    /// One per CLI in the crew.
    pub clis: Vec<SampleCli>,
    /// The spend as it reads, such as `~$1.86`.
    pub spend: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SampleCli {
    /// Such as `Claude`, `Codex`, `agy`.
    pub label: String,
    /// The adapter's id, which picks its color.
    pub id: String,
    /// The CLI the main session is on.
    pub main: bool,
    /// Used, from 0 to 1, where it's known.
    pub five_hours: Option<f64>,
    pub seven_days: Option<f64>,
}

/// The bottom line `settings` describes, `width` columns wide, as the live line would draw
/// these numbers.
pub fn preview(
    theme: &Theme,
    settings: &BottomLineSettings,
    sample: &Sample,
    width: usize,
) -> Vec<Span<'static>> {
    let line = sample_line(settings, sample);
    let mode = ModeLine {
        approval: approval(&sample.mode),
        ..ModeLine::default()
    };
    let width = u16::try_from(width).unwrap_or(u16::MAX);
    let ctx = RenderCtx::new(width, theme, Clock::still(Instant::now()));
    let state = ModeState {
        line: Some(&line),
        ..ModeState::default()
    };
    mode.line(&ctx, state).spans
}

/// The mode a banner names. A sample that names none previews the line in auto mode.
fn approval(banner: &str) -> ApprovalMode {
    ApprovalMode::ALL
        .into_iter()
        .find(|mode| mode.banner() == banner.trim())
        .unwrap_or(ApprovalMode::Auto)
}

fn sample_line(settings: &BottomLineSettings, sample: &Sample) -> BottomLine {
    let set = &settings.plan_usage;
    // Gauges are one cell a window, so every CLI has room for both of its own.
    let both = settings.form == LineForm::Gauges;
    let clis: Vec<CliUse> = sample
        .clis
        .iter()
        .map(|cli| chip(cli, both, set))
        .filter(|cli| shows(cli, set.show))
        .collect();
    let words = |text: &str| PartBody::Words {
        text: text.to_owned(),
        tone: Tone::Dim,
    };
    let part = |name: &String| -> Option<LinePart> {
        let body = match name.as_str() {
            MODE => return Some(LinePart::Mode),
            PLAN_USAGE => {
                return (!clis.is_empty()).then(|| LinePart::PlanUsage(clis.clone()));
            }
            // It shows once the conversation has published one, and a preview has none.
            ARTIFACT => return None,
            BRANCH => words(sample.branch.as_deref().unwrap_or("main")),
            SPEND => words(sample.spend.as_deref().unwrap_or("~$1.86")),
            CONTEXT => PartBody::Meter {
                label: CONTEXT.to_owned(),
                label_role: Role::Dim,
                ratio: 0.21,
                value: "21%".to_owned(),
                fill: Role::BarFill,
            },
            TOKENS => words("48.2k tokens"),
            // As wide as the stats mod's words, so the preview gives up what the line would.
            STATS => words("stats 1.2M tok"),
            // What other code draws there, such as a mod's chip: its name holds the place.
            other => words(other),
        };
        Some(LinePart::Added(AddedPart {
            name: name.clone(),
            body,
        }))
    };
    with_mode(BottomLine {
        form: settings.form,
        left: settings.left.iter().filter_map(part).collect(),
        right: settings.right.iter().filter_map(part).collect(),
        drop_first: settings.narrow.drop.clone(),
        as_left: set.shown_as == PlanUsageAs::Left,
    })
}

/// A CLI's chip from the share of each window it has used. A window that is used up puts
/// the CLI out, as a reading the plan turned down does on the live line.
fn chip(cli: &SampleCli, both: bool, set: &PlanUsageLine) -> CliUse {
    let mut chip = CliUse::new(sanitize_line(&cli.label).trim(), Role::for_cli(&cli.id));
    chip.main = cli.main;
    let windows: Vec<WindowUse> = [("5h", cli.five_hours), ("7d", cli.seven_days)]
        .into_iter()
        .filter_map(|(label, used)| Some(WindowUse::new(label, used?)))
        .collect();
    if windows.iter().any(|window| window.used >= 1.0) {
        chip.state = CliState::Out { until: None };
    }
    chip.windows = shown_windows(windows, cli.main || both, set.windows);
    chip
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::bottom_line::PlanUsageShow;

    use super::*;
    use crate::theme::ColorLevel;

    fn cli(label: &str, id: &str, five_hours: f64, seven_days: f64) -> SampleCli {
        SampleCli {
            label: label.to_owned(),
            id: id.to_owned(),
            main: false,
            five_hours: Some(five_hours),
            seven_days: Some(seven_days),
        }
    }

    /// Sample numbers: Claude in use at 1% and 56%, Codex at 11%, agy at 2%.
    fn crew() -> Sample {
        let mut claude = cli("Claude", "claude-code", 0.01, 0.56);
        claude.main = true;
        Sample {
            clis: vec![
                claude,
                cli("Codex", "codex", 0.01, 0.11),
                cli("agy", "antigravity-cli", 0.02, 0.01),
            ],
            ..Sample::default()
        }
    }

    fn settings(text: &str) -> BottomLineSettings {
        BottomLineSettings::from_toml(text).unwrap()
    }

    fn text(settings: &BottomLineSettings, sample: &Sample, width: usize) -> String {
        let theme = Theme::dark().with_level(ColorLevel::None);
        let spans = preview(&theme, settings, sample, width);
        let text: String = spans.iter().map(|span| span.content.as_ref()).collect();
        assert!(crate::text::str_width(&text) <= width, "{text}");
        text
    }

    /// The line as it ships, at 120 columns: the mode at the left, a chip per
    /// CLI against the right edge, the one in use with both windows.
    #[test]
    fn the_preview_is_the_line_as_it_ships() {
        let shown = text(&BottomLineSettings::default(), &crew(), 120);
        assert_eq!(crate::text::str_width(&shown), 120);
        assert!(shown.starts_with("  ⏵⏵ auto mode on"), "{shown}");
        assert!(
            shown.ends_with(
                "● Claude 5h ░░░░░░ 1% 7d ███░░░ 56%  ○ Codex 7d █░░░░░ 11%  ○ agy 5h ░░░░░░ 2%"
            ),
            "{shown}"
        );
        // No artifact is published in a preview, so its part says nothing.
        assert!(!shown.contains("artifact"), "{shown}");

        let named = Sample {
            mode: "supervised mode on".to_owned(),
            ..crew()
        };
        let shown = text(&BottomLineSettings::default(), &named, 120);
        assert!(shown.contains("supervised mode on"), "{shown}");
    }

    /// The settings page's own sample: the branch after the mode and the spend last, with
    /// sample numbers where the app has none yet.
    #[test]
    fn a_ticked_part_shows_where_it_will_sit() {
        let picture =
            settings("left = [\"mode\", \"branch\"]\nright = [\"plan-usage\", \"spend\"]");
        let shown = text(&picture, &crew(), 150);
        assert!(shown.contains("auto mode on"), "{shown}");
        assert!(shown.contains(" · main"), "{shown}");
        assert!(shown.ends_with("○ agy 5h ░░░░░░ 2%  ~$1.86"), "{shown}");

        let known = Sample {
            branch: Some("r1-line".to_owned()),
            spend: Some("~$0.42".to_owned()),
            ..crew()
        };
        let shown = text(&picture, &known, 150);
        assert!(shown.contains(" · r1-line"), "{shown}");
        assert!(shown.ends_with("~$0.42"), "{shown}");

        let more = settings(
            "left = [\"mode\", \"checks\"]\nright = [\"context\", \"tokens\", \"plan-usage\"]",
        );
        let shown = text(&more, &crew(), 200);
        assert!(shown.contains(" · checks"), "{shown}");
        assert!(
            shown.contains("context █░░░░░ 21%  48.2k tokens  ● Claude"),
            "{shown}"
        );
        let stats = settings("right = [\"stats\", \"plan-usage\"]");
        let shown = text(&stats, &crew(), 200);
        assert!(shown.contains("stats 1.2M tok  ● Claude"), "{shown}");
    }

    /// The preview fits as the line does: the parts you added go first, in the file's order,
    /// then the chips step down, and every CLI stays.
    #[test]
    fn the_preview_gives_things_up_as_the_line_does() {
        let picture =
            settings("left = [\"mode\", \"branch\"]\nright = [\"plan-usage\", \"spend\"]");
        let widths = [200, 120, 100, 80, 60, 40];
        let rows: Vec<String> = widths
            .iter()
            .map(|&width| text(&picture, &crew(), width))
            .collect();
        assert!(rows[0].contains("(shift+tab to cycle)"), "{}", rows[0]);
        assert!(rows[0].contains("main") && rows[0].contains("$1.86"));
        assert!(!rows[2].contains("$1.86"), "{}", rows[2]);
        assert!(!rows[3].contains("main"), "{}", rows[3]);
        for row in &rows {
            for cli in ["Claude", "Codex", "agy"] {
                // A CLI that is fine ends as a dot in its color, and the one in use by name.
                assert!(
                    row.contains(cli) || (cli != "Claude" && row.contains('○')),
                    "{cli} in {row}"
                );
            }
        }
    }

    /// The plan-usage settings pick who shows, which windows and how they read, and a
    /// window that is used up puts its CLI out.
    #[test]
    fn the_plan_usage_settings_show_in_the_preview() {
        let main = settings("[plan-usage]\nshow = \"main\"");
        assert_eq!(main.plan_usage.show, PlanUsageShow::Main);
        let shown = text(&main, &crew(), 120);
        assert!(
            shown.contains("● Claude") && !shown.contains("Codex"),
            "{shown}"
        );

        let left = settings("[plan-usage]\nas = \"left\"\nwindows = \"7d\"");
        let shown = text(&left, &crew(), 150);
        assert!(
            shown.contains("● Claude 7d ███░░░ 44% left  ○ Codex 7d █████░ 89% left"),
            "{shown}"
        );

        let gauges = settings("form = \"gauges\"");
        let shown = text(&gauges, &crew(), 120);
        assert!(
            shown.ends_with("● Claude ▁▅  ○ Codex ▁▁  ○ agy ▁▁"),
            "{shown}"
        );

        let mut pressed = crew();
        pressed.clis[0].five_hours = Some(0.93);
        pressed.clis[2].five_hours = Some(1.0);
        let shown = text(&BottomLineSettings::default(), &pressed, 150);
        assert!(shown.contains("● Claude 5h ██████ 93% 7d"), "{shown}");
        assert!(shown.ends_with("○ agy ██████ out"), "{shown}");
    }
}
