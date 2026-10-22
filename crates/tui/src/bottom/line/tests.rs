use std::time::Instant;

use super::*;
use crate::bottom::{ApprovalMode, BottomRow, ModeState};
use crate::model::VimMode;
use crate::render::Clock;
use crate::text::{plain_text, str_width};
use crate::theme::{ColorLevel, Glyphs, Theme};

/// Without color a bar draws its cells as `█` and `░`, so a row reads as text.
fn theme() -> Theme {
    color_theme().with_level(ColorLevel::None)
}

fn color_theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

// The bars of the two moments below, six cells each.
const B93: &str = "██████";
const B56: &str = "███░░░";
const B11: &str = "█░░░░░";
const B01: &str = "░░░░░░";
const B02: &str = "░░░░░░";
const OUT: &str = "██████";

fn cli(name: &str, windows: &[(&str, f64)]) -> CliUse {
    let mut cli = CliUse::new(name, Role::for_cli(name));
    cli.windows = windows
        .iter()
        .map(|(label, used)| WindowUse::new(*label, *used))
        .collect();
    cli
}

/// A healthy moment: Claude in use at 1% and 56%, Codex at 11%, agy at 2%.
fn healthy() -> Vec<CliUse> {
    let mut claude = cli("Claude", &[("5h", 0.01), ("7d", 0.56)]);
    claude.main = true;
    vec![
        claude,
        cli("Codex", &[("7d", 0.11)]),
        cli("agy", &[("5h", 0.02)]),
    ]
}

/// A pressed moment, drawn at five widths: Claude near its five-hour limit, Codex fine,
/// agy out until Tuesday.
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

fn line_with(clis: Vec<CliUse>) -> BottomLine {
    BottomLine {
        right: vec![LinePart::PlanUsage(clis)],
        ..BottomLine::default()
    }
}

fn auto() -> BottomRow {
    BottomRow {
        approval: ApprovalMode::Auto,
        ..BottomRow::default()
    }
}

fn row_of(mode: &BottomRow, line: &BottomLine, width: u16) -> Line<'static> {
    let theme = theme();
    let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
    let state = ModeState {
        line: Some(line),
        ..ModeState::default()
    };
    mode.line(&ctx, state)
}

fn row(line: &BottomLine, width: u16) -> String {
    plain_text(&[row_of(&auto(), line, width)])
}

/// Both sides as `width` columns lay them out, without the run of spaces between them, so a
/// width change that only moves the gap reads the same.
fn sides(line: &BottomLine, width: u16) -> String {
    let theme = theme();
    let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
    let state = ModeState {
        line: Some(line),
        ..ModeState::default()
    };
    let (pieces, notice) = auto().pieces(&ctx, state);
    let (left, right) = fitted(line, &pieces, notice, &ctx);
    let text = |spans: &[Span<'static>]| -> String {
        spans.iter().map(|span| span.content.as_ref()).collect()
    };
    format!("{} | {}", text(&left).trim(), text(&right))
}

/// What the line gives up, in order, one step at a time on the pressed moment. The hints are
/// three steps, since the line has three of them.
#[test]
fn the_line_gives_things_up_in_the_fixed_order() {
    let theme = theme();
    let ctx = RenderCtx::new(200, &theme, Clock::still(Instant::now()));
    let mode = BottomRow {
        approval: ApprovalMode::Auto,
        background_agents: 2,
        ..BottomRow::default()
    };
    let state = ModeState {
        panel: true,
        ..ModeState::default()
    };
    let (pieces, _) = mode.pieces(&ctx, state);
    let mut line = line_with(pressed());
    line.right.push(LinePart::Added(AddedPart::words(
        "spend",
        "~$1.86",
        Tone::Dim,
    )));
    line.left.push(LinePart::Added(AddedPart::words(
        "branch",
        "main",
        Tone::Dim,
    )));
    line.drop_first = vec!["spend".into(), "branch".into()];
    let drawer = Drawer {
        line: &line,
        mode: &pieces,
        kit: Kit::new(&theme),
    };
    let order = fit::order(&line);
    let steps: Vec<String> = (0..=order.len())
        .map(|given| {
            let (left, right) = drawer.sides(&Shape::after(&order, given));
            let text = |spans: &[Span<'static>]| -> String {
                spans.iter().map(|span| span.content.as_ref()).collect()
            };
            format!("{} | {}", text(&left).trim(), text(&right))
        })
        .collect();
    let expected = [
        // Everything, as a terminal wider than 150 columns shows it.
        format!(
            "⏵⏵ auto mode on (shift+tab to cycle) · ? for shortcuts · ←  2 agents · ↓  to manage · main | ● Claude 5h {B93} 93% resets 4:40pm 7d {B56} 56%  ○ Codex 7d {B11} 11%  ○ agy {OUT} out until Tue 3:45pm  ~$1.86"
        ),
        // 1. The hints: the footer's, the cycle hint, then the panel's.
        format!(
            "⏵⏵ auto mode on (shift+tab to cycle) · ←  2 agents · ↓  to manage · main | ● Claude 5h {B93} 93% resets 4:40pm 7d {B56} 56%  ○ Codex 7d {B11} 11%  ○ agy {OUT} out until Tue 3:45pm  ~$1.86"
        ),
        format!(
            "⏵⏵ auto mode on · ←  2 agents · ↓  to manage · main | ● Claude 5h {B93} 93% resets 4:40pm 7d {B56} 56%  ○ Codex 7d {B11} 11%  ○ agy {OUT} out until Tue 3:45pm  ~$1.86"
        ),
        format!(
            "⏵⏵ auto mode on · ←  2 agents · main | ● Claude 5h {B93} 93% resets 4:40pm 7d {B56} 56%  ○ Codex 7d {B11} 11%  ○ agy {OUT} out until Tue 3:45pm  ~$1.86"
        ),
        // 2. The parts you added, in your own drop order.
        format!(
            "⏵⏵ auto mode on · ←  2 agents · main | ● Claude 5h {B93} 93% resets 4:40pm 7d {B56} 56%  ○ Codex 7d {B11} 11%  ○ agy {OUT} out until Tue 3:45pm"
        ),
        format!(
            "⏵⏵ auto mode on · ←  2 agents | ● Claude 5h {B93} 93% resets 4:40pm 7d {B56} 56%  ○ Codex 7d {B11} 11%  ○ agy {OUT} out until Tue 3:45pm"
        ),
        // 3. Words shorten.
        format!(
            "⏵⏵ auto mode on · ←  2 agents | ● Claude 5h {B93} 93% 4:40pm 7d {B56} 56%  ○ Codex 7d {B11} 11%  ○ agy {OUT} out · Tue 3:45pm"
        ),
        // 4. A CLI that is fine: its bar becomes one cell.
        format!(
            "⏵⏵ auto mode on · ←  2 agents | ● Claude 5h {B93} 93% 4:40pm 7d {B56} 56%  ○ Codex ▁ 11%  ○ agy {OUT} out · Tue 3:45pm"
        ),
        // 5. A CLI that is out: its bar goes, its words stay.
        format!(
            "⏵⏵ auto mode on · ←  2 agents | ● Claude 5h {B93} 93% 4:40pm 7d {B56} 56%  ○ Codex ▁ 11%  ○ agy out · Tue 3:45pm"
        ),
        // 6. The mode: "auto mode on" to "auto".
        format!(
            "⏵⏵ auto · ←  2 agents | ● Claude 5h {B93} 93% 4:40pm 7d {B56} 56%  ○ Codex ▁ 11%  ○ agy out · Tue 3:45pm"
        ),
        // 7. The main CLI's second window becomes one cell.
        format!(
            "⏵⏵ auto · ←  2 agents | ● Claude 5h {B93} 93% 4:40pm 7d ▅ 56%  ○ Codex ▁ 11%  ○ agy out · Tue 3:45pm"
        ),
        // 8. "out · Tue 3:45pm" becomes "out".
        format!(
            "⏵⏵ auto · ←  2 agents | ● Claude 5h {B93} 93% 4:40pm 7d ▅ 56%  ○ Codex ▁ 11%  ○ agy out"
        ),
        // 9. A CLI that is fine keeps only its name.
        format!(
            "⏵⏵ auto · ←  2 agents | ● Claude 5h {B93} 93% 4:40pm 7d ▅ 56%  ○ Codex  ○ agy out"
        ),
        // 10. The main CLI's second window goes.
        format!("⏵⏵ auto · ←  2 agents | ● Claude 5h {B93} 93% 4:40pm  ○ Codex  ○ agy out"),
        // 11. A CLI that is fine becomes a dot in its color.
        format!("⏵⏵ auto · ←  2 agents | ● Claude 5h {B93} 93% 4:40pm  ○  ○ agy out"),
        // 12. The main CLI's bar goes, its number stays.
        "⏵⏵ auto · ←  2 agents | ● Claude 93% 4:40pm  ○  ○ agy out".to_owned(),
        // 13. The mode becomes its sign alone.
        "⏵⏵ · ←  2 agents | ● Claude 93% 4:40pm  ○  ○ agy out".to_owned(),
        // Past the thirteen, so that no CLI is cut off on a terminal narrower still: the
        // arrow's count shortens, then the reset time, the names and the gaps go.
        "⏵⏵ · ←  2 | ● Claude 93% 4:40pm  ○  ○ agy out".to_owned(),
        "⏵⏵ · ←  2 | ● Claude 93%  ○  ○ agy out".to_owned(),
        "⏵⏵ · ←  2 | ● 93%  ○  ○ out".to_owned(),
        "⏵⏵ · ←  2 | ● 93% ○ ○ out".to_owned(),
    ];
    assert_eq!(steps.len(), expected.len());
    for (step, (got, want)) in steps.iter().zip(&expected).enumerate() {
        assert_eq!(got, want, "after {step} steps");
    }
}

/// The pressed moment at five widths. The mode sign is Claude Code's two-cell `⏵⏵` after two
/// columns, so at 100 the sides sit one column apart.
#[test]
fn the_pressed_moment_at_five_widths() {
    let line = line_with(pressed());
    assert_eq!(
        sides(&line, 120),
        format!(
            "⏵⏵ auto mode on | ● Claude 5h {B93} 93% 4:40pm 7d {B56} 56%  ○ Codex 7d {B11} 11%  ○ agy {OUT} out · Tue 3:45pm"
        )
    );
    assert_eq!(
        sides(&line, 100),
        format!(
            "⏵⏵ auto mode on | ● Claude 5h {B93} 93% 4:40pm 7d {B56} 56%  ○ Codex ▁ 11%  ○ agy out · Tue 3:45pm"
        )
    );
    assert_eq!(str_width(&row(&line, 100)), 100);
    assert_eq!(
        sides(&line, 80),
        format!("⏵⏵ auto | ● Claude 5h {B93} 93% 4:40pm 7d ▅ 56%  ○ Codex ▁ 11%  ○ agy out")
    );
    assert_eq!(
        sides(&line, 60),
        format!("⏵⏵ auto | ● Claude 5h {B93} 93% 4:40pm  ○ Codex  ○ agy out")
    );
    assert_eq!(sides(&line, 40), "⏵⏵ | ● Claude 93% 4:40pm  ○  ○ agy out");
}

/// Wider than 150 columns the line shows everything, the hint included, and a little wider
/// the mode's other hint too.
#[test]
fn a_wide_terminal_shows_everything() {
    let wide = row(&line_with(pressed()), 151);
    assert!(
        wide.starts_with("  ⏵⏵ auto mode on (shift+tab to cycle)  "),
        "{wide:?}"
    );
    assert!(wide.contains("93% resets 4:40pm"), "{wide:?}");
    assert!(wide.ends_with("out until Tue 3:45pm"), "{wide:?}");
    assert_eq!(
        str_width(&wide),
        151,
        "the chips end at the terminal's edge"
    );
    let wider = row(&line_with(pressed()), 170);
    assert!(
        wider.starts_with("  ⏵⏵ auto mode on (shift+tab to cycle) · ? for shortcuts  "),
        "{wider:?}"
    );
}

/// Every CLI in the crew is present in some form at every width, the row never spills, and
/// what never goes is there: the main CLI's fuller window and any CLI at 90% or out.
#[test]
fn no_cli_ever_leaves_the_line() {
    // Every form but text gives each CLI a dot. Text gives one to the CLI in use, and to
    // another only once its name is gone.
    let present = |form: LineForm, row: &str| {
        let hollow = row.matches('○').count();
        let named = ["Codex", "agy"]
            .iter()
            .filter(|name| row.contains(**name))
            .count();
        let others = if form == LineForm::Text {
            hollow + named
        } else {
            hollow
        };
        row.matches('●').count() == 1 && others == 2
    };
    for (clis, kept) in [(healthy(), vec!["56%"]), (pressed(), vec!["93%", "out"])] {
        for form in LineForm::ALL {
            let line = BottomLine {
                form,
                ..line_with(clis.clone())
            };
            // A gauge's cell stands for the number of a window that is fine.
            let kept: Vec<&str> = kept
                .iter()
                .copied()
                .filter(|word| form != LineForm::Gauges || *word != "56%")
                .collect();
            for width in 20..=200u16 {
                let row = row(&line, width);
                assert!(
                    str_width(&row) <= usize::from(width),
                    "{form:?} {width}: {row:?}"
                );
                assert!(present(form, &row), "{form:?} at {width}: {row:?}");
                assert!(!row.contains('…'), "{form:?} at {width} was cut: {row:?}");
                for word in &kept {
                    assert!(
                        row.contains(word),
                        "{form:?} at {width} lost {word}: {row:?}"
                    );
                }
            }
        }
    }
}

/// The vim mode and the arrow's count shorten, and past that the left side is cut, so a CLI
/// that is out or at 90% stays on the line beside them too.
#[test]
fn no_cli_leaves_the_line_beside_vim_agents_or_needs_you() {
    let modes = [
        BottomRow {
            background_agents: 2,
            ..auto()
        },
        BottomRow {
            needs_you: 1,
            ..auto()
        },
        BottomRow {
            vim: Some(VimMode::Insert),
            background_agents: 2,
            ..auto()
        },
    ];
    for mode in &modes {
        for form in LineForm::ALL {
            let line = BottomLine {
                form,
                ..line_with(pressed())
            };
            for width in 24..=200u16 {
                let row = plain_text(&[row_of(mode, &line, width)]);
                assert!(str_width(&row) <= usize::from(width), "{width}: {row:?}");
                for word in ["93%", "out"] {
                    assert!(
                        row.contains(word),
                        "{form:?} at {width} lost {word}: {row:?}"
                    );
                }
            }
        }
    }
    let narrow = plain_text(&[row_of(&modes[2], &line_with(pressed()), 40)]);
    // Claude Code keeps the vim label whole however narrow the line.
    assert!(narrow.contains("-- INSERT --"), "{narrow:?}");
}

#[test]
fn each_form_draws_as_the_proposal_has_it() {
    let at = |form, clis: Vec<CliUse>| {
        let line = BottomLine {
            form,
            ..line_with(clis)
        };
        sides(&line, 150)
    };
    assert_eq!(
        at(LineForm::Bars, healthy()),
        format!(
            "⏵⏵ auto mode on (shift+tab to cycle) · ? for shortcuts | ● Claude 5h {B01} 1% 7d {B56} 56%  ○ Codex 7d {B11} 11%  ○ agy 5h {B02} 2%"
        )
    );
    assert_eq!(
        at(LineForm::Compact, healthy()),
        format!(
            "⏵⏵ auto mode on (shift+tab to cycle) · ? for shortcuts | ● Claude {B56} 56%  ○ Codex {B11} 11%  ○ agy {B02} 2%"
        )
    );
    assert_eq!(
        at(LineForm::Gauges, healthy()),
        "⏵⏵ auto mode on (shift+tab to cycle) · ? for shortcuts | ● Claude ▁▅  ○ Codex ▁  ○ agy ▁"
    );
    assert_eq!(
        at(LineForm::Text, healthy()),
        "⏵⏵ auto mode on (shift+tab to cycle) · ? for shortcuts | ● Claude 5h 1% · 7d 56% used  Codex 7d 11% used  agy 5h 2% used"
    );
    assert_eq!(
        at(LineForm::Text, pressed()),
        "⏵⏵ auto mode on (shift+tab to cycle) · ? for shortcuts | ● Claude 5h 93% · 7d 56% used · resets 4:40pm  Codex 7d 11% used  agy out until Tue 3:45pm"
    );
    assert_eq!(
        at(LineForm::Gauges, pressed()),
        "⏵⏵ auto mode on (shift+tab to cycle) · ? for shortcuts | ● Claude █▅ 93% resets 4:40pm  ○ Codex ▁  ○ agy out until Tue 3:45pm"
    );
}

/// One color per CLI, the CLI in use bold with a filled dot, amber from 90% and red when out.
#[test]
fn the_chips_carry_their_colors() {
    let theme = color_theme();
    let kit = Kit::new(&theme);
    let line = line_with(pressed());
    let ctx = RenderCtx::new(120, &theme, Clock::still(Instant::now()));
    let state = ModeState {
        line: Some(&line),
        ..ModeState::default()
    };
    let drawn = auto().line(&ctx, state);
    let style = |text: &str| {
        let span = drawn.spans.iter().find(|span| span.content == text);
        span.unwrap_or_else(|| panic!("no {text:?} in {drawn:?}"))
            .style
    };
    assert_eq!(style("●"), kit.fg(Role::CliClaude));
    assert_eq!(style("Claude"), kit.bold(Role::Bright));
    assert_eq!(style("Codex"), theme.style(Role::Dim));
    assert_eq!(style("93%"), kit.bold(Role::Warn));
    assert_eq!(style("4:40pm"), theme.style(Role::Warn));
    assert_eq!(style("56%"), kit.bold(Role::Bright));
    assert_eq!(style("11%"), theme.style(Role::Dim));
    assert_eq!(style("out · Tue 3:45pm"), theme.style(Role::Danger));
    let dots: Vec<_> = drawn
        .spans
        .iter()
        .filter(|span| span.content == "○")
        .map(|span| span.style)
        .collect();
    assert_eq!(dots, [kit.fg(Role::CliCodex), kit.fg(Role::CliAgy)]);
    // The near window's bar is amber, the other is Claude's own color, agy's is red.
    let fills: Vec<_> = drawn
        .spans
        .iter()
        .filter(|span| span.content.starts_with('▄'))
        .filter_map(|span| span.style.fg)
        .collect();
    let color = |role| theme.color(role).unwrap();
    assert!(fills.contains(&color(Role::Warn)), "{fills:?}");
    assert!(fills.contains(&color(Role::CliClaude)), "{fills:?}");
    assert!(fills.contains(&color(Role::Danger)), "{fills:?}");
}

#[test]
fn added_parts_sit_on_their_side_and_leave_in_your_order() {
    let mut line = line_with(healthy());
    line.left.push(LinePart::Added(AddedPart {
        name: "artifact".into(),
        body: PartBody::Artifact {
            name: "weekly-usage-chart".into(),
            others: 2,
        },
    }));
    line.left.push(LinePart::Added(AddedPart::words(
        "branch",
        "main",
        Tone::Dim,
    )));
    line.right.insert(
        0,
        LinePart::Added(AddedPart {
            name: "context".into(),
            body: PartBody::Meter {
                label: "context".into(),
                label_role: Role::Dim,
                ratio: 0.21,
                value: "21%".into(),
                fill: Role::BarFill,
            },
        }),
    );
    line.right.push(LinePart::Added(AddedPart {
        name: "spend".into(),
        body: PartBody::Spark {
            text: "~$1.86".into(),
            values: vec![0.2, 0.3, 0.05, 0.5, 0.8, 0.3, 0.7],
            role: Role::CliClaude,
        },
    }));
    line.drop_first = vec!["spend".into(), "branch".into()];
    let chips =
        format!("● Claude 5h {B01} 1% 7d {B56} 56%  ○ Codex 7d {B11} 11%  ○ agy 5h {B02} 2%");
    assert_eq!(
        sides(&line, 230),
        format!(
            "⏵⏵ auto mode on (shift+tab to cycle) · ? for shortcuts · ⧉ weekly-usage-chart +2 more · main | context █░░░░░ 21%  {chips}  ~$1.86 ▂▃▁▅█▃▇"
        )
    );
    // Spend first, then the branch, then the rest from the last on the line back.
    let narrower = |width| sides(&line, width);
    assert_eq!(
        narrower(154),
        format!(
            "⏵⏵ auto mode on · ⧉ weekly-usage-chart +2 more · main | context █░░░░░ 21%  {chips}"
        )
    );
    assert_eq!(
        narrower(147),
        format!("⏵⏵ auto mode on · ⧉ weekly-usage-chart +2 more | context █░░░░░ 21%  {chips}")
    );
    assert_eq!(
        narrower(127),
        format!("⏵⏵ auto mode on · ⧉ weekly-usage-chart +2 more | {chips}")
    );
    assert_eq!(narrower(97), format!("⏵⏵ auto mode on | {chips}"));
}

fn artifact_part(name: &str, others: u32) -> BottomLine {
    let mut line = BottomLine::default();
    line.left.push(LinePart::Added(AddedPart {
        name: "artifact".into(),
        body: PartBody::Artifact {
            name: name.into(),
            others,
        },
    }));
    line
}

/// `⧉` and the newest artifact's name cut at 24 cells as Claude Code 2.1.292 draws it, then
/// how many more the conversation published, and no count of comments. Both are said in words
/// where the mark can't be drawn.
#[test]
fn an_artifact_reads_as_its_name_and_how_many_more() {
    let drawn = |name: &str, others| row(&artifact_part(name, others), 100);
    let one = drawn("weekly-usage-chart", 0);
    assert!(
        one.ends_with("for shortcuts · ⧉ weekly-usage-chart"),
        "{one}"
    );
    let long = drawn("weekly-usage-chart-with-every-picture", 0);
    assert!(long.ends_with("· ⧉ weekly-usage-chart-with…"), "{long}");
    let three = drawn("monthly-usage-page", 2);
    assert!(
        three.ends_with("for shortcuts · ⧉ monthly-usage-page +2 more"),
        "{three}"
    );
    let long = drawn("weekly-usage-chart-with-every-picture", 1);
    assert!(
        long.ends_with("· ⧉ weekly-usage-chart-with… +1 more"),
        "{long}"
    );
    let unnamed = row(&artifact_part("  ", 3), 100);
    assert!(unnamed.ends_with("? for shortcuts"), "{unnamed}");

    let mut ascii = theme();
    ascii.glyphs = Glyphs::ASCII;
    let ctx = RenderCtx::new(100, &ascii, Clock::still(Instant::now()));
    let spoken = |line: &BottomLine| {
        let state = ModeState {
            line: Some(line),
            ..ModeState::default()
        };
        plain_text(&[auto().line(&ctx, state)])
    };
    let one = spoken(&artifact_part("weekly-usage-chart", 0));
    assert!(one.ends_with("artifact weekly-usage-chart"), "{one}");
    let three = spoken(&artifact_part("monthly-usage-page", 2));
    assert!(
        three.ends_with("artifact monthly-usage-page +2 more"),
        "{three}"
    );
}

/// In color a bar is the lower half of its six cells, its edge to half a cell, on the
/// track's color. Without color or block glyphs it is the kit's meter.
#[test]
fn a_bar_is_half_a_cell_tall_with_its_edge_to_half_a_cell() {
    let theme = color_theme();
    let kit = Kit::new(&theme);
    let text = |spans: &[Span<'static>]| -> String {
        spans.iter().map(|span| span.content.as_ref()).collect()
    };
    let bar = |share| chip::small_bar(&kit, share, Role::CliClaude);
    let (fill, track) = (kit.fg(Role::CliClaude), kit.fg(Role::BarTrack));

    let half = bar(0.56);
    assert_eq!(text(&half), "▄▄▄▖▄▄");
    assert_eq!(half[0].style, fill);
    assert_eq!((half[1].content.as_ref(), half[1].style), ("▄▄", track));
    assert_eq!(text(&bar(0.93)), "▄▄▄▄▄▖");
    assert_eq!(bar(0.01), [Span::styled("▄▄▄▄▄▄", track)]);
    assert_eq!(bar(1.0), [Span::styled("▄▄▄▄▄▄", fill)]);
    for share in [0.0, 0.11, 0.5, 0.93, 1.0] {
        assert_eq!(spans_width(&bar(share)), 6, "{share}");
    }

    let plain = self::theme();
    assert_eq!(
        text(&chip::small_bar(&Kit::new(&plain), 0.56, Role::CliClaude)),
        B56
    );
    let mut ascii = color_theme();
    ascii.glyphs = Glyphs::ASCII;
    let blocks = chip::small_bar(&Kit::new(&ascii), 0.56, Role::CliClaude);
    assert_eq!(text(&blocks), "      ");
    assert!(blocks.iter().all(|span| span.style.bg.is_some()));
}

/// A running workflow's name is cut where an artifact's is, and keeps its own color beside
/// its bar. What other code drew for a part goes on the line as it is.
#[test]
fn a_named_meter_and_a_part_drawn_elsewhere() {
    let theme = color_theme();
    let kit = Kit::new(&theme);
    let mut line = BottomLine::default();
    line.right.push(LinePart::Added(AddedPart {
        name: "run".into(),
        body: PartBody::Meter {
            label: "add-due-dates-to-every-task-row".into(),
            label_role: Role::BarFill,
            ratio: 0.6,
            value: "3/5".into(),
            fill: Role::BarFill,
        },
    }));
    let chip = vec![kit.dim("checks "), kit.span("7 of 9 passed", Role::Warn)];
    line.right.push(LinePart::Added(AddedPart {
        name: "checks".into(),
        body: PartBody::Drawn(chip.clone()),
    }));
    let ctx = RenderCtx::new(120, &theme, Clock::still(Instant::now()));
    let state = ModeState {
        line: Some(&line),
        ..ModeState::default()
    };
    let drawn = auto().line(&ctx, state);
    assert!(
        plain_text(std::slice::from_ref(&drawn))
            .ends_with("add-due-dates-to-every-… ▄▄▄▖▄▄ 3/5  checks 7 of 9 passed"),
        "{drawn:?}"
    );
    let name = drawn
        .spans
        .iter()
        .find(|span| span.content.starts_with("add-due"));
    assert_eq!(name.map(|span| span.style), Some(kit.fg(Role::BarFill)));
    assert!(chip.iter().all(|span| drawn.spans.contains(span)));
    // A part drawn elsewhere leaves a narrow line whole, like any part you added.
    let narrow = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
    let state = ModeState {
        line: Some(&line),
        ..ModeState::default()
    };
    let text = plain_text(&[auto().line(&narrow, state)]);
    assert!(
        !text.contains("checks") && !text.contains("passed"),
        "{text}"
    );
}

/// A CLI nothing was read for still has its dot and name, and states that need you keep
/// their words at any width.
#[test]
fn unread_down_and_wrapping_up_clis_stay_in_words() {
    let mut claude = cli("Claude", &[]);
    claude.main = true;
    let mut codex = cli("Codex", &[]);
    codex.state = CliState::Down;
    let mut agy = cli("agy", &[]);
    agy.state = CliState::WrappingUp;
    let mut openrouter = cli("OpenRouter", &[]);
    openrouter.note = Some("$0.06".into());
    let line = line_with(vec![claude, codex, agy, openrouter]);
    assert!(
        row(&line, 120).ends_with("● Claude  ○ Codex down  ○ agy wrapping up  ○ OpenRouter $0.06"),
        "{}",
        row(&line, 120)
    );
    assert_eq!(
        sides(&line, 56),
        "⏵⏵ auto | ● Claude  ○ Codex down  ○ agy wrapping up  ○"
    );
}

/// `as = "left"` turns every number and bar round, and each number says so.
#[test]
fn numbers_can_say_what_is_left() {
    let line = BottomLine {
        as_left: true,
        form: LineForm::Text,
        ..line_with(healthy())
    };
    assert!(
        row(&line, 140)
            .ends_with("● Claude 5h 99% · 7d 44% left  Codex 7d 89% left  agy 5h 98% left"),
        "{}",
        row(&line, 140)
    );
    let bars = BottomLine {
        form: LineForm::Fit,
        ..line.clone()
    };
    assert!(
        row(&bars, 140).ends_with(
            "● Claude 5h ██████ 99% left 7d ███░░░ 44% left  ○ Codex 7d █████░ 89% left  ○ agy 5h ██████ 98% left"
        ),
        "{}",
        row(&bars, 140)
    );
    // However narrow, a number that is left never reads as one that is used.
    for width in [100, 80, 60, 40] {
        let row = row(&bars, width);
        assert!(row.contains("% left"), "{row}");
        assert_eq!(
            row.matches('%').count(),
            row.matches("% left").count(),
            "{row}"
        );
    }
}

/// A reading that can't be read again says how old it is in every form.
#[test]
fn every_form_says_how_old_a_reading_is() {
    let mut clis = healthy();
    for window in &mut clis[1].windows {
        window.age = Some("2d".into());
    }
    for form in [
        LineForm::Text,
        LineForm::Gauges,
        LineForm::Fit,
        LineForm::Compact,
    ] {
        let line = BottomLine {
            form,
            ..line_with(clis.clone())
        };
        let wide = row(&line, 200);
        assert!(wide.contains("2d ago"), "{form:?}: {wide}");
    }
}

#[test]
fn mode_can_sit_on_the_right() {
    let line = BottomLine {
        left: vec![LinePart::Added(AddedPart::words(
            "branch",
            "main",
            Tone::Dim,
        ))],
        right: vec![LinePart::Mode],
        ..BottomLine::default()
    };
    let row = row(&line, 40);
    assert!(row.starts_with("  main  "), "{row:?}");
    assert!(row.ends_with("⏵⏵ auto mode on"), "{row:?}");
}

/// The context part names whose context it shows, as its pictures draw it at the right end
/// of the line: the percent in the terminal's own colour while the session is at ease, and
/// red once it is close to starting afresh.
#[test]
fn the_context_part_matches_its_pictures() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    use crate::kit::Level;
    use crate::overview::golden::{Picture, Region, assert_matches, theme};

    let picture = Picture::load("context_part");
    for (row, percent, level) in [(0, 20, Level::Ok), (1, 95, Level::Danger)] {
        let theme = theme();
        let mut line = BottomLine::default();
        line.right.push(LinePart::Added(AddedPart {
            name: "context".into(),
            body: PartBody::Fill {
                label: "context".into(),
                who: "build:due-flag".into(),
                who_role: Role::Label,
                percent,
                level,
            },
        }));
        // The rows under the input keep the screen's last column free, so a screen 118 cells
        // wide draws them in 117.
        let ctx = RenderCtx::new(117, &theme, Clock::still(Instant::now()));
        let state = ModeState {
            line: Some(&line),
            ..ModeState::default()
        };
        let drawn = auto().line(&ctx, state);
        let mut buf = Buffer::empty(Rect::new(0, 0, 117, 1));
        buf.set_line(0, 0, &drawn, 117);
        // The part's 28 cells, flush right.
        let part = Region {
            rows: row..row + 1,
            columns: 89..117,
            at: (0, 0),
        };
        assert_matches(&picture, &buf, &part, &theme);
    }
}
