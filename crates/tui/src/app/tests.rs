use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Position, Size};

use super::input::QUIT_WINDOW;
use super::*;
use crate::bottom::{
    AgentRow, AgentState, ApprovalMode, CARD_WAITS_FOR_QUIET, CardOption, CardPrompt,
    HistorySearch, Picker, PickerEvent, PickerRow, VimMode, WorkingLine,
};
use crate::prompt_history::PromptEntry;
use crate::theme::Glyphs;

const COLUMNS: u16 = 60;
const ROWS: u16 = 16;

fn ui() -> (Ui<vt100::Parser>, Instant) {
    ui_at(COLUMNS)
}

fn ui_at(columns: u16) -> (Ui<vt100::Parser>, Instant) {
    crossterm::style::force_color_output(true);
    let terminal = InlineTerminal::new(
        vt100::Parser::new(ROWS, columns, 0),
        Size::new(columns, ROWS),
        Position::new(0, 2),
    );
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    let t0 = Instant::now();
    (Ui::new(terminal, theme, ViewModel::new("chat"), t0), t0)
}

fn press<W: Write>(ui: &mut Ui<W>, code: KeyCode, now: Instant) -> Vec<UiAction> {
    ui.handle_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), now)
}

fn chord<W: Write>(ui: &mut Ui<W>, c: char, now: Instant) -> Vec<UiAction> {
    ui.handle_event(
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)),
        now,
    )
}

fn type_text(ui: &mut Ui<vt100::Parser>, text: &str, now: Instant) {
    for c in text.chars() {
        press(ui, KeyCode::Char(c), now);
    }
}

fn card(id: u64) -> CardPrompt {
    let mut card = CardPrompt::new(
        id,
        "Merge card-002 into main?",
        "Merge card-002?",
        vec![
            CardOption::irreversible("Merge"),
            CardOption::new("Keep the branch"),
        ],
    );
    card.allow_note = true;
    card
}

fn screen(ui: &Ui<vt100::Parser>) -> Vec<String> {
    let screen = ui.terminal().writer().screen();
    screen
        .rows(0, screen.size().1)
        .map(|row| row.trim_end().to_owned())
        .collect()
}

/// The live region's rows from the first one drawn, leaving out the blank rows between history
/// and the rows pinned to the bottom.
fn live(ui: &Ui<vt100::Parser>) -> Vec<String> {
    let top = usize::from(ui.terminal().viewport().y);
    screen(ui)
        .split_off(top)
        .into_iter()
        .skip_while(String::is_empty)
        .collect()
}

#[test]
fn ctrl_c_clears_then_asks_twice_to_exit() {
    let (mut ui, t0) = ui();
    type_text(&mut ui, "draft", t0);
    assert!(chord(&mut ui, 'c', t0).is_empty());
    assert!(ui.model().bottom.composer.is_empty());
    let hint = ui.model().bottom.hint.clone().unwrap();
    assert_eq!(hint.text, "Press Ctrl-C again to exit");
    assert_eq!(hint.until, t0 + QUIT_WINDOW);
    assert_eq!(
        chord(&mut ui, 'c', t0 + Duration::from_millis(900)),
        [UiAction::Quit]
    );

    // Past the window, so the second press arms again.
    ui.model_mut().bottom.exit_note = Some("the run keeps going".into());
    chord(&mut ui, 'c', t0);
    assert!(chord(&mut ui, 'c', t0 + Duration::from_secs(2)).is_empty());
    assert_eq!(
        ui.model().bottom.hint.as_ref().unwrap().text,
        "Press Ctrl-C again to exit · the run keeps going"
    );
}

#[test]
fn enter_on_an_empty_prompt_still_goes_up() {
    let (mut ui, t0) = ui();
    assert_eq!(press(&mut ui, KeyCode::Enter, t0), [UiAction::submit("")]);
    type_text(&mut ui, "  ", t0);
    assert!(press(&mut ui, KeyCode::Enter, t0).is_empty());
}

#[test]
fn ctrl_d_quits_only_from_an_empty_draft() {
    let (mut ui, t0) = ui();
    type_text(&mut ui, "ab", t0);
    press(&mut ui, KeyCode::Home, t0);
    assert!(chord(&mut ui, 'd', t0).is_empty());
    assert_eq!(ui.model().bottom.composer.text(), "b");
    chord(&mut ui, 'u', t0);
    press(&mut ui, KeyCode::Delete, t0);
    ui.model_mut().bottom.composer.clear();
    assert!(chord(&mut ui, 'd', t0).is_empty());
    assert_eq!(
        ui.model().bottom.hint.as_ref().unwrap().text,
        "Press Ctrl-D again to exit"
    );
    assert!(chord(&mut ui, 'c', t0).is_empty());
    assert!(chord(&mut ui, 'd', t0).is_empty());
    assert_eq!(chord(&mut ui, 'd', t0), [UiAction::Quit]);
}

#[test]
fn esc_means_what_the_context_says() {
    let (mut ui, t0) = ui();
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());

    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    assert_eq!(press(&mut ui, KeyCode::Esc, t0), [UiAction::Interrupt]);

    ui.model_mut().bottom.card = Some(card(4));
    press(&mut ui, KeyCode::Tab, t0);
    assert!(ui.model().bottom.focused_card().is_some());
    press(&mut ui, KeyCode::Tab, t0);
    assert!(ui.model().bottom.card.as_ref().unwrap().has_open_note());
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    assert!(!ui.model().bottom.card.as_ref().unwrap().has_open_note());
    assert_eq!(
        press(&mut ui, KeyCode::Esc, t0),
        [UiAction::SetAside { card: 4 }]
    );
    assert!(ui.model().bottom.card.as_ref().unwrap().pinned);
    assert_eq!(press(&mut ui, KeyCode::Esc, t0), [UiAction::Interrupt]);

    ui.model_mut().bottom.agents.rows = vec![AgentRow::new("codex", "card-3", AgentState::Working)];
    press(&mut ui, KeyCode::Down, t0);
    assert!(ui.model().bottom.agents.has_focus());
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    assert!(!ui.model().bottom.agents.has_focus());
}

/// What the host opens on `SearchHistory`.
fn open_search(ui: &mut Ui<vt100::Parser>) {
    let prompts = vec![
        PromptEntry::new("fix the export test", Some(100)),
        PromptEntry::new("add a README", Some(200)),
    ];
    let search = HistorySearch::new(prompts.clone(), prompts, 260).with_vim(true);
    ui.model_mut().bottom.history_search = Some(search);
}

#[test]
fn vim_slash_searches_history_and_enter_only_fills_the_input() {
    let (mut ui, t0) = ui_at(100);
    ui.model_mut().bottom.mode.vim = Some(VimMode::Insert);
    type_text(&mut ui, "draft", t0);
    press(&mut ui, KeyCode::Esc, t0);
    assert_eq!(
        press(&mut ui, KeyCode::Char('/'), t0),
        [UiAction::SearchHistory]
    );
    open_search(&mut ui);
    type_text(&mut ui, "exp", t0);
    assert_eq!(ui.model().bottom.composer.text(), "draft", "typing filters");
    ui.frame(t0).unwrap();
    let rows = screen(&ui);
    assert!(
        rows.iter()
            .any(|row| row.starts_with("  Search prompts · everywhere")),
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.contains("❯ 2m ago   fix the export test")),
        "{rows:#?}"
    );
    assert!(
        !rows.iter().any(|row| row.contains("-- NORMAL --")),
        "the search stands in for the input and the mode line: {rows:#?}"
    );

    assert!(
        press(&mut ui, KeyCode::Enter, t0).is_empty(),
        "Enter never sends"
    );
    let bottom = &ui.model().bottom;
    assert!(bottom.history_search.is_none());
    assert_eq!(bottom.composer.text(), "fix the export test");
    assert_eq!(bottom.mode.vim, Some(VimMode::Normal));

    // Ctrl+R opens it too; Esc closes it with the input as it was, and a paste filters.
    assert_eq!(chord(&mut ui, 'r', t0), [UiAction::SearchHistory]);
    open_search(&mut ui);
    ui.handle_event(Event::Paste("READ".into()), t0);
    let search = ui.model().bottom.history_search.as_ref().unwrap();
    assert_eq!(search.filter(), "READ");
    assert_eq!(search.selected(), Some("add a README"));
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    assert!(ui.model().bottom.history_search.is_none());
    assert_eq!(ui.model().bottom.composer.text(), "fix the export test");
    assert_eq!(ui.model().bottom.mode.vim, Some(VimMode::Normal));
}

#[test]
fn ctrl_r_searches_history_with_default_keys() {
    let (mut ui, t0) = ui();
    type_text(&mut ui, "half typed", t0);
    assert_eq!(chord(&mut ui, 'r', t0), [UiAction::SearchHistory]);
    open_search(&mut ui);
    // Ctrl+C leaves the search, not the app.
    assert!(chord(&mut ui, 'c', t0).is_empty());
    assert!(ui.model().bottom.history_search.is_none());
    assert_eq!(ui.model().bottom.composer.text(), "half typed");
}

/// Keys `ms` milliseconds apart from `start`, as a terminal delivers a write.
fn type_at(ui: &mut Ui<vt100::Parser>, keys: &str, start: Instant, ms: u64) -> Vec<UiAction> {
    let mut actions = Vec::new();
    for (n, c) in keys.chars().enumerate() {
        let code = if c == '\r' {
            KeyCode::Enter
        } else {
            KeyCode::Char(c)
        };
        let at = start + Duration::from_millis(ms * n as u64);
        actions.extend(press(ui, code, at));
    }
    actions
}

#[test]
fn a_paste_typed_out_as_keys_goes_in_whole_and_never_sends() {
    let (mut ui, t0) = ui();
    ui.time_keys(true);
    let actions = type_at(&mut ui, "wor\r\rbar", t0, 1);
    assert!(actions.is_empty(), "{actions:?}");
    // It goes in once the keys stop.
    let due = ui.next_deadline(t0 + Duration::from_millis(10)).unwrap();
    assert!(due <= t0 + Duration::from_millis(20), "{due:?}");
    ui.frame(t0 + Duration::from_millis(30)).unwrap();
    assert_eq!(ui.model().bottom.composer.text(), "wor\n\nbar");

    // Typed at a person's pace, Enter sends.
    let later = t0 + Duration::from_secs(5);
    assert!(type_at(&mut ui, "!", later, 0).is_empty());
    let actions = press(&mut ui, KeyCode::Enter, later + Duration::from_millis(300));
    assert!(
        matches!(&actions[..], [UiAction::Submit { text, .. }] if text == "wor\n\nbar!"),
        "{actions:?}"
    );
}

#[test]
fn a_paste_typed_out_as_keys_never_picks_a_prompt_in_the_search() {
    let (mut ui, t0) = ui();
    ui.time_keys(true);
    open_search(&mut ui);
    let actions = type_at(&mut ui, "add\r\rREAD", t0, 1);
    assert!(actions.is_empty(), "{actions:?}");
    let bottom = &ui.model().bottom;
    let search = bottom
        .history_search
        .as_ref()
        .expect("the search stays open");
    assert_eq!(search.filter(), "add READ");
    assert_eq!(bottom.composer.text(), "", "nothing went in the input");

    // Enter on its own picks the prompt and still sends nothing.
    let actions = press(&mut ui, KeyCode::Enter, t0 + Duration::from_secs(1));
    assert!(actions.is_empty(), "{actions:?}");
    assert_eq!(ui.model().bottom.composer.text(), "add a README");
}

#[test]
fn esc_and_i_together_leave_the_search_for_insert() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.mode.vim = Some(VimMode::Insert);
    type_text(&mut ui, "draft", t0);
    press(&mut ui, KeyCode::Esc, t0);
    press(&mut ui, KeyCode::Char('/'), t0);
    open_search(&mut ui);
    // tmux, ssh, or quick fingers send Esc i in one read, which comes as Alt+i.
    let alt_i = Event::Key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::ALT));
    assert!(ui.handle_event(alt_i, t0).is_empty());
    let bottom = &ui.model().bottom;
    assert!(bottom.history_search.is_none());
    assert_eq!(bottom.mode.vim, Some(VimMode::Insert));
    assert_eq!(bottom.composer.text(), "draft");
    // `i` inserts before the cursor, which Esc moved back onto the last letter.
    press(&mut ui, KeyCode::Char('/'), t0);
    assert_eq!(ui.model().bottom.composer.text(), "draf/t");
}

#[test]
fn with_vim_keys_esc_leaves_insert_before_it_interrupts() {
    // Wide enough that the mode line keeps its hints.
    let (mut ui, t0) = ui_at(100);
    ui.model_mut().bottom.mode.vim = Some(VimMode::Insert);
    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    type_text(&mut ui, "fix it", t0);
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    assert_eq!(ui.model().bottom.mode.vim, Some(VimMode::Normal));
    ui.frame(t0).unwrap();
    let rows = screen(&ui);
    assert!(
        rows.iter().any(|row| row.starts_with(
            "  -- NORMAL -- ǁ supervised mode on (shift+tab to cycle) · esc to interrupt"
        )),
        "{rows:#?}"
    );
    press(&mut ui, KeyCode::Char('x'), t0);
    assert_eq!(ui.model().bottom.composer.text(), "fix i");
    press(&mut ui, KeyCode::Char('b'), t0);
    press(&mut ui, KeyCode::Char('d'), t0);
    press(&mut ui, KeyCode::Char('w'), t0);
    assert_eq!(ui.model().bottom.composer.text(), "fix ");
    press(&mut ui, KeyCode::Char('z'), t0);
    assert_eq!(ui.model().bottom.composer.text(), "fix ");
    press(&mut ui, KeyCode::Char('A'), t0);
    assert_eq!(ui.model().bottom.mode.vim, Some(VimMode::Insert));
    type_text(&mut ui, "now", t0);
    assert_eq!(ui.model().bottom.composer.text(), "fix now");
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    assert_eq!(press(&mut ui, KeyCode::Esc, t0), [UiAction::Interrupt]);
    press(&mut ui, KeyCode::Char('d'), t0);
    press(&mut ui, KeyCode::Char('d'), t0);
    assert!(ui.model().bottom.composer.is_empty());
    press(&mut ui, KeyCode::Char('p'), t0);
    assert_eq!(
        press(&mut ui, KeyCode::Enter, t0),
        [UiAction::submit("fix now")]
    );
}

#[test]
fn chat_typed_while_a_card_waits_never_answers_it() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    ui.model_mut().bottom.enter_sends = Some("Enter sends guidance to the run".into());
    ui.model_mut().bottom.card = Some(card(9));
    ui.frame(t0).unwrap();
    let rows = screen(&ui);
    assert!(rows.iter().any(|row| row == "    1. Merge"), "{rows:#?}");
    assert!(
        rows.iter()
            .any(|row| row == "❯ ↑ to answer the card, or type to guide the run"),
        "{rows:#?}"
    );
    // "1" is Merge's number and "2" Keep the branch's, and neither answers from the input.
    let mut actions = Vec::new();
    for c in "1 or 2, keep it".chars() {
        actions.extend(press(&mut ui, KeyCode::Char(c), t0));
    }
    assert!(actions.is_empty(), "{actions:?}");
    assert_eq!(ui.model().bottom.composer.text(), "1 or 2, keep it");
    assert!(ui.model().bottom.waiting_card().is_some());
    ui.frame(t0 + Duration::from_millis(20)).unwrap();
    let rows = screen(&ui);
    assert!(
        rows.iter()
            .any(|row| row.starts_with("  ǁ supervised mode on")
                && row.ends_with("Enter sends guidance to the run")),
        "{rows:#?}"
    );
    assert_eq!(
        press(&mut ui, KeyCode::Enter, t0),
        [UiAction::submit("1 or 2, keep it")]
    );
    assert!(ui.model().bottom.card.is_some());
}

#[test]
fn a_card_you_set_off_keeps_typing_and_slash_out_of_the_input() {
    let (mut ui, t0) = ui();
    let mut scope = CardPrompt::new(
        12,
        "Where should this be saved?",
        "Save where?",
        vec![
            CardOption::new("Just this session"),
            CardOption::new("This project, just me"),
            CardOption::new("This project, shared"),
            CardOption::new("Everywhere"),
        ],
    )
    .yours();
    scope.esc_answers = true;
    ui.model_mut().bottom.card = Some(scope);
    ui.frame(t0).unwrap();
    assert!(ui.model().bottom.focused_card().is_some());
    for c in ['s', '/', 'm'] {
        assert!(press(&mut ui, KeyCode::Char(c), t0).is_empty());
    }
    assert!(ui.model().bottom.composer.is_empty());
    assert!(!menu_open(&ui));
    // ↓ past the last option stays on the card.
    for _ in 0..5 {
        press(&mut ui, KeyCode::Down, t0);
    }
    assert!(ui.model().bottom.focused_card().is_some());
    assert!(press(&mut ui, KeyCode::Char('x'), t0).is_empty());
    assert!(ui.model().bottom.composer.is_empty());
    assert_eq!(
        press(&mut ui, KeyCode::Esc, t0),
        [UiAction::SetAside { card: 12 }]
    );
    assert!(ui.model().bottom.composer_has_focus());
}

#[test]
fn a_card_that_needs_you_keeps_typing_out_until_set_aside() {
    let (mut ui, t0) = ui_at(100);
    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    let mut merge = card(9);
    merge.needs_you = true;
    ui.model_mut().bottom.card = Some(merge);
    // Any event hands it the keys while the input is empty, as a card arriving does.
    ui.handle_event(Event::FocusGained, t0);
    assert!(ui.model().bottom.focused_card().is_some());
    ui.frame(t0).unwrap();
    let rows = screen(&ui);
    assert!(
        rows.iter()
            .any(|row| row == "❯ ↑↓ to move · Enter or a number to answer · Esc to set aside"),
        "{rows:#?}"
    );
    assert!(
        !rows.iter().any(|row| row.contains("type to guide the run")),
        "{rows:#?}"
    );

    // Letters, a paste, and the Enter after them reach neither the input nor the card.
    for c in "keep it".chars() {
        assert!(press(&mut ui, KeyCode::Char(c), t0).is_empty());
    }
    ui.handle_event(Event::Paste("keep the branch".into()), t0);
    assert!(ui.model().bottom.composer.is_empty());
    ui.frame(t0 + Duration::from_millis(20)).unwrap();
    assert!(
        screen(&ui).iter().any(|row| row
            == "❯ Typing waits while the card has the keys · ↑↓ to choose · Esc to set aside"),
        "{:#?}",
        screen(&ui)
    );
    assert!(press(&mut ui, KeyCode::Enter, t0).is_empty());
    assert!(ui.model().bottom.card.is_some());

    // Set aside, the input takes typing again, and ↑ on an empty input brings the card back.
    assert_eq!(
        press(&mut ui, KeyCode::Esc, t0),
        [UiAction::SetAside { card: 9 }]
    );
    assert!(ui.model().bottom.composer_has_focus());
    type_text(&mut ui, "keep it", t0);
    assert_eq!(
        press(&mut ui, KeyCode::Enter, t0),
        [UiAction::submit("keep it")]
    );
    assert!(
        ui.model().bottom.composer_has_focus(),
        "set aside, it waits for ↑"
    );
    assert!(press(&mut ui, KeyCode::Up, t0).is_empty());
    assert!(ui.model().bottom.focused_card().is_some());
    assert_eq!(press(&mut ui, KeyCode::Char('2'), t0), answer(9, 1));
}

#[test]
fn a_card_that_needs_you_waits_for_a_draft_to_be_sent() {
    let (mut ui, t0) = ui();
    type_text(&mut ui, "keep go", t0);
    let mut merge = card(9);
    merge.needs_you = true;
    ui.model_mut().bottom.card = Some(merge);
    type_text(&mut ui, "ing", t0);
    assert_eq!(ui.model().bottom.composer.text(), "keep going");
    assert!(ui.model().bottom.waiting_card().is_some());
    assert_eq!(
        press(&mut ui, KeyCode::Enter, t0),
        [UiAction::submit("keep going")]
    );
    // The next message may follow at once, so the card waits for the input to go quiet.
    let soon = t0 + Duration::from_millis(100);
    ui.frame(soon).unwrap();
    assert!(ui.model().bottom.waiting_card().is_some());
    let quiet = t0 + CARD_WAITS_FOR_QUIET;
    assert!(ui.next_deadline(soon).is_some_and(|at| at <= quiet));
    ui.frame(quiet).unwrap();
    assert!(
        ui.model().bottom.focused_card().is_some(),
        "with the draft sent and the input quiet, the card takes the keys"
    );
}

/// The probe from the review: a draft cleared with Ctrl+U never hands the keys to the card,
/// so the line typed after it lands in the input whole.
#[test]
fn clearing_a_draft_never_hands_a_card_the_keys() {
    let (mut ui, t0) = ui();
    type_text(&mut ui, "fix", t0);
    let mut merge = card(9);
    merge.needs_you = true;
    ui.model_mut().bottom.card = Some(merge);
    let at = |ms| t0 + Duration::from_millis(ms);
    ui.handle_event(Event::FocusGained, at(10));
    chord(&mut ui, 'u', at(20));
    assert!(ui.model().bottom.composer.is_empty());
    assert!(ui.model().bottom.focused_card().is_none());
    for (n, c) in "fix the slug bug, then 2 more".chars().enumerate() {
        let typed = press(&mut ui, KeyCode::Char(c), at(30 + n as u64 * 40));
        assert!(typed.is_empty(), "{c:?} did {typed:?}");
    }
    assert_eq!(
        ui.model().bottom.composer.text(),
        "fix the slug bug, then 2 more"
    );
    assert!(ui.model().bottom.waiting_card().is_some());
}

/// The unclear request follows the lead's reply, which you may answer by typing, so once you
/// type it leaves the keys with you even after the input goes quiet.
#[test]
fn the_unclear_request_leaves_the_keys_once_you_type() {
    let (mut ui, t0) = ui();
    let mut unclear = card(9);
    unclear.needs_you = true;
    unclear.yields_to_typing = true;
    ui.model_mut().bottom.card = Some(unclear);
    press(&mut ui, KeyCode::Char('i'), t0);
    press(&mut ui, KeyCode::Backspace, t0);
    ui.frame(t0 + CARD_WAITS_FOR_QUIET * 3).unwrap();
    assert!(ui.model().bottom.waiting_card().is_some());
    assert!(press(&mut ui, KeyCode::Up, t0 + CARD_WAITS_FOR_QUIET * 3).is_empty());
    assert!(
        ui.model().bottom.focused_card().is_some(),
        "↑ still brings it up"
    );
}

/// The paste-then-Enter and vim probes from the review, on a card that took the keys from a
/// quiet input: nothing typed answers it, Merge least of all.
#[test]
fn typing_meant_for_the_input_never_answers_a_card_that_took_the_keys() {
    let take = |vim: bool| {
        let (mut ui, t0) = ui();
        if vim {
            ui.model_mut().bottom.mode.vim = Some(VimMode::Insert);
        }
        let mut merge = card(9);
        merge.needs_you = true;
        ui.model_mut().bottom.card = Some(merge);
        ui.frame(t0).unwrap();
        assert!(ui.model().bottom.focused_card().is_some());
        (ui, t0)
    };
    let (mut ui, t0) = take(false);
    assert!(
        ui.handle_event(Event::Paste("keep the branch".into()), t0)
            .is_empty()
    );
    assert!(press(&mut ui, KeyCode::Enter, t0).is_empty());
    assert!(ui.model().bottom.card.is_some());

    for line in ["let me check", "we will see"] {
        let (mut ui, t0) = take(true);
        for c in line.chars() {
            let typed = press(&mut ui, KeyCode::Char(c), t0);
            assert!(typed.is_empty(), "{line:?}: {c:?} did {typed:?}");
        }
        assert!(press(&mut ui, KeyCode::Enter, t0).is_empty(), "{line:?}");
        assert!(ui.model().bottom.focused_card().is_some(), "{line:?}");
    }

    let (mut ui, t0) = take(false);
    for c in "also fix 2 more bugs".chars() {
        assert!(press(&mut ui, KeyCode::Char(c), t0).is_empty(), "{c:?}");
    }
    assert!(press(&mut ui, KeyCode::Enter, t0).is_empty());
    assert!(ui.model().bottom.card.is_some());
}

/// While a card that needs you has the keys, Ctrl+R and vim's `/` don't open the prompt
/// search and `!` doesn't start shell mode. Set aside, each works as before.
#[test]
fn a_card_with_the_keys_holds_off_the_search_and_shell_mode() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.mode.vim = Some(VimMode::Normal);
    ui.model_mut().bottom.card = Some(needs_you(9));
    ui.frame(t0).unwrap();
    assert!(ui.model().bottom.focused_card().is_some());

    assert!(chord(&mut ui, 'r', t0).is_empty());
    assert!(press(&mut ui, KeyCode::Char('/'), t0).is_empty());
    assert!(press(&mut ui, KeyCode::Char('!'), t0).is_empty());
    let bottom = &ui.model().bottom;
    assert!(bottom.history_search.is_none());
    assert!(bottom.focused_card().is_some());
    assert!(!bottom.composer.is_shell());
    assert!(bottom.composer.is_empty());

    assert_eq!(
        press(&mut ui, KeyCode::Esc, t0),
        [UiAction::SetAside { card: 9 }]
    );
    assert_eq!(
        press(&mut ui, KeyCode::Char('/'), t0),
        [UiAction::SearchHistory]
    );
    open_search(&mut ui);
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    assert_eq!(chord(&mut ui, 'r', t0), [UiAction::SearchHistory]);
    open_search(&mut ui);
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    assert_eq!(ui.model().bottom.mode.vim, Some(VimMode::Normal));
    type_text(&mut ui, "i!", t0);
    assert!(ui.model().bottom.composer.is_shell());
    assert!(
        ui.model()
            .bottom
            .card
            .as_ref()
            .is_some_and(|card| card.pinned)
    );
}

fn needs_you(id: u64) -> CardPrompt {
    let mut card = card(id);
    card.needs_you = true;
    card
}

/// An open prompt search is a draft in use: a card that needs you waits however long it
/// stays quiet, and takes the keys once the search closes on an empty input and it's quiet.
#[test]
fn an_open_search_keeps_a_card_waiting() {
    let (mut ui, t0) = ui();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    assert_eq!(chord(&mut ui, 'r', t0), [UiAction::SearchHistory]);
    open_search(&mut ui);
    ui.model_mut().bottom.card = Some(needs_you(9));
    ui.frame(at(5000)).unwrap();
    assert!(ui.model().bottom.waiting_card().is_some());
    assert_eq!(ui.model().bottom.card_takes_keys_at(), None);
    type_text(&mut ui, "exp", at(5000));
    assert_eq!(ui.model().bottom.composer.text(), "", "typing filters");
    assert!(press(&mut ui, KeyCode::Esc, at(6000)).is_empty());
    assert!(ui.model().bottom.history_search.is_none());
    ui.frame(at(6100)).unwrap();
    assert!(ui.model().bottom.waiting_card().is_some());
    assert!(
        ui.next_deadline(at(6100))
            .is_some_and(|due| due <= at(6000) + CARD_WAITS_FOR_QUIET)
    );
    ui.frame(at(6000) + CARD_WAITS_FOR_QUIET).unwrap();
    assert!(ui.model().bottom.focused_card().is_some());
}

/// `!` waiting for its command is a draft too, until Backspace leaves shell mode.
#[test]
fn shell_mode_keeps_a_card_waiting() {
    let (mut ui, t0) = ui();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    press(&mut ui, KeyCode::Char('!'), t0);
    assert!(ui.model().bottom.composer.is_shell());
    ui.model_mut().bottom.card = Some(needs_you(9));
    ui.frame(at(5000)).unwrap();
    assert!(ui.model().bottom.waiting_card().is_some());
    assert_eq!(ui.model().bottom.card_takes_keys_at(), None);
    press(&mut ui, KeyCode::Backspace, at(6000));
    assert!(!ui.model().bottom.composer.is_shell());
    ui.frame(at(6000) + CARD_WAITS_FOR_QUIET).unwrap();
    assert!(ui.model().bottom.focused_card().is_some());
}

#[test]
fn a_focused_card_takes_keys_and_answers() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.card = Some(card(9));
    assert!(press(&mut ui, KeyCode::Up, t0).is_empty());
    assert!(ui.model().bottom.focused_card().is_some());
    assert!(press(&mut ui, KeyCode::Char('z'), t0).is_empty());
    press(&mut ui, KeyCode::Backspace, t0);
    assert!(ui.model().bottom.composer.is_empty());

    press(&mut ui, KeyCode::Down, t0);
    press(&mut ui, KeyCode::Tab, t0);
    type_text(&mut ui, "later", t0);
    assert!(press(&mut ui, KeyCode::Left, t0).is_empty());
    ui.handle_event(Event::Paste(" please".into()), t0);
    assert_eq!(
        press(&mut ui, KeyCode::Enter, t0),
        [UiAction::Answer {
            card: 9,
            option: 1,
            note: Some("later please".into()),
            checked: Vec::new(),
        }]
    );
    assert!(ui.model().bottom.card.is_none());

    ui.model_mut().bottom.card = Some(card(10));
    press(&mut ui, KeyCode::Tab, t0);
    press(&mut ui, KeyCode::Down, t0);
    assert!(press(&mut ui, KeyCode::Char('y'), t0).is_empty());
    assert_eq!(ui.model().bottom.card.as_ref().unwrap().armed, None);
    // After a letter a number waits for a key meant for the card.
    assert!(press(&mut ui, KeyCode::Char('1'), t0).is_empty());
    assert_eq!(ui.model().bottom.card.as_ref().unwrap().armed, None);
    press(&mut ui, KeyCode::Up, t0);
    assert!(press(&mut ui, KeyCode::Char('1'), t0).is_empty());
    assert_eq!(ui.model().bottom.card.as_ref().unwrap().armed, Some(0));
    assert_eq!(
        press(&mut ui, KeyCode::Enter, t0),
        [UiAction::Answer {
            card: 10,
            option: 0,
            note: None,
            checked: Vec::new(),
        }]
    );
}

#[test]
fn down_past_the_last_option_goes_back_to_the_input() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.card = Some(card(9));
    press(&mut ui, KeyCode::Up, t0);
    press(&mut ui, KeyCode::Down, t0);
    assert!(ui.model().bottom.focused_card().is_some());
    assert!(press(&mut ui, KeyCode::Down, t0).is_empty());
    let waiting = ui
        .model()
        .bottom
        .card
        .as_ref()
        .expect("the card still waits");
    assert!(!waiting.focused && !waiting.pinned);
    ui.frame(t0).unwrap();
    let rows = screen(&ui);
    assert!(rows.iter().any(|row| row == "  ↑ to answer"), "{rows:#?}");
    assert!(rows.iter().any(|row| row.starts_with("❯ ")), "{rows:#?}");

    type_text(&mut ui, "hi", t0);
    assert_eq!(ui.model().bottom.composer.text(), "hi");
    press(&mut ui, KeyCode::Backspace, t0);
    press(&mut ui, KeyCode::Backspace, t0);

    // ↑ comes back to the option it left.
    press(&mut ui, KeyCode::Up, t0);
    assert!(ui.model().bottom.focused_card().is_some());
    assert_eq!(
        press(&mut ui, KeyCode::Enter, t0),
        [UiAction::Answer {
            card: 9,
            option: 1,
            note: None,
            checked: Vec::new(),
        }]
    );
}

#[test]
fn up_takes_back_the_queue_or_reopens_a_pinned_card() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.composer.remember("older");
    ui.model_mut().bottom.queued = vec!["first".into(), "second".into()];
    assert_eq!(
        press(&mut ui, KeyCode::Up, t0),
        [UiAction::TakeBack(vec!["first".into(), "second".into()])]
    );
    assert_eq!(ui.model().bottom.queued.len(), 2);
    assert!(ui.model().bottom.composer.is_empty());

    ui.model_mut().bottom.queued.clear();
    let mut pinned = card(3);
    pinned.pinned = true;
    ui.model_mut().bottom.card = Some(pinned);
    assert!(press(&mut ui, KeyCode::Up, t0).is_empty());
    let opened = ui.model().bottom.card.as_ref().unwrap();
    assert!(!opened.pinned && opened.focused);

    ui.model_mut().bottom.card = None;
    press(&mut ui, KeyCode::Up, t0);
    assert_eq!(ui.model().bottom.composer.text(), "older");
}

fn answer(card: u64, option: usize) -> Vec<UiAction> {
    vec![UiAction::Answer {
        card,
        option,
        note: None,
        checked: Vec::new(),
    }]
}

#[test]
fn a_card_moves_with_vim_keys_and_answers_by_number() {
    // Without vim keys `k` is a letter, so a number waits for a key meant for the card.
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.card = Some(card(20));
    press(&mut ui, KeyCode::Up, t0);
    assert!(press(&mut ui, KeyCode::Char('k'), t0).is_empty());
    assert!(press(&mut ui, KeyCode::Char('2'), t0).is_empty());
    press(&mut ui, KeyCode::Up, t0);
    assert_eq!(press(&mut ui, KeyCode::Char('2'), t0), answer(20, 1));

    let (mut ui, t0) = ui_at(80);
    ui.model_mut().bottom.mode.vim = Some(VimMode::Insert);
    ui.model_mut().bottom.card = Some(card(21));
    press(&mut ui, KeyCode::Up, t0);
    let selected = |ui: &Ui<vt100::Parser>| ui.model().bottom.card.as_ref().unwrap().selected;
    press(&mut ui, KeyCode::Char('j'), t0);
    assert_eq!(selected(&ui), 1);
    assert!(press(&mut ui, KeyCode::Char('k'), t0).is_empty());
    assert_eq!(selected(&ui), 0);
    press(&mut ui, KeyCode::Char('G'), t0);
    assert_eq!(selected(&ui), 1);
    press(&mut ui, KeyCode::Char('g'), t0);
    assert_eq!(selected(&ui), 0);
    // Numbers alone, with no letter column.
    ui.frame(t0).unwrap();
    let rows = live(&ui);
    let row = |label: &str| rows.iter().find(|row| row.contains(label)).cloned();
    assert!(
        row("1. Merge").is_some_and(|row| row.ends_with("Merge")),
        "{rows:#?}"
    );
    let keep = row("2. Keep the branch");
    assert!(keep.is_some_and(|row| row.ends_with("branch")), "{rows:#?}");
    // A note takes every letter.
    press(&mut ui, KeyCode::Char('j'), t0);
    press(&mut ui, KeyCode::Tab, t0);
    type_text(&mut ui, "jk", t0);
    assert_eq!(
        ui.model()
            .bottom
            .card
            .as_ref()
            .unwrap()
            .note
            .as_ref()
            .unwrap()
            .text,
        "jk"
    );
    press(&mut ui, KeyCode::Esc, t0);
    assert_eq!(press(&mut ui, KeyCode::Char('l'), t0), answer(21, 1));

    // `h` sets a card aside, as Esc does.
    ui.model_mut().bottom.card = Some(card(22));
    press(&mut ui, KeyCode::Up, t0);
    assert_eq!(
        press(&mut ui, KeyCode::Char('h'), t0),
        [UiAction::SetAside { card: 22 }]
    );
    assert!(ui.model().bottom.card.as_ref().unwrap().pinned);
}

#[test]
fn the_agent_panel_moves_with_vim_keys() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.mode.vim = Some(VimMode::Insert);
    ui.model_mut().bottom.agents.rows = vec![
        AgentRow::new("claude-code", "card-2", AgentState::Working),
        AgentRow::new("codex", "card-3", AgentState::Working),
        AgentRow::new("agy", "card-4", AgentState::Working),
    ];
    press(&mut ui, KeyCode::Down, t0);
    let selected = |ui: &Ui<vt100::Parser>| ui.model().bottom.agents.selected;
    press(&mut ui, KeyCode::Char('j'), t0);
    assert_eq!(selected(&ui), Some(1));
    press(&mut ui, KeyCode::Char('G'), t0);
    assert_eq!(selected(&ui), Some(2));
    press(&mut ui, KeyCode::Char('g'), t0);
    assert_eq!(selected(&ui), Some(0));
    press(&mut ui, KeyCode::Char('j'), t0);
    let codex = crate::bottom::AgentRef {
        run: None,
        session: "codex".into(),
    };
    assert_eq!(
        press(&mut ui, KeyCode::Char('l'), t0),
        [UiAction::OpenAgent(codex)]
    );
    // `h` leaves the panel, as Esc does, and types nothing.
    press(&mut ui, KeyCode::Char('h'), t0);
    assert!(!ui.model().bottom.agents.has_focus());
    assert!(ui.model().bottom.composer.is_empty());
    // `k` on the first row leaves too, as ↑ does.
    press(&mut ui, KeyCode::Down, t0);
    press(&mut ui, KeyCode::Char('k'), t0);
    assert!(!ui.model().bottom.agents.has_focus());
}

#[test]
fn the_transcript_closes_on_h_only_with_vim_keys() {
    let (mut ui, t0) = ui();
    chord(&mut ui, 'o', t0);
    press(&mut ui, KeyCode::Char('h'), t0);
    assert!(ui.overlay.is_some());
    press(&mut ui, KeyCode::Esc, t0);
    assert!(ui.overlay.is_none());

    ui.model_mut().bottom.mode.vim = Some(VimMode::Insert);
    chord(&mut ui, 'o', t0);
    assert!(ui.overlay.is_some());
    press(&mut ui, KeyCode::Char('h'), t0);
    assert!(ui.overlay.is_none());
}

#[test]
fn down_on_an_empty_prompt_focuses_the_agents() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.agents.rows = vec![
        AgentRow::new("claude-code", "card-2", AgentState::Working),
        AgentRow::new("codex", "card-3", AgentState::Working),
    ];
    type_text(&mut ui, "x", t0);
    press(&mut ui, KeyCode::Down, t0);
    assert!(!ui.model().bottom.agents.has_focus());
    press(&mut ui, KeyCode::Backspace, t0);

    press(&mut ui, KeyCode::Down, t0);
    assert_eq!(ui.model().bottom.agents.selected, Some(0));
    press(&mut ui, KeyCode::Down, t0);
    let codex = crate::bottom::AgentRef {
        run: None,
        session: "codex".into(),
    };
    assert_eq!(
        press(&mut ui, KeyCode::Enter, t0),
        [UiAction::OpenAgent(codex.clone())]
    );
    // The first `x` asks on the mode line, like Ctrl+C, and the second stops.
    assert!(press(&mut ui, KeyCode::Char('x'), t0).is_empty());
    assert_eq!(
        ui.model().bottom.active_hint(t0),
        Some("x again to stop codex · esc to keep")
    );
    assert!(ui.model().bottom.card.is_none());
    assert_eq!(
        press(&mut ui, KeyCode::Char('x'), t0),
        [UiAction::StopAgent(codex)]
    );
    assert_eq!(ui.model().bottom.active_hint(t0), None);
    press(&mut ui, KeyCode::Char('h'), t0);
    assert!(!ui.model().bottom.agents.has_focus());
    assert_eq!(ui.model().bottom.composer.text(), "h");

    press(&mut ui, KeyCode::Backspace, t0);
    press(&mut ui, KeyCode::Down, t0);
    assert!(ui.model().bottom.agents.has_focus());
    ui.model_mut().bottom.agents.rows.clear();
    ui.frame(t0).unwrap();
    ui.model_mut().bottom.agents.rows = vec![AgentRow::new("agy", "review", AgentState::Waiting)];
    assert!(!ui.model().bottom.agents.has_focus());
    press(&mut ui, KeyCode::Char('k'), t0);
    assert_eq!(ui.model().bottom.composer.text(), "k");
}

#[test]
fn global_keys_ask_the_host() {
    let (mut ui, t0) = ui();
    assert_eq!(
        ui.handle_event(
            Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
            t0
        ),
        [UiAction::ModeChanged(ApprovalMode::Shadow)]
    );
    assert_eq!(ui.model().bottom.mode.approval, ApprovalMode::Shadow);
    assert_eq!(chord(&mut ui, 't', t0), [UiAction::OpenTree]);
    assert_eq!(chord(&mut ui, 'g', t0), [UiAction::EditDraft]);
    assert_eq!(press(&mut ui, KeyCode::Left, t0), [UiAction::OpenCrewView]);
    type_text(&mut ui, "ab", t0);
    assert!(press(&mut ui, KeyCode::Left, t0).is_empty());
    assert_eq!(
        ui.handle_event(Event::FocusLost, t0),
        [UiAction::Focus(false)]
    );
    assert_eq!(
        ui.handle_event(Event::FocusGained, t0),
        [UiAction::Focus(true)]
    );
    press(&mut ui, KeyCode::End, t0);
    assert_eq!(press(&mut ui, KeyCode::Enter, t0), [UiAction::submit("ab")]);
}

#[test]
fn key_releases_are_ignored() {
    let (mut ui, t0) = ui();
    let mut release = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
    release.kind = KeyEventKind::Release;
    ui.handle_event(Event::Key(release), t0);
    assert!(ui.model().bottom.composer.is_empty());
}

#[test]
fn frames_print_nothing_when_nothing_changed() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.composer.label = Some("solo".into());
    assert!(ui.frame(t0).unwrap());
    let rows = live(&ui);
    assert!(rows[0].ends_with(" solo ─"), "{rows:?}");
    assert_eq!(rows[1], "❯ Try \"review the last commit\"");
    assert_eq!(
        rows.len(),
        4,
        "the live region ends on the last row: {rows:?}"
    );
    assert_eq!(ui.next_deadline(t0), None);

    let later = t0 + Duration::from_secs(5);
    let written = ui.terminal().writer().screen().contents_formatted();
    assert!(!ui.frame(later).unwrap());
    assert_eq!(
        ui.terminal().writer().screen().contents_formatted(),
        written
    );
}

#[test]
fn an_idle_frame_writes_zero_bytes() {
    crossterm::style::force_color_output(true);
    let terminal = InlineTerminal::new(Vec::new(), Size::new(COLUMNS, ROWS), Position::new(0, 2));
    let t0 = Instant::now();
    let mut ui = Ui::new(terminal, Theme::dark(), ViewModel::new("chat"), t0);
    assert!(ui.frame(t0).unwrap());
    let written = ui.terminal().writer().len();
    assert!(!ui.frame(t0 + Duration::from_secs(1)).unwrap());
    // Marked dirty, but the frame comes out the same.
    ui.model_mut();
    assert!(!ui.frame(t0 + Duration::from_secs(2)).unwrap());
    assert_eq!(ui.terminal().writer().len(), written);
}

#[test]
fn frames_keep_their_distance() {
    let (mut ui, t0) = ui();
    assert!(ui.frame(t0).unwrap());
    type_text(&mut ui, "a", t0);
    assert_eq!(ui.next_deadline(t0), Some(t0 + MIN_FRAME_INTERVAL));
    assert!(!ui.frame(t0 + Duration::from_millis(5)).unwrap());
    assert!(ui.frame(t0 + MIN_FRAME_INTERVAL).unwrap());
    assert_eq!(live(&ui)[1], "❯ a");
}

#[test]
fn working_animates_on_its_own_schedule() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    assert!(ui.frame(t0).unwrap());
    assert_eq!(live(&ui)[0], "∙ Rowing…", "{:?}", screen(&ui));
    // The shimmer steps every 120 ms, while the spinner holds at `∙` for 450 ms.
    assert_eq!(ui.next_deadline(t0), Some(t0 + Duration::from_millis(120)));
    assert!(ui.frame(t0 + Duration::from_millis(120)).unwrap());
    assert!(live(&ui)[0].starts_with("∙ Rowing…"), "{:?}", screen(&ui));
    assert!(ui.frame(t0 + Duration::from_millis(480)).unwrap());
    assert!(live(&ui)[0].starts_with("✢ Rowing…"), "{:?}", screen(&ui));

    let mut reduced = ui.theme().clone();
    reduced.reduced_motion = true;
    ui.set_theme(reduced);
    ui.frame(t0 + Duration::from_millis(600)).unwrap();
    assert_eq!(
        ui.next_deadline(t0 + Duration::from_millis(600)),
        Some(t0 + Duration::from_secs(1))
    );
}

#[test]
fn a_hint_redraws_when_it_expires() {
    let (mut ui, t0) = ui();
    ui.frame(t0).unwrap();
    chord(&mut ui, 'c', t0);
    ui.frame(t0 + Duration::from_millis(20)).unwrap();
    assert!(
        screen(&ui)
            .iter()
            .any(|row| row.contains("Press Ctrl-C again to exit"))
    );
    let expiry = t0 + QUIT_WINDOW;
    assert_eq!(
        ui.next_deadline(t0 + Duration::from_millis(20)),
        Some(expiry)
    );
    assert!(ui.frame(expiry).unwrap());
    assert!(!screen(&ui).iter().any(|row| row.contains("Press Ctrl-C")));
    assert_eq!(ui.next_deadline(expiry), None);
}

#[test]
fn the_title_shows_status() {
    let (mut ui, t0) = ui();
    ui.frame(t0).unwrap();
    assert_eq!(ui.terminal().title(), Some("✳ chat"));
    ui.model_mut().bottom.agents.rows = vec![AgentRow::new("codex", "c", AgentState::Working)];
    ui.frame(t0 + Duration::from_millis(260)).unwrap();
    assert_eq!(ui.terminal().title(), Some("◐ chat"));
    ui.frame(t0 + Duration::from_millis(1_000)).unwrap();
    assert_eq!(ui.terminal().title(), Some("◑ chat"));
    ui.model_mut().bottom.card = Some(card(1));
    ui.frame(t0 + Duration::from_millis(1_100)).unwrap();
    assert_eq!(ui.terminal().title(), Some("◆ needs you · chat"));
}

#[test]
fn typed_keys_kept_when_card_appears() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    type_text(&mut ui, "yes ", t0);
    ui.frame(t0).unwrap();
    // The card shows between two keys; what follows still goes to the prompt.
    ui.model_mut().bottom.card = Some(card(12));
    ui.frame(t0 + Duration::from_millis(20)).unwrap();
    let mut actions = Vec::new();
    for c in "keep it".chars() {
        actions.extend(press(&mut ui, KeyCode::Char(c), t0));
    }
    assert!(actions.is_empty(), "{actions:?}");
    assert_eq!(ui.model().bottom.composer.text(), "yes keep it");
    let waiting = ui.model().bottom.waiting_card().unwrap();
    assert!(!waiting.pinned && waiting.armed.is_none());
}

#[test]
fn esc_in_the_input_never_answers_a_card() {
    let (mut ui, t0) = ui();
    let mut setup = card(8);
    setup.esc_answers = true;
    ui.model_mut().bottom.card = Some(setup);

    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    let waiting = ui.model().bottom.card.as_ref().unwrap();
    assert!(!waiting.pinned && !waiting.focused);

    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    assert_eq!(press(&mut ui, KeyCode::Esc, t0), [UiAction::Interrupt]);
    assert!(!ui.model().bottom.card.as_ref().unwrap().pinned);

    // With the card focused, Esc answers it as its hint says.
    press(&mut ui, KeyCode::Tab, t0);
    assert!(ui.model().bottom.focused_card().is_some());
    assert_eq!(
        press(&mut ui, KeyCode::Esc, t0),
        [UiAction::SetAside { card: 8 }]
    );
}

fn with_commands(ui: &mut Ui<vt100::Parser>) {
    ui.model_mut().bottom.menus.commands = vec![
        crate::bottom::MenuItem::new("model", "Pick the model"),
        crate::bottom::MenuItem::new("preset", "Pick the preset"),
    ];
}

fn menu_open(ui: &Ui<vt100::Parser>) -> bool {
    let bottom = &ui.model().bottom;
    bottom.menus.is_open(bottom.composer.text())
}

#[test]
fn esc_hides_the_menu_until_the_next_edit() {
    let (mut ui, t0) = ui();
    with_commands(&mut ui);
    type_text(&mut ui, "/", t0);
    assert!(menu_open(&ui));
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    assert_eq!(ui.model().bottom.composer.text(), "/");
    assert!(!menu_open(&ui));

    // Past the double tap, any edit brings it back, even one that ends on the same text.
    let later = t0 + QUIT_WINDOW * 2;
    press(&mut ui, KeyCode::Char('m'), later);
    assert!(menu_open(&ui));
    press(&mut ui, KeyCode::Esc, later);
    press(&mut ui, KeyCode::Backspace, later + QUIT_WINDOW * 2);
    assert_eq!(ui.model().bottom.composer.text(), "/");
    assert!(menu_open(&ui));

    press(&mut ui, KeyCode::Esc, later);
    ui.handle_event(Event::Paste("p".into()), later + QUIT_WINDOW * 2);
    assert_eq!(ui.model().bottom.composer.text(), "/p");
    assert!(menu_open(&ui));
}

#[test]
fn a_second_esc_clears_the_input() {
    let (mut ui, t0) = ui();
    with_commands(&mut ui);
    let soon = t0 + Duration::from_millis(400);

    // The Esc that hides the menu is the first press.
    type_text(&mut ui, "/mo", t0);
    press(&mut ui, KeyCode::Esc, t0);
    let hint = ui.model().bottom.hint.clone().unwrap();
    assert_eq!(hint.text, "Esc again to clear");
    assert_eq!(hint.until, t0 + QUIT_WINDOW);
    assert!(press(&mut ui, KeyCode::Esc, soon).is_empty());
    assert!(ui.model().bottom.composer.is_empty());
    assert_eq!(ui.model().bottom.hint, None);
    assert_eq!(ui.model().bottom.composer.history(), ["/mo"]);

    // A draft clears the same way, and only from two presses close together.
    type_text(&mut ui, "fix it", t0);
    press(&mut ui, KeyCode::Esc, t0);
    press(&mut ui, KeyCode::Esc, t0 + QUIT_WINDOW * 2);
    assert_eq!(ui.model().bottom.composer.text(), "fix it");
    press(
        &mut ui,
        KeyCode::Esc,
        t0 + QUIT_WINDOW * 2 + Duration::from_millis(400),
    );
    assert!(ui.model().bottom.composer.is_empty());

    // Any other key in between lets the first press go.
    type_text(&mut ui, "ab", t0);
    press(&mut ui, KeyCode::Esc, t0);
    press(&mut ui, KeyCode::Char('c'), t0);
    assert_eq!(ui.model().bottom.hint, None);
    press(&mut ui, KeyCode::Esc, soon);
    assert_eq!(ui.model().bottom.composer.text(), "abc");

    // While a run works, the Esc after the menu's still clears rather than interrupting.
    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    ui.model_mut().bottom.composer.clear();
    type_text(&mut ui, "/p", t0);
    press(&mut ui, KeyCode::Esc, t0);
    assert!(press(&mut ui, KeyCode::Esc, soon).is_empty());
    assert!(ui.model().bottom.composer.is_empty());
}

#[test]
fn with_vim_keys_only_the_esc_that_hides_the_menu_starts_a_clear() {
    let (mut ui, t0) = ui();
    with_commands(&mut ui);
    ui.model_mut().bottom.mode.vim = Some(VimMode::Insert);
    let soon = t0 + Duration::from_millis(400);

    type_text(&mut ui, "/", t0);
    press(&mut ui, KeyCode::Esc, t0);
    assert_eq!(ui.model().bottom.mode.vim, Some(VimMode::Insert));
    press(&mut ui, KeyCode::Esc, soon);
    assert!(ui.model().bottom.composer.is_empty());
    assert_eq!(ui.model().bottom.mode.vim, Some(VimMode::Insert));

    // Esc is how you reach NORMAL, so pressing it twice keeps the draft.
    type_text(&mut ui, "fix it", t0);
    press(&mut ui, KeyCode::Esc, t0);
    assert_eq!(ui.model().bottom.mode.vim, Some(VimMode::Normal));
    press(&mut ui, KeyCode::Esc, soon);
    press(&mut ui, KeyCode::Esc, soon);
    assert_eq!(ui.model().bottom.composer.text(), "fix it");
    assert_eq!(ui.model().bottom.hint, None);
}

#[test]
fn esc_sets_card_aside_then_interrupts() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    ui.model_mut().bottom.card = Some(card(5));
    assert!(ui.model().bottom.focused_card().is_none());
    assert_eq!(
        press(&mut ui, KeyCode::Esc, t0),
        [UiAction::SetAside { card: 5 }]
    );
    assert!(ui.model().bottom.card.as_ref().unwrap().pinned);
    assert_eq!(press(&mut ui, KeyCode::Esc, t0), [UiAction::Interrupt]);
    assert_eq!(press(&mut ui, KeyCode::Esc, t0), [UiAction::Interrupt]);

    // Idle, a waiting card still takes the first Esc, and the next does nothing.
    ui.model_mut().bottom.working = None;
    ui.model_mut().bottom.card = Some(card(6));
    assert_eq!(
        press(&mut ui, KeyCode::Esc, t0),
        [UiAction::SetAside { card: 6 }]
    );
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());

    // In vim INSERT the first Esc only leaves INSERT.
    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    ui.model_mut().bottom.mode.vim = Some(VimMode::Insert);
    ui.model_mut().bottom.card = Some(card(7));
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    assert_eq!(ui.model().bottom.mode.vim, Some(VimMode::Normal));
    assert_eq!(
        press(&mut ui, KeyCode::Esc, t0),
        [UiAction::SetAside { card: 7 }]
    );
    assert_eq!(press(&mut ui, KeyCode::Esc, t0), [UiAction::Interrupt]);
}

#[test]
fn ctrl_c_clears_then_exits() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.exit_note = Some("the run keeps going".into());
    type_text(&mut ui, "draft", t0);
    assert!(chord(&mut ui, 'c', t0).is_empty());
    assert!(ui.model().bottom.composer.is_empty());
    // Text typed while the exit is armed is cleared, and the window starts over.
    type_text(&mut ui, "more", t0);
    let t1 = t0 + Duration::from_millis(100);
    assert!(chord(&mut ui, 'c', t1).is_empty());
    assert!(ui.model().bottom.composer.is_empty());
    assert_eq!(
        ui.model().bottom.hint.as_ref().unwrap().text,
        "Press Ctrl-C again to exit · the run keeps going"
    );
    // The hint goes away by itself once the window closes.
    assert_eq!(ui.next_deadline(t1), Some(t1));
    ui.frame(t1).unwrap();
    assert_eq!(
        ui.next_deadline(t1 + Duration::from_millis(20)),
        Some(t1 + QUIT_WINDOW)
    );
    assert_eq!(
        chord(&mut ui, 'c', t1 + Duration::from_millis(950)),
        [UiAction::Quit]
    );

    // A rebound key names itself in the hint.
    let overrides = [("clear_or_exit".to_owned(), "ctrl+q".to_owned())].into();
    ui.set_keymap(crate::keymap::Keymap::from_settings(&overrides).unwrap());
    let t2 = t0 + Duration::from_secs(5);
    assert!(chord(&mut ui, 'c', t2).is_empty());
    assert_eq!(ui.model().bottom.hint, None);
    assert!(chord(&mut ui, 'q', t2).is_empty());
    assert!(
        ui.model()
            .bottom
            .hint
            .as_ref()
            .unwrap()
            .text
            .starts_with("Press Ctrl-Q again to exit")
    );
    assert_eq!(chord(&mut ui, 'q', t2), [UiAction::Quit]);
}

#[test]
fn rebound_keys_reach_the_ui() {
    let (mut ui, t0) = ui();
    let overrides = [
        ("crew_view".to_owned(), "ctrl+left".to_owned()),
        ("submit".to_owned(), "ctrl+s".to_owned()),
        ("newline".to_owned(), "enter".to_owned()),
    ]
    .into();
    ui.set_keymap(crate::keymap::Keymap::from_settings(&overrides).unwrap());
    assert!(press(&mut ui, KeyCode::Left, t0).is_empty());
    assert_eq!(
        ui.handle_event(
            Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL)),
            t0
        ),
        [UiAction::OpenCrewView]
    );
    type_text(&mut ui, "a", t0);
    assert!(press(&mut ui, KeyCode::Enter, t0).is_empty());
    type_text(&mut ui, "b", t0);
    // Shift+Enter lost its action, so it does nothing now.
    assert!(
        ui.handle_event(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)),
            t0
        )
        .is_empty()
    );
    assert_eq!(ui.model().bottom.composer.text(), "a\nb");
    assert_eq!(chord(&mut ui, 's', t0), [UiAction::submit("a\nb")]);
}

#[derive(Debug)]
struct Probe {
    name: &'static str,
    closes_on_esc: bool,
}

impl crate::views::View for Probe {
    fn render(
        &mut self,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
        _ctx: &crate::views::ViewCtx<'_>,
    ) {
        buf.set_string(
            area.x,
            area.y,
            format!("view {}", self.name),
            ratatui::style::Style::default(),
        );
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        _ctx: &crate::views::ViewCtx<'_>,
    ) -> crate::views::ViewOutcome {
        use crate::views::ViewOutcome;
        match key.code {
            KeyCode::Esc if self.closes_on_esc => ViewOutcome::Close,
            KeyCode::Char('o') => ViewOutcome::Open(Box::new(Probe {
                name: "inner",
                closes_on_esc: false,
            })),
            KeyCode::Char('s') => ViewOutcome::Send(argonauts_core::app::UiCommand::Stop),
            KeyCode::Char('j') => ViewOutcome::Consumed,
            _ => ViewOutcome::Ignored,
        }
    }
}

#[test]
fn a_paste_goes_to_the_view_on_top() {
    use argonauts_core::app::SettingRow;
    use argonauts_core::settings::SettingScope;

    let (mut ui, t0) = ui();
    let state = crate::state::AppState::default();
    type_text(&mut ui, "kept", t0);
    let row = |key: &str| SettingRow {
        key: key.to_owned(),
        value: String::new(),
        layer: "built-in".to_owned(),
        preset_from: None,
        default_scope: SettingScope::Global,
        safety: false,
        allowed: Vec::new(),
        description: String::new(),
    };
    ui.push_view(Box::new(crate::views::settings::config::ConfigView::new(
        vec![row("ui.accent"), row("ui.theme")],
    )));
    assert!(
        ui.handle_event_in(Event::Paste("accent".into()), t0, &state)
            .is_empty()
    );
    ui.frame_in(t0 + Duration::from_millis(20), &state).unwrap();
    let shown = screen(&ui).join("\n");
    assert!(shown.contains("Search: accent"), "{shown}");
    assert!(shown.contains("1 of 2 match"), "{shown}");
    assert_eq!(ui.model().bottom.composer.text(), "kept");
}

#[test]
fn view_stack_esc_returns_to_chat() {
    let (mut ui, t0) = ui();
    let state = crate::state::AppState::default();
    type_text(&mut ui, "kept", t0);
    ui.frame(t0).unwrap();
    let chat = screen(&ui);

    ui.push_view(Box::new(Probe {
        name: "outer",
        closes_on_esc: true,
    }));
    let key = |ui: &mut Ui<vt100::Parser>, code| {
        ui.handle_event_in(
            Event::Key(KeyEvent::new(code, KeyModifiers::NONE)),
            t0,
            &state,
        )
    };
    let mut at = t0;
    let mut draw = |ui: &mut Ui<vt100::Parser>| {
        at += Duration::from_millis(20);
        ui.frame_in(at, &state).unwrap();
    };
    draw(&mut ui);
    assert!(ui.terminal().writer().screen().alternate_screen());
    assert_eq!(screen(&ui)[0], "view outer");

    // Keys reach the view, never the prompt behind it.
    assert!(key(&mut ui, KeyCode::Char('j')).is_empty());
    assert_eq!(
        key(&mut ui, KeyCode::Char('s')),
        [UiAction::Command(argonauts_core::app::UiCommand::Stop)]
    );
    assert!(key(&mut ui, KeyCode::Char('o')).is_empty());
    assert_eq!(ui.views_open(), 2);
    draw(&mut ui);
    assert_eq!(screen(&ui)[0], "view inner");

    // The inner view ignores Esc, and Esc still goes back one view.
    assert!(key(&mut ui, KeyCode::Esc).is_empty());
    draw(&mut ui);
    assert_eq!(screen(&ui)[0], "view outer");
    assert!(key(&mut ui, KeyCode::Esc).is_empty());
    assert_eq!(ui.views_open(), 0);
    draw(&mut ui);
    assert!(!ui.terminal().writer().screen().alternate_screen());
    assert_eq!(ui.model().bottom.composer.text(), "kept");
    assert_eq!(screen(&ui), chat);
}

#[test]
fn the_overlay_opens_on_the_alternate_screen_and_comes_back() {
    let (mut ui, t0) = ui();
    type_text(&mut ui, "kept", t0);
    ui.frame(t0).unwrap();
    assert!(chord(&mut ui, 'o', t0).is_empty());
    assert!(ui.is_overlay_open());
    let t1 = t0 + Duration::from_millis(20);
    ui.frame(t1).unwrap();
    assert!(ui.terminal().writer().screen().alternate_screen());
    let rows = screen(&ui);
    assert!(
        rows[usize::from(ROWS) - 1].contains("esc to close"),
        "{rows:?}"
    );

    press(&mut ui, KeyCode::Char('x'), t1);
    press(&mut ui, KeyCode::Esc, t1);
    assert!(!ui.is_overlay_open());
    let t2 = t1 + Duration::from_millis(20);
    ui.frame(t2).unwrap();
    assert!(!ui.terminal().writer().screen().alternate_screen());
    assert_eq!(ui.model().bottom.composer.text(), "kept");
    assert_eq!(live(&ui)[1], "❯ kept");
}

#[test]
fn resize_redraws_at_the_new_width() {
    let (mut ui, t0) = ui();
    ui.frame(t0).unwrap();
    ui.terminal_mut()
        .writer_mut()
        .screen_mut()
        .set_size(ROWS, 40);
    ui.handle_event(Event::Resize(40, ROWS), t0);
    ui.frame(t0 + Duration::from_millis(20)).unwrap();
    let rows: Vec<String> = ui
        .terminal()
        .writer()
        .screen()
        .rows(0, 40)
        .map(|row| row.trim_end().to_owned())
        .collect();
    let top = usize::from(ROWS) - 4;
    assert_eq!(rows[top], "─".repeat(40), "{rows:?}");
    assert_eq!(rows[top + 2], "─".repeat(40), "{rows:?}");
}

// A byte buffer stands in for the screen, since vt100 itself overflows on a one column grid.
#[test]
fn tiny_screens_and_resizes_never_panic() {
    for (columns, rows) in [(1, 1), (2, 2), (3, 1), (5, 3), (12, 4), (80, 2)] {
        crossterm::style::force_color_output(true);
        let terminal = InlineTerminal::new(
            Vec::new(),
            Size::new(columns, rows),
            Position::new(0, rows - 1),
        );
        let t0 = Instant::now();
        let mut ui = Ui::new(terminal, Theme::dark(), ViewModel::new("tiny"), t0);
        {
            let bottom = &mut ui.model_mut().bottom;
            bottom.working = Some(WorkingLine::new("Rowing", t0));
            bottom.agents.rows = vec![AgentRow::new("codex", "card-3", AgentState::Working)];
            bottom.card = Some(card(1));
            bottom.queued = vec!["later".into()];
        }
        ui.terminal_mut()
            .insert_history(vec![ratatui::text::Line::from("history")]);
        let mut now = t0;
        for (step, (width, height)) in [(columns, rows), (1, 1), (columns + 7, rows + 3)]
            .into_iter()
            .enumerate()
        {
            now += Duration::from_millis(40);
            ui.handle_event(Event::Resize(width, height), now);
            ui.frame(now).unwrap();
            if step == 1 {
                press(&mut ui, KeyCode::Esc, now);
                chord(&mut ui, 'o', now);
            }
        }
        chord(&mut ui, 'o', now);
        ui.frame(now + Duration::from_millis(40)).unwrap();
        ui.finish().unwrap();
        assert!(!ui.terminal().writer().is_empty());
    }
}

#[test]
fn with_restored_clears_runs_and_repaints() {
    let (mut ui, t0) = ui();
    type_text(&mut ui, "draft", t0);
    ui.frame(t0).unwrap();
    let ran = ui.with_restored(|| 42).unwrap();
    assert_eq!(ran, 42);
    assert!(screen(&ui).iter().all(String::is_empty));
    ui.frame(t0 + Duration::from_millis(20)).unwrap();
    assert_eq!(live(&ui)[1], "❯ draft");
}

#[test]
fn finish_leaves_a_clean_screen() {
    let (mut ui, t0) = ui();
    ui.frame(t0).unwrap();
    ui.finish().unwrap();
    assert!(screen(&ui).iter().all(String::is_empty));
}

#[test]
fn finish_prints_history_that_came_after_the_last_frame() {
    let (mut ui, t0) = ui();
    ui.frame(t0).unwrap();
    ui.terminal_mut()
        .insert_history(vec![ratatui::text::Line::from("the run keeps going")]);
    ui.finish().unwrap();
    let rows = screen(&ui);
    assert_eq!(rows[0], "the run keeps going");
    assert!(rows[1..].iter().all(String::is_empty), "{rows:?}");
    assert_eq!(ui.terminal().writer().screen().cursor_position(), (1, 0));
}

#[test]
fn a_child_program_gets_the_main_screen_even_from_the_overlay() {
    let (mut ui, t0) = ui();
    ui.frame(t0).unwrap();
    chord(&mut ui, 'o', t0);
    ui.frame(t0 + Duration::from_millis(20)).unwrap();
    assert!(ui.terminal().writer().screen().alternate_screen());

    let on_main = ui
        .with_restored(|| true)
        .map(|_| !ui.terminal().writer().screen().alternate_screen())
        .unwrap();
    assert!(on_main);
    ui.frame(t0 + Duration::from_millis(40)).unwrap();
    assert!(ui.terminal().writer().screen().alternate_screen());
    ui.finish().unwrap();
    assert!(!ui.terminal().writer().screen().alternate_screen());
}

#[test]
fn a_view_option_opens_full_screen_and_esc_returns_to_the_card() {
    let (mut ui, t0) = ui();
    let mut with_view = card(3);
    with_view.options.push(CardOption::view("Show the diff"));
    ui.model_mut().bottom.card = Some(with_view);
    ui.frame(t0).unwrap();
    press(&mut ui, KeyCode::Up, t0);
    let actions = press(&mut ui, KeyCode::Char('3'), t0);
    assert_eq!(actions, [UiAction::OpenView { card: 3, option: 2 }]);
    assert!(
        ui.model()
            .bottom
            .card
            .as_ref()
            .is_some_and(|card| !card.pinned)
    );

    let diff = crate::cell::DiffView::between("a\n", "b\n", 0);
    let cell = crate::cell::DiffCell {
        title: "Update(a.rs)".into(),
        diff,
    };
    ui.open_view("the diff", vec![cell.into()]);
    let t1 = t0 + Duration::from_millis(20);
    ui.frame(t1).unwrap();
    assert!(ui.terminal().writer().screen().alternate_screen());
    let rows = screen(&ui);
    assert_eq!(rows[0], "● Update(a.rs)", "{rows:?}");
    assert!(
        rows[usize::from(ROWS) - 1].contains("Showing the diff · esc to go back"),
        "{rows:?}"
    );

    press(&mut ui, KeyCode::Esc, t1);
    let t2 = t1 + Duration::from_millis(20);
    ui.frame(t2).unwrap();
    assert!(!ui.terminal().writer().screen().alternate_screen());
    assert!(
        screen(&ui)
            .iter()
            .any(|row| row.contains("5. Show the diff") || row.contains("3. Show the diff")),
        "{:?}",
        screen(&ui)
    );
}

#[test]
fn finishing_prints_what_was_live_then_the_exit_lines() {
    let (mut ui, t0) = ui();
    let session = crate::cell::ToolCall::described(crate::cell::ToolKind::Agent, "builder")
        .with_item(crate::cell::ToolItem::text("halfway"));
    ui.model_mut().history.push_active(session);
    ui.frame(t0).unwrap();
    ui.finish_with(vec![
        "run-01 keeps going · 1 agent working".into(),
        "Reattach with: argonauts".into(),
    ])
    .unwrap();
    let rows = screen(&ui);
    let text = rows.join("\n");
    assert!(
        text.contains("● builder\n  ⎿  halfway\n\nrun-01 keeps going · 1 agent working\nReattach with: argonauts"),
        "{rows:?}"
    );
    assert!(ui.model().history.active_cells().next().is_none());
}

#[test]
fn slash_opens_the_menu_under_the_input_and_question_mark_the_shortcuts() {
    let (mut ui, t0) = ui();
    let menus = &mut ui.model_mut().bottom.menus;
    menus.commands = vec![
        crate::bottom::MenuItem::new("model", "Pick the model"),
        crate::bottom::MenuItem::new("preset", "Pick the preset"),
    ];
    menus.shortcuts = vec![
        "/ for commands".into(),
        "ctrl + o for the transcript".into(),
    ];
    ui.frame(t0).unwrap();

    type_text(&mut ui, "/", t0);
    let t1 = t0 + Duration::from_millis(20);
    ui.frame(t1).unwrap();
    let rows = screen(&ui);
    let rule = rows
        .iter()
        .rposition(|row| row.starts_with('─') && !row.contains("peer"))
        .unwrap();
    assert!(rows[rule + 1].starts_with("  /model"), "{rows:?}");
    assert!(!rows.iter().any(|row| row.contains("mode on")), "{rows:?}");

    let actions = press(&mut ui, KeyCode::Enter, t1);
    assert_eq!(actions, [UiAction::submit("/model")]);
    assert!(ui.model().bottom.composer.is_empty());

    assert!(press(&mut ui, KeyCode::Char('?'), t1).is_empty());
    assert!(ui.model().bottom.menus.shortcuts_open);
    assert!(ui.model().bottom.composer.is_empty());
    press(&mut ui, KeyCode::Esc, t1);
    assert!(!ui.model().bottom.menus.shortcuts_open);
}

#[test]
fn an_open_picker_takes_keys_before_the_composer() {
    let (mut ui, t0) = ui();
    let mut picker = Picker::new("Theme", "Sets ui.theme");
    picker.rows = ["auto", "dark", "light"]
        .into_iter()
        .map(|label| PickerRow::new(label, ""))
        .collect();
    ui.model_mut().bottom.picker = Some(picker);
    assert_eq!(
        press(&mut ui, KeyCode::Char('3'), t0),
        [UiAction::Picker(PickerEvent::Picked(2))]
    );
    assert_eq!(
        press(&mut ui, KeyCode::Esc, t0),
        [UiAction::Picker(PickerEvent::Closed)]
    );
    assert!(ui.model().bottom.composer.is_empty());
}

#[test]
fn visual_shows_on_the_mode_line_and_esc_leaves_it_before_interrupting() {
    let (mut ui, t0) = ui_at(100);
    ui.model_mut().bottom.mode.vim = Some(VimMode::Insert);
    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    type_text(&mut ui, "fix it", t0);
    press(&mut ui, KeyCode::Esc, t0);
    assert!(press(&mut ui, KeyCode::Char('v'), t0).is_empty());
    assert_eq!(ui.model().bottom.mode.vim, Some(VimMode::Visual));
    ui.frame(t0).unwrap();
    let rows = screen(&ui);
    assert!(
        rows.iter()
            .any(|row| row == "  -- VISUAL -- ǁ supervised mode on (shift+tab to cycle)"),
        "{rows:#?}"
    );

    // Esc in VISUAL goes back to NORMAL and never stops the turn.
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    assert_eq!(ui.model().bottom.mode.vim, Some(VimMode::Normal));
    ui.frame(t0 + Duration::from_millis(50)).unwrap();
    let rows = screen(&ui);
    assert!(
        rows.iter().any(|row| row.starts_with(
            "  -- NORMAL -- ǁ supervised mode on (shift+tab to cycle) · esc to interrupt"
        )),
        "{rows:#?}"
    );

    // Nor does Esc after a command's first key, which it only drops.
    press(&mut ui, KeyCode::Char('d'), t0);
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    press(&mut ui, KeyCode::Char('w'), t0);
    assert_eq!(ui.model().bottom.composer.text(), "fix it");
    assert_eq!(press(&mut ui, KeyCode::Esc, t0), [UiAction::Interrupt]);

    // A waiting card stays open while Esc leaves VISUAL LINE.
    ui.model_mut().bottom.card = Some(card(3));
    press(&mut ui, KeyCode::Char('V'), t0);
    assert_eq!(ui.model().bottom.mode.vim, Some(VimMode::Visual));
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    assert!(!ui.model().bottom.card.as_ref().unwrap().pinned);

    // A visual delete ends VISUAL, and the mode line follows.
    press(&mut ui, KeyCode::Char('0'), t0);
    press(&mut ui, KeyCode::Char('v'), t0);
    press(&mut ui, KeyCode::Char('e'), t0);
    press(&mut ui, KeyCode::Char('d'), t0);
    assert_eq!(ui.model().bottom.composer.text(), " it");
    assert_eq!(ui.model().bottom.mode.vim, Some(VimMode::Normal));
}

fn shift_tab<W: Write>(ui: &mut Ui<W>, now: Instant) -> Vec<UiAction> {
    ui.handle_event(
        Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
        now,
    )
}

#[test]
fn a_typed_picker_line_takes_pastes_and_keeps_the_keys_it_has_no_use_for() {
    let (mut ui, t0) = ui();
    let mut picker = Picker::new("Add an endpoint", "Its base URL");
    picker.input = Some("http".into());
    ui.model_mut().bottom.picker = Some(picker);
    assert_eq!(
        ui.handle_event(Event::Paste("://localhost:11434/v1\n".into()), t0),
        [UiAction::Picker(PickerEvent::Moved)]
    );
    for code in [
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::Delete,
    ] {
        assert_eq!(
            press(&mut ui, code, t0),
            [UiAction::Picker(PickerEvent::Moved)]
        );
    }
    let approval = ui.model().bottom.mode.approval;
    assert!(shift_tab(&mut ui, t0).is_empty());
    assert_eq!(ui.model().bottom.mode.approval, approval);
    let picker = ui.model().bottom.picker.as_ref().unwrap();
    assert_eq!(picker.input.as_deref(), Some("http://localhost:11434/v1"));
    assert!(ui.model().bottom.composer.is_empty());

    // The quit keys still reach past it.
    assert!(chord(&mut ui, 'c', t0).is_empty());
    assert_eq!(
        chord(&mut ui, 'c', t0 + Duration::from_millis(100)),
        [UiAction::Quit]
    );
}

#[test]
fn a_picker_list_filters_on_a_paste_and_never_opens_the_crew_view() {
    let (mut ui, t0) = ui();
    let mut picker = Picker::new("Model", "Sets the model");
    picker.rows = ["haiku", "opus", "sonnet"]
        .into_iter()
        .map(|label| PickerRow::new(label, ""))
        .collect();
    ui.model_mut().bottom.picker = Some(picker);
    assert_eq!(
        press(&mut ui, KeyCode::Left, t0),
        [UiAction::Picker(PickerEvent::Left)]
    );
    ui.handle_event(Event::Paste("son".into()), t0);
    let picker = ui.model().bottom.picker.as_ref().unwrap();
    assert_eq!(picker.filter, "son");
    assert_eq!(picker.pointed(), Some(2));
    assert!(ui.model().bottom.composer.is_empty());
}

#[test]
fn a_focused_card_holds_every_key_but_the_quit_keys() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.card = Some(card(5));
    press(&mut ui, KeyCode::Up, t0);
    assert!(ui.model().bottom.focused_card().is_some());
    let approval = ui.model().bottom.mode.approval;
    assert!(shift_tab(&mut ui, t0).is_empty());
    assert_eq!(ui.model().bottom.mode.approval, approval);
    assert!(press(&mut ui, KeyCode::Left, t0).is_empty());
    assert!(chord(&mut ui, 'o', t0).is_empty());
    assert!(ui.model().bottom.focused_card().is_some());
    assert!(chord(&mut ui, 'd', t0).is_empty());
    assert_eq!(
        chord(&mut ui, 'd', t0 + Duration::from_millis(100)),
        [UiAction::Quit]
    );
}

fn answered_pick() -> (crate::bottom::AnsweredPick, argonauts_core::app::UiCommand) {
    let run = argonauts_core::RunId::new("run-20261002-101500-3fa2").unwrap();
    let id = argonauts_core::names::CheckpointId::numbered(4);
    let pick =
        crate::bottom::AnsweredPick::new(run.clone(), id.clone(), "card-003 handed to Codex");
    (
        pick,
        argonauts_core::app::UiCommand::RememberPick { run, id },
    )
}

#[test]
fn up_picks_the_answered_handoff_pick_and_m_remembers_it() {
    let (mut ui, t0) = ui_at(80);
    ui.model_mut().bottom.composer.remember("older");
    let (pick, remember) = answered_pick();
    ui.model_mut().bottom.answered_pick = Some(pick);
    ui.frame(t0).unwrap();
    let rows = live(&ui);
    assert!(
        rows.iter()
            .any(|row| row == "◆ card-003 handed to Codex · ↑ to remember this pick"),
        "{rows:#?}"
    );

    assert!(press(&mut ui, KeyCode::Up, t0).is_empty());
    assert!(ui.model().bottom.pick_selected());
    assert!(
        ui.model().bottom.composer.is_empty(),
        "↑ went to the pick first"
    );
    ui.frame(t0 + Duration::from_secs(1)).unwrap();
    let rows = live(&ui);
    let selected = "❯ card-003 handed to Codex · m to save it as a rule · esc to dismiss";
    assert!(rows.iter().any(|row| row == selected), "{rows:#?}");

    assert_eq!(
        press(&mut ui, KeyCode::Char('m'), t0),
        [UiAction::Command(remember)]
    );
    assert_eq!(ui.model().bottom.answered_pick, None);
    assert!(ui.model().bottom.composer.is_empty(), "the m wasn't typed");
    press(&mut ui, KeyCode::Up, t0);
    assert_eq!(ui.model().bottom.composer.text(), "older");
}

#[test]
fn the_answered_pick_steps_aside_for_history_typing_and_esc() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.composer.remember("older");
    let (pick, _) = answered_pick();

    // A second ↑ goes on to the history, and the pick stays offered.
    ui.model_mut().bottom.answered_pick = Some(pick.clone());
    press(&mut ui, KeyCode::Up, t0);
    press(&mut ui, KeyCode::Up, t0);
    assert_eq!(ui.model().bottom.composer.text(), "older");
    assert!(!ui.model().bottom.pick_selected());
    assert!(ui.model().bottom.answered_pick.is_some());

    // Typing leaves the line and types, and sending lets the pick go.
    let (mut ui, t0) = ui_at(COLUMNS);
    ui.model_mut().bottom.answered_pick = Some(pick.clone());
    press(&mut ui, KeyCode::Up, t0);
    type_text(&mut ui, "hi", t0);
    assert_eq!(ui.model().bottom.composer.text(), "hi");
    assert!(!ui.model().bottom.pick_selected());
    press(&mut ui, KeyCode::Enter, t0);
    assert_eq!(ui.model().bottom.answered_pick, None);

    // Esc lets it go without interrupting anything.
    let (mut ui, t0) = ui_at(COLUMNS);
    ui.model_mut().bottom.answered_pick = Some(pick);
    press(&mut ui, KeyCode::Up, t0);
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    assert_eq!(ui.model().bottom.answered_pick, None);
}

#[test]
fn a_waiting_card_comes_before_the_answered_pick() {
    let (mut ui, t0) = ui();
    let (pick, _) = answered_pick();
    ui.model_mut().bottom.answered_pick = Some(pick);
    ui.model_mut().bottom.card = Some(card(9));
    press(&mut ui, KeyCode::Up, t0);
    assert!(ui.model().bottom.focused_card().is_some());
    assert!(!ui.model().bottom.pick_selected());
}

#[test]
fn a_bang_switches_to_shell_mode_and_esc_leaves_it_before_interrupting() {
    let (mut ui, t0) = ui_at(100);
    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    type_text(&mut ui, "!git status", t0);
    assert!(ui.model().bottom.composer.is_shell());
    ui.frame(t0).unwrap();
    let rows = screen(&ui);
    assert!(rows.iter().any(|row| row == "! git status"), "{rows:#?}");
    assert!(
        rows.iter().any(|row| row == "  ! for shell mode"),
        "the mode line says only that: {rows:#?}"
    );

    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    assert!(!ui.model().bottom.composer.is_shell());
    assert_eq!(ui.model().bottom.composer.text(), "git status");
    assert_eq!(press(&mut ui, KeyCode::Esc, t0), [UiAction::Interrupt]);
}

#[test]
fn with_vim_keys_esc_goes_to_normal_before_it_leaves_shell_mode() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.mode.vim = Some(VimMode::Insert);
    type_text(&mut ui, "!ls", t0);
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    assert_eq!(ui.model().bottom.mode.vim, Some(VimMode::Normal));
    assert!(ui.model().bottom.composer.is_shell());
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    assert!(!ui.model().bottom.composer.is_shell());
}

#[test]
fn enter_in_shell_mode_runs_the_command_and_ctrl_c_stops_it() {
    use argonauts_core::app::UiCommand;

    let (mut ui, t0) = ui();
    type_text(&mut ui, "!sleep 25", t0);
    assert_eq!(
        press(&mut ui, KeyCode::Enter, t0),
        [UiAction::Shell {
            command: "sleep 25".into()
        }]
    );
    assert!(!ui.model().bottom.composer.is_shell());

    ui.model_mut().bottom.shell_running = true;
    type_text(&mut ui, "draft", t0);
    assert_eq!(
        chord(&mut ui, 'c', t0),
        [UiAction::Command(UiCommand::StopShell)]
    );
    assert_eq!(
        ui.model().bottom.composer.text(),
        "draft",
        "stopping the command leaves the draft alone"
    );
    assert!(ui.model().bottom.hint.is_none(), "and never arms the exit");

    ui.model_mut().bottom.shell_running = false;
    assert!(chord(&mut ui, 'c', t0).is_empty());
    assert!(ui.model().bottom.composer.is_empty());
}

#[test]
fn ctrl_c_in_an_empty_shell_input_clears_the_mode() {
    let (mut ui, t0) = ui();
    type_text(&mut ui, "!", t0);
    assert!(chord(&mut ui, 'c', t0).is_empty());
    assert!(!ui.model().bottom.composer.is_shell());
    assert_eq!(
        ui.model().bottom.hint.as_ref().unwrap().text,
        "Press Ctrl-C again to exit"
    );
}

#[test]
fn in_shell_mode_slash_question_mark_and_at_are_the_commands_own() {
    let (mut ui, t0) = ui_at(80);
    with_commands(&mut ui);
    type_text(&mut ui, "!?", t0);
    assert!(!ui.model().bottom.menus.shortcuts_open);
    assert_eq!(ui.model().bottom.composer.text(), "?");
    press(&mut ui, KeyCode::Backspace, t0);
    type_text(&mut ui, "/bin/ls @src", t0);
    ui.frame(t0).unwrap();
    let rows = screen(&ui);
    assert!(
        !rows.iter().any(|row| row.contains("Pick the model")),
        "no menu opens: {rows:#?}"
    );
    assert_eq!(
        press(&mut ui, KeyCode::Enter, t0),
        [UiAction::Shell {
            command: "/bin/ls @src".into()
        }]
    );
}
