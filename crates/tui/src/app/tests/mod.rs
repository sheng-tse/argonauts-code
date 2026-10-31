use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Position, Size};

use super::input::QUIT_WINDOW;
use super::*;
use crate::base::input::prompt_history::PromptEntry;
use crate::base::theme::Glyphs;
use crate::bottom::{
    AgentRow, AgentState, ApprovalMode, HistorySearch, Picker, PickerEvent, PickerRow,
    QUESTION_GUARD, QuestionOption, QuestionPrompt, VimMode, WorkingLine,
};

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
    (
        Ui::new(terminal, theme, ViewModel::new("conversation"), t0),
        t0,
    )
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

fn question(id: u64) -> QuestionPrompt {
    let mut question = QuestionPrompt::new(
        id,
        "Merge task-002 into main?",
        "Merge task-002?",
        vec![
            QuestionOption::irreversible("Merge"),
            QuestionOption::new("Keep the branch"),
        ],
    );
    question.allow_note = true;
    question
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

mod over_question;

/// The first start after an update shows its row above the input, and your first key takes
/// it away.
#[test]
fn the_updated_row_shows_until_the_first_key() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.updated = Some("Updated to 0.1.1 · /release-notes".to_owned());
    ui.frame(t0).unwrap();
    let rows = live(&ui);
    assert_eq!(rows[0], "● Updated to 0.1.1 · /release-notes", "{rows:#?}");
    assert_eq!(rows[1], "", "{rows:#?}");
    assert!(rows[2].starts_with('─'), "the input under it: {rows:#?}");

    let later = t0 + Duration::from_millis(100);
    press(&mut ui, KeyCode::Char('h'), later);
    ui.frame(later).unwrap();
    let rows = live(&ui);
    assert!(rows.iter().any(|row| row == "❯ h"), "{rows:#?}");
    assert!(
        !rows.iter().any(|row| row.contains("Updated to")),
        "{rows:#?}"
    );
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

    ui.model_mut().bottom.question = Some(question(4));
    press(&mut ui, KeyCode::Tab, t0);
    assert!(ui.model().bottom.focused_question().is_some());
    press(&mut ui, KeyCode::Tab, t0);
    assert!(ui.model().bottom.question.as_ref().unwrap().has_open_note());
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    assert!(!ui.model().bottom.question.as_ref().unwrap().has_open_note());
    assert_eq!(
        press(&mut ui, KeyCode::Esc, t0),
        [UiAction::Dismiss { question: 4 }]
    );
    assert!(ui.model().bottom.question.as_ref().unwrap().dismissed);
    assert_eq!(press(&mut ui, KeyCode::Esc, t0), [UiAction::Interrupt]);

    ui.model_mut().bottom.agents.rows = vec![AgentRow::new("codex", "task-3", AgentState::Working)];
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
        !rows.iter().any(|row| row.contains("supervised mode on")),
        "the search stands in for the input and the bottom line: {rows:#?}"
    );

    assert!(
        press(&mut ui, KeyCode::Enter, t0).is_empty(),
        "Enter never sends"
    );
    let bottom = &ui.model().bottom;
    assert!(bottom.history_search.is_none());
    assert_eq!(bottom.composer.text(), "fix the export test");
    assert_eq!(bottom.mode.vim, Some(VimMode::Normal));

    // In NORMAL Ctrl+R redoes, as in vim, so `/` opens it again; Esc closes it with the
    // input as it was, and a paste filters.
    assert!(chord(&mut ui, 'r', t0).is_empty());
    assert_eq!(ui.model().bottom.composer.text(), "fix the export test");
    assert_eq!(
        press(&mut ui, KeyCode::Char('/'), t0),
        [UiAction::SearchHistory]
    );
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

/// A picture dropped into a terminal that types the path out as keys, as Terminal.app does,
/// becomes its chip as a pasted one does, though the path starts with `/`.
#[test]
fn a_picture_path_typed_out_as_keys_becomes_a_chip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shot.png");
    std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
    let (mut ui, t0) = ui();
    ui.time_keys(true);
    let typed = path.display().to_string();
    assert!(type_at(&mut ui, &typed, t0, 1).is_empty());
    ui.frame(t0 + Duration::from_millis(600)).unwrap();
    assert_eq!(ui.model().bottom.composer.text(), "[Image #1] ");
}

/// A `?` in a paste typed out as keys is part of the text, not the key that opens the shortcuts,
/// though the draft looks empty while the paste is held back.
#[test]
fn a_question_mark_in_a_paste_typed_out_as_keys_stays_in_it() {
    let (mut ui, t0) = ui();
    ui.time_keys(true);
    assert!(type_at(&mut ui, "what did you change?", t0, 1).is_empty());
    assert!(!ui.model().bottom.menus.shortcuts_open);
    ui.frame(t0 + Duration::from_millis(60)).unwrap();
    assert_eq!(ui.model().bottom.composer.text(), "what did you change?");

    // On an empty prompt at a person's pace, `?` still opens them.
    let mut ui = ui_at(100).0;
    ui.time_keys(true);
    assert!(type_at(&mut ui, "?", t0, 0).is_empty());
    assert!(ui.model().bottom.menus.shortcuts_open);
}

/// After a failure, `r` in the empty input opens the text behind it once, as its hint says,
/// and Esc comes back. Then `r` is a letter again, so a message such as "retry" types as it
/// is.
#[test]
fn r_after_a_failure_shows_its_raw_text_once() {
    let (mut ui, t0) = ui();
    let raw = crate::bridge::failure::RawFailure {
        title: "The run failed".into(),
        text: "fatal: one\nhint: two".into(),
    };
    ui.model_mut().raw_failure = Some(raw);
    press(&mut ui, KeyCode::Char('r'), t0);
    assert!(ui.is_overlay_open());
    assert_eq!(ui.model().bottom.composer.text(), "");
    press(&mut ui, KeyCode::Esc, t0);
    assert!(!ui.is_overlay_open());
    assert_eq!(ui.model().raw_failure, None);

    type_text(&mut ui, "retry", t0);
    assert!(!ui.is_overlay_open());
    assert_eq!(ui.model().bottom.composer.text(), "retry");
}

/// With no failure behind it, `r` is only ever a letter.
#[test]
fn r_with_no_failure_is_a_letter() {
    let (mut ui, t0) = ui();
    type_text(&mut ui, "r", t0);
    assert!(!ui.is_overlay_open());
    assert_eq!(ui.model().bottom.composer.text(), "r");
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
    // Wide enough that the bottom line keeps its hints.
    let (mut ui, t0) = ui_at(100);
    ui.model_mut().bottom.mode.vim = Some(VimMode::Insert);
    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    type_text(&mut ui, "fix it", t0);
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    assert_eq!(ui.model().bottom.mode.vim, Some(VimMode::Normal));
    ui.frame(t0).unwrap();
    let rows = screen(&ui);
    assert!(
        rows.iter()
            .any(|row| row
                .starts_with("  ǁ supervised mode on (shift+tab to cycle) · esc to interrupt")),
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

/// A question that shows while you type takes the keys at once. The draft goes aside, the rest
/// of the line is dropped as a burst typed as it showed, so neither the number in it nor the
/// Enter that ends it answers, and Esc brings the draft back as it was.
#[test]
fn a_question_that_shows_while_you_type_sets_the_draft_aside() {
    let (mut ui, t0) = ui();
    let at = |ms| t0 + Duration::from_millis(ms);
    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    type_text(&mut ui, "keep the CSV pa", t0);
    ui.model_mut().bottom.question = Some(needs_you(9));
    let mut actions = Vec::new();
    for c in "th, or 2".chars() {
        actions.extend(press(&mut ui, KeyCode::Char(c), at(10)));
    }
    actions.extend(press(&mut ui, KeyCode::Enter, at(20)));
    assert!(actions.is_empty(), "{actions:?}");
    assert!(ui.model().bottom.focused_question().is_some());
    assert!(
        ui.model().bottom.composer.is_empty(),
        "the draft went aside"
    );
    ui.frame(at(30)).unwrap();
    let rows = screen(&ui);
    assert!(
        rows.iter().any(|row| row.ends_with("1. Merge")),
        "{rows:#?}"
    );
    assert!(
        !rows
            .iter()
            .any(|row| row.contains("to answer the question") || row.contains("CSV")),
        "no input waits under the question: {rows:#?}"
    );

    assert_eq!(
        press(&mut ui, KeyCode::Esc, at(700)),
        [UiAction::Dismiss { question: 9 }]
    );
    assert_eq!(ui.model().bottom.composer.text(), "keep the CSV pa");
    type_text(&mut ui, "th", at(800));
    assert_eq!(
        press(&mut ui, KeyCode::Enter, at(800)),
        [UiAction::submit("keep the CSV path")]
    );
    assert!(
        ui.model()
            .bottom
            .question
            .as_ref()
            .is_some_and(|question| question.dismissed)
    );
}

#[test]
fn a_question_you_set_off_keeps_typing_and_slash_out_of_the_input() {
    let (mut ui, t0) = ui();
    let mut fallback = QuestionPrompt::new(
        12,
        "Codex is out until 4:10 PM. Who carries on?",
        "Who carries on?",
        vec![
            QuestionOption::new("Claude Code"),
            QuestionOption::new("agy"),
            QuestionOption::new("OpenCode"),
            QuestionOption::new("Wait for Codex"),
        ],
    )
    .yours();
    fallback.esc_answers = true;
    ui.model_mut().bottom.question = Some(fallback);
    ui.frame(t0).unwrap();
    assert!(ui.model().bottom.focused_question().is_some());
    for c in ['s', '/', 'm'] {
        assert!(press(&mut ui, KeyCode::Char(c), t0).is_empty());
    }
    assert!(ui.model().bottom.composer.is_empty());
    assert!(!menu_open(&ui));
    // ↓ past the last option stays on the question.
    for _ in 0..5 {
        press(&mut ui, KeyCode::Down, t0);
    }
    assert!(ui.model().bottom.focused_question().is_some());
    assert!(press(&mut ui, KeyCode::Char('x'), t0).is_empty());
    assert!(ui.model().bottom.composer.is_empty());
    assert_eq!(
        press(&mut ui, KeyCode::Esc, t0 + QUESTION_GUARD),
        [UiAction::Dismiss { question: 12 }]
    );
    assert!(ui.model().bottom.composer_has_focus());
}

#[test]
fn a_question_that_needs_you_keeps_typing_out_until_passed_on() {
    let (mut ui, t0) = ui_at(100);
    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    let mut merge = question(9);
    merge.needs_you = true;
    ui.model_mut().bottom.question = Some(merge);
    // Any event hands it the keys, as a question arriving does.
    ui.handle_event(Event::FocusGained, t0);
    assert!(ui.model().bottom.focused_question().is_some());
    ui.frame(t0).unwrap();
    // The question stands in for the input: only its rule shows, its own key line, then the mode.
    let rules = |rows: &[String]| {
        rows.iter()
            .filter(|row| !row.is_empty() && row.chars().all(|c| c == '─'))
            .count()
    };
    let rows = screen(&ui);
    assert_eq!(rules(&rows), 1, "{rows:#?}");
    assert_eq!(
        rows[rows.len() - 3..],
        [
            "  Enter to choose · Tab to add a note · Esc to carry on without me",
            "",
            "  ǁ supervised mode on"
        ],
        "{rows:#?}"
    );
    assert!(
        !rows
            .iter()
            .any(|row| row.contains("to answer the question")),
        "{rows:#?}"
    );

    // Letters, a paste, and the Enter after them, typed as it shows, reach neither the input
    // nor the question, and nothing on screen talks about them.
    for c in "keep it".chars() {
        assert!(press(&mut ui, KeyCode::Char(c), t0).is_empty());
    }
    ui.handle_event(Event::Paste("keep the branch".into()), t0);
    assert!(ui.model().bottom.composer.is_empty());
    ui.frame(t0 + Duration::from_millis(20)).unwrap();
    assert!(
        !screen(&ui).iter().any(|row| row.contains("Typing")),
        "{:#?}",
        screen(&ui)
    );
    assert!(press(&mut ui, KeyCode::Enter, t0 + QUESTION_GUARD / 2).is_empty());
    assert!(ui.model().bottom.question.is_some());

    // Passed on, it's off the screen for good, and the input takes typing and ↑ again.
    assert_eq!(
        press(&mut ui, KeyCode::Esc, t0 + QUESTION_GUARD * 3),
        [UiAction::Dismiss { question: 9 }]
    );
    assert!(ui.model().bottom.composer_has_focus());
    ui.frame(t0 + Duration::from_millis(40)).unwrap();
    let rows = screen(&ui);
    assert_eq!(rules(&rows), 2, "the input's rules are back: {rows:#?}");
    type_text(&mut ui, "keep it", t0);
    assert_eq!(
        press(&mut ui, KeyCode::Enter, t0),
        [UiAction::submit("keep it")]
    );
    press(&mut ui, KeyCode::Up, t0);
    assert!(
        ui.model().bottom.focused_question().is_none(),
        "↑  never brings back what you passed on"
    );
}

/// The draft comes back whole once the question is answered: a `!` command waiting in shell mode
/// returns in shell mode.
#[test]
fn a_draft_comes_back_whole_once_the_question_is_answered() {
    let (mut ui, t0) = ui();
    type_text(&mut ui, "!ls", t0);
    assert!(ui.model().bottom.composer.is_shell());
    ui.model_mut().bottom.question = Some(needs_you(9));
    ui.frame(t0).unwrap();
    let bottom = &ui.model().bottom;
    assert!(bottom.focused_question().is_some());
    assert!(bottom.composer.is_empty() && !bottom.composer.is_shell());
    assert_eq!(
        press(&mut ui, KeyCode::Char('2'), t0 + QUESTION_GUARD),
        answer(9, 1)
    );
    let bottom = &ui.model().bottom;
    assert!(bottom.composer.is_shell());
    assert_eq!(bottom.composer.text(), "ls");
    assert!(bottom.composer_has_focus());
}

/// A question takes the keys from the agent panel the moment it shows, and the Enter meant for the
/// panel's row neither opens the agent nor answers the question.
#[test]
fn a_question_takes_the_keys_from_the_agent_panel() {
    let (mut ui, t0) = ui();
    let at = |ms| t0 + Duration::from_millis(ms);
    ui.model_mut().bottom.agents.rows = vec![
        AgentRow::new("codex", "task-3", AgentState::Working),
        AgentRow::new("agy", "task-4", AgentState::Working),
    ];
    press(&mut ui, KeyCode::Down, t0);
    assert!(ui.model().bottom.agents.has_focus());
    ui.model_mut().bottom.question = Some(needs_you(9));
    ui.frame(at(10)).unwrap();
    let bottom = &ui.model().bottom;
    assert!(bottom.focused_question().is_some());
    assert!(!bottom.agents.has_focus());
    assert!(
        !screen(&ui)
            .iter()
            .any(|row| row.contains("to answer the question"))
    );
    assert!(press(&mut ui, KeyCode::Enter, at(50)).is_empty());
    assert!(press(&mut ui, KeyCode::Down, at(60)).is_empty());
    assert_eq!(
        press(&mut ui, KeyCode::Enter, at(50) + QUESTION_GUARD),
        answer(9, 1)
    );
    assert!(ui.model().bottom.composer_has_focus());
}

/// For a moment after a question takes the keys, only the arrows work on it: Enter, Esc, and a
/// number meant for what had the keys do nothing, and each holds the guard up a moment longer.
#[test]
fn keys_pressed_as_a_question_shows_never_answer_it() {
    let (mut ui, t0) = ui();
    let at = |ms| t0 + Duration::from_millis(ms);
    ui.model_mut().bottom.question = Some(needs_you(9));
    ui.frame(t0).unwrap();
    let selected = |ui: &Ui<vt100::Parser>| ui.model().bottom.question.as_ref().unwrap().selected;
    assert!(press(&mut ui, KeyCode::Enter, at(50)).is_empty());
    assert!(press(&mut ui, KeyCode::Esc, at(100)).is_empty());
    assert!(ui.model().bottom.focused_question().is_some());
    assert!(press(&mut ui, KeyCode::Down, at(150)).is_empty());
    assert_eq!(selected(&ui), 1, "the arrows still move");
    assert!(press(&mut ui, KeyCode::Char('1'), at(200)).is_empty());
    assert!(press(&mut ui, KeyCode::Enter, t0 + QUESTION_GUARD).is_empty());
    let past = t0 + QUESTION_GUARD * 2;
    assert!(press(&mut ui, KeyCode::Up, past).is_empty());
    assert_eq!(press(&mut ui, KeyCode::Enter, past), answer(9, 0));
}

/// With vim keys, `j` and `k` move the question as the arrows do even as it shows, and `l` typed
/// mid-line is a letter, never Enter. Set aside, the INSERT draft comes back.
#[test]
fn a_question_that_shows_mid_line_with_vim_keys_moves_but_never_answers() {
    let (mut ui, t0) = ui();
    let at = |ms| t0 + Duration::from_millis(ms);
    ui.model_mut().bottom.mode.vim = Some(VimMode::Insert);
    type_text(&mut ui, "ok ", t0);
    ui.model_mut().bottom.question = Some(needs_you(9));
    let selected = |ui: &Ui<vt100::Parser>| ui.model().bottom.question.as_ref().unwrap().selected;
    assert!(press(&mut ui, KeyCode::Char('j'), at(10)).is_empty());
    assert_eq!(selected(&ui), 1);
    assert!(press(&mut ui, KeyCode::Char('k'), at(20)).is_empty());
    assert_eq!(selected(&ui), 0);
    for c in "let it".chars() {
        assert!(press(&mut ui, KeyCode::Char(c), at(30)).is_empty(), "{c:?}");
    }
    // The rest of the line is dropped with the burst it came in.
    assert!(press(&mut ui, KeyCode::Enter, at(40)).is_empty());
    assert!(press(&mut ui, KeyCode::Char('l'), at(50)).is_empty());
    assert!(ui.model().bottom.focused_question().is_some());
    assert_eq!(
        press(&mut ui, KeyCode::Esc, at(700)),
        [UiAction::Dismiss { question: 9 }]
    );
    assert_eq!(ui.model().bottom.composer.text(), "ok ");
    assert_eq!(ui.model().bottom.mode.vim, Some(VimMode::Insert));
}

/// The paste-then-Enter and vim probes from the review, on a question that took the keys as it
/// showed: nothing typed answers it, Merge least of all.
#[test]
fn typing_meant_for_the_input_never_answers_a_question_that_took_the_keys() {
    let take = |vim: bool| {
        let (mut ui, t0) = ui();
        if vim {
            ui.model_mut().bottom.mode.vim = Some(VimMode::Insert);
        }
        let mut merge = question(9);
        merge.needs_you = true;
        ui.model_mut().bottom.question = Some(merge);
        ui.frame(t0).unwrap();
        assert!(ui.model().bottom.focused_question().is_some());
        (ui, t0)
    };
    let (mut ui, t0) = take(false);
    assert!(
        ui.handle_event(Event::Paste("keep the branch".into()), t0)
            .is_empty()
    );
    assert!(press(&mut ui, KeyCode::Enter, t0).is_empty());
    assert!(ui.model().bottom.question.is_some());

    for line in ["let me check", "we will see"] {
        let (mut ui, t0) = take(true);
        for c in line.chars() {
            let typed = press(&mut ui, KeyCode::Char(c), t0);
            assert!(typed.is_empty(), "{line:?}: {c:?} did {typed:?}");
        }
        assert!(press(&mut ui, KeyCode::Enter, t0).is_empty(), "{line:?}");
        assert!(ui.model().bottom.focused_question().is_some(), "{line:?}");
    }

    let (mut ui, t0) = take(false);
    for c in "also fix 2 more bugs".chars() {
        assert!(press(&mut ui, KeyCode::Char(c), t0).is_empty(), "{c:?}");
    }
    assert!(press(&mut ui, KeyCode::Enter, t0).is_empty());
    assert!(ui.model().bottom.question.is_some());
}

/// While a question that needs you has the keys, Ctrl+R and vim's `/` don't open the prompt
/// search and `!` doesn't start shell mode. Set aside, each works as before.
#[test]
fn a_question_with_the_keys_holds_off_the_search_and_shell_mode() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.mode.vim = Some(VimMode::Normal);
    ui.model_mut().bottom.question = Some(needs_you(9));
    ui.frame(t0).unwrap();
    assert!(ui.model().bottom.focused_question().is_some());

    assert!(chord(&mut ui, 'r', t0).is_empty());
    assert!(press(&mut ui, KeyCode::Char('/'), t0).is_empty());
    assert!(press(&mut ui, KeyCode::Char('!'), t0).is_empty());
    let bottom = &ui.model().bottom;
    assert!(bottom.history_search.is_none());
    assert!(bottom.focused_question().is_some());
    assert!(!bottom.composer.is_shell());
    assert!(bottom.composer.is_empty());

    assert_eq!(
        press(&mut ui, KeyCode::Esc, t0 + QUESTION_GUARD),
        [UiAction::Dismiss { question: 9 }]
    );
    assert_eq!(
        press(&mut ui, KeyCode::Char('/'), t0),
        [UiAction::SearchHistory]
    );
    open_search(&mut ui);
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    // Ctrl+R in NORMAL redoes, so it opens no search.
    assert!(chord(&mut ui, 'r', t0).is_empty());
    assert_eq!(ui.model().bottom.mode.vim, Some(VimMode::Normal));
    type_text(&mut ui, "i!", t0);
    assert!(ui.model().bottom.composer.is_shell());
    assert!(
        ui.model()
            .bottom
            .question
            .as_ref()
            .is_some_and(|question| question.dismissed)
    );
}

fn needs_you(id: u64) -> QuestionPrompt {
    let mut question = question(id);
    question.needs_you = true;
    question
}

/// A question takes the keys from an open prompt search, which waits under it with its filter and
/// comes back once the question is set aside.
#[test]
fn a_question_takes_the_keys_from_the_prompt_search() {
    let (mut ui, t0) = ui();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    assert_eq!(chord(&mut ui, 'r', t0), [UiAction::SearchHistory]);
    open_search(&mut ui);
    type_text(&mut ui, "exp", t0);
    ui.model_mut().bottom.question = Some(needs_you(9));
    ui.frame(at(10)).unwrap();
    assert!(ui.model().bottom.focused_question().is_some());
    let rows = screen(&ui);
    assert!(
        rows.iter().any(|row| row.ends_with("1. Merge")),
        "{rows:#?}"
    );
    assert!(
        !rows.iter().any(|row| row.contains("fix the export test")),
        "the search waits under the question: {rows:#?}"
    );
    assert!(press(&mut ui, KeyCode::Char('o'), at(20)).is_empty());
    let search = |ui: &Ui<vt100::Parser>| {
        ui.model()
            .bottom
            .history_search
            .as_ref()
            .map(|search| search.filter().to_owned())
    };
    assert_eq!(search(&ui).as_deref(), Some("exp"));
    assert_eq!(
        press(&mut ui, KeyCode::Esc, at(500)),
        [UiAction::Dismiss { question: 9 }]
    );
    ui.frame(at(510)).unwrap();
    assert!(
        screen(&ui)
            .iter()
            .any(|row| row.contains("fix the export test")),
        "{:#?}",
        screen(&ui)
    );
    assert!(press(&mut ui, KeyCode::Char('o'), at(520)).is_empty());
    assert_eq!(search(&ui).as_deref(), Some("expo"));
}

#[test]
fn a_focused_question_takes_keys_and_answers() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.question = Some(question(9));
    assert!(press(&mut ui, KeyCode::Up, t0).is_empty());
    assert!(ui.model().bottom.focused_question().is_some());
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
            question: 9,
            option: 1,
            note: Some("later please".into()),
            checked: Vec::new(),
        }]
    );
    assert!(ui.model().bottom.question.is_none());

    ui.model_mut().bottom.question = Some(question(10));
    press(&mut ui, KeyCode::Tab, t0);
    press(&mut ui, KeyCode::Down, t0);
    assert!(press(&mut ui, KeyCode::Char('y'), t0).is_empty());
    assert_eq!(ui.model().bottom.question.as_ref().unwrap().armed, None);
    // A letter does nothing, and the number after it picks.
    assert!(press(&mut ui, KeyCode::Char('1'), t0).is_empty());
    assert_eq!(ui.model().bottom.question.as_ref().unwrap().armed, Some(0));
    assert_eq!(
        press(&mut ui, KeyCode::Enter, t0),
        [UiAction::Answer {
            question: 10,
            option: 0,
            note: None,
            checked: Vec::new(),
        }]
    );
}

#[test]
fn down_past_the_last_option_goes_back_to_the_input() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.question = Some(question(9));
    press(&mut ui, KeyCode::Up, t0);
    press(&mut ui, KeyCode::Down, t0);
    assert!(ui.model().bottom.focused_question().is_some());
    assert!(press(&mut ui, KeyCode::Down, t0).is_empty());
    let waiting = ui
        .model()
        .bottom
        .question
        .as_ref()
        .expect("the task still waits");
    assert!(!waiting.focused && !waiting.dismissed);
    ui.frame(t0).unwrap();
    let rows = screen(&ui);
    assert!(rows.iter().any(|row| row == "  ↑  to answer"), "{rows:#?}");
    assert!(rows.iter().any(|row| row.starts_with("❯ ")), "{rows:#?}");

    type_text(&mut ui, "hi", t0);
    assert_eq!(ui.model().bottom.composer.text(), "hi");
    press(&mut ui, KeyCode::Backspace, t0);
    press(&mut ui, KeyCode::Backspace, t0);

    // ↑ comes back to the option it left.
    press(&mut ui, KeyCode::Up, t0);
    assert!(ui.model().bottom.focused_question().is_some());
    assert_eq!(
        press(&mut ui, KeyCode::Enter, t0),
        [UiAction::Answer {
            question: 9,
            option: 1,
            note: None,
            checked: Vec::new(),
        }]
    );
}

#[test]
fn up_takes_back_the_queue_or_reopens_a_pinned_question() {
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
    ui.model_mut().bottom.question = Some(question(3));
    assert!(press(&mut ui, KeyCode::Up, t0).is_empty());
    let opened = ui.model().bottom.question.as_ref().unwrap();
    assert!(!opened.dismissed && opened.focused);

    ui.model_mut().bottom.question = None;
    press(&mut ui, KeyCode::Up, t0);
    assert_eq!(ui.model().bottom.composer.text(), "older");
}

fn answer(question: u64, option: usize) -> Vec<UiAction> {
    vec![UiAction::Answer {
        question,
        option,
        note: None,
        checked: Vec::new(),
    }]
}

#[test]
fn a_question_moves_with_vim_keys_and_answers_by_number() {
    // Without vim keys `k` is a letter, which does nothing, and a number answers.
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.question = Some(question(20));
    press(&mut ui, KeyCode::Up, t0);
    assert!(press(&mut ui, KeyCode::Char('k'), t0).is_empty());
    assert_eq!(press(&mut ui, KeyCode::Char('2'), t0), answer(20, 1));

    let (mut ui, t0) = ui_at(80);
    ui.model_mut().bottom.mode.vim = Some(VimMode::Insert);
    ui.model_mut().bottom.question = Some(question(21));
    press(&mut ui, KeyCode::Up, t0);
    let selected = |ui: &Ui<vt100::Parser>| ui.model().bottom.question.as_ref().unwrap().selected;
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
            .question
            .as_ref()
            .unwrap()
            .note
            .as_ref()
            .unwrap()
            .text(),
        "jk"
    );
    press(&mut ui, KeyCode::Esc, t0);
    assert_eq!(press(&mut ui, KeyCode::Char('l'), t0), answer(21, 1));

    // `h` sets a question aside, as Esc does.
    ui.model_mut().bottom.question = Some(question(22));
    press(&mut ui, KeyCode::Up, t0);
    assert_eq!(
        press(&mut ui, KeyCode::Char('h'), t0),
        [UiAction::Dismiss { question: 22 }]
    );
    assert!(ui.model().bottom.question.as_ref().unwrap().dismissed);
}

/// In NORMAL, `j` and `k` are ↓ and ↑ in full, as in Claude Code: `k` walks back through what
/// you typed, `j` forward to the blank newest line, one more `j` into the list under the input,
/// and `k` from its top comes back to the prompt.
#[test]
fn with_vim_keys_j_and_k_are_the_arrows_in_normal() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.mode.vim = Some(VimMode::Insert);
    ui.model_mut()
        .bottom
        .composer
        .set_history(["fix the money rounding".to_owned()]);
    ui.model_mut().bottom.agents.rows =
        vec![AgentRow::new("claude-code", "task-2", AgentState::Working)];
    press(&mut ui, KeyCode::Esc, t0);
    press(&mut ui, KeyCode::Char('k'), t0);
    assert_eq!(ui.model().bottom.composer.text(), "fix the money rounding");
    press(&mut ui, KeyCode::Char('j'), t0);
    assert_eq!(ui.model().bottom.composer.text(), "");
    assert!(!ui.model().bottom.agents.has_focus());
    press(&mut ui, KeyCode::Char('j'), t0);
    assert!(ui.model().bottom.agents.has_focus(), "into the list, as ↓");
    press(&mut ui, KeyCode::Char('k'), t0);
    assert!(!ui.model().bottom.agents.has_focus(), "and back, as ↑");
    assert!(ui.model().bottom.composer.is_empty());
    // A command half typed keeps its own `j`: `dj` deletes, it never leaves for the list.
    press(&mut ui, KeyCode::Char('d'), t0);
    press(&mut ui, KeyCode::Char('j'), t0);
    assert!(!ui.model().bottom.agents.has_focus());
}

#[test]
fn the_agent_panel_moves_with_vim_keys() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.mode.vim = Some(VimMode::Insert);
    ui.model_mut().bottom.agents.rows = vec![
        AgentRow::new("claude-code", "task-2", AgentState::Working),
        AgentRow::new("codex", "task-3", AgentState::Working),
        AgentRow::new("agy", "task-4", AgentState::Working),
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
        AgentRow::new("claude-code", "task-2", AgentState::Working),
        AgentRow::new("codex", "task-3", AgentState::Working),
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
    // The first `x` asks on the bottom line, like Ctrl+C, and the second stops.
    assert!(press(&mut ui, KeyCode::Char('x'), t0).is_empty());
    assert_eq!(
        ui.model().bottom.active_hint(t0),
        Some("x again to stop codex · esc to keep")
    );
    assert!(ui.model().bottom.question.is_none());
    assert_eq!(
        press(&mut ui, KeyCode::Char('x'), t0),
        [UiAction::StopAgent(codex)]
    );
    assert_eq!(ui.model().bottom.active_hint(t0), None);
    // Typing with an agent's row selected starts a message to that agent, as in Claude Code:
    // the row stays selected, and Enter sends the draft there.
    // Once a draft is there, `x` and the digits that act on the list are letters of it.
    press(&mut ui, KeyCode::Char('h'), t0);
    press(&mut ui, KeyCode::Char('x'), t0);
    assert!(press(&mut ui, KeyCode::Char('2'), t0).is_empty());
    assert!(ui.model().bottom.agents.has_focus());
    assert_eq!(ui.model().bottom.composer.text(), "hx2");
    assert_eq!(
        press(&mut ui, KeyCode::Enter, t0),
        [UiAction::MessageAgent {
            agent: crate::bottom::AgentRef {
                run: None,
                session: "codex".into(),
            },
            name: "codex".into(),
            text: "hx2".into(),
        }]
    );
    assert!(ui.model().bottom.composer.is_empty());
    assert!(ui.model().bottom.agents.has_focus());
    // Esc with a draft lets the list go and keeps the draft, which is the main agent's now.
    press(&mut ui, KeyCode::Char('h'), t0);
    press(&mut ui, KeyCode::Esc, t0);
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

/// Ctrl+X Ctrl+K stops every background agent on its second press, in Claude Code's words,
/// and says so when none is running.
#[test]
fn the_stop_all_chord_asks_once_then_stops_every_background_agent() {
    let (mut ui, t0) = ui();
    let stop_all = |ui: &mut Ui<vt100::Parser>, at: Instant| {
        assert!(chord(ui, 'x', at).is_empty());
        chord(ui, 'k', at)
    };
    assert!(stop_all(&mut ui, t0).is_empty());
    let hint = ui.model().bottom.hint.clone().unwrap();
    assert_eq!(hint.text, "No background agents running");

    ui.model_mut().bottom.agents.rows = vec![
        AgentRow::main(),
        AgentRow::new("codex", "task-3", AgentState::Working),
    ];
    assert!(stop_all(&mut ui, t0).is_empty());
    let hint = ui.model().bottom.hint.clone().unwrap();
    assert_eq!(
        hint.text,
        "Press ctrl+x ctrl+k again to stop background agents"
    );
    // Too late is a first press again.
    let late = t0 + QUIT_WINDOW + Duration::from_millis(1);
    assert!(stop_all(&mut ui, late).is_empty());
    assert_eq!(
        stop_all(&mut ui, late + Duration::from_millis(400)),
        [UiAction::StopAllAgents]
    );
    assert!(ui.model().bottom.hint.is_none());
    // It works from the list too, where the hint names it.
    press(&mut ui, KeyCode::Down, late);
    assert!(ui.model().bottom.agents.has_focus());
    let listed = late + Duration::from_secs(10);
    assert!(stop_all(&mut ui, listed).is_empty());
    assert_eq!(
        stop_all(&mut ui, listed + Duration::from_millis(400)),
        [UiAction::StopAllAgents]
    );
}

#[test]
fn global_keys_ask_the_host() {
    let (mut ui, t0) = ui();
    assert_eq!(
        ui.handle_event(
            Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
            t0
        ),
        [UiAction::ModeChanged(ApprovalMode::Auto)]
    );
    assert_eq!(ui.model().bottom.mode.approval, ApprovalMode::Auto);
    assert_eq!(chord(&mut ui, 't', t0), [UiAction::OpenTree]);
    assert_eq!(chord(&mut ui, 'g', t0), [UiAction::EditDraft]);
    assert_eq!(chord(&mut ui, 'z', t0), [UiAction::Suspend]);
    assert_eq!(
        press(&mut ui, KeyCode::Left, t0),
        [UiAction::OpenAgentPanel]
    );
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
    ui.model_mut().bottom.composer.label = Some("captain".into());
    assert!(ui.frame(t0).unwrap());
    let rows = live(&ui);
    assert!(rows[0].ends_with(" captain ─"), "{rows:?}");
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
    let mut ui = Ui::new(terminal, Theme::dark(), ViewModel::new("conversation"), t0);
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
    assert_eq!(ui.terminal().title(), Some("✳ conversation"));
    ui.model_mut().bottom.agents.rows = vec![AgentRow::new("codex", "c", AgentState::Working)];
    ui.frame(t0 + Duration::from_millis(260)).unwrap();
    assert_eq!(ui.terminal().title(), Some("◐ conversation"));
    ui.frame(t0 + Duration::from_millis(1_000)).unwrap();
    assert_eq!(ui.terminal().title(), Some("◑ conversation"));
    ui.model_mut().bottom.question = Some(question(1));
    ui.frame(t0 + Duration::from_millis(1_100)).unwrap();
    assert_eq!(ui.terminal().title(), Some("◆ needs you · conversation"));
}

#[test]
fn typed_keys_kept_when_question_appears() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    type_text(&mut ui, "yes ", t0);
    ui.frame(t0).unwrap();
    // The question shows between two keys; what follows still goes to the prompt.
    ui.model_mut().bottom.question = Some(question(12));
    ui.frame(t0 + Duration::from_millis(20)).unwrap();
    let mut actions = Vec::new();
    for c in "keep it".chars() {
        actions.extend(press(&mut ui, KeyCode::Char(c), t0));
    }
    assert!(actions.is_empty(), "{actions:?}");
    assert_eq!(ui.model().bottom.composer.text(), "yes keep it");
    let waiting = ui.model().bottom.waiting_question().unwrap();
    assert!(!waiting.dismissed && waiting.armed.is_none());
}

#[test]
fn esc_in_the_input_never_answers_a_question() {
    let (mut ui, t0) = ui();
    let mut setup = question(8);
    setup.esc_answers = true;
    ui.model_mut().bottom.question = Some(setup);

    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    let waiting = ui.model().bottom.question.as_ref().unwrap();
    assert!(!waiting.dismissed && !waiting.focused);

    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    assert_eq!(press(&mut ui, KeyCode::Esc, t0), [UiAction::Interrupt]);
    assert!(!ui.model().bottom.question.as_ref().unwrap().dismissed);

    // With the question focused, Esc answers it as its hint says.
    press(&mut ui, KeyCode::Tab, t0);
    assert!(ui.model().bottom.focused_question().is_some());
    assert_eq!(
        press(&mut ui, KeyCode::Esc, t0),
        [UiAction::Dismiss { question: 8 }]
    );
}

fn with_commands(ui: &mut Ui<vt100::Parser>) {
    ui.model_mut().bottom.menus.commands = vec![
        crate::bottom::MenuItem::new("model", "Pick the model"),
        crate::bottom::MenuItem::new("permissions", "Pick the rules"),
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
fn esc_sets_question_aside_then_interrupts() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    ui.model_mut().bottom.question = Some(question(5));
    assert!(ui.model().bottom.focused_question().is_none());
    assert_eq!(
        press(&mut ui, KeyCode::Esc, t0),
        [UiAction::Dismiss { question: 5 }]
    );
    assert!(ui.model().bottom.question.as_ref().unwrap().dismissed);
    assert_eq!(press(&mut ui, KeyCode::Esc, t0), [UiAction::Interrupt]);
    assert_eq!(press(&mut ui, KeyCode::Esc, t0), [UiAction::Interrupt]);

    // Idle, a waiting question still takes the first Esc, and the next does nothing.
    ui.model_mut().bottom.working = None;
    ui.model_mut().bottom.question = Some(question(6));
    assert_eq!(
        press(&mut ui, KeyCode::Esc, t0),
        [UiAction::Dismiss { question: 6 }]
    );
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());

    // In vim INSERT the first Esc only leaves INSERT.
    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    ui.model_mut().bottom.mode.vim = Some(VimMode::Insert);
    ui.model_mut().bottom.question = Some(question(7));
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    assert_eq!(ui.model().bottom.mode.vim, Some(VimMode::Normal));
    assert_eq!(
        press(&mut ui, KeyCode::Esc, t0),
        [UiAction::Dismiss { question: 7 }]
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
    ui.set_keymap(crate::base::input::keymap::Keymap::from_settings(&overrides).unwrap());
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
        ("agent_panel_view".to_owned(), "ctrl+left".to_owned()),
        ("submit".to_owned(), "ctrl+s".to_owned()),
        ("newline".to_owned(), "enter".to_owned()),
    ]
    .into();
    ui.set_keymap(crate::base::input::keymap::Keymap::from_settings(&overrides).unwrap());
    assert!(press(&mut ui, KeyCode::Left, t0).is_empty());
    assert_eq!(
        ui.handle_event(
            Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL)),
            t0
        ),
        [UiAction::OpenAgentPanel]
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

/// In a settings picker the key `save_for_session` binds picks for this session only, and a
/// rebinding moves it: ctrl+s is then the picker's own key again.
#[test]
fn a_rebound_save_for_session_picks_for_this_session() {
    let (mut ui, t0) = ui();
    assert_eq!(
        ui.model().bottom.save_for_session_key.as_deref(),
        Some("ctrl+s")
    );
    let overrides = [("save_for_session".to_owned(), "ctrl+l".to_owned())].into();
    ui.set_keymap(crate::base::input::keymap::Keymap::from_settings(&overrides).unwrap());
    assert_eq!(
        ui.model().bottom.save_for_session_key.as_deref(),
        Some("ctrl+l")
    );
    let mut picker = Picker::new("Theme", "");
    picker.flow.saves_settings = true;
    picker.set_rows(vec![PickerRow::new("dark", "")]);
    ui.model_mut().bottom.picker = Some(picker);
    let ctrl = |c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    ui.handle_event(ctrl('s'), t0);
    let picker = ui.model().bottom.picker.as_ref().expect("still open");
    assert!(!picker.for_session);
    assert_eq!(
        ui.handle_event(ctrl('l'), t0),
        [UiAction::Picker(PickerEvent::Picked(0))]
    );
    let picker = ui
        .model()
        .bottom
        .picker
        .as_ref()
        .expect("the flow closes it");
    assert!(picker.for_session);
}

#[derive(Debug)]
struct Probe {
    name: &'static str,
    closes_on_esc: bool,
}

impl crate::frame::view::View for Probe {
    fn render(
        &mut self,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
        _ctx: &crate::frame::view::ViewCtx<'_>,
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
        _ctx: &crate::frame::view::ViewCtx<'_>,
    ) -> crate::frame::view::ViewOutcome {
        use crate::frame::view::ViewOutcome;
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

/// A view that docks where the input was, as following an agent does.
#[derive(Debug)]
struct DockedProbe;

impl crate::frame::view::View for DockedProbe {
    fn docked(&self) -> bool {
        true
    }

    fn render(
        &mut self,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
        _ctx: &crate::frame::view::ViewCtx<'_>,
    ) {
        buf.set_string(
            area.x,
            area.y,
            "docked probe",
            ratatui::style::Style::default(),
        );
    }

    fn handle_key(
        &mut self,
        _key: KeyEvent,
        _ctx: &crate::frame::view::ViewCtx<'_>,
    ) -> crate::frame::view::ViewOutcome {
        crate::frame::view::ViewOutcome::Consumed
    }
}

/// A docked view draws where a question would, so a question that asks closes it as it takes the
/// keys, and the keys go to the question.
#[test]
fn a_question_takes_the_keys_from_a_docked_view() {
    let (mut ui, t0) = ui();
    let state = crate::frame::state::AppState::default();
    ui.push_view(Box::new(DockedProbe));
    ui.frame_in(t0, &state).unwrap();
    assert!(screen(&ui).iter().any(|row| row == "docked probe"));
    ui.model_mut().bottom.question = Some(needs_you(9));
    let shown = t0 + Duration::from_millis(10);
    ui.frame_in(shown, &state).unwrap();
    assert_eq!(ui.views_open(), 0);
    let rows = screen(&ui);
    assert!(!rows.iter().any(|row| row == "docked probe"), "{rows:#?}");
    assert!(
        rows.iter().any(|row| row.ends_with("1. Merge")),
        "{rows:#?}"
    );
    let two = Event::Key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
    assert_eq!(
        ui.handle_event_in(two, shown + QUESTION_GUARD, &state),
        answer(9, 1)
    );
}

/// A view on its own screen would keep the keys, so it closes as the question takes them. A
/// view the question opens itself, such as its diff, stays over it.
#[test]
fn a_question_takes_the_keys_from_a_full_screen_view() {
    let (mut ui, t0) = ui();
    let state = crate::frame::state::AppState::default();
    let probe = || Probe {
        name: "outer",
        closes_on_esc: true,
    };
    ui.push_view(Box::new(probe()));
    ui.frame_in(t0, &state).unwrap();
    assert!(ui.terminal().writer().screen().alternate_screen());
    ui.model_mut().bottom.question = Some(needs_you(9));
    let shown = t0 + Duration::from_millis(10);
    ui.frame_in(shown, &state).unwrap();
    assert_eq!(ui.views_open(), 0);
    assert!(!ui.terminal().writer().screen().alternate_screen());
    let rows = screen(&ui);
    assert!(
        rows.iter().any(|row| row.ends_with("1. Merge")),
        "{rows:#?}"
    );

    ui.push_view(Box::new(probe()));
    ui.frame_in(shown + Duration::from_millis(20), &state)
        .unwrap();
    assert_eq!(ui.views_open(), 1, "its own view stays open over it");
}

/// A question that asks closes the Ctrl+O transcript too, and its number keys answer it.
#[test]
fn a_question_takes_the_keys_from_the_transcript() {
    let (mut ui, t0) = ui();
    chord(&mut ui, 'o', t0);
    assert!(ui.overlay.is_some());
    ui.model_mut().bottom.question = Some(needs_you(9));
    ui.frame(t0 + Duration::from_millis(10)).unwrap();
    assert!(ui.overlay.is_none());
    assert_eq!(
        press(
            &mut ui,
            KeyCode::Char('2'),
            t0 + Duration::from_millis(10) + QUESTION_GUARD
        ),
        answer(9, 1)
    );
}

/// A question a command opens while an answer runs, as `/git`'s does, has the keys, and Esc
/// answers that question alone: the answer keeps running.
#[test]
fn esc_on_a_question_opened_mid_answer_leaves_the_answer_running() {
    let (mut ui, t0) = ui();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    let mut question = question(9);
    question.focused = true;
    ui.model_mut().bottom.question = Some(question);
    ui.frame(at(10)).unwrap();
    assert!(ui.model().bottom.focused_question().is_some());
    assert_eq!(
        press(&mut ui, KeyCode::Esc, at(500)),
        [UiAction::Dismiss { question: 9 }]
    );
    assert!(ui.model().bottom.working.is_some());
}

/// The mods pinned above the input sit between the working line and the input's rule. Ctrl+X
/// Tab gives them the keys, the bottom line lists what the keys do, and each change goes to the
/// controller as one setting.
#[test]
fn the_mods_above_the_input_take_the_keys_with_ctrl_x_tab() {
    use argonauts_core::input::{ModPlaceChange, ModsAction, SlashCommand};
    use argonauts_core::settings::ui::ModName;

    let (mut ui, t0) = ui();
    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    let band = &mut ui.model_mut().bottom.band;
    band.mods = vec![ModName::PlanUsage, ModName::Spend];
    band.usage = Some(std::sync::Arc::new(crate::testing::sample::usage()));
    band.wall = crate::testing::sample::now();
    ui.frame(t0).unwrap();
    let rows = screen(&ui);
    let at = |rows: &[String], text: &str| rows.iter().position(|row| row.contains(text));
    let (working, plan, input) = (
        at(&rows, "Rowing…").unwrap(),
        at(&rows, "◆ plan usage").unwrap(),
        at(&rows, "❯").unwrap(),
    );
    assert!(working < plan && plan < input, "{rows:#?}");

    assert!(chord(&mut ui, 'x', t0).is_empty());
    assert!(press(&mut ui, KeyCode::Tab, t0).is_empty());
    assert!(ui.model().bottom.band.has_focus());
    ui.frame(t0 + Duration::from_millis(20)).unwrap();
    let rows = screen(&ui);
    assert!(
        rows.iter()
            .any(|row| row.contains("tab next · - fold · enter open · p unpin")),
        "{rows:#?}"
    );
    let changed = |the_mod, change| {
        [UiAction::Command(argonauts_core::app::UiCommand::Slash {
            command: SlashCommand::Mods {
                action: ModsAction::Change { the_mod, change },
                scope: None,
            },
        })]
    };
    assert_eq!(
        press(&mut ui, KeyCode::Char('-'), t0),
        changed(ModName::PlanUsage, ModPlaceChange::Fold)
    );
    assert_eq!(
        press(&mut ui, KeyCode::Enter, t0),
        [UiAction::OpenTab(ModName::PlanUsage)]
    );
    assert!(ui.model().bottom.composer.is_empty(), "nothing was typed");
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    assert!(ui.model().bottom.composer_has_focus());
    assert!(
        ui.model().bottom.working.is_some(),
        "Esc only left the band"
    );

    // Ctrl+X Ctrl+A hides them to one line, and shows them again.
    chord(&mut ui, 'x', t0);
    chord(&mut ui, 'a', t0);
    // The change of height waits a moment, then lands in one frame.
    ui.frame(t0 + Duration::from_millis(40)).unwrap();
    assert!(at(&screen(&ui), "◆ plan usage").is_some());
    ui.frame(t0 + Duration::from_millis(200)).unwrap();
    let rows = screen(&ui);
    assert!(
        at(&rows, "2 mods hidden · Codex 5-hour 92%").is_some(),
        "{rows:#?}"
    );
    assert!(at(&rows, "◆ plan usage").is_none(), "{rows:#?}");
    chord(&mut ui, 'x', t0);
    chord(&mut ui, 'a', t0);
    assert!(!ui.model().bottom.band.hidden);
}

/// A view a command opens while an answer runs draws as a panel under the working line, and
/// Esc closes the panel, never the answer.
#[test]
fn a_view_opened_as_a_panel_draws_under_the_working_line() {
    let (mut ui, t0) = ui();
    let state = crate::frame::state::AppState::default();
    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    ui.push_panel(Box::new(Probe {
        name: "outer",
        closes_on_esc: false,
    }));
    assert!(ui.views_in_panel());
    ui.frame_in(t0, &state).unwrap();
    assert!(!ui.terminal().writer().screen().alternate_screen());
    let rows = screen(&ui);
    let at = |text: &str| rows.iter().position(|row| row.contains(text));
    let (working, view) = (at("Rowing…").unwrap(), at("view outer").unwrap());
    assert!(working < view, "{rows:#?}");
    assert!(
        rows[view - 1].chars().all(|c| c == '─') && !rows[view - 1].is_empty(),
        "the panel's rule: {rows:#?}"
    );
    assert!(
        at("esc closes this · the answer keeps running").is_some_and(|hint| hint > view),
        "{rows:#?}"
    );
    assert!(
        !rows.iter().any(|row| row.starts_with('❯')),
        "the panel takes the input's place: {rows:#?}"
    );

    // A view opened from it stays in the panel.
    let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
    assert!(
        ui.handle_event_in(key(KeyCode::Char('o')), t0, &state)
            .is_empty()
    );
    assert_eq!(ui.views_open(), 2);
    assert!(ui.views_in_panel());

    // Esc goes back, then closes the panel, and never interrupts.
    for left in [1, 0] {
        let actions = ui.handle_event_in(key(KeyCode::Esc), t0, &state);
        assert!(actions.is_empty(), "{actions:?}");
        assert_eq!(ui.views_open(), left);
    }
    assert!(ui.model().bottom.working.is_some());
}

/// A view opened any other way takes the screen, as before, whatever runs. And a panel leaves
/// nothing behind: the next view opens on its own screen.
#[test]
fn a_view_opened_by_a_key_takes_the_screen_whatever_runs() {
    let (mut ui, t0) = ui();
    let state = crate::frame::state::AppState::default();
    let probe = || Probe {
        name: "outer",
        closes_on_esc: true,
    };
    ui.model_mut().bottom.working = Some(WorkingLine::new("Rowing", t0));
    ui.push_panel(Box::new(probe()));
    ui.close_views();
    ui.push_view(Box::new(probe()));
    assert!(!ui.views_in_panel());
    ui.frame_in(t0, &state).unwrap();
    assert!(ui.terminal().writer().screen().alternate_screen());

    // Over views on their own screen, a panel joins them there.
    ui.push_panel(Box::new(probe()));
    assert!(!ui.views_in_panel());
}

#[test]
fn a_paste_goes_to_the_view_on_top() {
    use argonauts_core::app::SettingRow;
    use argonauts_core::settings::SettingScope;

    let (mut ui, t0) = ui();
    let state = crate::frame::state::AppState::default();
    type_text(&mut ui, "kept", t0);
    let row = |key: &str| SettingRow {
        key: key.to_owned(),
        value: String::new(),
        layer: "built-in".to_owned(),
        workflow_from: None,
        default_scope: SettingScope::Global,
        safety: false,
        allowed: Vec::new(),
        description: String::new(),
        picked: None,
    };
    ui.push_view(Box::new(
        crate::views::settings::all_settings::AllSettingsView::new(vec![
            row("ui.accent"),
            row("ui.theme"),
        ]),
    ));
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
fn view_stack_esc_returns_to_conversation() {
    let (mut ui, t0) = ui();
    let state = crate::frame::state::AppState::default();
    type_text(&mut ui, "kept", t0);
    ui.frame(t0).unwrap();
    let conversation = screen(&ui);

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
    assert_eq!(screen(&ui), conversation);
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
            bottom.agents.rows = vec![AgentRow::new("codex", "task-3", AgentState::Working)];
            bottom.question = Some(question(1));
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
fn a_view_option_opens_full_screen_and_esc_returns_to_the_question() {
    let (mut ui, t0) = ui();
    let mut with_view = question(3);
    with_view
        .options
        .push(QuestionOption::view("Show the diff"));
    ui.model_mut().bottom.question = Some(with_view);
    ui.frame(t0).unwrap();
    press(&mut ui, KeyCode::Up, t0);
    let actions = press(&mut ui, KeyCode::Char('3'), t0);
    assert_eq!(
        actions,
        [UiAction::OpenView {
            question: 3,
            option: 2
        }]
    );
    assert!(
        ui.model()
            .bottom
            .question
            .as_ref()
            .is_some_and(|question| !question.dismissed)
    );

    let diff = crate::cells::cell::DiffView::between("a\n", "b\n", 0);
    let cell = crate::cells::cell::DiffCell {
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
    let session =
        crate::cells::cell::ToolCall::described(crate::cells::cell::ToolKind::Agent, "builder")
            .with_item(crate::cells::cell::ToolItem::text("halfway"));
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
        crate::bottom::MenuItem::new("permissions", "Pick the rules"),
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
    let rule = rows.iter().rposition(|row| row.starts_with('─')).unwrap();
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
fn visual_shows_on_the_bottom_row_and_esc_leaves_it_before_interrupting() {
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
        rows.iter()
            .any(|row| row
                .starts_with("  ǁ supervised mode on (shift+tab to cycle) · esc to interrupt")),
        "{rows:#?}"
    );

    // Nor does Esc after a command's first key, which it only drops.
    press(&mut ui, KeyCode::Char('d'), t0);
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    press(&mut ui, KeyCode::Char('w'), t0);
    assert_eq!(ui.model().bottom.composer.text(), "fix it");
    assert_eq!(press(&mut ui, KeyCode::Esc, t0), [UiAction::Interrupt]);

    // A waiting question stays open while Esc leaves VISUAL LINE.
    ui.model_mut().bottom.question = Some(question(3));
    press(&mut ui, KeyCode::Char('V'), t0);
    assert_eq!(ui.model().bottom.mode.vim, Some(VimMode::Visual));
    assert!(press(&mut ui, KeyCode::Esc, t0).is_empty());
    assert!(!ui.model().bottom.question.as_ref().unwrap().dismissed);

    // A visual delete ends VISUAL, and the bottom line follows.
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

/// A question that asks takes the keys from an open picker, which waits under it, hidden, and has
/// them again once the question is set aside.
#[test]
fn a_question_takes_the_keys_from_an_open_picker() {
    let (mut ui, t0) = ui();
    let mut picker = Picker::new("Theme", "Sets ui.theme");
    picker.rows = ["auto", "dark", "light"]
        .into_iter()
        .map(|label| PickerRow::new(label, ""))
        .collect();
    ui.model_mut().bottom.picker = Some(picker);
    ui.model_mut().bottom.question = Some(needs_you(9));
    ui.frame(t0).unwrap();
    assert!(ui.model().bottom.focused_question().is_some());
    assert!(!screen(&ui).iter().any(|row| row.contains("light")));
    assert!(press(&mut ui, KeyCode::Char('3'), t0).is_empty());
    assert_eq!(
        press(&mut ui, KeyCode::Esc, t0 + QUESTION_GUARD),
        [UiAction::Dismiss { question: 9 }]
    );
    ui.frame(t0 + QUESTION_GUARD).unwrap();
    assert!(screen(&ui).iter().any(|row| row.contains("light")));
    assert_eq!(
        press(&mut ui, KeyCode::Char('3'), t0 + QUESTION_GUARD),
        [UiAction::Picker(PickerEvent::Picked(2))]
    );
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
fn a_picker_list_filters_on_a_paste_and_never_opens_the_agent_panel_view() {
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
fn a_focused_question_holds_every_key_but_shift_tab_and_the_quit_keys() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.question = Some(question(5));
    press(&mut ui, KeyCode::Up, t0);
    assert!(ui.model().bottom.focused_question().is_some());
    // Shift+Tab still cycles the mode, as at Claude Code's prompts: autopilot settles the question,
    // which keeps the keys until then.
    assert_eq!(
        shift_tab(&mut ui, t0),
        [UiAction::ModeChanged(ApprovalMode::Auto)]
    );
    assert_eq!(
        shift_tab(&mut ui, t0),
        [UiAction::ModeChanged(ApprovalMode::Autopilot)]
    );
    assert_eq!(ui.model().bottom.mode.approval, ApprovalMode::Autopilot);
    assert!(ui.model().bottom.focused_question().is_some());
    assert!(press(&mut ui, KeyCode::Left, t0).is_empty());
    assert!(chord(&mut ui, 'o', t0).is_empty());
    assert!(ui.model().bottom.focused_question().is_some());
    assert!(chord(&mut ui, 'd', t0).is_empty());
    assert_eq!(
        chord(&mut ui, 'd', t0 + Duration::from_millis(100)),
        [UiAction::Quit]
    );
}

fn answered_pick() -> (crate::bottom::AnsweredPick, argonauts_core::app::UiCommand) {
    let run = argonauts_core::RunId::new("run-20261002-101500-3fa2").unwrap();
    let id = argonauts_core::base::names::ApprovalId::numbered(4);
    let pick =
        crate::bottom::AnsweredPick::new(run.clone(), id.clone(), "task-003 fell back to Codex");
    (
        pick,
        argonauts_core::app::UiCommand::RememberPick { run, id },
    )
}

#[test]
fn up_picks_the_answered_fallback_pick_and_m_remembers_it() {
    let (mut ui, t0) = ui_at(80);
    ui.model_mut().bottom.composer.remember("older");
    let (pick, remember) = answered_pick();
    ui.model_mut().bottom.answered_pick = Some(pick);
    ui.frame(t0).unwrap();
    let rows = live(&ui);
    assert!(
        rows.iter()
            .any(|row| row == "◆ task-003 fell back to Codex · ↑  to remember this pick"),
        "{rows:#?}"
    );

    assert!(press(&mut ui, KeyCode::Up, t0).is_empty());
    assert!(ui.model().bottom.pick_selected());
    assert!(
        ui.model().bottom.composer.is_empty(),
        "↑  went to the pick first"
    );
    ui.frame(t0 + Duration::from_secs(1)).unwrap();
    let rows = live(&ui);
    let selected = "❯ task-003 fell back to Codex · m to save it as a rule · esc to dismiss";
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
fn a_waiting_question_comes_before_the_answered_pick() {
    let (mut ui, t0) = ui();
    let (pick, _) = answered_pick();
    ui.model_mut().bottom.answered_pick = Some(pick);
    ui.model_mut().bottom.question = Some(question(9));
    press(&mut ui, KeyCode::Up, t0);
    assert!(ui.model().bottom.focused_question().is_some());
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
        "the bottom line says only that: {rows:#?}"
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

/// A view hides the bottom line, so why one of its keys can't act shows over its footer while
/// the notice lasts, wrapped when it needs two rows, and the footer comes back after.
#[test]
fn a_views_notice_shows_over_its_footer_while_it_lasts() {
    let (mut ui, t0) = ui_at(80);
    let mut state = crate::bridge::mentions::fixture::working_state();
    let run = state.attached.clone().unwrap();
    state.runs.get_mut(&run).unwrap().finished =
        Some(argonauts_core::runs::events::run::RunOutcome::Merged);
    ui.push_view(Box::new(crate::views::session::SessionView::new(
        run,
        argonauts_core::base::names::SessionKey::numbered(1),
    )));
    let actions = ui.handle_event_in(
        Event::Key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE)),
        t0,
        &state,
    );
    let [UiAction::Notice(text)] = actions.as_slice() else {
        panic!("t says why it can't take over");
    };
    ui.model_mut().bottom.mode.notice = Some(crate::bottom::ModeNotice {
        text: text.clone(),
        tone: crate::cells::cell::Tone::Dim,
        until: t0 + Duration::from_secs(4),
    });
    ui.frame_in(t0 + Duration::from_millis(20), &state).unwrap();
    let rows = screen(&ui);
    let bottom = rows[rows.len() - 2..].join(" ");
    assert!(
        bottom.starts_with("That run already merged, so its agents are done.")
            && bottom.ends_with("type /follow-up and what you want next."),
        "{rows:#?}"
    );

    ui.frame_in(t0 + Duration::from_secs(5), &state).unwrap();
    let rows = screen(&ui);
    assert!(rows[rows.len() - 1].contains("esc back"), "{rows:#?}");
}

/// The agent panel asks for the crew's rows each time it opens: from an agent's session with ←,
/// and on its own after a take-over, as from the input.
#[test]
fn the_agent_panel_asks_for_its_rows_however_it_opens() {
    use argonauts_core::app::ViewRequest;

    let (mut ui, t0) = ui_at(80);
    let state = crate::bridge::mentions::fixture::working_state();
    let run = state.attached.clone().unwrap();
    ui.push_view(Box::new(crate::views::session::SessionView::new(
        run,
        argonauts_core::base::names::SessionKey::numbered(1),
    )));
    assert!(ui.take_asked().is_empty());
    let left = Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    ui.handle_event_in(left, t0, &state);
    assert_eq!(ui.take_asked(), [ViewRequest::Crew]);
    assert!(ui.take_asked().is_empty(), "asked once");

    ui.close_views();
    ui.push_view(Box::new(crate::views::crew::AgentPanelView::new(&state)));
    assert_eq!(ui.take_asked(), [ViewRequest::Crew]);
}

use argonauts_core::app::UiCommand as Command;

#[test]
fn a_mod_s_own_chord_hides_it_or_shows_it_again() {
    use argonauts_core::app::ViewData;
    use argonauts_core::input::SlashCommand;
    use argonauts_core::settings::mods::view::{ModRow, ModRowKind};
    use argonauts_core::settings::mods::{ModOrigin, ModsView, builtin, check};

    let (name, text) = builtin::STARTERS
        .iter()
        .find(|(name, _)| *name == "checks")
        .unwrap();
    let view = ModsView {
        rows: vec![ModRow {
            name: (*name).to_owned(),
            origin: ModOrigin::Yours,
            path: String::new(),
            kind: ModRowKind::Card {
                file: Box::new(check::read(name, text).unwrap()),
                built_in: None,
                reading: None,
                note: None,
                needs_trust: false,
                asks_for_line: false,
                hidden_by_line: false,
            },
        }],
        refresh: true,
        ..ModsView::default()
    };
    let (mut ui, t0) = ui();
    // Before the mods are known, ctrl+x k is no chord.
    chord(&mut ui, 'x', t0);
    assert!(press(&mut ui, KeyCode::Char('k'), t0).is_empty());
    ui.model_mut().bottom.composer.clear();

    assert!(!ui.offer_view_data(&ViewData::Mods {
        view: Box::new(view)
    }));
    assert!(chord(&mut ui, 'x', t0).is_empty());
    assert_eq!(
        press(&mut ui, KeyCode::Char('k'), t0),
        [UiAction::Command(Command::Slash {
            command: SlashCommand::ModFile {
                name: "checks".to_owned(),
                words: vec!["toggle".to_owned()],
            },
        })]
    );
    assert!(ui.model().bottom.composer.is_empty());
}

fn notice() -> crate::bottom::Notice {
    use argonauts_core::agents::screen::{Drawn, DrawnButton, DrawnLook, DrawnRow, DrawnSpan};
    let button = |label: &str, handle| DrawnButton {
        label: label.to_owned(),
        plugin: "cc-plugin-you-should-know".to_owned(),
        handle,
    };
    crate::bottom::Notice::new(Drawn {
        rows: vec![DrawnRow {
            spans: vec![DrawnSpan {
                text: "✦ Heads up · Dana is away until Monday.".to_owned(),
                look: DrawnLook::Plain,
            }],
        }],
        buttons: vec![
            button("Learn more", 11),
            button("Knew this already", 12),
            button("Dismiss", 13),
        ],
        left_out: Vec::new(),
    })
}

#[test]
fn ctrl_x_chords_send_now_and_reach_the_notice() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.send_now = true;
    ui.model_mut().bottom.queued = vec!["also check the changelog".into()];
    // Ctrl+X waits for the second key, which completes the chord.
    assert!(chord(&mut ui, 'x', t0).is_empty());
    assert_eq!(
        chord(&mut ui, 's', t0 + Duration::from_millis(400)),
        [UiAction::Command(Command::SendNow)]
    );
    // Ctrl+Enter, where the terminal tells it apart.
    let ctrl_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL);
    assert_eq!(
        ui.handle_event(Event::Key(ctrl_enter), t0),
        [UiAction::Command(Command::SendNow)]
    );
    // Ctrl+X then nothing for 1.5 s does nothing, and the next key acts alone.
    assert!(chord(&mut ui, 'x', t0).is_empty());
    let late = t0 + Duration::from_millis(1600);
    assert!(chord(&mut ui, 's', late).is_empty());
    press(&mut ui, KeyCode::Char('a'), late);
    assert_eq!(ui.model().bottom.composer.text(), "a");

    // Ctrl+X Tab gives the notice the keys; a number presses its button.
    ui.model_mut().bottom.notice = notice();
    chord(&mut ui, 'x', t0);
    press(&mut ui, KeyCode::Tab, t0);
    assert!(ui.model().bottom.notice.has_focus());
    assert_eq!(
        press(&mut ui, KeyCode::Char('0'), t0),
        [UiAction::Command(Command::PressNotice {
            plugin: "cc-plugin-you-should-know".into(),
            handle: 13,
        })]
    );
    press(&mut ui, KeyCode::Right, t0);
    assert_eq!(
        press(&mut ui, KeyCode::Enter, t0),
        [UiAction::Command(Command::PressNotice {
            plugin: "cc-plugin-you-should-know".into(),
            handle: 12,
        })]
    );
    // Typing never reaches the input while it has the keys; Esc gives them back.
    press(&mut ui, KeyCode::Char('b'), t0);
    assert_eq!(ui.model().bottom.composer.text(), "a");
    press(&mut ui, KeyCode::Esc, t0);
    assert!(!ui.model().bottom.notice.has_focus());
    // Ctrl+X Ctrl+A hides it and shows it again.
    chord(&mut ui, 'x', t0);
    chord(&mut ui, 'a', t0);
    assert!(!ui.model().bottom.notice.shows());
    chord(&mut ui, 'x', t0);
    chord(&mut ui, 'a', t0);
    assert!(ui.model().bottom.notice.shows());
}

#[test]
fn ctrl_x_c_pins_or_unpins_the_context_mod() {
    use argonauts_core::input::{ModPlaceChange, ModsAction, SlashCommand};
    use argonauts_core::settings::ui::{ModName, Place};

    let (mut ui, t0) = ui();
    let toggle = |the_mod| {
        UiAction::Command(Command::Slash {
            command: SlashCommand::Mods {
                action: ModsAction::Change {
                    the_mod,
                    change: ModPlaceChange::Toggle {
                        place: Place::AboveInput,
                    },
                },
                scope: None,
            },
        })
    };
    assert!(chord(&mut ui, 'x', t0).is_empty());
    assert_eq!(
        press(&mut ui, KeyCode::Char('c'), t0),
        [toggle(ModName::Context)]
    );
    chord(&mut ui, 'x', t0);
    assert_eq!(
        press(&mut ui, KeyCode::Char('q'), t0),
        [toggle(ModName::PlanUsage)]
    );
    // The second key never reaches the input.
    assert_eq!(ui.model().bottom.composer.text(), "");
}

#[test]
fn send_now_with_text_and_nothing_queued_sends_the_text_first() {
    let (mut ui, t0) = ui();
    ui.model_mut().bottom.send_now = true;
    type_text(&mut ui, "also this", t0);
    let ctrl_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL);
    assert_eq!(
        ui.handle_event(Event::Key(ctrl_enter), t0),
        [
            UiAction::submit("also this"),
            UiAction::Command(Command::SendNow)
        ]
    );
    // Where the session can't take it, the key does nothing of the kind.
    ui.model_mut().bottom.send_now = false;
    type_text(&mut ui, "later", t0);
    assert!(
        !ui.handle_event(Event::Key(ctrl_enter), t0)
            .contains(&UiAction::Command(Command::SendNow))
    );
}

/// Opt+W ignores the word `ultracode` in the draft and brings it back, and does nothing a
/// draft without the word would notice. Where Option isn't Meta, macOS sends `∑` for it.
#[test]
fn opt_w_ignores_the_workflow_word_and_undoes_it() {
    let (mut ui, t0) = ui();
    ui.model.bottom.workflow_key = "opt+w".into();
    let opt_w = |ui: &mut Ui<vt100::Parser>| {
        ui.handle_event(
            Event::Key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::ALT)),
            t0,
        )
    };
    type_text(&mut ui, "ultracode fix it", t0);
    assert!(ui.model.bottom.composer.workflow_requested());

    opt_w(&mut ui);
    assert!(!ui.model.bottom.composer.workflow_requested());
    let said = ui.model.bottom.word_notice.clone().unwrap();
    assert_eq!(
        said.text,
        "Ultracode keyword ignored for this prompt · opt+w to undo"
    );
    assert_eq!(said.until, t0 + crate::bottom::WORD_IGNORED_NOTICE);

    opt_w(&mut ui);
    assert!(ui.model.bottom.composer.workflow_requested());
    assert_eq!(ui.model.bottom.word_notice, None);

    press(&mut ui, KeyCode::Char('∑'), t0);
    assert!(!ui.model.bottom.composer.workflow_requested());
    assert_eq!(ui.model.bottom.composer.text(), "ultracode fix it");

    // Without the word the same character is a letter like any other.
    ui.model.bottom.composer.clear();
    type_text(&mut ui, "sum ∑", t0);
    assert_eq!(ui.model.bottom.composer.text(), "sum ∑");
}

/// A seat's numbered logo goes out with the first frame that draws it, in the same update, and
/// not again.
#[test]
fn a_numbered_logo_goes_out_with_the_frame_that_first_draws_it() {
    use crate::bottom::{RunRowKind, WorkflowRow, WorkflowRowState};
    use argonauts_core::look::logos::{Logo, LogoMark};

    crate::base::kit::logo::set_takes_logos(true);
    let terminal = InlineTerminal::new(Vec::new(), Size::new(COLUMNS, ROWS), Position::new(0, 2));
    let theme = crate::base::kit::logo::golden::pictured(&Theme::dark());
    let t0 = Instant::now();
    let mut ui = Ui::new(terminal, theme, ViewModel::new("conversation"), t0);
    let mut seat = WorkflowRow::new("run-01#s-001", "Opus 5.5 · 2", WorkflowRowState::Running);
    seat.kind = RunRowKind::Work;
    seat.depth = 1;
    seat.logo = Some(Logo {
        agent: argonauts_core::base::names::AgentId("claude-code".to_owned()),
        mark: Some(LogoMark::ClaudeCode),
    });
    seat.number = Some(2);
    ui.model.bottom.workflows.rows = vec![
        WorkflowRow::new("run-01", "Rate limit the API", WorkflowRowState::Running),
        seat,
    ];
    ui.frame(t0).unwrap();
    let written = String::from_utf8_lossy(ui.terminal().writer()).into_owned();
    // Claude Code's logo numbered 2, on a dark screen.
    let picture = format!("a=p,U=1,i={}", 0x4c_5000 + 2);
    assert_eq!(written.matches(&picture).count(), 1, "{written:?}");
    ui.model.bottom.workflows.rows[1].name = "Opus 5.5 · 2 ".to_owned();
    ui.frame(t0 + Duration::from_secs(1)).unwrap();
    let written = String::from_utf8_lossy(ui.terminal().writer()).into_owned();
    assert_eq!(written.matches(&picture).count(), 1, "sent once");
    crate::base::kit::logo::set_takes_logos(false);
}
