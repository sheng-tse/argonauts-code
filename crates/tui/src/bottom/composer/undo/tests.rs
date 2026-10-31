use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::Composer;
use crate::bottom::VimMode;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// Keys as the screen sends them: vim's own first, then the usual editing.
fn press(composer: &mut Composer, key: KeyEvent) {
    if composer.vim_key(key).is_none() {
        composer.handle_key(key);
    }
}

fn type_text(composer: &mut Composer, text: &str) {
    for c in text.chars() {
        press(composer, key(KeyCode::Char(c)));
    }
}

fn vim() -> Composer {
    let mut composer = Composer::new();
    composer.set_vim(true);
    composer
}

#[test]
fn ctrl_underscore_undoes_a_word_at_a_time_without_vim_keys() {
    let mut c = Composer::new();
    type_text(&mut c, "fix the export");
    // Ctrl+_ arrives as Ctrl+7 from most terminals, and as Ctrl+_ or Ctrl+- with the keyboard
    // protocol.
    press(&mut c, ctrl('7'));
    assert_eq!(c.text(), "fix the ");
    press(&mut c, ctrl('_'));
    assert_eq!(c.text(), "fix ");
    press(&mut c, ctrl('-'));
    assert_eq!(c.text(), "");
    press(&mut c, ctrl('_'));
    assert_eq!(c.text(), "", "nothing left to undo");
}

#[test]
fn a_run_of_deletes_and_a_paste_are_a_step_each() {
    let mut c = Composer::new();
    type_text(&mut c, "hello");
    c.handle_paste(" world");
    for _ in 0..3 {
        press(&mut c, key(KeyCode::Backspace));
    }
    assert_eq!(c.text(), "hello wo");
    press(&mut c, ctrl('_'));
    assert_eq!(c.text(), "hello world", "the deletes come back together");
    press(&mut c, ctrl('_'));
    assert_eq!(c.text(), "hello", "then the paste goes");
    // Moving starts a new step, even for the same kind of edit.
    press(&mut c, key(KeyCode::Left));
    type_text(&mut c, "X");
    press(&mut c, key(KeyCode::End));
    type_text(&mut c, "Y");
    assert_eq!(c.text(), "hellXoY");
    press(&mut c, ctrl('_'));
    assert_eq!(c.text(), "hellXo");
}

#[test]
fn sending_starts_a_draft_with_nothing_to_undo() {
    let mut c = Composer::new();
    type_text(&mut c, "first");
    press(&mut c, key(KeyCode::Enter));
    assert!(c.is_empty());
    press(&mut c, ctrl('_'));
    assert!(c.is_empty(), "the sent prompt never comes back by undo");
}

#[test]
fn vim_u_undoes_what_insert_typed_as_one_step_and_ctrl_r_redoes_it() {
    let mut c = vim();
    type_text(&mut c, "fix the export test");
    press(&mut c, key(KeyCode::Esc));
    assert_eq!(c.vim_mode(), Some(VimMode::Normal));
    press(&mut c, key(KeyCode::Char('u')));
    assert_eq!(
        c.text(),
        "",
        "everything typed between INSERT and Esc is one step"
    );
    press(&mut c, ctrl('r'));
    assert_eq!(c.text(), "fix the export test");
    press(&mut c, ctrl('r'));
    assert_eq!(c.text(), "fix the export test", "nothing more to redo");
}

#[test]
fn each_normal_mode_change_is_a_step_of_its_own() {
    let mut c = vim();
    type_text(&mut c, "one two three");
    press(&mut c, key(KeyCode::Esc));
    type_text(&mut c, "0dw");
    assert_eq!(c.text(), "two three");
    type_text(&mut c, "x");
    assert_eq!(c.text(), "wo three");
    // `cw` and what is typed after it are one step.
    type_text(&mut c, "cwTWO");
    press(&mut c, key(KeyCode::Esc));
    assert_eq!(c.text(), "TWO three");

    press(&mut c, key(KeyCode::Char('u')));
    assert_eq!(c.text(), "wo three");
    press(&mut c, key(KeyCode::Char('u')));
    assert_eq!(c.text(), "two three");
    press(&mut c, key(KeyCode::Char('u')));
    assert_eq!(c.text(), "one two three");
    press(&mut c, ctrl('r'));
    press(&mut c, ctrl('r'));
    assert_eq!(c.text(), "wo three");
    // A new change drops what was left to redo.
    type_text(&mut c, "x");
    press(&mut c, ctrl('r'));
    assert_eq!(c.text(), "o three");
}

#[test]
fn a_move_in_normal_mode_is_no_step() {
    let mut c = vim();
    type_text(&mut c, "abc");
    press(&mut c, key(KeyCode::Esc));
    type_text(&mut c, "hhll0$");
    press(&mut c, key(KeyCode::Char('u')));
    assert_eq!(c.text(), "", "the one step is what INSERT typed");
}

/// A paste in NORMAL, as a dropped picture's path is, is a step of its own, so undo goes back
/// through it and redo forward again in order.
#[test]
fn a_paste_in_normal_mode_is_a_step_of_its_own() {
    let mut c = vim();
    type_text(&mut c, "abc");
    press(&mut c, key(KeyCode::Esc));
    c.handle_paste(" PASTED");
    let pasted = c.text().to_owned();
    type_text(&mut c, "0x");
    let cut = c.text().to_owned();
    assert_ne!(cut, pasted);

    press(&mut c, key(KeyCode::Char('u')));
    assert_eq!(c.text(), pasted.as_str());
    press(&mut c, key(KeyCode::Char('u')));
    assert_eq!(c.text(), "abc");
    press(&mut c, key(KeyCode::Char('u')));
    assert_eq!(c.text(), "");
    press(&mut c, ctrl('r'));
    assert_eq!(c.text(), "abc");
    press(&mut c, ctrl('r'));
    assert_eq!(c.text(), pasted.as_str());
    press(&mut c, ctrl('r'));
    assert_eq!(c.text(), cut.as_str());
}
