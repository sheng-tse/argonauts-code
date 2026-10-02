use std::time::Instant;

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

fn merge_card() -> CardPrompt {
    let mut card = CardPrompt::new(
        7,
        "Merge this run into main?",
        "Merge run-01?",
        vec![
            CardOption::irreversible("Merge"),
            CardOption::new("Open a pull request"),
            CardOption::checklist(
                "Request changes on some cards",
                vec![
                    "card-002 Add the JSON writer".into(),
                    "card-003 Wire --json into the CLI".into(),
                ],
            ),
            CardOption::new("Keep the branch"),
            CardOption::irreversible("Abandon the run"),
            CardOption::view("Show the diff"),
        ],
    );
    card.detail = vec!["4 cards · 7 files · +120 -18 · full suite passed".into()];
    card.allow_note = true;
    card
}

fn focused(mut card: CardPrompt) -> CardPrompt {
    card.focus();
    card
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctx(width: u16, theme: &Theme) -> RenderCtx<'_> {
    RenderCtx::new(width, theme, Clock::still(Instant::now()))
}

fn answered(index: usize) -> CardEvent {
    CardEvent::Answered {
        index,
        note: None,
        checked: Vec::new(),
    }
}

fn last_row(card: &CardPrompt, ctx: &RenderCtx<'_>) -> String {
    let lines = card.lines(ctx);
    plain_text(&lines[lines.len() - 1..])
}

#[test]
fn expanded_reads_like_the_reference() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut card = focused(merge_card());
    card.question = Some("Merge it now?".into());
    let view = card.layout(&ctx);
    assert_eq!(
        plain_text(&view.lines),
        format!(
            "{}\n\
             ◆ Merge this run into main?\n\
             \x20 4 cards · 7 files · +120 -18 · full suite passed\n\
             \n\
             \x20 Merge it now?\n\
             \x20 ❯ 1. Merge\n\
             \x20   2. Open a pull request\n\
             \x20   3. Request changes on some cards\n\
             \x20   4. Keep the branch\n\
             \x20   5. Abandon the run\n\
             \x20   6. Show the diff\n\
             \n\
             \x20 Enter to choose · Tab to add a note · Esc to set aside",
            "─".repeat(80)
        )
    );
    let lines = &view.lines;
    let card_style = theme.style(Role::Card);
    let dim = theme.style(Role::Dim);
    assert_eq!(lines[0].spans[0].style, card_style);
    assert_eq!(lines[1].spans[0].style, theme.style(Role::Warning));
    assert_eq!(
        lines[1].spans[1].style,
        card_style.add_modifier(Modifier::BOLD)
    );
    assert_eq!(lines[2].spans[1].style, dim);
    assert_eq!(lines[5].spans[1].style, card_style);
    assert_eq!(lines[5].spans[2].style, dim);
    assert_eq!(lines[5].spans[3].style, card_style);
    assert_eq!(lines[5].spans.len(), 4, "no letter after the label");
    assert_eq!(lines[6].spans[2].style, dim);
    assert_eq!(lines[6].spans[3].style, theme.style(Role::Text));
    // `extra` is the details and their blank row; `tail` the hints and theirs.
    assert_eq!(view.extra, 2..4);
    assert_eq!(view.tail, 2);
}

#[test]
fn a_new_card_waits_without_the_keys() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut card = merge_card();
    assert!(card.waits());
    let lines = card.lines(&ctx);
    assert_eq!(plain_text(&lines[4..5]), "    1. Merge");
    assert_eq!(lines[4].spans[3].style, theme.style(Role::Text));
    assert_eq!(last_row(&card, &ctx), "  ↑ to answer");
    for code in [
        KeyCode::Char('y'),
        KeyCode::Char('1'),
        KeyCode::Enter,
        KeyCode::Esc,
    ] {
        assert_eq!(card.handle_key(key(code)), CardEvent::Unhandled);
    }
    assert!(!card.handle_paste("text"));
    card.focus();
    assert!(!card.waits());
    assert_eq!(card.handle_key(key(KeyCode::Char('4'))), answered(3));
}

#[test]
fn pinned_is_one_line_and_gives_the_keys_back() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut card = focused(merge_card());
    assert_eq!(card.handle_key(key(KeyCode::Esc)), CardEvent::Pinned);
    assert!(!card.focused);
    assert_eq!(
        plain_text(&card.lines(&ctx)),
        "◆ Merge run-01? · needs you · ↑ to answer"
    );
    assert_eq!(
        card.handle_key(key(KeyCode::Char('2'))),
        CardEvent::Unhandled
    );
    card.focus();
    assert!(!card.pinned);
}

#[test]
fn keys_move_and_answer() {
    let mut card = focused(merge_card());
    assert_eq!(card.handle_key(key(KeyCode::Up)), CardEvent::Moved);
    assert_eq!(card.selected, 0);
    card.handle_key(key(KeyCode::Down));
    assert_eq!(card.handle_key(key(KeyCode::Enter)), answered(1));
    assert_eq!(card.handle_key(key(KeyCode::Char('4'))), answered(3));
    assert_eq!(card.handle_key(key(KeyCode::Char('2'))), answered(1));
    for c in ['9', '0', 'q'] {
        assert_eq!(card.handle_key(key(KeyCode::Char(c))), CardEvent::Unhandled);
    }
    // Options answer to their numbers alone, never to letters.
    for c in ['y', 'p', 'P', 'k', 'd'] {
        assert_eq!(card.handle_key(key(KeyCode::Char(c))), CardEvent::Unhandled);
    }
}

#[test]
fn down_past_the_last_option_hands_the_keys_back() {
    let welcome = CardPrompt::new(
        1,
        "Welcome to Argonauts",
        "Welcome",
        vec![CardOption::new("Continue").recommended()],
    );
    let mut card = focused(welcome);
    assert_eq!(card.handle_key(key(KeyCode::Down)), CardEvent::BackToInput);
    assert!(card.waits() && !card.pinned);
    assert_eq!(card.handle_key(key(KeyCode::Enter)), CardEvent::Unhandled);
    card.focus();
    assert_eq!(card.handle_key(key(KeyCode::Up)), CardEvent::Moved);
    assert_eq!(card.handle_key(key(KeyCode::Enter)), answered(0));

    let mut card = focused(merge_card());
    for _ in 1..card.options.len() {
        assert_eq!(card.handle_key(key(KeyCode::Down)), CardEvent::Moved);
    }
    assert_eq!(card.handle_key(key(KeyCode::Down)), CardEvent::BackToInput);
    assert_eq!(card.selected, card.options.len() - 1);
    card.focus();
    assert_eq!(card.handle_key(key(KeyCode::Up)), CardEvent::Moved);
    assert_eq!(card.handle_key(key(KeyCode::Esc)), CardEvent::Pinned);

    // An armed option lets go, so the key that comes back can't answer it unseen.
    let mut card = focused(CardPrompt::new(
        2,
        "Abandon run-01?",
        "Abandon?",
        vec![
            CardOption::new("Keep it"),
            CardOption::irreversible("Abandon the run"),
        ],
    ));
    card.handle_key(key(KeyCode::Char('2')));
    assert_eq!(card.armed, Some(1));
    assert_eq!(card.handle_key(key(KeyCode::Down)), CardEvent::BackToInput);
    assert_eq!(card.armed, None);
}

#[test]
fn down_inside_an_open_checklist_stays_in_it() {
    let mut card = focused(merge_card());
    card.handle_key(key(KeyCode::Char('3')));
    assert!(card.checklist.is_some());
    for _ in 0..3 {
        assert_eq!(card.handle_key(key(KeyCode::Down)), CardEvent::Moved);
    }
    assert!(card.focused);
    assert_eq!(card.checklist.as_ref().map(|list| list.cursor), Some(1));
}

#[test]
fn vim_keys_stand_in_for_the_arrows_inside_a_checklist_too() {
    let mut card = focused(merge_card());
    let vim = |card: &mut CardPrompt, c: char| {
        let key = card.vim_key(key(KeyCode::Char(c)));
        card.handle_key(key)
    };
    vim(&mut card, 'G');
    assert_eq!(card.selected, 5);
    vim(&mut card, 'g');
    vim(&mut card, 'j');
    vim(&mut card, 'j');
    assert_eq!(card.selected, 2);
    vim(&mut card, 'l');
    assert!(card.checklist.is_some());
    vim(&mut card, 'G');
    assert_eq!(card.checklist.as_ref().map(|list| list.cursor), Some(1));
    vim(&mut card, 'k');
    assert_eq!(card.checklist.as_ref().map(|list| list.cursor), Some(0));
    // `h` closes the checklist, as Esc does, then sets the card aside.
    assert_eq!(vim(&mut card, 'h'), CardEvent::Moved);
    assert!(card.checklist.is_none() && card.focused);
    assert_eq!(vim(&mut card, 'h'), CardEvent::Pinned);

    // Home and End go to the first and last option without vim keys.
    let mut card = focused(merge_card());
    card.handle_key(key(KeyCode::End));
    assert_eq!(card.selected, 5);
    card.handle_key(key(KeyCode::Home));
    assert_eq!(card.selected, 0);
}

/// Esc backs out of a confirm; when the option itself cancels, the hint says so in other words.
#[test]
fn confirming_a_cancel_offers_a_way_back() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut card = focused(CardPrompt::new(
        8,
        "card-001 escalated · Fix add",
        "What now?",
        vec![
            CardOption::new("Retry with a note"),
            CardOption::irreversible("Cancel the card"),
        ],
    ));
    card.handle_key(key(KeyCode::Char('2')));
    assert_eq!(
        last_row(&card, &ctx),
        "  Enter to cancel the card · Esc to go back"
    );
}

#[test]
fn an_option_that_cant_be_undone_waits_for_enter() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut card = focused(merge_card());
    card.selected = 3;
    assert_eq!(card.handle_key(key(KeyCode::Char('1'))), CardEvent::Moved);
    assert_eq!((card.selected, card.armed), (0, Some(0)));
    assert_eq!(last_row(&card, &ctx), "  Enter to merge · Esc to cancel");
    assert_eq!(card.handle_key(key(KeyCode::Esc)), CardEvent::Moved);
    assert_eq!(card.armed, None);
    assert!(card.focused && !card.pinned);
    card.handle_key(key(KeyCode::Char('1')));
    assert_eq!(card.handle_key(key(KeyCode::Enter)), answered(0));
    assert_eq!(card.handle_key(key(KeyCode::Char('5'))), CardEvent::Moved);
    assert_eq!(
        last_row(&card, &ctx),
        "  Enter to abandon the run · Esc to cancel"
    );
    assert_eq!(card.handle_key(key(KeyCode::Char('5'))), answered(4));
    card.handle_key(key(KeyCode::Char('1')));
    card.handle_key(key(KeyCode::Down));
    assert_eq!(card.armed, None);
    assert_eq!(card.handle_key(key(KeyCode::Char('2'))), answered(1));
    card.selected = 0;
    assert_eq!(card.handle_key(key(KeyCode::Enter)), answered(0));
}

#[test]
fn a_view_option_opens_and_keeps_the_card() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut card = focused(merge_card());
    assert_eq!(
        card.handle_key(key(KeyCode::Char('6'))),
        CardEvent::OpenView { index: 5 }
    );
    assert!(!card.pinned);
    assert_eq!(card.selected, 0);
    card.selected = 5;
    assert_eq!(
        card.handle_key(key(KeyCode::Enter)),
        CardEvent::OpenView { index: 5 }
    );
    assert_eq!(card.handle_key(key(KeyCode::Tab)), CardEvent::Unhandled);
    assert_eq!(last_row(&card, &ctx), "  Enter to open · Esc to set aside");
}

#[test]
fn tab_turns_the_label_into_the_start_of_a_note() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut card = focused(merge_card());
    assert_eq!(card.handle_key(key(KeyCode::Tab)), CardEvent::Moved);
    let view = card.layout(&ctx);
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
        last_row(&card, &ctx),
        "  Enter to choose with the note · Esc to close the note"
    );

    for c in "ship i".chars() {
        card.handle_key(key(KeyCode::Char(c)));
    }
    assert!(card.handle_paste("\x1b[31mnow\n"));
    let view = card.layout(&ctx);
    assert_eq!(plain_text(&view.lines[4..5]), "  ❯ 1. Merge, ship inow");
    assert_eq!(view.cursor, Some((23, 4)));
    assert_eq!(
        card.handle_key(key(KeyCode::Enter)),
        CardEvent::Answered {
            index: 0,
            note: Some("ship inow".into()),
            checked: Vec::new(),
        }
    );
    assert!(!card.has_open_note());
}

#[test]
fn a_long_note_wraps_under_the_option_and_grows_the_card() {
    let theme = theme();
    let ctx = ctx(40, &theme);
    let mut card = focused(merge_card());
    card.handle_key(key(KeyCode::Tab));
    let before = card.lines(&ctx).len();
    for c in "keep the commits separate and say why in the message ".chars() {
        card.handle_key(key(KeyCode::Char(c)));
    }
    let view = card.layout(&ctx);
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
    let mut card = focused(merge_card());
    let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
    let text = |card: &CardPrompt| card.note.as_ref().unwrap().text.clone();
    card.handle_key(key(KeyCode::Tab));
    for c in "squash the commits".chars() {
        card.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(card.handle_key(ctrl('w')), CardEvent::Moved);
    assert_eq!(text(&card), "squash the ");
    for code in [KeyCode::Left, KeyCode::Right, KeyCode::Home, KeyCode::End] {
        assert_eq!(card.handle_key(key(code)), CardEvent::Moved);
    }
    card.handle_key(key(KeyCode::Char('e')));
    card.handle_key(key(KeyCode::Char('\u{301}')));
    card.handle_key(key(KeyCode::Backspace));
    assert_eq!(text(&card), "squash the ");

    assert_eq!(card.handle_key(ctrl('c')), CardEvent::Moved);
    assert_eq!(text(&card), "");
    assert_eq!(card.handle_key(ctrl('c')), CardEvent::Unhandled);
    card.handle_key(key(KeyCode::Char('x')));
    assert_eq!(card.handle_key(ctrl('u')), CardEvent::Moved);
    assert_eq!(text(&card), "");
    assert!(card.has_open_note());
}

#[test]
fn esc_closes_the_note_before_pinning() {
    let mut card = focused(merge_card());
    card.handle_key(key(KeyCode::Tab));
    card.handle_key(key(KeyCode::Char('y')));
    assert_eq!(card.note.as_ref().unwrap().text, "y");
    assert_eq!(card.handle_key(key(KeyCode::Esc)), CardEvent::Moved);
    assert!(!card.pinned);
    assert_eq!(card.handle_key(key(KeyCode::Esc)), CardEvent::Pinned);
    assert!(card.pinned);
}

#[test]
fn notes_need_permission() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut card = focused(merge_card());
    card.allow_note = false;
    assert_eq!(card.handle_key(key(KeyCode::Tab)), CardEvent::Unhandled);
    assert!(!card.handle_paste("text"));
    assert_eq!(
        last_row(&card, &ctx),
        "  Enter to choose · Esc to set aside"
    );
}

#[test]
fn a_long_label_wraps_under_itself_with_no_letter_column() {
    let theme = theme();
    let mut card = focused(CardPrompt::new(
        3,
        "codex hit its billing cap",
        "Hand off card-003?",
        vec![
            CardOption::new("I topped up, retry"),
            CardOption::new(
                "Hand card-003 to claude-code/opus, which has the most quota left today",
            ),
            CardOption::new("Take over myself"),
        ],
    ));
    card.question = Some("What now?".into());
    for width in [80, 60, 40] {
        let ctx = ctx(width, &theme);
        let lines = card.lines(&ctx);
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
    let text = plain_text(&card.lines(&ctx));
    assert!(
        text.contains(
            "    2. Hand card-003 to claude-code/opus, which has the most\n       quota left today\n"
        ),
        "{text}"
    );
}

#[test]
fn a_question_card_has_chips_descriptions_and_its_own_answer() {
    let theme = theme();
    let ctx = ctx(70, &theme);
    let mut card = CardPrompt::new(
        4,
        "The goal needs one answer",
        "Goal question?",
        vec![
            CardOption::new("Per API key")
                .with_description("2 of 3 restatements read it this way")
                .recommended(),
            CardOption::new("Per account").with_description("1 of 3 restatements read it this way"),
            CardOption::free_text("Type my own answer"),
        ],
    );
    card.chips = vec![
        CardChip {
            label: "Rate limit".into(),
            answered: false,
        },
        CardChip {
            label: "Storage".into(),
            answered: false,
        },
    ];
    card.question = Some("Should the rate limit count per API key or per account?".into());
    card.focus();
    let lines = card.lines(&ctx);
    assert_eq!(
        plain_text(&lines[1..]),
        "◆ The goal needs one answer\n\
         \x20 ☐ Rate limit   ☐ Storage\n\
         \n\
         \x20 Should the rate limit count per API key or per account?\n\
         \x20 ❯ 1. Per API key (recommended)\n\
         \x20      2 of 3 restatements read it this way\n\
         \x20   2. Per account\n\
         \x20      1 of 3 restatements read it this way\n\
         \x20   3. Type my own answer\n\
         \n\
         \x20 Enter to choose · Esc to set aside"
    );
    let card_style = theme.style(Role::Card);
    assert_eq!(
        lines[2].spans[1].style,
        card_style.add_modifier(Modifier::REVERSED)
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

    assert_eq!(card.handle_key(key(KeyCode::Char('3'))), CardEvent::Moved);
    let view = card.layout(&ctx);
    assert_eq!(plain_text(&view.lines[9..10]), "  ❯ 3. Type my own answer");
    assert_eq!(view.cursor, Some((7, 9)));
    assert_eq!(last_row(&card, &ctx), "  Enter to answer · Esc to close");
    assert_eq!(card.handle_key(key(KeyCode::Enter)), CardEvent::Moved);
    for c in "per org".chars() {
        card.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(plain_text(&card.lines(&ctx)[9..10]), "  ❯ 3. per org");
    assert_eq!(
        card.handle_key(key(KeyCode::Enter)),
        CardEvent::Answered {
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
    let mut card = focused(merge_card());
    assert_eq!(card.handle_key(key(KeyCode::Char('3'))), CardEvent::Moved);
    let text = plain_text(&card.lines(&ctx));
    assert!(
        text.contains(
            "  ❯ 3. Request changes on some cards\n\
             \x20      ❯ ☐ card-002 Add the JSON writer\n\
             \x20        ☐ card-003 Wire --json into the CLI\n"
        ),
        "{text}"
    );
    assert_eq!(
        last_row(&card, &ctx),
        "  Space to tick · Enter to send · Esc to go back"
    );
    assert_eq!(card.handle_key(key(KeyCode::Enter)), CardEvent::Moved);
    card.handle_key(key(KeyCode::Down));
    card.handle_key(key(KeyCode::Char(' ')));
    assert!(plain_text(&card.lines(&ctx)).contains("❯ ☒ card-003"));
    assert_eq!(
        card.handle_key(key(KeyCode::Enter)),
        CardEvent::Answered {
            index: 2,
            note: None,
            checked: vec![1],
        }
    );
    card.handle_key(key(KeyCode::Char('3')));
    assert_eq!(card.handle_key(key(KeyCode::Esc)), CardEvent::Moved);
    assert!(card.checklist.is_none() && card.focused);
}

#[test]
fn a_checklist_that_asks_for_a_note_opens_it_after_the_ticks() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut card = merge_card();
    card.options[2].asks_note = true;
    let mut card = focused(card);
    card.handle_key(key(KeyCode::Char('3')));
    assert_eq!(
        last_row(&card, &ctx),
        "  Space to tick · Enter to add a note · Esc to go back"
    );
    card.handle_key(key(KeyCode::Char(' ')));
    assert_eq!(card.handle_key(key(KeyCode::Enter)), CardEvent::Moved);
    let text = plain_text(&card.lines(&ctx));
    assert!(
        text.contains(
            "  ❯ 3. Request changes on some cards, and add a note\n\
             \x20      ❯ ☒ card-002 Add the JSON writer\n"
        ),
        "{text}"
    );
    assert_eq!(last_row(&card, &ctx), "  Enter to send · Esc to go back");

    // Esc goes back to the ticks, and the note opens again after them.
    for c in "wrong".chars() {
        card.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(card.handle_key(key(KeyCode::Esc)), CardEvent::Moved);
    assert!(card.note.is_none() && card.checklist.is_some());
    card.handle_key(key(KeyCode::Down));
    card.handle_key(key(KeyCode::Char(' ')));
    card.handle_key(key(KeyCode::Enter));
    for c in "split the writer".chars() {
        card.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(
        card.handle_key(key(KeyCode::Enter)),
        CardEvent::Answered {
            index: 2,
            note: Some("split the writer".into()),
            checked: vec![0, 1],
        }
    );
    assert!(card.checklist.is_none() && card.note.is_none());
}

#[test]
fn an_option_that_asks_for_a_note_opens_it_first() {
    let mut card = merge_card();
    card.options[1].asks_note = true;
    let mut card = focused(card);
    assert_eq!(card.handle_key(key(KeyCode::Char('2'))), CardEvent::Moved);
    assert_eq!(card.note.as_ref().map(|note| note.option), Some(1));
    for c in "draft".chars() {
        card.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(
        card.handle_key(key(KeyCode::Enter)),
        CardEvent::Answered {
            index: 1,
            note: Some("draft".into()),
            checked: Vec::new(),
        }
    );

    // An empty note answers on the second Enter, and a card without notes answers at once.
    let mut card = merge_card();
    card.options[1].asks_note = true;
    let mut card = focused(card);
    card.selected = 1;
    assert_eq!(card.handle_key(key(KeyCode::Enter)), CardEvent::Moved);
    assert_eq!(card.handle_key(key(KeyCode::Enter)), answered(1));
    let mut card = merge_card();
    card.options[1].asks_note = true;
    card.allow_note = false;
    let mut card = focused(card);
    assert_eq!(card.handle_key(key(KeyCode::Char('2'))), answered(1));
}

#[test]
fn a_body_sits_between_dashed_rules_and_a_footer_ends_the_card() {
    let theme = theme();
    let ctx = ctx(40, &theme);
    let mut card = CardPrompt::new(
        5,
        "Approve the plan?",
        "Plan?",
        vec![
            CardOption::new("Approve the plan"),
            CardOption::view("Show the plan"),
        ],
    );
    card.agent = Some(CardAgent {
        name: "GPT 5.6 on Codex".into(),
        worker: "codex/gpt-5.6".into(),
        color: Some(AgentColor::Purple),
        detail: "planner".into(),
    });
    card.body = vec!["1. Parse --json".into(), "2. Write JSON rows".into()];
    card.question = Some("Build it this way?".into());
    card.footer = Some("ctrl+g to edit the plan · plans/run-01.md".into());
    card.focus();
    let view = card.layout(&ctx);
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
             \x20 Enter to choose · Esc to set aside\n\
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
    let mut card = focused(CardPrompt::new(
        1,
        "\x1b]0;pwned\x07Apply the change?",
        "Apply?",
        vec![CardOption::new("A very long option label indeed")],
    ));
    card.question = Some("Go\x1b[2J ahead?".into());
    card.detail = vec!["\x1b[31mred\x1b[0m".into()];
    let lines = card.lines(&ctx);
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
    let mut card = merge_card();
    card.options
        .push(CardOption::toggle("Remember this pick", false));
    let mut card = focused(card);
    let toggle = card.options.len() - 1;

    assert_eq!(
        card.handle_key(key(KeyCode::Char('7'))),
        CardEvent::OpenView { index: toggle }
    );
    assert!(card.options[toggle].on);
    assert_eq!(card.selected, 0, "the pointer stays where the answer is");
    let row = |card: &CardPrompt| {
        let lines = card.lines(&ctx);
        plain_text(&lines)
            .lines()
            .find(|row| row.contains("Remember this pick"))
            .map(str::to_owned)
            .unwrap()
    };
    assert!(
        row(&card).contains("☒ Remember this pick"),
        "{}",
        row(&card)
    );

    card.selected = toggle;
    assert_eq!(
        card.handle_key(key(KeyCode::Enter)),
        CardEvent::OpenView { index: toggle }
    );
    assert!(!card.options[toggle].on);
    assert!(row(&card).contains("☐ Remember this pick"));
    assert_eq!(card.handle_key(key(KeyCode::Tab)), CardEvent::Unhandled);
    assert_eq!(
        last_row(&card, &ctx),
        "  Enter to switch · Esc to set aside"
    );
}

#[test]
fn the_hint_line_says_what_esc_does() {
    let theme = theme();
    let ctx = ctx(80, &theme);
    let mut card = focused(merge_card());
    card.esc_hint = "Esc to cancel".into();
    card.selected = 3;
    assert_eq!(
        last_row(&card, &ctx),
        "  Enter to choose · Tab to add a note · Esc to cancel"
    );
}

#[test]
fn a_card_you_set_off_holds_the_keys_until_answered() {
    let mut card = CardPrompt::new(
        1,
        "Where should this be saved?",
        "Save where?",
        vec![
            CardOption::new("Just this session"),
            CardOption::new("This project, just me").recommended(),
            CardOption::new("This project, shared"),
            CardOption::new("Everywhere"),
        ],
    )
    .yours();
    assert!(card.focused && !card.waits(), "it opens with the keys");
    for c in ['s', 'm', 'p', 'e', '/'] {
        assert_eq!(card.handle_key(key(KeyCode::Char(c))), CardEvent::Unhandled);
    }
    for _ in 0..card.options.len() {
        assert_eq!(card.handle_key(key(KeyCode::Down)), CardEvent::Moved);
    }
    assert!(card.focused, "↓ past the last option keeps the keys");
    assert_eq!(card.selected, 3);
    assert_eq!(card.handle_key(key(KeyCode::Char('2'))), answered(1));
    assert_eq!(card.handle_key(key(KeyCode::Esc)), CardEvent::Pinned);
    assert!(!card.focused);
}
