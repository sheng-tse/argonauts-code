use std::time::{Duration, Instant};

use argonauts_core::keywords::Keyword;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::*;
use crate::bottom::VimMode;
use crate::render::Clock;
use crate::text::marks::decoration;
use crate::text::plain_text;
use crate::theme::{Glyphs, Role, Theme};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn with(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

fn ctrl(c: char) -> KeyEvent {
    with(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn type_text(composer: &mut Composer, text: &str) {
    for c in text.chars() {
        composer.handle_key(key(KeyCode::Char(c)));
    }
}

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn render(composer: &Composer, width: u16) -> ComposerView {
    let theme = theme();
    let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
    composer.render(&ctx, true, false)
}

#[test]
fn typing_and_moving_by_graphemes_and_words() {
    let mut c = Composer::new();
    type_text(&mut c, "hello world");
    assert_eq!(c.text(), "hello world");
    for _ in 0..5 {
        c.handle_key(key(KeyCode::Left));
    }
    type_text(&mut c, "big ");
    assert_eq!(c.text(), "hello big world");
    assert_eq!(c.handle_key(key(KeyCode::Home)), ComposerEvent::Moved);
    assert_eq!(c.cursor(), 0);
    assert_eq!(c.handle_key(key(KeyCode::Left)), ComposerEvent::Unhandled);
    c.handle_key(with(KeyCode::Right, KeyModifiers::ALT));
    assert_eq!(c.cursor(), 5);
    c.handle_key(ctrl('e'));
    assert_eq!(c.cursor(), c.text().len());
    c.handle_key(with(KeyCode::Left, KeyModifiers::ALT));
    assert_eq!(c.cursor(), 10);
    c.handle_key(ctrl('a'));
    c.handle_key(key(KeyCode::Delete));
    assert_eq!(c.text(), "ello big world");
    c.handle_key(key(KeyCode::End));
    c.handle_key(key(KeyCode::Backspace));
    assert_eq!(c.text(), "ello big worl");
}

#[test]
fn kill_keys_delete_words_and_lines_and_yank_them_back() {
    let mut c = Composer::new();
    type_text(&mut c, "one two three");
    assert_eq!(c.handle_key(ctrl('w')), ComposerEvent::Edited);
    assert_eq!(c.text(), "one two ");
    c.handle_key(ctrl('y'));
    assert_eq!(c.text(), "one two three");
    c.handle_key(ctrl('a'));
    c.handle_key(with(KeyCode::Right, KeyModifiers::ALT));
    c.handle_key(ctrl('k'));
    assert_eq!(c.text(), "one");
    c.handle_key(ctrl('u'));
    assert_eq!(c.text(), "");
    assert_eq!(c.handle_key(ctrl('u')), ComposerEvent::Unhandled);
    type_text(&mut c, "a b");
    c.handle_key(with(KeyCode::Backspace, KeyModifiers::ALT));
    assert_eq!(c.text(), "a ");
}

#[test]
fn enter_submits_trimmed_and_remembers() {
    let mut c = Composer::new();
    assert_eq!(c.handle_key(key(KeyCode::Enter)), ComposerEvent::Unhandled);
    type_text(&mut c, "   ");
    assert_eq!(c.handle_key(key(KeyCode::Enter)), ComposerEvent::Unhandled);
    c.clear();
    type_text(&mut c, "hi there");
    c.handle_key(with(KeyCode::Enter, KeyModifiers::SHIFT));
    assert_eq!(c.handle_key(key(KeyCode::Enter)), submitted("hi there"));
    assert!(c.is_empty());
    assert_eq!(c.history(), ["hi there"]);
}

#[test]
fn newlines_come_from_modified_enter_and_a_trailing_backslash() {
    let mut c = Composer::new();
    type_text(&mut c, "a");
    c.handle_key(with(KeyCode::Enter, KeyModifiers::SHIFT));
    type_text(&mut c, "b");
    c.handle_key(with(KeyCode::Enter, KeyModifiers::ALT));
    type_text(&mut c, "c");
    c.handle_key(ctrl('j'));
    type_text(&mut c, "d\\");
    assert_eq!(c.handle_key(key(KeyCode::Enter)), ComposerEvent::Edited);
    type_text(&mut c, "e");
    assert_eq!(c.text(), "a\nb\nc\nd\ne");
    assert_eq!(
        c.handle_key(key(KeyCode::Enter)),
        submitted("a\nb\nc\nd\ne")
    );
}

#[test]
fn history_walks_back_and_restores_the_draft() {
    let mut c = Composer::new();
    assert_eq!(c.handle_key(key(KeyCode::Up)), ComposerEvent::AtTop);
    c.remember("first");
    c.remember("second");
    c.remember("second");
    assert_eq!(c.history(), ["first", "second"]);
    type_text(&mut c, "draft");

    assert_eq!(c.handle_key(key(KeyCode::Up)), ComposerEvent::Moved);
    assert_eq!(c.text(), "second");
    assert!(c.is_browsing_history());
    c.handle_key(key(KeyCode::Up));
    assert_eq!(c.text(), "first");
    assert_eq!(c.handle_key(key(KeyCode::Up)), ComposerEvent::AtTop);
    assert_eq!(c.text(), "first");

    c.handle_key(key(KeyCode::Down));
    assert_eq!(c.text(), "second");
    c.handle_key(key(KeyCode::Down));
    assert_eq!(c.text(), "draft");
    assert!(!c.is_browsing_history());
    assert_eq!(c.handle_key(key(KeyCode::Down)), ComposerEvent::Unhandled);
}

#[test]
fn editing_a_recalled_entry_makes_it_the_draft() {
    let mut c = Composer::new();
    c.remember("recalled");
    c.handle_key(key(KeyCode::Up));
    type_text(&mut c, "!");
    assert!(!c.is_browsing_history());
    assert_eq!(c.text(), "recalled!");
    assert_eq!(c.handle_key(key(KeyCode::Enter)), submitted("recalled!"));
}

#[test]
fn up_and_down_move_between_rows_first() {
    let mut c = Composer::new();
    c.remember("older");
    c.set_text("line one\nline two\nend");
    render(&c, 40);
    assert!(!c.cursor_on_first_row());
    c.handle_key(key(KeyCode::Up));
    assert_eq!(c.cursor(), "line one\nlin".len());
    c.handle_key(key(KeyCode::Up));
    assert_eq!(c.cursor(), "lin".len());
    assert!(c.cursor_on_first_row());
    assert_eq!(c.handle_key(key(KeyCode::Up)), ComposerEvent::Moved);
    assert_eq!(c.text(), "older");

    c.set_text("wrapped text across rows");
    render(&c, 12);
    c.handle_key(key(KeyCode::Home));
    c.handle_key(key(KeyCode::Down));
    assert!(!c.cursor_on_first_row());
    assert_eq!(c.cursor(), "wrapped ".len());
    c.handle_key(key(KeyCode::Down));
    c.handle_key(key(KeyCode::Down));
    assert_eq!(c.cursor(), "wrapped text across ".len());
    assert_eq!(c.handle_key(key(KeyCode::Down)), ComposerEvent::Moved);
    assert_eq!(c.cursor(), c.text().len());
}

#[test]
fn long_pastes_collapse_and_expand_on_submit() {
    let mut c = Composer::new();
    type_text(&mut c, "see ");
    c.handle_paste("line 1\r\nline 2\r\nline 3\nline 4");
    assert_eq!(c.text(), "see [Pasted text #1 +3 lines]");
    type_text(&mut c, " ok");
    c.handle_paste(&"x".repeat(900));
    assert_eq!(c.text(), "see [Pasted text #1 +3 lines] ok[Pasted text #2]");
    c.handle_key(key(KeyCode::Backspace));
    assert_eq!(c.text(), "see [Pasted text #1 +3 lines] ok");

    c.handle_key(key(KeyCode::Left));
    c.handle_key(key(KeyCode::Left));
    c.handle_key(key(KeyCode::Left));
    c.handle_key(key(KeyCode::Left));
    assert_eq!(c.cursor(), "see ".len());
    assert_eq!(
        c.handle_key(key(KeyCode::Enter)),
        ComposerEvent::Submitted {
            text: "see line 1\nline 2\nline 3\nline 4 ok".into(),
            shown: "see [Pasted text #1 +3 lines] ok".into(),
        }
    );
}

const PASTED: &str = "one\ntwo\nthree\nfour";

fn draft_with_paste() -> Composer {
    let mut c = Composer::new();
    type_text(&mut c, "see ");
    c.handle_paste(PASTED);
    assert_eq!(c.text(), "see [Pasted text #1 +3 lines]");
    c
}

#[test]
fn the_editor_gets_pastes_expanded_and_its_text_is_sent() {
    let mut c = draft_with_paste();
    let sent = c.expanded_text();
    assert_eq!(sent, format!("see {PASTED}"));
    c.set_edited_text(&sent, &format!("{sent}\nfive"));
    assert_eq!(
        c.handle_key(key(KeyCode::Enter)),
        submitted(&format!("see {PASTED}\nfive"))
    );
}

#[test]
fn a_draft_the_editor_left_alone_keeps_its_pastes_collapsed() {
    let mut c = draft_with_paste();
    let sent = c.expanded_text();
    c.set_edited_text(&sent, &sent);
    assert_eq!(c.text(), "see [Pasted text #1 +3 lines]");
    assert_eq!(
        c.handle_key(key(KeyCode::Enter)),
        ComposerEvent::Submitted {
            text: format!("see {PASTED}"),
            shown: "see [Pasted text #1 +3 lines]".into(),
        }
    );
}

#[test]
fn a_killed_paste_yanks_back_whole() {
    let mut c = draft_with_paste();
    c.handle_key(ctrl('u'));
    assert!(c.is_empty());
    c.handle_key(ctrl('y'));
    c.handle_key(ctrl('y'));
    assert_eq!(
        c.text(),
        "see [Pasted text #1 +3 lines]see [Pasted text #1 +3 lines]"
    );
    assert_eq!(
        c.handle_key(key(KeyCode::Enter)),
        ComposerEvent::Submitted {
            text: format!("see {PASTED}see {PASTED}"),
            shown: "see [Pasted text #1 +3 lines]see [Pasted text #1 +3 lines]".into(),
        }
    );
}

#[test]
fn vim_puts_back_a_deleted_or_yanked_paste_whole() {
    let mut c = draft_with_paste();
    c.set_vim(true);
    c.handle_key(key(KeyCode::Esc));
    normal(&mut c, "yyp");
    assert_eq!(
        c.text(),
        "see [Pasted text #1 +3 lines]\nsee [Pasted text #1 +3 lines]"
    );
    normal(&mut c, "ddP");
    assert_eq!(c.expanded_text(), format!("see {PASTED}\nsee {PASTED}"));
    normal(&mut c, "0wD");
    assert_eq!(c.text(), "see \nsee [Pasted text #1 +3 lines]");
    normal(&mut c, "p");
    assert_eq!(c.expanded_text(), format!("see {PASTED}\nsee {PASTED}"));
}

#[test]
fn short_pastes_go_in_as_text_without_escapes() {
    let mut c = Composer::new();
    c.handle_paste("\x1b[31mred\x1b[0m\tand\rmore");
    assert_eq!(c.text(), "red\tand\nmore");
    c.handle_paste("");
    assert_eq!(c.text(), "red\tand\nmore");
    // The tab is kept for the agent, and drawn as spaces.
    let shown = plain_text(&render(&c, 40).lines);
    assert!(shown.contains("❯ red    and"), "{shown}");
}

#[test]
fn pastes_reach_the_agent_as_pasted() {
    let emoji = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} \u{1F44D}\u{1F3FD} \u{2764}\u{FE0F}";
    let make = "all:\n\tcargo build\r\nname\tsize\n";
    let mut c = Composer::new();
    c.handle_paste(&format!("{make}{emoji}\x1b]0;title\x07"));
    let pasted = format!("all:\n\tcargo build\nname\tsize\n{emoji}");
    assert_eq!(c.expanded_text(), pasted);
    assert_eq!(
        c.handle_key(key(KeyCode::Enter)),
        ComposerEvent::Submitted {
            text: pasted,
            shown: "[Pasted text #1 +3 lines]".into(),
        }
    );

    c.handle_paste("a\tb");
    let view = render(&c, 40);
    assert_eq!(plain_text(&view.lines[1..2]), "❯ a    b");
    assert_eq!(view.cursor, (2 + 6, 1));
}

#[test]
fn set_text_sanitizes() {
    let mut c = Composer::new();
    c.set_text("taken\x1b]0;x\x07 back");
    assert_eq!(c.text(), "taken back");
    assert_eq!(c.cursor(), c.text().len());
}

#[test]
fn renders_rules_label_prompt_and_cursor() {
    let mut c = Composer::new();
    c.label = Some("council".into());
    c.placeholder = Some("Ask anything".into());
    let view = render(&c, 40);
    assert_eq!(
        plain_text(&view.lines),
        "────────────────────────────── council ─\n\
         ❯ Ask anything\n\
         ────────────────────────────────────────"
    );
    assert_eq!(view.cursor, (2, 1));
    let theme = theme();
    // The name is in the rule's own grey. Purple is kept for the ultracode tag.
    assert_eq!(
        view.lines[0].spans[2].style,
        decoration(theme.style(Role::Rule))
    );
    assert_eq!(
        view.lines[1].spans[0].style,
        decoration(theme.style(Role::Text))
    );
    let input = &view.lines[1].spans;
    assert_eq!(input[1].content, "A");
    assert!(input[1].style.add_modifier.contains(Modifier::REVERSED));
    assert_eq!(input[2].content, "sk anything");
    assert_eq!(input[2].style, theme.style(Role::Dim));
    let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
    let busy = c.render(&ctx, true, true);
    let dim = decoration(theme.style(Role::Dim));
    assert_eq!(busy.lines[1].spans[0].style, dim);
    let unfocused = c.render(&ctx, false, false);
    assert_eq!(unfocused.lines[1].spans[0].style, dim);
    assert_eq!(unfocused.lines[1].spans[1].content, "Ask anything");

    type_text(&mut c, "hello");
    c.handle_key(key(KeyCode::Left));
    let view = render(&c, 40);
    assert_eq!(plain_text(&view.lines[1..2]), "❯ hello");
    assert_eq!(view.cursor, (6, 1));
    let block: Vec<_> = view.lines[1]
        .spans
        .iter()
        .filter(|s| s.style.add_modifier.contains(Modifier::REVERSED))
        .collect();
    assert_eq!(block[0].content, "o");
}

#[test]
fn a_label_that_does_not_fit_leaves_a_plain_rule() {
    let mut c = Composer::new();
    c.label = Some("a-very-long-label".into());
    let view = render(&c, 12);
    assert_eq!(plain_text(&view.lines[..1]), "────────────");
}

#[test]
fn input_wraps_and_scrolls_to_the_cursor() {
    let mut c = Composer::new();
    c.set_text(
        &(1..=14)
            .map(|i| format!("row {i}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let view = render(&c, 30);
    assert_eq!(view.lines.len(), MAX_INPUT_ROWS + 2);
    assert_eq!(plain_text(&view.lines[1..2]), "  row 5");
    assert_eq!(plain_text(&view.lines[10..11]), "  row 14");
    assert_eq!(view.cursor, (8, 10));

    for _ in 0..13 {
        c.handle_key(key(KeyCode::Up));
    }
    let view = render(&c, 30);
    assert_eq!(plain_text(&view.lines[1..2]), "❯ row 1");
    assert_eq!(view.cursor.1, 1);

    let mut wrapped = Composer::new();
    wrapped.set_text("the quick brown fox jumps");
    let view = render(&wrapped, 14);
    assert_eq!(
        plain_text(&view.lines[1..view.lines.len() - 1]),
        "❯ the quick\n  brown fox\n  jumps"
    );
}

#[test]
fn unfocused_draws_no_block() {
    let mut c = Composer::new();
    c.set_text("text");
    let theme = theme();
    let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
    let view = c.render(&ctx, false, false);
    assert!(
        view.lines
            .iter()
            .flat_map(|line| &line.spans)
            .all(|span| !span.style.add_modifier.contains(Modifier::REVERSED))
    );
}

#[test]
fn an_empty_prompt_offers_an_example_while_nothing_runs() {
    let mut composer = Composer::new();
    composer.example = Some("fix the flaky export test".into());
    assert_eq!(
        composer.placeholder_text(true).as_deref(),
        Some("Try \"fix the flaky export test\"")
    );
    assert_eq!(composer.placeholder_text(false), None);
    composer.placeholder = Some("↑ to answer the question, or type to guide the run".into());
    assert_eq!(
        composer.placeholder_text(false).as_deref(),
        Some("↑ to answer the question, or type to guide the run")
    );
}

#[test]
fn a_fresh_prompt_offers_the_built_in_example() {
    let mut composer = Composer::new();
    assert_eq!(
        composer.placeholder_text(true),
        Some(format!("Try \"{BUILT_IN_EXAMPLE}\""))
    );
    assert_eq!(composer.placeholder_text(false), None);
    composer.end_example();
    assert_eq!(composer.placeholder_text(true), None);
}

#[test]
fn the_example_goes_after_the_first_prompt() {
    let mut composer = Composer::new();
    composer.example = Some("fix the flaky export test".into());
    type_text(&mut composer, "hi");
    composer.handle_key(key(KeyCode::Enter));
    assert_eq!(composer.placeholder_text(true), None);
}

#[test]
fn history_walks_multiline_draft_first() {
    let mut c = Composer::new();
    c.set_history(["older".to_owned(), "newest".to_owned()]);
    c.set_text("one\ntwo\nthree");
    render(&c, 40);
    assert_eq!(c.handle_key(key(KeyCode::Up)), ComposerEvent::Moved);
    assert_eq!(c.cursor(), "one\ntwo".len());
    c.handle_key(key(KeyCode::Up));
    assert_eq!(c.cursor(), "one".len());
    assert!(!c.is_browsing_history());

    c.handle_key(key(KeyCode::Up));
    assert_eq!(c.text(), "newest");
    c.handle_key(key(KeyCode::Up));
    assert_eq!(c.text(), "older");
    assert_eq!(c.handle_key(key(KeyCode::Up)), ComposerEvent::AtTop);

    // ↓ walks back toward the newest entry, then to the draft as it was.
    c.handle_key(key(KeyCode::Down));
    assert_eq!(c.text(), "newest");
    c.handle_key(key(KeyCode::Down));
    assert_eq!(c.text(), "one\ntwo\nthree");
    assert!(!c.is_browsing_history());

    // A repeat moves to the newest place instead of showing twice.
    c.remember("older");
    assert_eq!(c.history(), ["newest", "older"]);
}

#[test]
fn ctrl_r_and_vim_slash_ask_for_the_history_search() {
    let mut c = Composer::new();
    type_text(&mut c, "draft");
    assert_eq!(c.handle_key(ctrl('r')), ComposerEvent::SearchHistory);
    assert_eq!(
        c.text(),
        "draft",
        "the draft waits while the search is open"
    );

    c.set_vim(true);
    // `/` in INSERT is typed; in NORMAL it searches, as in Claude Code.
    type_text(&mut c, "/");
    assert_eq!(c.text(), "draft/");
    c.handle_key(key(KeyCode::Esc));
    assert_eq!(c.vim_mode(), Some(VimMode::Normal));
    assert_eq!(
        c.handle_key(key(KeyCode::Char('/'))),
        ComposerEvent::SearchHistory
    );
    assert_eq!(c.text(), "draft/");
    assert_eq!(c.vim_mode(), Some(VimMode::Normal));

    // A picked prompt fills the input, the cursor on its last letter in NORMAL.
    c.fill("fix the export test");
    assert_eq!(c.text(), "fix the export test");
    assert_eq!(c.cursor(), "fix the export tes".len());
    assert_eq!(c.vim_mode(), Some(VimMode::Normal));
}

fn key_at(c: &mut Composer, text: &str, start: Instant, gap: Duration) -> Vec<ComposerEvent> {
    let mut events = Vec::new();
    for (i, ch) in text.chars().enumerate() {
        let code = if ch == '\n' {
            KeyCode::Enter
        } else {
            KeyCode::Char(ch)
        };
        events.push(c.handle_key_at(key(code), start + gap * i as u32));
    }
    events
}

#[test]
fn paste_burst_becomes_one_paste() {
    let mut c = Composer::new();
    let t0 = Instant::now();
    let ms = Duration::from_millis;
    key_at(&mut c, "ask ", t0, ms(90));
    assert_eq!(c.text(), "ask ");
    assert_eq!(c.paste_burst_due(), None);

    let pasted = "fn main() {\n    run();\n}\n// done";
    let start = t0 + ms(1000);
    let events = key_at(&mut c, pasted, start, ms(1));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, ComposerEvent::Submitted { .. })),
        "{events:?}"
    );
    let last = start + ms(pasted.chars().count() as u64 - 1);
    assert!(c.paste_burst_due().is_some_and(|due| due > last));
    assert!(!c.flush_paste_burst(last + ms(2)));
    assert!(c.flush_paste_burst(last + ms(40)));
    assert_eq!(c.text(), "ask [Pasted text #1 +3 lines]");
    assert_eq!(c.expanded_text(), format!("ask {pasted}"));

    // Typing at a person's pace is typing, and Enter sends.
    let later = last + ms(3000);
    key_at(&mut c, "!", later, ms(0));
    assert_eq!(
        c.handle_key_at(key(KeyCode::Enter), later + ms(200)),
        ComposerEvent::Submitted {
            text: format!("ask {pasted}!"),
            shown: "ask [Pasted text #1 +3 lines]!".into(),
        }
    );

    // A short burst goes in as plain text, and any other key puts it in first.
    key_at(&mut c, "two words", later + ms(5000), ms(1));
    assert_eq!(c.text(), "");
    assert_eq!(c.handle_key(key(KeyCode::Home)), ComposerEvent::Edited);
    assert_eq!(c.text(), "two words");
    assert_eq!(c.cursor(), 0);
}

#[test]
fn long_paste_shows_label_and_expands_on_submit() {
    let mut c = Composer::new();
    type_text(&mut c, "review ");
    let long: Vec<String> = (1..=120).map(|n| format!("line {n}")).collect();
    let long = long.join("\n");
    c.handle_paste(&long);
    assert_eq!(c.text(), "review [Pasted text #1 +119 lines]");
    let shown = plain_text(&render(&c, 80).lines);
    assert!(
        shown.contains("❯ review [Pasted text #1 +119 lines]"),
        "{shown}"
    );
    assert_eq!(
        c.handle_key(key(KeyCode::Enter)),
        ComposerEvent::Submitted {
            text: format!("review {long}"),
            shown: "review [Pasted text #1 +119 lines]".into(),
        }
    );
    assert_eq!(c.history(), [format!("review {long}")]);
}

fn normal(c: &mut Composer, keys: &str) {
    for ch in keys.chars() {
        c.handle_key(key(KeyCode::Char(ch)));
    }
}

#[test]
fn vim_visual_and_text_objects() {
    let mut c = Composer::new();
    c.set_vim(true);
    type_text(&mut c, r#"say "hi there" to f(a, b) now"#);
    c.handle_key(key(KeyCode::Esc));

    normal(&mut c, "0wlci\"");
    assert_eq!(c.vim_mode(), Some(VimMode::Insert));
    type_text(&mut c, "bye");
    c.handle_key(key(KeyCode::Esc));
    assert_eq!(c.text(), r#"say "bye" to f(a, b) now"#);

    normal(&mut c, "wwdaw");
    assert_eq!(c.text(), r#"say "bye" f(a, b) now"#);
    normal(&mut c, "wldi(");
    assert_eq!(c.text(), r#"say "bye" f() now"#);
    normal(&mut c, "P");
    assert_eq!(c.text(), r#"say "bye" f(a, b) now"#);

    normal(&mut c, "0ve");
    assert_eq!(c.vim_mode(), Some(VimMode::Visual));
    assert_eq!(c.visual_range(), Some(0..3));
    let rows = render(&c, 60).lines;
    let reversed: String = rows[1]
        .spans
        .iter()
        .filter(|s| s.style.add_modifier.contains(Modifier::REVERSED))
        .map(|s| s.content.as_ref())
        .collect();
    assert_eq!(reversed, "say");
    normal(&mut c, "d");
    assert_eq!(c.vim_mode(), Some(VimMode::Normal));
    assert_eq!(c.text(), r#" "bye" f(a, b) now"#);

    normal(&mut c, "$bviwU");
    assert_eq!(c.text(), r#" "bye" f(a, b) NOW"#);
    assert_eq!(c.vim_mode(), Some(VimMode::Normal));

    normal(&mut c, "0va\"y");
    assert_eq!(c.visual_range(), None);
    normal(&mut c, "$p");
    assert_eq!(c.text(), r#" "bye" f(a, b) NOW"bye" "#);

    c.set_text("keep\ndrop this\nkeep too");
    c.handle_key(key(KeyCode::Up));
    normal(&mut c, "Vd");
    assert_eq!(c.text(), "keep\nkeep too");
    normal(&mut c, "p");
    assert_eq!(c.text(), "keep\nkeep too\ndrop this");
    normal(&mut c, "ggyyGp");
    assert_eq!(c.text(), "keep\nkeep too\ndrop this\nkeep");
}

#[test]
fn vim_normal_mode_is_reported() {
    let mut c = Composer::new();
    assert_eq!(c.vim_mode(), None);
    c.set_vim(true);
    assert_eq!(c.vim_mode(), Some(VimMode::Insert));
    type_text(&mut c, "hello");
    assert_eq!(c.handle_key(key(KeyCode::Esc)), ComposerEvent::Moved);
    assert_eq!(c.vim_mode(), Some(VimMode::Normal));
    // NORMAL's Esc goes on, so it can still interrupt the run.
    assert_eq!(c.handle_key(key(KeyCode::Esc)), ComposerEvent::Unhandled);
    normal(&mut c, "qz");
    assert_eq!(c.text(), "hello");
    normal(&mut c, "v");
    assert_eq!(c.vim_mode(), Some(VimMode::Visual));
    c.handle_key(key(KeyCode::Esc));
    assert_eq!(c.vim_mode(), Some(VimMode::Normal));
    normal(&mut c, "A");
    assert_eq!(c.vim_mode(), Some(VimMode::Insert));
    c.handle_key(key(KeyCode::Esc));
    assert_eq!(c.handle_key(key(KeyCode::Enter)), submitted("hello"));
    assert_eq!(c.vim_mode(), Some(VimMode::Normal));
    c.set_vim(false);
    assert_eq!(c.vim_mode(), None);
    type_text(&mut c, "v");
    assert_eq!(c.text(), "v");
}

/// A submission whose echo reads as it was typed.
fn submitted(text: &str) -> ComposerEvent {
    ComposerEvent::Submitted {
        text: text.into(),
        shown: text.into(),
    }
}

fn shell(command: &str) -> ComposerEvent {
    ComposerEvent::Shell {
        command: command.into(),
    }
}

#[test]
fn a_bang_on_an_empty_input_switches_to_shell_mode_and_enter_runs_it() {
    let mut c = Composer::new();
    assert_eq!(c.handle_key(key(KeyCode::Char('!'))), ComposerEvent::Edited);
    assert!(c.is_shell());
    assert!(c.is_empty(), "the ! is the mode, not the text");
    // A second ! is part of the command.
    type_text(&mut c, "!! && git status");
    assert_eq!(c.text(), "!! && git status");
    assert_eq!(c.handle_key(key(KeyCode::Enter)), shell("!! && git status"));
    assert!(!c.is_shell(), "the input goes back to messages");
    assert!(c.is_empty());
    assert_eq!(c.history(), ["!!! && git status"]);

    // Anywhere but the start of an empty input, ! is just a character.
    type_text(&mut c, "hi!");
    assert_eq!(c.text(), "hi!");
    assert!(!c.is_shell());
}

#[test]
fn backspace_on_an_empty_input_or_leaving_keeps_what_was_typed() {
    let mut c = Composer::new();
    type_text(&mut c, "!a");
    assert!(c.is_shell());
    c.handle_key(key(KeyCode::Backspace));
    assert!(c.is_shell(), "the first Backspace takes the letter");
    assert_eq!(c.handle_key(key(KeyCode::Backspace)), ComposerEvent::Edited);
    assert!(!c.is_shell(), "the next one leaves shell mode");
    assert_eq!(
        c.handle_key(key(KeyCode::Backspace)),
        ComposerEvent::Unhandled
    );

    type_text(&mut c, "!ls -la");
    assert!(c.leave_shell());
    assert_eq!(c.text(), "ls -la", "Esc leaves the draft as typed");
    assert!(!c.leave_shell());
    c.clear();
    type_text(&mut c, "!");
    c.clear();
    assert!(!c.is_shell(), "clearing the input clears its mode");
    // Whitespace alone runs nothing.
    type_text(&mut c, "!  ");
    assert_eq!(c.handle_key(key(KeyCode::Enter)), ComposerEvent::Unhandled);
}

#[test]
fn history_brings_a_command_back_in_shell_mode() {
    let mut c = Composer::new();
    type_text(&mut c, "!ls");
    c.handle_key(key(KeyCode::Enter));
    type_text(&mut c, "hello");
    c.handle_key(key(KeyCode::Enter));
    type_text(&mut c, "!pwd");

    c.handle_key(key(KeyCode::Up));
    assert_eq!((c.text(), c.is_shell()), ("hello", false));
    c.handle_key(key(KeyCode::Up));
    assert_eq!((c.text(), c.is_shell()), ("ls", true));
    c.handle_key(key(KeyCode::Down));
    assert_eq!((c.text(), c.is_shell()), ("hello", false));
    c.handle_key(key(KeyCode::Down));
    assert_eq!(
        (c.text(), c.is_shell()),
        ("pwd", true),
        "the draft comes back in its own mode"
    );

    // The history search hands back an entry as it was kept, with its `!`.
    c.clear();
    assert_eq!(c.handle_key(ctrl('r')), ComposerEvent::SearchHistory);
    c.fill("!ls");
    assert_eq!((c.text(), c.is_shell()), ("ls", true));
    c.fill("hello");
    assert_eq!(
        (c.text(), c.is_shell()),
        ("hello", false),
        "a prompt picked in shell mode never runs as a command"
    );
}

#[test]
fn a_pasted_or_fast_typed_bang_command_opens_shell_mode() {
    let mut c = Composer::new();
    c.handle_paste("!git log -3");
    assert_eq!((c.text(), c.is_shell()), ("git log -3", true));
    c.clear();
    type_text(&mut c, "see ");
    c.handle_paste("!this");
    assert_eq!((c.text(), c.is_shell()), ("see !this", false));

    let mut fast = Composer::new();
    let t0 = Instant::now();
    key_at(&mut fast, "!echo hi", t0, Duration::from_millis(1));
    fast.flush_paste_burst(t0 + Duration::from_secs(1));
    assert_eq!((fast.text(), fast.is_shell()), ("echo hi", true));
}

/// Enter runs shell mode's text outside the sandbox, so none of it may hide behind a label.
#[test]
fn a_paste_of_several_lines_never_runs_hidden() {
    // A notebook cell or a Markdown image opens with `!`, and stays a message.
    let mut c = Composer::new();
    let cell = "!pip install torch\nimport torch\nprint(torch.__version__)\nx = 1";
    c.handle_paste(cell);
    assert!(!c.is_shell());
    assert_eq!(c.expanded_text(), cell);
    assert!(matches!(
        c.handle_key(key(KeyCode::Enter)),
        ComposerEvent::Submitted { .. }
    ));
    c.handle_paste("![diagram](docs/flow.png)\nwhy does this loop?");
    assert!(!c.is_shell());

    // In shell mode a long paste shows in full and runs as shown.
    let mut c = Composer::new();
    type_text(&mut c, "!");
    let script = (1..=6)
        .map(|n| format!("echo {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    c.handle_paste(&script);
    assert!(c.is_shell());
    assert_eq!(c.text(), script, "no [Pasted text] label");
    c.handle_paste(&format!(" {}", "x".repeat(900)));
    assert!(!c.text().contains("[Pasted text"), "{}", c.text());
    assert_eq!(
        c.handle_key(key(KeyCode::Enter)),
        shell(&format!("{script} {}", "x".repeat(900)))
    );
}

#[test]
fn shell_mode_draws_its_marker_rules_and_placeholder_in_its_color() {
    let mut c = Composer::new();
    c.label = Some("council".into());
    type_text(&mut c, "!");
    let view = render(&c, 50);
    assert_eq!(
        plain_text(&view.lines),
        "──────────────────────────────────────── council ─\n\
         ! Run a shell command in the repo folder\n\
         ──────────────────────────────────────────────────"
    );
    let theme = theme();
    let pink = theme.style(Role::Shell);
    assert_eq!(view.lines[0].spans[0].style, decoration(pink));
    assert_eq!(view.lines[0].spans[2].style, decoration(pink));
    assert_eq!(view.lines[1].spans[0].style, decoration(pink));
    assert_eq!(view.lines[2].spans[0].style, decoration(pink));

    type_text(&mut c, "git status");
    let view = render(&c, 50);
    assert_eq!(plain_text(&view.lines[1..2]), "! git status");
    assert_eq!(view.cursor, (12, 1));
}

const ULTRATHINK: Keyword = Keyword::new(
    "ultrathink",
    argonauts_core::keywords::KeywordLook::Rainbow,
    true,
);

fn keyword_composer(text: &str) -> Composer {
    let mut c = Composer::new();
    c.keywords = vec![ULTRATHINK];
    type_text(&mut c, text);
    c
}

#[test]
fn the_main_agents_keyword_takes_its_rainbow_letter_by_letter() {
    let c = keyword_composer("please ultrathink about this");
    assert_eq!(c.text(), "please ultrathink about this");
    let theme = theme();
    let row = &render(&c, 40).lines[1].spans;
    assert_eq!(row[1], Span::raw("please "));
    for (index, letter) in "ultrathink".chars().enumerate() {
        assert_eq!(
            row[2 + index],
            Span::styled(letter.to_string(), theme.rainbow(index, false))
        );
    }
    assert_eq!(row[12], Span::raw(" about this"));
    assert_eq!(
        row[13],
        Span::styled(" ", Style::new().add_modifier(Modifier::REVERSED))
    );
}

#[test]
fn the_cursor_block_keeps_the_letters_color() {
    let mut c = keyword_composer("ultrathink");
    c.handle_key(key(KeyCode::Home));
    let theme = theme();
    let row = &render(&c, 40).lines[1].spans;
    let block = Style::new().add_modifier(Modifier::REVERSED);
    assert_eq!(
        row[1],
        Span::styled("u", theme.rainbow(0, false).patch(block))
    );
    assert_eq!(row[2], Span::styled("l", theme.rainbow(1, false)));
}

#[test]
fn the_shimmer_animates_until_shell_mode_or_the_word_goes() {
    let theme = theme();
    let clock = Clock::still(Instant::now());
    let mut c = keyword_composer("ultrathink");
    assert!(c.next_change(&clock, &theme).is_some());
    for _ in 0.."think".len() {
        c.handle_key(key(KeyCode::Backspace));
    }
    assert_eq!(c.next_change(&clock, &theme), None);

    let shell = keyword_composer("!ultrathink");
    assert!(shell.shell);
    assert_eq!(shell.next_change(&clock, &theme), None);
    // Past the shell's `!`, the text is plain.
    let row = &render(&shell, 40).lines[1].spans;
    assert!(row[1..].iter().all(|span| span.style.fg.is_none()));
}

/// The input's rule: the model alone, the workflow first when it isn't solo, and a purple
/// `ultracode` at the far right, two spaces after the name.
#[test]
fn the_rule_names_the_model_and_keeps_purple_for_ultracode() {
    let rule = |label: &str, ultracode: bool, width: u16| {
        let mut c = Composer::new();
        c.label = Some(label.into());
        c.ultracode = ultracode;
        render(&c, width).lines.remove(0)
    };
    let text = |line: &Line<'static>| plain_text(std::slice::from_ref(line));

    assert_eq!(
        text(&rule("Opus 5.5", false, 40)),
        "───────────────────────────── Opus 5.5 ─"
    );
    assert_eq!(
        text(&rule("council · Opus 5.5", false, 40)),
        "─────────────────── council · Opus 5.5 ─"
    );
    let on = rule("Opus 5.5", true, 40);
    assert_eq!(text(&on), "────────────────── Opus 5.5  ultracode ─");
    let theme = theme();
    let tag = on.spans.iter().find(|span| span.content == "ultracode");
    assert_eq!(tag.unwrap().style, theme.style(Role::Ultracode));
    let name = on.spans.iter().find(|span| span.content == "Opus 5.5");
    assert_eq!(name.unwrap().style, decoration(theme.style(Role::Rule)));

    // Too narrow for both, the name goes first, then the tag.
    assert_eq!(text(&rule("Opus 5.5", true, 20)), "──────── ultracode ─");
    assert_eq!(text(&rule("Opus 5.5", true, 12)), "────────────");
}

/// Claude Code's own handling of the word: one flat purple while it counts, Opt+W to ignore
/// it for the prompt, and the ignore forgotten once the word leaves the draft.
#[test]
fn the_word_ultracode_asks_for_a_workflow_until_it_is_ignored() {
    let theme = theme();
    let purple = theme.style(Role::Ultracode);
    let word_style = |c: &Composer| {
        let view = render(c, 60);
        view.lines[1]
            .spans
            .iter()
            .find(|span| span.content.contains("ultracode"))
            .map(|span| span.style)
    };
    let mut c = Composer::new();
    type_text(&mut c, "ultracode fix the flaky export test");
    assert!(c.workflow_requested());
    assert_eq!(word_style(&c), Some(purple), "one run, one color");

    assert_eq!(c.toggle_workflow_word(), Some(true));
    assert!(!c.workflow_requested());
    assert_ne!(word_style(&c), Some(purple));
    assert_eq!(
        c.toggle_workflow_word(),
        Some(false),
        "pressed again, it's back"
    );
    assert!(c.workflow_requested());

    // Ignored, then sent: the request goes without the workflow.
    c.toggle_workflow_word();
    c.handle_key(key(KeyCode::Enter));
    assert!(!c.take_workflow_request());

    // Taking the word out forgets the ignore, so typing it again counts afresh.
    type_text(&mut c, "ultracode");
    c.toggle_workflow_word();
    c.handle_key(key(KeyCode::Backspace));
    assert!(!c.workflow_requested());
    c.handle_key(key(KeyCode::Char('e')));
    assert!(c.workflow_requested());
    c.handle_key(key(KeyCode::Enter));
    assert!(c.take_workflow_request());
    assert!(!c.take_workflow_request(), "read once");
}

#[test]
fn the_workflow_word_counts_only_in_a_message() {
    let mut c = Composer::new();
    type_text(&mut c, "/effort ultracode on");
    assert!(!c.workflow_requested());
    assert_eq!(c.toggle_workflow_word(), None, "the key is free");

    let mut c = Composer::new();
    type_text(&mut c, "!echo ultracode");
    assert!(!c.workflow_requested());

    let mut c = Composer::field();
    type_text(&mut c, "ultracode");
    assert!(!c.workflow_requested());

    // A question about the word asks nothing, and nothing of it is drawn purple.
    let mut c = Composer::new();
    type_text(&mut c, "what is ultracode?");
    assert!(!c.workflow_requested());
    assert!(
        c.drawn_keywords()
            .iter()
            .all(|keyword| keyword.word != argonauts_core::keywords::ULTRACODE.word)
    );
}
