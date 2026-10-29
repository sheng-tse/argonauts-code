use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Modifier;

use super::*;
use crate::render::{Clock, RenderCtx};
use crate::text::plain_text;
use crate::theme::{AgentColor, Glyphs, Role, Theme};

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn merge_question() -> QuestionPrompt {
    let mut question = QuestionPrompt::new(
        7,
        "Merge this run into main?",
        "Merge run-01?",
        vec![
            QuestionOption::irreversible("Merge"),
            QuestionOption::new("Open a pull request"),
            QuestionOption::checklist(
                "Request changes on some tasks",
                vec![
                    "task-002 Add the JSON writer".into(),
                    "task-003 Wire --json into the CLI".into(),
                ],
            ),
            QuestionOption::new("Keep the branch"),
            QuestionOption::irreversible("Abandon the run"),
            QuestionOption::view("Show the diff"),
        ],
    );
    question.detail = vec!["4 tasks · 7 files · +120 -18 · full suite passed".into()];
    question.allow_note = true;
    question
}

fn focused(mut question: QuestionPrompt) -> QuestionPrompt {
    question.focus();
    question
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctx(width: u16, theme: &Theme) -> RenderCtx<'_> {
    RenderCtx::new(width, theme, Clock::still(Instant::now()))
}

fn answered(index: usize) -> QuestionEvent {
    QuestionEvent::Answered {
        index,
        note: None,
        checked: Vec::new(),
    }
}

fn last_row(question: &QuestionPrompt, ctx: &RenderCtx<'_>) -> String {
    let lines = question.lines(ctx);
    plain_text(&lines[lines.len() - 1..])
}

#[test]
fn expanded_reads_like_the_reference() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut question = focused(merge_question());
    question.question = Some("Merge it now?".into());
    let view = question.layout(&ctx);
    assert_eq!(
        plain_text(&view.lines),
        format!(
            "{}\n\
             ◆ Merge this run into main?\n\
             \x20 4 tasks · 7 files · +120 -18 · full suite passed\n\
             \n\
             \x20 Merge it now?\n\
             \x20 ❯ 1. Merge\n\
             \x20   2. Open a pull request\n\
             \x20   3. Request changes on some tasks\n\
             \x20   4. Keep the branch\n\
             \x20   5. Abandon the run\n\
             \x20   6. Show the diff\n\
             \n\
             \x20 Enter to choose · Tab to add a note · Esc to carry on without me",
            "─".repeat(80)
        )
    );
    let lines = &view.lines;
    let question_style = theme.style(Role::Question);
    let dim = theme.style(Role::Dim);
    assert_eq!(
        lines[0].spans[0].style,
        crate::text::marks::decoration(question_style)
    );
    assert_eq!(lines[1].spans[0].style, theme.style(Role::Warn));
    assert_eq!(
        lines[1].spans[1].style,
        question_style.add_modifier(Modifier::BOLD)
    );
    assert_eq!(lines[2].spans[1].style, dim);
    assert_eq!(lines[5].spans[1].style, question_style);
    assert_eq!(lines[5].spans[2].style, dim);
    assert_eq!(lines[5].spans[3].style, question_style);
    assert_eq!(lines[5].spans.len(), 4, "no letter after the label");
    assert_eq!(lines[6].spans[2].style, dim);
    assert_eq!(lines[6].spans[3].style, theme.style(Role::Text));
    // `extra` is the details and their blank row; `tail` the hints and theirs.
    assert_eq!(view.extra, 2..4);
    assert_eq!(view.tail, 2);
}

#[test]
fn a_new_question_waits_without_the_keys() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut question = merge_question();
    assert!(question.waits());
    let lines = question.lines(&ctx);
    assert_eq!(plain_text(&lines[4..5]), "    1. Merge");
    assert_eq!(lines[4].spans[3].style, theme.style(Role::Text));
    assert_eq!(last_row(&question, &ctx), "  ↑  to answer");
    for code in [
        KeyCode::Char('y'),
        KeyCode::Char('1'),
        KeyCode::Enter,
        KeyCode::Esc,
    ] {
        assert_eq!(question.handle_key(key(code)), QuestionEvent::Unhandled);
    }
    assert!(!question.handle_paste("text"));
    question.focus();
    assert!(!question.waits());
    assert_eq!(question.handle_key(key(KeyCode::Char('4'))), answered(3));
}

#[test]
fn esc_takes_it_off_the_screen_and_gives_the_keys_back() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut question = focused(merge_question());
    assert_eq!(
        question.handle_key(key(KeyCode::Esc)),
        QuestionEvent::Dismissed
    );
    assert!(!question.focused && !question.claims_keys());
    assert!(question.lines(&ctx).is_empty(), "no line stays behind");
    assert_eq!(
        question.handle_key(key(KeyCode::Char('2'))),
        QuestionEvent::Unhandled
    );
    question.focus();
    assert!(!question.dismissed);
}

#[test]
fn keys_move_and_answer() {
    let mut question = focused(merge_question());
    assert_eq!(question.handle_key(key(KeyCode::Up)), QuestionEvent::Moved);
    assert_eq!(question.selected, 0);
    question.handle_key(key(KeyCode::Down));
    assert_eq!(question.handle_key(key(KeyCode::Enter)), answered(1));
    assert_eq!(question.handle_key(key(KeyCode::Char('4'))), answered(3));
    assert_eq!(question.handle_key(key(KeyCode::Char('2'))), answered(1));
    for c in ['9', '0', 'q'] {
        assert_eq!(
            question.handle_key(key(KeyCode::Char(c))),
            QuestionEvent::Unhandled
        );
    }
    // Options answer to their numbers alone, never to letters.
    for c in ['y', 'p', 'P', 'k', 'd'] {
        assert_eq!(
            question.handle_key(key(KeyCode::Char(c))),
            QuestionEvent::Unhandled
        );
    }
}

#[test]
fn down_past_the_last_option_hands_the_keys_back() {
    let welcome = QuestionPrompt::new(
        1,
        "Welcome to Argonauts Code",
        "Welcome",
        vec![QuestionOption::new("Continue").recommended()],
    );
    let mut question = focused(welcome);
    assert_eq!(
        question.handle_key(key(KeyCode::Down)),
        QuestionEvent::BackToInput
    );
    assert!(question.waits() && !question.dismissed);
    assert_eq!(
        question.handle_key(key(KeyCode::Enter)),
        QuestionEvent::Unhandled
    );
    question.focus();
    assert_eq!(question.handle_key(key(KeyCode::Up)), QuestionEvent::Moved);
    assert_eq!(question.handle_key(key(KeyCode::Enter)), answered(0));

    let mut question = focused(merge_question());
    for _ in 1..question.options.len() {
        assert_eq!(
            question.handle_key(key(KeyCode::Down)),
            QuestionEvent::Moved
        );
    }
    assert_eq!(
        question.handle_key(key(KeyCode::Down)),
        QuestionEvent::BackToInput
    );
    assert_eq!(question.selected, question.options.len() - 1);
    question.focus();
    assert_eq!(question.handle_key(key(KeyCode::Up)), QuestionEvent::Moved);
    assert_eq!(
        question.handle_key(key(KeyCode::Esc)),
        QuestionEvent::Dismissed
    );

    // An armed option lets go, so the key that comes back can't answer it unseen.
    let mut question = focused(QuestionPrompt::new(
        2,
        "Abandon run-01?",
        "Abandon?",
        vec![
            QuestionOption::new("Keep it"),
            QuestionOption::irreversible("Abandon the run"),
        ],
    ));
    question.handle_key(key(KeyCode::Char('2')));
    assert_eq!(question.armed, Some(1));
    assert_eq!(
        question.handle_key(key(KeyCode::Down)),
        QuestionEvent::BackToInput
    );
    assert_eq!(question.armed, None);
}

#[test]
fn down_inside_an_open_checklist_stays_in_it() {
    let mut question = focused(merge_question());
    question.handle_key(key(KeyCode::Char('3')));
    assert!(question.checklist.is_some());
    for _ in 0..3 {
        assert_eq!(
            question.handle_key(key(KeyCode::Down)),
            QuestionEvent::Moved
        );
    }
    assert!(question.focused);
    assert_eq!(question.checklist.as_ref().map(|list| list.cursor), Some(1));
}

#[test]
fn vim_keys_work_as_the_arrows_inside_a_checklist_too() {
    let mut question = focused(merge_question());
    let vim = |question: &mut QuestionPrompt, c: char| {
        let key = question.read_key(key(KeyCode::Char(c)), true);
        question.handle_key(key)
    };
    vim(&mut question, 'G');
    assert_eq!(question.selected, 5);
    vim(&mut question, 'g');
    vim(&mut question, 'j');
    vim(&mut question, 'j');
    assert_eq!(question.selected, 2);
    vim(&mut question, 'l');
    assert!(question.checklist.is_some());
    vim(&mut question, 'G');
    assert_eq!(question.checklist.as_ref().map(|list| list.cursor), Some(1));
    vim(&mut question, 'k');
    assert_eq!(question.checklist.as_ref().map(|list| list.cursor), Some(0));
    // `h` closes the checklist, as Esc does, then sets the question aside.
    assert_eq!(vim(&mut question, 'h'), QuestionEvent::Moved);
    assert!(question.checklist.is_none() && question.focused);
    assert_eq!(vim(&mut question, 'h'), QuestionEvent::Dismissed);

    // Home and End go to the first and last option without vim keys.
    let mut question = focused(merge_question());
    question.handle_key(key(KeyCode::End));
    assert_eq!(question.selected, 5);
    question.handle_key(key(KeyCode::Home));
    assert_eq!(question.selected, 0);
}

/// Esc backs out of a confirm; when the option itself cancels, the hint says so in other words.
#[test]
fn confirming_a_cancel_offers_a_way_back() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut question = focused(QuestionPrompt::new(
        8,
        "task-001 needs you · Fix add",
        "What now?",
        vec![
            QuestionOption::new("Retry with a note"),
            QuestionOption::irreversible("Cancel the task"),
        ],
    ));
    question.handle_key(key(KeyCode::Char('2')));
    assert_eq!(
        last_row(&question, &ctx),
        "  Enter to cancel the task · Esc to go back"
    );
}

#[test]
fn an_option_that_cant_be_undone_waits_for_enter() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut question = focused(merge_question());
    question.selected = 3;
    assert_eq!(
        question.handle_key(key(KeyCode::Char('1'))),
        QuestionEvent::Moved
    );
    assert_eq!((question.selected, question.armed), (0, Some(0)));
    assert_eq!(
        last_row(&question, &ctx),
        "  Enter to merge · Esc to cancel"
    );
    assert_eq!(question.handle_key(key(KeyCode::Esc)), QuestionEvent::Moved);
    assert_eq!(question.armed, None);
    assert!(question.focused && !question.dismissed);
    question.handle_key(key(KeyCode::Char('1')));
    assert_eq!(question.handle_key(key(KeyCode::Enter)), answered(0));
    assert_eq!(
        question.handle_key(key(KeyCode::Char('5'))),
        QuestionEvent::Moved
    );
    assert_eq!(
        last_row(&question, &ctx),
        "  Enter to abandon the run · Esc to cancel"
    );
    assert_eq!(question.handle_key(key(KeyCode::Char('5'))), answered(4));
    question.handle_key(key(KeyCode::Char('1')));
    question.handle_key(key(KeyCode::Down));
    assert_eq!(question.armed, None);
    assert_eq!(question.handle_key(key(KeyCode::Char('2'))), answered(1));
    question.selected = 0;
    assert_eq!(question.handle_key(key(KeyCode::Enter)), answered(0));
}

#[test]
fn a_view_option_opens_and_keeps_the_question() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut question = focused(merge_question());
    assert_eq!(
        question.handle_key(key(KeyCode::Char('6'))),
        QuestionEvent::OpenView { index: 5 }
    );
    assert!(!question.dismissed);
    assert_eq!(question.selected, 0);
    question.selected = 5;
    assert_eq!(
        question.handle_key(key(KeyCode::Enter)),
        QuestionEvent::OpenView { index: 5 }
    );
    assert_eq!(
        question.handle_key(key(KeyCode::Tab)),
        QuestionEvent::Unhandled
    );
    assert_eq!(
        last_row(&question, &ctx),
        "  Enter to open · Esc to carry on without me"
    );
}

#[test]
fn tab_turns_the_label_into_the_start_of_a_note() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut question = focused(merge_question());
    assert_eq!(question.handle_key(key(KeyCode::Tab)), QuestionEvent::Moved);
    let view = question.layout(&ctx);
    assert_eq!(
        plain_text(&view.lines[4..5]),
        "  ❯ 1. Merge, and add a note"
    );
    assert_eq!(view.cursor, Some((14, 4)));
    assert_eq!(
        view.lines[4].spans[4].style.add_modifier,
        Modifier::REVERSED
    );
    assert_eq!(view.lines[4].spans[5].style, theme.style(Role::Dim));
    assert_eq!(
        last_row(&question, &ctx),
        "  Enter to choose with the note · Esc to close the note"
    );

    for c in "ship i".chars() {
        question.handle_key(key(KeyCode::Char(c)));
    }
    assert!(question.handle_paste("\x1b[31mnow\n"));
    let view = question.layout(&ctx);
    assert_eq!(plain_text(&view.lines[4..5]), "  ❯ 1. Merge, ship inow");
    assert_eq!(view.cursor, Some((23, 4)));
    assert_eq!(
        question.handle_key(key(KeyCode::Enter)),
        QuestionEvent::Answered {
            index: 0,
            note: Some("ship inow".into()),
            checked: Vec::new(),
        }
    );
    assert!(!question.has_open_note());
}

#[test]
fn a_long_note_wraps_under_the_option_and_grows_the_question() {
    let theme = theme();
    let ctx = ctx(40, &theme);
    let mut question = focused(merge_question());
    question.handle_key(key(KeyCode::Tab));
    let before = question.lines(&ctx).len();
    for c in "keep the commits separate and say why in the message ".chars() {
        question.handle_key(key(KeyCode::Char(c)));
    }
    let view = question.layout(&ctx);
    // The rule, the title, two rows of detail, and a blank row come first.
    assert_eq!(
        plain_text(&view.lines[5..7]),
        "  ❯ 1. Merge, keep the commits separate\n       and say why in the message"
    );
    assert_eq!(view.lines.len(), before + 1);
    assert_eq!(view.cursor, Some((34, 6)));
    for line in &view.lines {
        assert!(line.width() <= 40, "{line:?}");
    }
}

#[test]
fn a_note_edits_at_its_end_and_keeps_its_keys() {
    let mut question = focused(merge_question());
    let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
    let text = |question: &QuestionPrompt| question.note.as_ref().unwrap().text();
    question.handle_key(key(KeyCode::Tab));
    for c in "squash the commits".chars() {
        question.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(question.handle_key(ctrl('w')), QuestionEvent::Moved);
    assert_eq!(text(&question), "squash the ");
    for code in [KeyCode::Left, KeyCode::Right, KeyCode::Home, KeyCode::End] {
        assert_eq!(question.handle_key(key(code)), QuestionEvent::Moved);
    }
    question.handle_key(key(KeyCode::Char('e')));
    question.handle_key(key(KeyCode::Char('\u{301}')));
    question.handle_key(key(KeyCode::Backspace));
    assert_eq!(text(&question), "squash the ");

    assert_eq!(question.handle_key(ctrl('c')), QuestionEvent::Moved);
    assert_eq!(text(&question), "");
    assert_eq!(question.handle_key(ctrl('c')), QuestionEvent::Unhandled);
    question.handle_key(key(KeyCode::Char('x')));
    assert_eq!(question.handle_key(ctrl('u')), QuestionEvent::Moved);
    assert_eq!(text(&question), "");
    assert!(question.has_open_note());
}

#[test]
fn esc_closes_the_note_before_pinning() {
    let mut question = focused(merge_question());
    question.handle_key(key(KeyCode::Tab));
    question.handle_key(key(KeyCode::Char('y')));
    assert_eq!(question.note.as_ref().unwrap().text(), "y");
    assert_eq!(question.handle_key(key(KeyCode::Esc)), QuestionEvent::Moved);
    assert!(!question.dismissed);
    assert_eq!(
        question.handle_key(key(KeyCode::Esc)),
        QuestionEvent::Dismissed
    );
    assert!(question.dismissed);
}

#[test]
fn notes_need_permission() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut question = focused(merge_question());
    question.allow_note = false;
    assert_eq!(
        question.handle_key(key(KeyCode::Tab)),
        QuestionEvent::Unhandled
    );
    assert!(!question.handle_paste("text"));
    assert_eq!(
        last_row(&question, &ctx),
        "  Enter to choose · Esc to carry on without me"
    );
}

#[test]
fn a_long_label_wraps_under_itself_with_no_letter_column() {
    let theme = theme();
    let mut question = focused(QuestionPrompt::new(
        3,
        "codex hit its billing cap",
        "Fallback for task-003?",
        vec![
            QuestionOption::new("I topped up, retry"),
            QuestionOption::new(
                "Give task-003 to claude-code/opus, which has the most plan usage left today",
            ),
            QuestionOption::new("Take over myself"),
        ],
    ));
    question.question = Some("What now?".into());
    for width in [80, 60, 40] {
        let ctx = ctx(width, &theme);
        let lines = question.lines(&ctx);
        let text = plain_text(&lines);
        for start in ["1. I topped up, retry", "3. Take over myself"] {
            let row = text.lines().find(|row| row.contains(start)).unwrap();
            assert!(row.ends_with(start), "a letter at {width}: {row:?}\n{text}");
        }
        for line in &lines {
            assert!(line.width() <= usize::from(width), "{line:?}");
        }
    }
    let ctx = ctx(60, &theme);
    let text = plain_text(&question.lines(&ctx));
    assert!(
        text.contains(
            "    2. Give task-003 to claude-code/opus, which has the most\n       plan usage left today\n"
        ),
        "{text}"
    );
}

#[test]
fn a_question_question_has_chips_descriptions_and_its_own_answer() {
    let theme = theme();
    let ctx = ctx(70, &theme);
    let mut question = QuestionPrompt::new(
        4,
        "The goal needs one answer",
        "Goal question?",
        vec![
            QuestionOption::new("Per API key")
                .with_description("2 of 3 readings read it this way")
                .recommended(),
            QuestionOption::new("Per account").with_description("1 of 3 readings read it this way"),
            QuestionOption::free_text("Type my own answer"),
        ],
    );
    question.chips = vec![
        QuestionChip {
            label: "Rate limit".into(),
            answered: false,
        },
        QuestionChip {
            label: "Storage".into(),
            answered: false,
        },
    ];
    question.question = Some("Should the rate limit count per API key or per account?".into());
    question.focus();
    let lines = question.lines(&ctx);
    assert_eq!(
        plain_text(&lines[1..]),
        "◆ The goal needs one answer\n\
         \x20 ☐ Rate limit   ☐ Storage\n\
         \n\
         \x20 Should the rate limit count per API key or per account?\n\
         \x20 ❯ 1. Per API key (recommended)\n\
         \x20      2 of 3 readings read it this way\n\
         \x20   2. Per account\n\
         \x20      1 of 3 readings read it this way\n\
         \x20   3. Type my own answer\n\
         \n\
         \x20 Enter to choose · Esc to carry on without me"
    );
    let question_style = theme.style(Role::Question);
    assert_eq!(
        lines[2].spans[1].style,
        question_style.add_modifier(Modifier::REVERSED)
    );
    assert_eq!(lines[2].spans[3].style, theme.style(Role::Dim));
    assert!(
        lines[4].spans[1]
            .style
            .add_modifier
            .contains(Modifier::BOLD)
    );
    assert_eq!(lines[6].spans[1].style, theme.style(Role::Dim));
    assert_eq!(lines[5].spans[4].style, theme.style(Role::Dim));
    assert_eq!(lines[9].spans[3].style, theme.style(Role::Dim));

    assert_eq!(
        question.handle_key(key(KeyCode::Char('3'))),
        QuestionEvent::Moved
    );
    let view = question.layout(&ctx);
    assert_eq!(plain_text(&view.lines[9..10]), "  ❯ 3. Type my own answer");
    assert_eq!(view.cursor, Some((7, 9)));
    assert_eq!(
        last_row(&question, &ctx),
        "  Enter to answer · Esc to close"
    );
    assert_eq!(
        question.handle_key(key(KeyCode::Enter)),
        QuestionEvent::Moved
    );
    for c in "per org".chars() {
        question.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(plain_text(&question.lines(&ctx)[9..10]), "  ❯ 3. per org");
    assert_eq!(
        question.handle_key(key(KeyCode::Enter)),
        QuestionEvent::Answered {
            index: 2,
            note: Some("per org".into()),
            checked: Vec::new(),
        }
    );
}

#[test]
fn a_checklist_option_ticks_rows_and_sends_them() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut question = focused(merge_question());
    assert_eq!(
        question.handle_key(key(KeyCode::Char('3'))),
        QuestionEvent::Moved
    );
    let text = plain_text(&question.lines(&ctx));
    assert!(
        text.contains(
            "  ❯ 3. Request changes on some tasks\n\
             \x20      ❯ ☐ task-002 Add the JSON writer\n\
             \x20        ☐ task-003 Wire --json into the CLI\n"
        ),
        "{text}"
    );
    assert_eq!(
        last_row(&question, &ctx),
        "  Space to tick · Enter to send · Esc to go back"
    );
    assert_eq!(
        question.handle_key(key(KeyCode::Enter)),
        QuestionEvent::Moved
    );
    question.handle_key(key(KeyCode::Down));
    question.handle_key(key(KeyCode::Char(' ')));
    assert!(plain_text(&question.lines(&ctx)).contains("❯ ☒ task-003"));
    assert_eq!(
        question.handle_key(key(KeyCode::Enter)),
        QuestionEvent::Answered {
            index: 2,
            note: None,
            checked: vec![1],
        }
    );
    question.handle_key(key(KeyCode::Char('3')));
    assert_eq!(question.handle_key(key(KeyCode::Esc)), QuestionEvent::Moved);
    assert!(question.checklist.is_none() && question.focused);
}

#[test]
fn a_checklist_that_asks_for_a_note_opens_it_after_the_ticks() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut question = merge_question();
    question.options[2].asks_note = true;
    let mut question = focused(question);
    question.handle_key(key(KeyCode::Char('3')));
    assert_eq!(
        last_row(&question, &ctx),
        "  Space to tick · Enter to add a note · Esc to go back"
    );
    question.handle_key(key(KeyCode::Char(' ')));
    assert_eq!(
        question.handle_key(key(KeyCode::Enter)),
        QuestionEvent::Moved
    );
    let text = plain_text(&question.lines(&ctx));
    assert!(
        text.contains(
            "  ❯ 3. Request changes on some tasks, and add a note\n\
             \x20      ❯ ☒ task-002 Add the JSON writer\n"
        ),
        "{text}"
    );
    assert_eq!(
        last_row(&question, &ctx),
        "  Enter to send · Esc to go back"
    );

    // Esc goes back to the ticks, and the note opens again after them.
    for c in "wrong".chars() {
        question.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(question.handle_key(key(KeyCode::Esc)), QuestionEvent::Moved);
    assert!(question.note.is_none() && question.checklist.is_some());
    question.handle_key(key(KeyCode::Down));
    question.handle_key(key(KeyCode::Char(' ')));
    question.handle_key(key(KeyCode::Enter));
    for c in "split the writer".chars() {
        question.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(
        question.handle_key(key(KeyCode::Enter)),
        QuestionEvent::Answered {
            index: 2,
            note: Some("split the writer".into()),
            checked: vec![0, 1],
        }
    );
    assert!(question.checklist.is_none() && question.note.is_none());
}

#[test]
fn an_option_that_asks_for_a_note_opens_it_first() {
    let mut question = merge_question();
    question.options[1].asks_note = true;
    let mut question = focused(question);
    assert_eq!(
        question.handle_key(key(KeyCode::Char('2'))),
        QuestionEvent::Moved
    );
    assert_eq!(question.note.as_ref().map(|note| note.option), Some(1));
    for c in "draft".chars() {
        question.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(
        question.handle_key(key(KeyCode::Enter)),
        QuestionEvent::Answered {
            index: 1,
            note: Some("draft".into()),
            checked: Vec::new(),
        }
    );

    // An empty note answers on the second Enter, and a question without notes answers at once.
    let mut question = merge_question();
    question.options[1].asks_note = true;
    let mut question = focused(question);
    question.selected = 1;
    assert_eq!(
        question.handle_key(key(KeyCode::Enter)),
        QuestionEvent::Moved
    );
    assert_eq!(question.handle_key(key(KeyCode::Enter)), answered(1));
    let mut question = merge_question();
    question.options[1].asks_note = true;
    question.allow_note = false;
    let mut question = focused(question);
    assert_eq!(question.handle_key(key(KeyCode::Char('2'))), answered(1));
}

#[test]
fn a_body_sits_between_dashed_rules_and_a_footer_ends_the_question() {
    let theme = theme();
    let ctx = ctx(40, &theme);
    let mut question = QuestionPrompt::new(
        5,
        "Approve the plan?",
        "Plan?",
        vec![
            QuestionOption::new("Approve the plan"),
            QuestionOption::view("Show the plan"),
        ],
    );
    question.agent = Some(QuestionAgent {
        name: "GPT 5.6 on Codex".into(),
        worker: "codex/gpt-5.6".into(),
        color: Some(AgentColor::Purple),
        detail: "planner".into(),
    });
    question.body = vec!["1. Parse --json".into(), "2. Write JSON rows".into()];
    question.question = Some("Build it this way?".into());
    question.footer = Some("ctrl+g to edit the plan · plans/run-01.md".into());
    question.focus();
    let view = question.layout(&ctx);
    assert_eq!(
        plain_text(&view.lines[1..]),
        format!(
            "◆ Approve the plan?\n\
             \x20 Agent: GPT 5.6 on Codex · planner\n\
             {dashes}\n\
             \x20 1. Parse --json\n\
             \x20 2. Write JSON rows\n\
             {dashes}\n\
             \x20 Build it this way?\n\
             \x20 ❯ 1. Approve the plan\n\
             \x20   2. Show the plan\n\
             \n\
             \x20 Enter to choose\n\
             \x20 ctrl+g to edit the plan · plans/run-0…",
            dashes = "╌".repeat(40)
        )
    );
    assert_eq!(view.lines[2].spans[2].style, theme.style(Role::AgentPurple));
    assert_eq!(view.extra, 2..7);
    assert_eq!(view.tail, 3);
}

#[test]
fn untrusted_text_is_sanitized_and_wrapped_to_fit() {
    let theme = theme();
    let ctx = ctx(24, &theme);
    let mut question = focused(QuestionPrompt::new(
        1,
        "\x1b]0;pwned\x07Apply the change?",
        "Apply?",
        vec![QuestionOption::new("A very long option label indeed")],
    ));
    question.question = Some("Go\x1b[2J ahead?".into());
    question.detail = vec!["\x1b[31mred\x1b[0m".into()];
    let lines = question.lines(&ctx);
    let text = plain_text(&lines);
    assert!(!text.contains('\x1b'), "{text:?}");
    assert!(text.contains("◆ Apply the change?"), "{text}");
    assert!(text.contains("  Go ahead?"), "{text}");
    assert!(
        text.contains("  ❯ 1. A very long\n       option label\n       indeed"),
        "{text}"
    );
    for line in &lines {
        assert!(line.width() <= 24, "{line:?}");
    }
}

#[test]
fn a_toggle_flips_in_place_and_never_answers() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut question = merge_question();
    question
        .options
        .push(QuestionOption::toggle("Remember this pick", false));
    let mut question = focused(question);
    let toggle = question.options.len() - 1;

    assert_eq!(
        question.handle_key(key(KeyCode::Char('7'))),
        QuestionEvent::OpenView { index: toggle }
    );
    assert!(question.options[toggle].on);
    assert_eq!(
        question.selected, 0,
        "the pointer stays where the answer is"
    );
    let row = |question: &QuestionPrompt| {
        let lines = question.lines(&ctx);
        plain_text(&lines)
            .lines()
            .find(|row| row.contains("Remember this pick"))
            .map(str::to_owned)
            .unwrap()
    };
    assert!(
        row(&question).contains("☒ Remember this pick"),
        "{}",
        row(&question)
    );

    question.selected = toggle;
    assert_eq!(
        question.handle_key(key(KeyCode::Enter)),
        QuestionEvent::OpenView { index: toggle }
    );
    assert!(!question.options[toggle].on);
    assert!(row(&question).contains("☐ Remember this pick"));
    assert_eq!(
        question.handle_key(key(KeyCode::Tab)),
        QuestionEvent::Unhandled
    );
    assert_eq!(
        last_row(&question, &ctx),
        "  Enter to switch · Esc to carry on without me"
    );
}

#[test]
fn the_hint_line_says_what_esc_does() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut question = focused(merge_question());
    question.esc_hint = "Esc to cancel".into();
    question.selected = 3;
    assert_eq!(
        last_row(&question, &ctx),
        "  Enter to choose · Tab to add a note · Esc to cancel"
    );
}

#[test]
fn a_question_you_set_off_holds_the_keys_until_answered() {
    let mut question = QuestionPrompt::new(
        1,
        "Codex is out until 4:10 PM. Who carries on?",
        "Who carries on?",
        vec![
            QuestionOption::new("Claude Code").recommended(),
            QuestionOption::new("agy"),
            QuestionOption::new("OpenCode"),
            QuestionOption::new("Wait for Codex"),
        ],
    )
    .yours();
    assert!(
        question.focused && !question.waits(),
        "it opens with the keys"
    );
    for c in ['s', 'm', 'p', 'e', '/'] {
        assert_eq!(
            question.handle_key(key(KeyCode::Char(c))),
            QuestionEvent::Unhandled
        );
    }
    for _ in 0..question.options.len() {
        assert_eq!(
            question.handle_key(key(KeyCode::Down)),
            QuestionEvent::Moved
        );
    }
    assert!(question.focused, "↓  past the last option keeps the keys");
    assert_eq!(question.selected, 3);
    assert_eq!(question.handle_key(key(KeyCode::Char('2'))), answered(1));
    assert_eq!(
        question.handle_key(key(KeyCode::Esc)),
        QuestionEvent::Dismissed
    );
    assert!(!question.focused);
}

#[test]
fn a_question_that_needs_you_takes_the_keys_until_set_aside() {
    let now = Instant::now();
    let mut question = merge_question();
    question.needs_you = true;
    assert!(question.waits());
    assert!(question.take_keys(now), "it takes the keys as it shows");
    assert!(question.focused && !question.take_keys(now));
    for c in ['s', 'm', '/'] {
        assert_eq!(
            question.handle_key(key(KeyCode::Char(c))),
            QuestionEvent::Unhandled
        );
    }
    for _ in 0..question.options.len() {
        assert_eq!(
            question.handle_key(key(KeyCode::Down)),
            QuestionEvent::Moved
        );
    }
    assert!(question.focused, "↓  past the last option keeps the keys");
    assert_eq!(
        question.handle_key(key(KeyCode::Esc)),
        QuestionEvent::Dismissed
    );
    assert!(
        !question.take_keys(now),
        "set aside, it leaves the keys alone"
    );
    question.focus();
    assert_eq!(question.handle_key(key(KeyCode::Char('2'))), answered(1));
}

#[test]
fn letters_do_nothing_and_a_digit_picks() {
    let mut question = focused(merge_question());
    for c in "more thing".chars() {
        assert_eq!(
            question.handle_key(key(KeyCode::Char(c))),
            QuestionEvent::Unhandled
        );
    }
    assert_eq!(question.selected, 0);
    // Merge can't be undone, so its number arms it and Enter confirms.
    assert_eq!(
        question.handle_key(key(KeyCode::Char('1'))),
        QuestionEvent::Moved
    );
    assert_eq!(question.armed, Some(0));
    assert_eq!(question.handle_key(key(KeyCode::Enter)), answered(0));
    // A letter after the number disarms it, so the Enter that ends a line confirms nothing.
    let mut question = focused(merge_question());
    question.handle_key(key(KeyCode::Char('1')));
    question.handle_key(key(KeyCode::Char('x')));
    assert_eq!(question.armed, None);
    // Every other option answers at its number.
    let mut question = focused(merge_question());
    assert_eq!(question.handle_key(key(KeyCode::Char('2'))), answered(1));
}

/// The input hides while a question has the keys, so the question's own key line names them,
/// and it never talks about typing.
#[test]
fn the_key_line_names_the_keys_while_a_question_has_them() {
    let theme = theme();
    let ctx = ctx(120, &theme);
    let mut question = focused(merge_question());
    let first = last_row(&question, &ctx);
    assert!(first.ends_with("Esc to carry on without me"), "{first}");
    question.handle_key(key(KeyCode::Char('5')));
    assert!(last_row(&question, &ctx).ends_with("· Esc to cancel"));
    question.handle_key(key(KeyCode::Char('y')));
    assert_eq!(last_row(&question, &ctx), first);
}

/// A question that took the keys by itself at `now`, as one work waits on does as it shows.
fn took_the_keys_at(now: Instant) -> QuestionPrompt {
    let mut question = merge_question();
    question.needs_you = true;
    assert!(question.take_keys(now));
    assert!(!question.engaged);
    question
}

#[test]
fn vim_keys_answer_once_the_question_is_engaged() {
    // Once you've moved with the arrows, `l` answers as Enter does.
    let mut question = took_the_keys_at(Instant::now());
    question.read_key(key(KeyCode::Down), true);
    question.handle_key(key(KeyCode::Down));
    let read = question.read_key(key(KeyCode::Char('l')), true);
    assert_eq!(question.handle_key(read), answered(1));
    // A question you brought up yourself takes them from the start.
    let mut question = focused(merge_question());
    let read = question.read_key(key(KeyCode::Char('j')), true);
    question.handle_key(read);
    let read = question.read_key(key(KeyCode::Char('l')), true);
    assert_eq!(question.handle_key(read), answered(1));
}

/// A line typed as a question shows is dropped whole, each key keeping the guard up, and the
/// first key after a pause works, as Claude Code's dialogs do.
#[test]
fn a_burst_typed_as_a_question_shows_is_dropped_whole() {
    let t0 = Instant::now();
    let mut question = took_the_keys_at(t0);
    let mut now = t0;
    let line = "let me check 2".chars().map(KeyCode::Char);
    for code in line.chain([KeyCode::Enter]) {
        now += Duration::from_millis(100);
        assert!(question.held_off(&key(code), now), "{code:?}");
    }
    assert!(!question.held_off(&key(KeyCode::Char('2')), now + QUESTION_GUARD));
}

/// For a moment after a question shows, only the arrows work on it: Enter, Esc, Tab and the
/// digits meant for what had the keys do nothing. That holds for a question you set off too.
/// ↑ hands it the keys with nothing held off.
#[test]
fn a_question_that_just_showed_holds_off_all_but_the_arrows() {
    let t0 = Instant::now();
    let soon = t0 + QUESTION_GUARD / 2;
    let mut question = took_the_keys_at(t0);
    for code in [
        KeyCode::Enter,
        KeyCode::Esc,
        KeyCode::Tab,
        KeyCode::Char('1'),
    ] {
        assert!(question.held_off(&key(code), soon), "{code:?}");
    }
    for code in [KeyCode::Up, KeyCode::Down, KeyCode::Home, KeyCode::End] {
        assert!(!question.held_off(&key(code), soon), "{code:?}");
    }
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert!(
        !question.held_off(&ctrl_c, soon),
        "the quit keys still work"
    );
    let later = soon + QUESTION_GUARD;
    assert!(!question.held_off(&key(KeyCode::Enter), later));

    question.handle_key(key(KeyCode::Esc));
    assert!(question.dismissed);
    question.focus();
    assert!(
        !question.held_off(&key(KeyCode::Char('2')), soon),
        "↑  hands it over"
    );

    let mut yours = merge_question().yours();
    assert!(!yours.take_keys(t0), "it has them already");
    assert!(yours.held_off(&key(KeyCode::Enter), soon));
}

/// A question an agent asks in its own words, with nothing to pick from.
fn free_text_question() -> QuestionPrompt {
    let mut question = QuestionPrompt::new(
        3,
        "Question from build:greet",
        "Question from build:greet",
        vec![QuestionOption::free_text("Type my own answer")],
    );
    question.question = Some("Should greet.py say hello or hi?".into());
    question.needs_you = true;
    question
}

fn typed(question: &mut QuestionPrompt, text: &str) {
    for c in text.chars() {
        assert_eq!(
            question.handle_key(key(KeyCode::Char(c))),
            QuestionEvent::Moved
        );
    }
}

/// A free-text question is its answer field, open from the start: no list of one, and no
/// Enter to open it.
#[test]
fn a_free_text_question_shows_its_answer_field_open() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let question = focused(free_text_question());
    assert!(question.is_answer_field() && question.has_open_note());
    let view = question.layout(&ctx);
    let text = plain_text(&view.lines);
    assert!(text.contains("\n  ❯ Type your answer\n"), "{text}");
    assert!(!text.contains("1."), "{text}");
    assert!(!text.contains("Type my own answer"), "{text}");
    assert_eq!(
        last_row(&question, &ctx),
        "  Enter to send · Esc to carry on without me"
    );
    let row = view
        .lines
        .iter()
        .position(|line| plain_text(std::slice::from_ref(line)).contains("Type your answer"))
        .unwrap();
    assert_eq!(
        view.cursor,
        Some((4, row as u16)),
        "on the placeholder's first letter"
    );

    let waiting = free_text_question().layout(&ctx);
    let text = plain_text(&waiting.lines);
    assert!(text.contains("\n    Type your answer\n"), "{text}");
    assert_eq!(waiting.cursor, None, "the input under it has the cursor");
}

#[test]
fn the_answer_field_takes_every_key_and_enter_sends_it() {
    let mut question = free_text_question();
    assert!(question.take_keys(Instant::now()));
    assert_eq!(
        question.handle_key(key(KeyCode::Enter)),
        QuestionEvent::Moved,
        "an empty answer sends nothing"
    );
    typed(&mut question, "1 hi, then j k");
    for code in [KeyCode::Up, KeyCode::Down, KeyCode::Tab] {
        assert_eq!(question.handle_key(key(code)), QuestionEvent::Moved);
    }
    assert_eq!(question.note.as_ref().unwrap().text(), "1 hi, then j k");
    assert_eq!(
        question.handle_key(key(KeyCode::Enter)),
        QuestionEvent::Answered {
            index: 0,
            note: Some("1 hi, then j k".into()),
            checked: Vec::new(),
        }
    );
}

#[test]
fn esc_takes_the_answer_field_off_the_screen_and_keeps_what_you_typed() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut question = focused(free_text_question());
    typed(&mut question, "hi");
    assert_eq!(
        question.handle_key(key(KeyCode::Esc)),
        QuestionEvent::Dismissed
    );
    assert!(question.dismissed && !question.focused);
    assert!(question.lines(&ctx).is_empty());
    question.focus();
    assert_eq!(question.note.as_ref().unwrap().text(), "hi");
    assert!(plain_text(&question.lines(&ctx)).contains("  ❯ hi"));
}

#[test]
fn down_hands_the_keys_back_from_an_answer_field_that_waits() {
    let mut question = free_text_question();
    question.needs_you = false;
    question.focus();
    assert_eq!(
        question.handle_key(key(KeyCode::Down)),
        QuestionEvent::BackToInput
    );
    assert!(!question.focused && question.has_open_note());
}

/// The answer field is the input's own editor: Shift+Enter, Alt+Enter, and Ctrl+J start a new
/// line, a paste keeps its line breaks, and the arrows, Home, and End move the cursor. Only
/// Enter alone sends, and it sends every line.
#[test]
fn the_answer_field_edits_like_the_input() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut question = focused(free_text_question());
    typed(&mut question, "line one");
    for modifiers in [KeyModifiers::SHIFT, KeyModifiers::ALT] {
        let event = question.handle_key(KeyEvent::new(KeyCode::Enter, modifiers));
        assert_eq!(
            event,
            QuestionEvent::Moved,
            "{modifiers:?} starts a new line"
        );
    }
    let ctrl_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL);
    assert_eq!(question.handle_key(ctrl_j), QuestionEvent::Moved);
    assert_eq!(question.note.as_ref().unwrap().text(), "line one\n\n\n");

    assert!(question.handle_paste("line two\n\n  line three"));
    assert_eq!(
        question.note.as_ref().unwrap().text(),
        "line one\n\n\nline two\n\n  line three"
    );
    let text = plain_text(&question.layout(&ctx).lines);
    assert!(text.contains("  ❯ line one\n"), "{text}");
    assert!(text.contains("\n    line two\n"), "{text}");
    assert!(text.contains("\n      line three"), "{text}");

    // Home goes to the line's start, and ↑ to the row above, without leaving the question. Keys
    // that have nowhere to go stay in the field too.
    for code in [KeyCode::Home, KeyCode::Home, KeyCode::Up, KeyCode::Up] {
        assert_eq!(
            question.handle_key(key(code)),
            QuestionEvent::Moved,
            "{code:?}"
        );
    }
    typed(&mut question, "and ");
    assert_eq!(question.handle_key(key(KeyCode::End)), QuestionEvent::Moved);
    assert_eq!(question.handle_key(key(KeyCode::End)), QuestionEvent::Moved);
    assert_eq!(
        question.handle_key(key(KeyCode::Left)),
        QuestionEvent::Moved
    );
    assert_eq!(
        question.handle_key(key(KeyCode::Right)),
        QuestionEvent::Moved
    );
    typed(&mut question, "!");
    assert_eq!(
        question.note.as_ref().unwrap().text(),
        "line one\n\n\nand line two!\n\n  line three"
    );
    assert!(question.focused, "the arrows stayed in the field");

    assert_eq!(
        question.handle_key(key(KeyCode::Enter)),
        QuestionEvent::Answered {
            index: 0,
            note: Some("line one\n\n\nand line two!\n\n  line three".into()),
            checked: Vec::new(),
        }
    );
}

/// The own answer under a question's choices edits the same way, and `!` is a letter there,
/// never the input's shell mode.
#[test]
fn an_own_answer_under_choices_edits_like_the_input_too() {
    let mut question = QuestionPrompt::new(
        4,
        "Question from build:greet",
        "Question from build:greet",
        vec![
            QuestionOption::new("hello"),
            QuestionOption::free_text("Type my own answer"),
        ],
    );
    question.focus();
    assert_eq!(
        question.handle_key(key(KeyCode::Char('2'))),
        QuestionEvent::Moved
    );
    typed(&mut question, "!both");
    let shift_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
    assert_eq!(question.handle_key(shift_enter), QuestionEvent::Moved);
    typed(&mut question, "said once");
    assert_eq!(
        question.handle_key(key(KeyCode::Enter)),
        QuestionEvent::Answered {
            index: 1,
            note: Some("!both\nsaid once".into()),
            checked: Vec::new(),
        }
    );
}

/// With choices, the own answer stays their last option and opens its field in the question.
#[test]
fn choices_with_an_own_answer_keep_it_as_an_option() {
    let mut question = QuestionPrompt::new(
        4,
        "Question from build:greet",
        "Question from build:greet",
        vec![
            QuestionOption::new("hello"),
            QuestionOption::new("hi"),
            QuestionOption::free_text("Type my own answer"),
        ],
    );
    assert!(!question.is_answer_field() && !question.has_open_note());
    question.focus();
    assert_eq!(
        question.handle_key(key(KeyCode::Char('3'))),
        QuestionEvent::Moved
    );
    assert!(question.has_open_note());
    assert_eq!(question.handle_key(key(KeyCode::Esc)), QuestionEvent::Moved);
    assert!(
        !question.has_open_note() && question.focused,
        "Esc closes the field first"
    );
}
