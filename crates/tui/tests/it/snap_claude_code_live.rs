//! What Claude Code draws under the transcript: You should know above the input, a message
//! waiting with the send-now hint, and what a mod pushes, at 80, 100, and 120
//! columns, with the frame check at every width.

use crate::common;
use argonauts_core::screen::{Drawn, DrawnButton, DrawnLook, DrawnRow, DrawnSpan};
use argonauts_tui::bottom::Notice;
use argonauts_tui::model::{
    AddedPart, ApprovalMode, BottomState, CliUse, LinePart, ModeNotice, QuestionOption,
    QuestionPrompt, Tone, WindowUse, WorkingLine,
};
use argonauts_tui::stability;
use argonauts_tui::theme::Role;
use common::{assert_fits, at, ctx, fg, style_of, t0, text, theme};
use ratatui::style::Modifier;

const WIDTHS: [u16; 3] = [80, 100, 120];

/// 38 seconds into the turn.
const NOW: f64 = 38.0;

const RELEASE: &str =
    "Release 1.4.0 requires Dana's security sign-off, and Dana is away until Monday.";

fn captain() -> BottomState {
    let mut bottom = BottomState::new();
    bottom.composer.label = Some("captain".into());
    bottom.mode.approval = ApprovalMode::Auto;
    let mut working = WorkingLine::new("Rowing", t0());
    working.tokens_down = 2_900;
    bottom.working = Some(working);
    let mut claude = CliUse::new("Claude", Role::CliClaude);
    claude.main = true;
    claude.windows = vec![WindowUse::new("5h", 0.53)];
    bottom.line.right = vec![LinePart::PlanUsage(vec![claude])];
    bottom
}

/// A Claude Code mod's status text, as the sync puts it on the line: last, and first to go.
fn push_plugin_status(bottom: &mut BottomState, text: &str) {
    let part = AddedPart::words("pr-tools", text, Tone::Dim);
    bottom.line.right.push(LinePart::Added(part));
    bottom.line.drop_first.insert(0, "pr-tools".into());
}

fn notice(line: &str) -> Notice {
    let span = |text: &str, look| DrawnSpan {
        text: text.to_owned(),
        look,
    };
    let button = |label: &str, handle| DrawnButton {
        label: label.to_owned(),
        plugin: "cc-plugin-you-should-know".to_owned(),
        handle,
    };
    // As 2.1.289 draws it: the star and the tag in `subtle`, then the line.
    Notice::new(Drawn {
        rows: vec![DrawnRow {
            spans: vec![
                span("✦", DrawnLook::Dim),
                span(" Heads up · ", DrawnLook::Dim),
                span(line, DrawnLook::Plain),
            ],
        }],
        buttons: vec![
            button("Learn more", 725_020_656),
            button("Knew this already", 725_020_657),
            button("Dismiss", 725_020_658),
        ],
        left_out: Vec::new(),
    })
}

fn snapshot_widths(name: &str, bottom: &BottomState) {
    let theme = theme();
    for width in stability::WIDTHS {
        stability::assert_lines(&bottom.render(&ctx(&theme, width, NOW)).lines, width);
    }
    for width in WIDTHS {
        let view = bottom.render(&ctx(&theme, width, NOW));
        assert_fits(&view.lines, width);
        insta::assert_snapshot!(format!("{name}_{width}"), text(&view.lines));
    }
}

#[test]
fn you_should_know_above_the_input() {
    let mut bottom = captain();
    bottom.notice = notice(RELEASE);
    snapshot_widths("notice", &bottom);
    let theme = theme();
    let view = bottom.render(&ctx(&theme, 100, NOW));
    let all = text(&view.lines);
    assert!(
        all.find("✦ Heads up").unwrap() < all.find("captain ─").unwrap(),
        "it sits right above the input:\n{all}"
    );
    assert!(all.contains("  1 Learn more   2 Knew this already   0 Dismiss"));
    assert_eq!(
        style_of(&view.lines, "✦").and_then(|s| s.fg),
        fg(Role::Question)
    );
    assert_eq!(
        style_of(&view.lines, "Heads up").and_then(|s| s.fg),
        fg(Role::Dim)
    );
}

#[test]
fn a_long_notice_wraps_under_its_star() {
    let mut bottom = captain();
    let long = format!("{RELEASE} The changelog also still lists 1.3.9 as the latest.");
    bottom.notice = notice(&long);
    snapshot_widths("notice_wrapped", &bottom);
    let theme = theme();
    let all = text(&bottom.render(&ctx(&theme, 80, NOW)).lines);
    let star = all.lines().position(|row| row.starts_with("✦")).unwrap();
    let next = all.lines().nth(star + 1).unwrap();
    assert!(next.starts_with("  ") && !next.trim().is_empty(), "{all}");
}

#[test]
fn the_notice_with_the_keys() {
    let mut bottom = captain();
    bottom.notice = notice(RELEASE);
    assert!(bottom.notice.focus());
    snapshot_widths("notice_focused", &bottom);
    let theme = theme();
    let view = bottom.render(&ctx(&theme, 100, NOW));
    let all = text(&view.lines);
    assert!(all.contains("←/→ to choose · enter to press · esc for the input"));
    let pressed = style_of(&view.lines, "1 Learn more").unwrap();
    assert!(pressed.add_modifier.contains(Modifier::REVERSED));
}

#[test]
fn a_question_with_the_keys_hides_the_notice() {
    let mut bottom = captain();
    bottom.notice = notice(RELEASE);
    let mut question = QuestionPrompt::new(
        3,
        "Allow Bash(./slow-test.sh)?",
        "Allow Bash?",
        vec![QuestionOption::new("Yes"), QuestionOption::new("No")],
    );
    question.focus();
    bottom.question = Some(question);
    snapshot_widths("notice_behind_a_question", &bottom);
    let theme = theme();
    let all = text(&bottom.render(&ctx(&theme, 100, NOW)).lines);
    assert!(!all.contains("Heads up"), "{all}");
}

#[test]
fn a_message_waits_with_the_send_now_hint() {
    let mut bottom = captain();
    bottom.queued = vec!["Also check whether the changelog mentions the new --export flag.".into()];
    bottom.send_now = true;
    bottom.send_now_hint = Some("ctrl+enter to send now".into());
    snapshot_widths("send_now", &bottom);
    let theme = theme();
    let view = bottom.render(&ctx(&theme, 100, NOW));
    let all = text(&view.lines);
    let message = all.find("❯ Also check").unwrap();
    let hint = all.find("  ctrl+enter to send now").unwrap();
    assert!(
        message < hint && hint < all.find("captain ─").unwrap(),
        "{all}"
    );
    assert_eq!(
        style_of(&view.lines, "ctrl+enter").and_then(|s| s.fg),
        fg(Role::Dim)
    );
}

#[test]
fn what_a_plugin_pushes_on_the_notice_and_status_lines() {
    let mut bottom = captain();
    bottom.working = None;
    push_plugin_status(&mut bottom, "PR #482 · 2 reviews waiting");
    bottom.mode.notice = Some(ModeNotice {
        text: "pr-tools: 檢查清單已載入".into(),
        tone: Tone::Dim,
        until: at(NOW + 4.0),
    });
    snapshot_widths("plugin_output_pushes", &bottom);
    let theme = theme();
    let all = text(&bottom.render(&ctx(&theme, 120, NOW)).lines);
    assert!(all.contains("pr-tools: 檢查清單已載入"), "{all}");
}

#[test]
fn a_plugins_status_line_sits_after_the_chips_and_gives_way_first() {
    let mut bottom = captain();
    push_plugin_status(&mut bottom, "PR #482 · 2 reviews waiting");
    snapshot_widths("plugin_status", &bottom);
    let theme = theme();
    let wide = text(&bottom.render(&ctx(&theme, 120, NOW)).lines);
    let chip = wide.find("● Claude 5h").unwrap();
    let status = wide.find("PR #482").expect("room for it at 120");
    assert!(chip < status, "{wide}");
    let narrow = text(&bottom.render(&ctx(&theme, 60, NOW)).lines);
    assert!(
        !narrow.contains("PR #482") && narrow.contains("● Claude 5h"),
        "{narrow}"
    );
}
