//! A draft that starts with `/`: a real command stands out and says what Enter runs, and
//! what isn't a command says so, sends nothing, and keeps the draft for you to fix.

use std::time::Instant;

use argonauts_core::agents::cli_commands::CliCommandSet;
use argonauts_core::base::names::AgentId;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;

use super::super::*;
use crate::base::draw::render::Clock;
use crate::base::text::plain_text;
use crate::base::theme::{Glyphs, Theme};
use crate::cells::cell::Tone;
use crate::testing::pictures::picture_check::assert_rows_match;

const WIDTH: u16 = 118;

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

/// The input once the main agent CLI's commands have arrived, holding `draft`.
fn typed(draft: &str) -> BottomState {
    let mut state = BottomState::new();
    state.composer.cli_commands = Some(CliCommandSet::new(
        AgentId("claude-code".into()),
        "Claude Code",
        &[],
        Vec::new(),
    ));
    state.composer.set_text(draft);
    state
}

fn drawn(state: &BottomState) -> Vec<Line<'static>> {
    let theme = theme();
    let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
    state.render(&ctx).lines
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// The rows the picture draws for one input: its two rules, the draft between them, and the
/// bottom line.
fn input_rows(draft: &str) -> Vec<Line<'static>> {
    let lines = drawn(&typed(draft));
    let rule = lines
        .iter()
        .position(|line| plain_text(std::slice::from_ref(line)).starts_with('─'))
        .expect("the input's top rule");
    lines[rule..rule + 4].to_vec()
}

/// The picture's two inputs, each under its caption: a real command, then one that isn't. Its
/// last part draws the other choice, a chip behind the command, which wasn't built.
#[test]
fn slash_commands_a_real_one_is_bold_and_an_unknown_one_says_so() {
    let picture = (
        include_str!("../../../tests/golden/bottom/slash_commands.txt"),
        include_str!("../../../tests/golden/bottom/slash_commands.html"),
    );
    for (draft, rows) in [
        ("/workflow try spec-first ", 1..5),
        ("/wokflow try spec-first add a burst limit", 7..11),
    ] {
        assert_rows_match(
            "slash_commands",
            &input_rows(draft),
            picture,
            rows,
            &theme(),
            &[],
        );
    }
}

/// On a screen too narrow for the whole note after the draft, the row keeps why Enter does
/// nothing and the bottom line names the fix, so neither is cut.
#[test]
fn a_narrow_screen_names_the_fix_on_the_bottom_line() {
    let mut state = typed("/wokflow try spec-first add a burst limit");
    let theme = theme();
    let at = |state: &BottomState, width: u16| {
        let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
        plain_text(&state.render(&ctx).lines)
    };
    let narrow = at(&state, 80);
    let rows: Vec<&str> = narrow.lines().map(str::trim_end).collect();
    assert!(
        rows.contains(&"❯ /wokflow try spec-first add a burst limit   isn't a command"),
        "{narrow}"
    );
    assert!(
        rows.contains(&"  enter sends nothing · did you mean /workflow? · tab fixes it"),
        "{narrow}"
    );
    assert!(!narrow.contains('…'), "{narrow}");

    // With room for all of it the note stays whole after the draft.
    let wide = at(&state, WIDTH);
    assert!(
        wide.contains("isn't a command · did you mean /workflow? · tab fixes it"),
        "{wide}"
    );
    assert!(
        wide.lines()
            .any(|row| row.trim_end() == "  enter sends nothing"),
        "{wide}"
    );

    // Nothing near it: there is no fix to move.
    state
        .composer
        .set_text("/zzzzzz and a long row of words that reaches the far edge of it");
    let narrow = at(&state, 80);
    assert!(
        narrow
            .lines()
            .any(|row| row.trim_end() == "  enter sends nothing"),
        "{narrow}"
    );
}

#[test]
fn enter_on_what_is_no_command_sends_nothing_and_tab_fixes_it() {
    let mut state = typed("/wokflow try spec-first add a burst limit");
    let before = state.composer.text().to_owned();
    assert_eq!(
        state.composer.handle_key(key(KeyCode::Enter)),
        ComposerEvent::Moved
    );
    assert_eq!(state.composer.text(), before, "the draft stays as it was");

    assert_eq!(
        state.composer.handle_key(key(KeyCode::Tab)),
        ComposerEvent::Edited
    );
    assert_eq!(
        state.composer.text(),
        "/workflow try spec-first add a burst limit"
    );
    assert_eq!(
        state.composer.cursor(),
        state.composer.text().len(),
        "the cursor stays at the end of what you typed"
    );
    let sent = state.composer.handle_key(key(KeyCode::Enter));
    assert!(
        matches!(&sent, ComposerEvent::Submitted { text, .. } if text.starts_with("/workflow try")),
        "{sent:?}"
    );
}

#[test]
fn a_draft_with_its_pictures_stays_whole_when_enter_sends_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shot.png");
    std::fs::write(&path, [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]).unwrap();
    let mut state = typed("/wokflow ");
    state.composer.handle_paste(&path.display().to_string());
    let before = state.composer.text().to_owned();
    assert!(before.contains("[Image #1]"), "{before}");
    assert_eq!(
        state.composer.handle_key(key(KeyCode::Enter)),
        ComposerEvent::Moved
    );
    assert_eq!(state.composer.text(), before);
    assert!(
        state.composer.take_sent_pictures().is_empty(),
        "nothing was sent, so no picture left with it"
    );
}

#[test]
fn a_prompt_that_starts_with_a_path_is_sent_as_text() {
    let mut state = typed("/Users/me/shots/menu.png why is the second row cut off?");
    let rows = plain_text(&drawn(&state));
    assert!(!rows.contains("isn't a command"), "{rows}");
    assert!(!rows.contains("enter runs"), "{rows}");
    let sent = state.composer.handle_key(key(KeyCode::Enter));
    assert!(matches!(sent, ComposerEvent::Submitted { .. }), "{sent:?}");
}

/// One word after the slash reads like a command's name, but a folder or file of that name
/// at the top of the disk makes it a path, and the prompt is sent.
#[test]
fn a_prompt_that_starts_with_a_one_word_path_is_sent_as_text() {
    for draft in ["/tmp is full, clean it", "/tmp", "/dev holds what?"] {
        let mut state = typed(draft);
        assert_eq!(state.composer.command(), None, "{draft}");
        let rows = plain_text(&drawn(&state));
        assert!(!rows.contains("isn't a command"), "{rows}");
        assert!(!rows.contains("enter sends nothing"), "{rows}");
        let sent = state.composer.handle_key(key(KeyCode::Enter));
        assert!(matches!(sent, ComposerEvent::Submitted { .. }), "{sent:?}");
    }
    // A name that is nothing on the disk either is still no command.
    let mut state = typed("/no-such-folder-here is full");
    assert!(plain_text(&drawn(&state)).contains("isn't a command"));
    assert_eq!(
        state.composer.handle_key(key(KeyCode::Enter)),
        ComposerEvent::Moved
    );
}

#[test]
fn a_name_is_not_judged_before_the_cli_s_commands_arrive_or_while_it_is_being_typed() {
    let mut early = BottomState::new();
    early.composer.set_text("/pr-tools:checklist the parser");
    assert_eq!(early.composer.command(), None);
    assert!(matches!(
        early.composer.handle_key(key(KeyCode::Enter)),
        ComposerEvent::Submitted { .. }
    ));

    let rows = plain_text(&drawn(&typed("/wor")));
    assert!(!rows.contains("isn't a command"), "{rows}");
    // In shell mode a slash is a path's.
    let mut shell = typed("");
    shell.composer.handle_key(key(KeyCode::Char('!')));
    for c in "/wokflow x".chars() {
        shell.composer.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(shell.composer.command(), None);
}

/// While a command is typed the line says what Enter runs where the mode stands, and the part
/// that names whose context is shown keeps its place at the right end, as the two pictures
/// have them: the words flush left after the margin, the part flush right.
#[test]
fn the_context_part_stays_beside_what_enter_runs() {
    use super::super::line::{AddedPart, LinePart, PartBody};
    use crate::base::kit::Level;
    use crate::base::theme::Role;

    let mut state = typed("/workflow try spec-first add a burst limit");
    state.line.right.push(LinePart::Added(AddedPart {
        name: "context".into(),
        body: PartBody::Fill {
            label: "context".into(),
            who: "build:due-flag".into(),
            who_role: Role::Label,
            percent: 20,
            level: Level::Ok,
        },
    }));
    let rows = plain_text(&drawn(&state));
    let line = rows
        .lines()
        .find(|row| row.contains("enter runs /workflow"))
        .unwrap_or_else(|| panic!("{rows}"));
    assert!(
        line.starts_with("  enter runs /workflow · shows what a workflow would run, runs nothing"),
        "{line}"
    );
    assert!(
        line.trim_end().ends_with("context · build:due-flag 20%"),
        "{line}"
    );

    // The draft gone, the mode has its place back and the part hasn't moved.
    state.composer.set_text("");
    let rows = plain_text(&drawn(&state));
    let line = rows
        .lines()
        .find(|row| row.contains("context · build:due-flag 20%"))
        .unwrap_or_else(|| panic!("{rows}"));
    assert!(!line.contains("enter runs"), "{line}");
}

/// A command the prompt just refused keeps its draft, and the line says why, whole, where it
/// would say what Enter runs. Once the notice goes, the line says that again.
#[test]
fn a_refused_command_says_why_before_what_enter_runs() {
    let refusal = "/effort: \"turbo\" isn't a level; use minimal, low, medium, high, xhigh, max";
    let mut state = typed("/effort turbo");
    state.mode.notice = Some(ModeNotice {
        text: refusal.to_owned(),
        tone: Tone::Warning,
        until: Instant::now() + std::time::Duration::from_secs(60),
    });
    let rows = plain_text(&drawn(&state));
    assert!(rows.contains(refusal), "{rows}");
    assert!(!rows.contains("enter runs"), "{rows}");

    state.mode.notice = None;
    let rows = plain_text(&drawn(&state));
    assert!(rows.contains("  enter runs /effort"), "{rows}");
}

#[test]
fn a_real_command_with_nothing_after_it_shows_what_it_takes() {
    let rows = plain_text(&input_rows("/model"));
    assert!(rows.contains("❯ /model [role] [model]"), "{rows}");
    assert!(rows.contains("  enter runs /model"), "{rows}");
    // A word half typed hides the hint, so it never trails the letters being typed.
    let rows = plain_text(&input_rows("/model wor"));
    assert!(rows.contains("❯ /model wor\n"), "{rows}");
}
