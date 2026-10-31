//! A question or an approval takes the keys the moment it shows, from wherever they were, and
//! stands in for the input until it's answered. One you set off yourself, such as
//! a confirm, closes on Esc as Claude Code's do. On a run's question, Esc goes back to the list
//! unanswered: the question waits, and "Carry on without me", its last numbered option, is how
//! whoever asked decides and goes on. Letters do nothing, a digit picks, and Enter confirms.
//! Keys typed in a burst as it
//! shows are dropped, as Claude Code's dialogs drop them, so a sentence meant for the input
//! can't answer it, and nothing on screen says so.

mod render;

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::base::text::sanitize_line;
use crate::base::theme::AgentColor;
use crate::bottom::composer::{Composer, ComposerEvent};
use crate::council::screens::edits::SheetKey;
use crate::frame::view::list;

pub use render::QuestionView;

pub const NOTE_PLACEHOLDER: &str = "and add a note";

/// Esc on an app's question that work waits on: nothing is picked, and the one who asked
/// decides.
pub const ESC_HINT: &str = "Esc to carry on without me";

/// Esc on a run's question: it leaves the screen unanswered and waits for you.
pub const BACK_HINT: &str = "Esc back to the list";

/// A run's question's last option: whoever asked decides in your place and goes on.
pub const CARRY_ON: &str = "Carry on without me";

/// What a free-text question's open answer field shows until you type.
pub const ANSWER_PLACEHOLDER: &str = "Type your answer";

/// How long after a question shows only the arrows work on it, counted again from each key it
/// drops: a burst typed as it shows was meant for whatever had the keys before, and the first key
/// after a pause works. Claude Code's dialogs wait as long.
pub const QUESTION_GUARD: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionPrompt {
    pub id: u64,
    pub title: String,
    /// A few words for it, where its whole title is too long.
    pub short_title: String,
    pub chips: Vec<QuestionChip>,
    pub agent: Option<QuestionAgent>,
    pub detail: Vec<String>,
    /// A table in plain text whose notes are dim, as the setup question shows a setup: each row is
    /// split by tabs into parts, plain then dim in turn.
    pub noted_rows: Vec<String>,
    /// The noted rows hold two columns laid out to the screen's width: a row of four parts is
    /// a left cell's plain and dim words then a right cell's, the first such row with only dim
    /// words names the columns, and a row of other parts is drawn as a noted row is.
    pub two_columns: bool,
    /// A letter that opens what the question is about without an option listed for it, with the
    /// index that option has in the question, as the setup question's `d` shows the diff.
    pub view_key: Option<(char, usize)>,
    /// What the question asks about at length goes into the conversation before the question opens,
    /// so this stays short.
    pub body: Vec<String>,
    pub question: Option<String>,
    pub options: Vec<QuestionOption>,
    pub selected: usize,
    pub focused: bool,
    /// You set it off yourself, so it holds the keys whenever it's on screen.
    pub takes_focus: bool,
    /// It waits on your answer: work stops for it, or it asks your view, as a spot check does.
    /// So it takes the keys when it shows and keeps them until you answer or hand it on. A
    /// draft in the input goes aside meanwhile.
    pub needs_you: bool,
    /// You worked it with keys meant for it: you brought it up with ↑ or Tab, moved with the
    /// arrows, or set it off yourself. Until then vim's `l` and `h` are letters here, since a
    /// conversation line can start with them.
    pub engaged: bool,
    /// Until then only the arrows work on it, since it has only just shown.
    pub guard_until: Option<Instant>,
    /// It has shown, so the guard it starts with is spent.
    pub shown: bool,
    /// Esc took it off the screen unanswered. It comes back only when you open it again, from
    /// the list of what needs you or its run's row.
    pub dismissed: bool,
    pub allow_note: bool,
    /// A note on an option after Tab, or the answer typed into a free-text option.
    pub note: Option<QuestionNote>,
    pub note_placeholder: String,
    /// An option that can't be undone, picked by its number and waiting for Enter or the same
    /// number.
    pub armed: Option<usize>,
    pub checklist: Option<Checklist>,
    pub footer: Option<String>,
    /// What Esc does here, for the hint line, such as `Esc to cancel` on a confirm.
    pub esc_hint: String,
    /// Esc closes it with an answer, such as cancel or the recommended answers, instead of
    /// handing it on. Then only Esc on the focused question does that.
    pub esc_answers: bool,
    /// Only an option's own key answers it: its number, or the first letter of its label, `y`
    /// for `Yes`. Enter and Esc leave it as it is, so a key that slipped never runs what it
    /// offers.
    pub keys_only: bool,
    /// A run's question, which autopilot settles, so its key line names Shift+Tab as Claude Code's
    /// prompts do.
    pub offers_shift_tab: bool,
    /// Draws each option's description on the option's own row, this many cells after the
    /// longest label, where every row fits the width. Otherwise a description goes under its
    /// option.
    pub descriptions_beside: Option<usize>,
    /// Drawn between two faint rules, one cell in, with its title alone on its row, as the
    /// guided setup's steps and the start's own questions are.
    pub framed: bool,
    /// Its title is a warning, drawn amber, as a settings problem is. Only a framed question
    /// draws it so.
    pub warns: bool,
    /// What Enter does where "choose" isn't the word, such as `Enter to confirm` on a trust
    /// question.
    pub enter_hint: Option<String>,
    /// A sheet one option opens in the question's own place, such as Approve with edits on a
    /// council's plan. It takes the keys until it answers or goes back.
    pub sheet: Option<Box<crate::council::screens::edits::InQuestion>>,
}

/// One question's chip on a question that asks several.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionChip {
    pub label: String,
    /// The first chip not answered is the current one.
    pub answered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionAgent {
    /// Its display name, such as `GPT-6.1 Sol on Codex`.
    pub name: String,
    /// Its id, such as `codex/gpt-6.1-sol`, which keeps its color for the run.
    pub worker: String,
    pub color: Option<AgentColor>,
    pub detail: String,
}

/// A note on an option, or an answer typed into a free-text one, in the input box's own
/// editor: an answer takes new lines, pastes, and the cursor keys as the input does, and a note
/// is a line typed at its end.
#[derive(Debug, Clone)]
pub struct QuestionNote {
    pub option: usize,
    pub editor: Composer,
}

impl QuestionNote {
    pub fn new(option: usize) -> Self {
        Self {
            option,
            editor: Composer::field(),
        }
    }

    pub fn with_text(option: usize, text: &str) -> Self {
        let mut note = Self::new(option);
        note.editor.set_text(text);
        note
    }

    /// What was typed, pasted blocks in full.
    pub fn text(&self) -> String {
        self.editor.expanded_text()
    }

    pub fn is_empty(&self) -> bool {
        self.editor.is_empty()
    }
}

impl PartialEq for QuestionNote {
    fn eq(&self, other: &Self) -> bool {
        self.option == other.option
            && self.editor.text() == other.editor.text()
            && self.editor.cursor() == other.editor.cursor()
    }
}

impl Eq for QuestionNote {}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Checklist {
    pub option: usize,
    pub checked: Vec<bool>,
    pub cursor: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionOption {
    pub label: String,
    pub kind: OptionKind,
    /// A model the explanation starts with, such as the one that could take every paused task.
    pub model: Option<OptionModel>,
    pub description: Option<String>,
    /// What taking it costs, said beside its confirm, such as `its work is removed`.
    pub warning: Option<String>,
    /// A time the explanation ends with, counting down.
    pub counts_to: Option<Countdown>,
    pub recommended: bool,
    /// Rows a checklist option ticks.
    pub items: Vec<String>,
    /// Whether a toggle is on.
    pub on: bool,
    /// Its answer carries your words, such as the changes to make. On a question that takes notes,
    /// picking it opens the note line first, and a checklist opens it after the ticks.
    pub asks_note: bool,
    /// The council's seats that back it, as a side of a point left open: drawn after its label,
    /// in one column for every option that has some.
    pub seats: Vec<crate::council::Face>,
}

/// A model an option names, drawn in its CLI's color.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionModel {
    /// As people read it, such as `Gemini 3.6 Pro`.
    pub name: String,
    /// Its CLI's id, such as `antigravity-cli`.
    pub cli: String,
}

/// A time an option's explanation counts down to, as in `tried again in 4m48s`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Countdown {
    /// The words before the time left, such as `tried again in`.
    pub words: String,
    pub at: Instant,
}

impl Countdown {
    /// `tried again in 4m48s`, and `tried again in a moment` once the time has come.
    pub fn text(&self, now: Instant) -> String {
        let left = self.at.saturating_duration_since(now);
        // A second shows for as long as any of it is left.
        let secs = left.as_secs() + u64::from(left.subsec_nanos() > 0);
        if secs == 0 {
            return format!("{} a moment", self.words);
        }
        let time = crate::base::format::duration_slot(Duration::from_secs(secs));
        format!("{} {time}", self.words)
    }

    /// When its text next changes: as the second it shows runs out. `None` once it is due.
    fn next_change(&self, now: Instant) -> Option<Instant> {
        let left = self.at.checked_duration_since(now)?;
        if left.is_zero() {
            return None;
        }
        let into_second = match left.subsec_nanos() {
            0 => Duration::from_secs(1),
            part => Duration::from_nanos(u64::from(part)),
        };
        Some(now + into_second)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum OptionKind {
    #[default]
    Answer,
    /// Its number only moves the pointer, and Enter or the same number again answers.
    Irreversible,
    /// The question stays, and picking it never counts as the answer.
    View,
    /// Opens a line inside the question for your own answer.
    FreeText,
    Checklist,
    /// Its number flips it on or off and reports `OpenView`, so the host can read the new state.
    /// The question stays, and it never counts as the answer.
    Toggle,
}

impl QuestionOption {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            kind: OptionKind::Answer,
            model: None,
            description: None,
            warning: None,
            counts_to: None,
            recommended: false,
            items: Vec::new(),
            on: false,
            asks_note: false,
            seats: Vec::new(),
        }
    }

    pub fn irreversible(label: impl Into<String>) -> Self {
        Self::new(label).with_kind(OptionKind::Irreversible)
    }

    pub fn view(label: impl Into<String>) -> Self {
        Self::new(label).with_kind(OptionKind::View)
    }

    pub fn free_text(label: impl Into<String>) -> Self {
        Self::new(label).with_kind(OptionKind::FreeText)
    }

    pub fn checklist(label: impl Into<String>, items: Vec<String>) -> Self {
        Self {
            items,
            ..Self::new(label).with_kind(OptionKind::Checklist)
        }
    }

    pub fn toggle(label: impl Into<String>, on: bool) -> Self {
        Self {
            on,
            ..Self::new(label).with_kind(OptionKind::Toggle)
        }
    }

    pub fn with_kind(mut self, kind: OptionKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn recommended(mut self) -> Self {
        self.recommended = true;
        self
    }

    pub fn asks_note(mut self) -> Self {
        self.asks_note = true;
        self
    }

    pub fn is_view(&self) -> bool {
        self.kind == OptionKind::View
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionEvent {
    Answered {
        index: usize,
        note: Option<String>,
        checked: Vec<usize>,
    },
    /// The question stays open and nothing was answered.
    OpenView {
        index: usize,
    },
    /// Esc took it off the screen with nothing picked. What that means is the question's: one work
    /// waits on is passed on, and one you set off closes.
    Dismissed,
    /// ↓ went past the last option, so the keys go back to the input and the question waits.
    BackToInput,
    /// The selection moved, or a note or checklist changed.
    Moved,
    Unhandled,
}

impl QuestionPrompt {
    pub fn new(
        id: u64,
        title: impl Into<String>,
        short_title: impl Into<String>,
        options: Vec<QuestionOption>,
    ) -> Self {
        let mut question = Self {
            id,
            title: title.into(),
            short_title: short_title.into(),
            chips: Vec::new(),
            agent: None,
            detail: Vec::new(),
            noted_rows: Vec::new(),
            two_columns: false,
            view_key: None,
            body: Vec::new(),
            question: None,
            options,
            selected: 0,
            focused: false,
            takes_focus: false,
            needs_you: false,
            engaged: false,
            guard_until: None,
            shown: false,
            dismissed: false,
            allow_note: false,
            note: None,
            note_placeholder: NOTE_PLACEHOLDER.to_owned(),
            armed: None,
            checklist: None,
            footer: None,
            esc_hint: ESC_HINT.to_owned(),
            esc_answers: false,
            keys_only: false,
            offers_shift_tab: false,
            descriptions_beside: None,
            framed: false,
            warns: false,
            enter_hint: None,
            sheet: None,
        };
        question.open_field();
        question
    }

    /// A free-text question: its only option is your own answer, so the question shows its answer
    /// field open, rather than a list of one to open it from.
    pub fn is_answer_field(&self) -> bool {
        matches!(self.options.as_slice(), [only] if only.kind == OptionKind::FreeText)
    }

    fn open_field(&mut self) {
        if self.is_answer_field() && self.note.is_none() {
            self.note = Some(QuestionNote::new(0));
        }
    }

    pub fn waits(&self) -> bool {
        !self.focused
    }

    /// You gave it the keys, such as with ↑ on an empty input.
    pub fn focus(&mut self) {
        self.dismissed = false;
        self.focused = true;
        self.engaged = true;
        self.shown = true;
        self.guard_until = None;
        self.open_field();
    }

    /// A question you set off yourself: it opens with the keys, as Claude Code's questions do.
    pub fn yours(mut self) -> Self {
        self.takes_focus = true;
        self.focused = true;
        self.engaged = true;
        self
    }

    /// It asks you something: you set it off, or work waits on your answer. A question that asks
    /// takes the keys and stands in for the input, whatever its kind. A run's
    /// question your rules or autopilot still decide asks nothing yet, so it stays off the screen.
    pub fn asks(&self) -> bool {
        self.takes_focus || self.needs_you
    }

    /// Whether it takes the keys as it shows: any question that asks, until it's passed on.
    pub fn claims_keys(&self) -> bool {
        !self.dismissed && self.asks()
    }

    /// When the question next changes on its own: as a time one of its options counts down to
    /// loses a second. `None` when it shows no such time, or isn't on screen.
    pub fn next_change(&self, now: Instant) -> Option<Instant> {
        if self.dismissed {
            return None;
        }
        self.options
            .iter()
            .filter_map(|option| option.counts_to.as_ref()?.next_change(now))
            .min()
    }

    /// Takes the keys if it claims them, the moment it shows. Until [`QUESTION_GUARD`] has passed
    /// only the arrows work on it, on a question you set off too.
    pub fn take_keys(&mut self, now: Instant) -> bool {
        if !self.shown {
            self.shown = true;
            self.guard_until = Some(now + QUESTION_GUARD);
        }
        let takes = !self.focused && self.claims_keys();
        if takes {
            self.focus();
            self.engaged = self.takes_focus;
            self.guard_until = Some(now + QUESTION_GUARD);
        }
        takes
    }

    /// It showed, or dropped a key, less than [`QUESTION_GUARD`] ago.
    pub fn guarded(&self, now: Instant) -> bool {
        self.guard_until.is_some_and(|until| now < until)
    }

    /// Whether `key`, as the question reads it, is dropped because the question only just showed.
    /// The arrows still move, and Ctrl and Alt keys go on to the quit keys. Each key dropped
    /// holds the guard up a moment longer, so the rest of a burst is dropped too.
    pub fn held_off(&mut self, key: &KeyEvent, now: Instant) -> bool {
        if !self.guarded(now) {
            return false;
        }
        let moves = matches!(
            key.code,
            KeyCode::Up | KeyCode::Down | KeyCode::Home | KeyCode::End
        );
        if moves
            || key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return false;
        }
        self.guard_until = Some(now + QUESTION_GUARD);
        true
    }

    /// The key as the question reads it. Arrows, Home, End, and Tab are meant for the question, so
    /// they engage it. With vim keys on, `j` and `k` move, `g` and `G` go to the first and last
    /// option, and once the question is engaged `l` answers as Enter does and `h` hands it on as
    /// Esc does. A note being typed keeps its letters.
    pub fn read_key(&mut self, key: KeyEvent, vim: bool) -> KeyEvent {
        if matches!(
            key.code,
            KeyCode::Up | KeyCode::Down | KeyCode::Home | KeyCode::End | KeyCode::Tab
        ) {
            self.engaged = true;
        }
        if !vim || self.note.is_some() {
            return key;
        }
        match list::vim_equivalent(&key, KeyCode::Esc) {
            Some(meant) if self.engaged || !matches!(meant.code, KeyCode::Enter | KeyCode::Esc) => {
                meant
            }
            _ => key,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> QuestionEvent {
        if !self.focused || self.dismissed {
            return QuestionEvent::Unhandled;
        }
        if self.options.is_empty() {
            return match key.code {
                KeyCode::Esc => self.dismiss(),
                _ => QuestionEvent::Unhandled,
            };
        }
        self.selected = self.selected.min(self.options.len() - 1);
        if let Some(sheet) = self.sheet.as_mut().filter(|sheet| sheet.open) {
            return match sheet.key(key) {
                SheetKey::Moved => QuestionEvent::Moved,
                SheetKey::Answer(note) => QuestionEvent::Answered {
                    index: sheet.option,
                    note,
                    checked: Vec::new(),
                },
            };
        }
        if let Some(event) = self.checklist_key(key) {
            return event;
        }
        if let Some(event) = self.field_key(key) {
            return event;
        }
        if let Some(event) = self.note_key(key) {
            return event;
        }
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if let Some((letter, index)) = self.view_key
            && plain
            && key.code == KeyCode::Char(letter)
        {
            return QuestionEvent::OpenView { index };
        }
        if let KeyCode::Char(c) = key.code
            && plain
            && let Some(index) = self.option_for_letter(c)
        {
            return self.pick(index, true);
        }
        match key.code {
            // A y/N question answers to its letters alone: Enter and Esc do nothing.
            KeyCode::Enter | KeyCode::Esc if self.keys_only => QuestionEvent::Moved,
            KeyCode::Up => {
                self.move_to(self.selected.saturating_sub(1));
                QuestionEvent::Moved
            }
            // A question that asks keeps the keys, so typing never slips into the input under it.
            // Esc is the way out.
            KeyCode::Down if self.selected + 1 == self.options.len() && self.asks() => {
                QuestionEvent::Moved
            }
            KeyCode::Down if self.selected + 1 == self.options.len() => self.back_to_input(),
            KeyCode::Down => {
                self.move_to(self.selected + 1);
                QuestionEvent::Moved
            }
            KeyCode::Home => {
                self.move_to(0);
                QuestionEvent::Moved
            }
            KeyCode::End => {
                self.move_to(self.options.len() - 1);
                QuestionEvent::Moved
            }
            // Enter is the confirmation, so an option that can't be undone goes through too.
            KeyCode::Enter => self.pick(self.selected, true),
            KeyCode::Tab if self.can_note(self.selected) => {
                self.armed = None;
                self.note = Some(QuestionNote::new(self.selected));
                QuestionEvent::Moved
            }
            KeyCode::Esc if self.armed.is_some() => {
                self.armed = None;
                QuestionEvent::Moved
            }
            KeyCode::Esc => self.dismiss(),
            KeyCode::Char(c) if plain => match self.option_for_digit(c) {
                Some(index) => {
                    let confirmed = self.armed == Some(index);
                    self.pick(index, confirmed)
                }
                // A letter does nothing, but disarms an option that can't be undone, so the
                // Enter that ends a typed line never confirms it.
                None => {
                    self.armed = None;
                    QuestionEvent::Unhandled
                }
            },
            _ => QuestionEvent::Unhandled,
        }
    }

    /// The answer field is the whole question, so Esc hands the question on rather than closing the
    /// field. The arrows move between the field's rows, and past its edges have nowhere to go,
    /// though ↓ still hands the keys back to the input on a question that waits for them.
    fn field_key(&mut self, key: KeyEvent) -> Option<QuestionEvent> {
        if !self.is_answer_field() {
            return None;
        }
        self.open_field();
        let editor = &self.note.as_ref()?.editor;
        let (top, bottom) = (editor.cursor_on_first_row(), editor.cursor_on_last_row());
        match key.code {
            KeyCode::Esc => Some(self.dismiss()),
            KeyCode::Down if bottom && !self.asks() => Some(self.back_to_input()),
            KeyCode::Up if top => Some(QuestionEvent::Moved),
            KeyCode::Down if bottom => Some(QuestionEvent::Moved),
            KeyCode::Tab => Some(QuestionEvent::Moved),
            _ => None,
        }
    }

    /// Whether the open note is an answer typed into a free-text option, which takes every key
    /// the input does.
    fn note_is_answer(&self) -> bool {
        self.note.as_ref().is_some_and(|note| {
            self.options
                .get(note.option)
                .is_some_and(|option| option.kind == OptionKind::FreeText)
        })
    }

    fn checklist_key(&mut self, key: KeyEvent) -> Option<QuestionEvent> {
        let option = self.checklist.as_ref()?.option;
        // The note on the ticks takes the keys once it's open, and Esc goes back to the ticks.
        if self.note.is_some() {
            if key.code == KeyCode::Enter {
                return Some(self.send_checklist());
            }
            return match self.note_key(key) {
                Some(event) => Some(event),
                None if key.modifiers.contains(KeyModifiers::CONTROL) => None,
                None => Some(QuestionEvent::Moved),
            };
        }
        let wants_note = self.wants_note(option);
        let list = self.checklist.as_mut()?;
        let items = self
            .options
            .get(option)
            .map_or(0, |option| option.items.len());
        match key.code {
            KeyCode::Up => list.cursor = list.cursor.saturating_sub(1),
            KeyCode::Down => list.cursor = (list.cursor + 1).min(items.saturating_sub(1)),
            KeyCode::Home => list.cursor = 0,
            KeyCode::End => list.cursor = items.saturating_sub(1),
            KeyCode::Char(' ') => {
                if let Some(tick) = list.checked.get_mut(list.cursor) {
                    *tick = !*tick;
                }
            }
            // Sending nothing back would decide nothing, so Enter waits for a tick.
            KeyCode::Enter if !list.checked.contains(&true) => {}
            KeyCode::Enter if wants_note => {
                self.note = Some(QuestionNote::new(option));
            }
            KeyCode::Enter => return Some(self.send_checklist()),
            KeyCode::Esc => self.checklist = None,
            // Other keys do nothing while the checklist is open, except the global ones.
            _ if key.modifiers.contains(KeyModifiers::CONTROL) => return None,
            _ => {}
        }
        Some(QuestionEvent::Moved)
    }

    /// The ticked rows, with the note when one was written.
    fn send_checklist(&mut self) -> QuestionEvent {
        let Some(list) = self.checklist.as_ref() else {
            return QuestionEvent::Unhandled;
        };
        let checked: Vec<usize> = list
            .checked
            .iter()
            .enumerate()
            .filter_map(|(index, ticked)| ticked.then_some(index))
            .collect();
        if checked.is_empty() {
            return QuestionEvent::Moved;
        }
        let index = list.option;
        self.checklist = None;
        QuestionEvent::Answered {
            index,
            note: self.take_note(),
            checked,
        }
    }

    fn take_note(&mut self) -> Option<String> {
        self.note
            .take()
            .map(|note| note.text().trim().to_owned())
            .filter(|text| !text.is_empty())
    }

    /// An answer takes the keys the input does: Shift+Enter, Alt+Enter, and Ctrl+J start a new
    /// line, and the arrows, Home, and End move its cursor. Enter alone sends it. A note on an
    /// option is a line typed at its end, so keys that would move its cursor are taken too,
    /// where they would otherwise reach the prompt.
    fn note_key(&mut self, key: KeyEvent) -> Option<QuestionEvent> {
        let answer = self.note_is_answer();
        let note = self.note.as_mut()?;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Esc => {
                self.note = None;
                return Some(QuestionEvent::Moved);
            }
            KeyCode::Char('c') if ctrl && !note.is_empty() => {
                note.editor.clear();
                return Some(QuestionEvent::Moved);
            }
            KeyCode::Char('c') if ctrl => return None,
            KeyCode::Enter if !(answer && (shift || alt)) => return None,
            KeyCode::Tab => return None,
            _ => {}
        }
        if answer {
            // Past the answer's first or last row, the arrows are the question's.
            match key.code {
                KeyCode::Up if note.editor.cursor_on_first_row() => return None,
                KeyCode::Down if note.editor.cursor_on_last_row() => return None,
                _ => {}
            }
            // A key the editor had no use for, such as End at the line's end, stays in the
            // field, so it never moves the question's pointer and closes the answer. The app's
            // own shortcuts, such as Ctrl+O, go on to it.
            let ctrl_only = ctrl && !alt;
            return match note.editor.handle_key(key) {
                ComposerEvent::Unhandled if ctrl_only => None,
                _ => Some(QuestionEvent::Moved),
            };
        }
        match key.code {
            // Plain keys, and AltGr, which arrives as Ctrl+Alt on some systems.
            KeyCode::Char(_) if ctrl == alt => {}
            KeyCode::Char('u' | 'w') if ctrl => {}
            KeyCode::Backspace => {}
            KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End | KeyCode::Delete => {
                return Some(QuestionEvent::Moved);
            }
            _ => return None,
        }
        note.editor.handle_key(key);
        Some(QuestionEvent::Moved)
    }

    /// Returns whether a note took the text. An answer keeps a paste's line breaks, as the
    /// input does; a note takes it as one line.
    pub fn handle_paste(&mut self, text: &str) -> bool {
        let answer = self.note_is_answer();
        match &mut self.note {
            Some(note) if self.focused && !self.dismissed => {
                if answer {
                    note.editor.handle_paste(text);
                } else {
                    note.editor.handle_paste(&sanitize_line(text));
                }
                true
            }
            _ => false,
        }
    }

    pub fn has_open_note(&self) -> bool {
        self.note.is_some()
    }

    fn can_note(&self, index: usize) -> bool {
        self.note.is_none()
            && self
                .options
                .get(index)
                .is_some_and(|option| match option.kind {
                    OptionKind::FreeText => true,
                    OptionKind::View | OptionKind::Checklist | OptionKind::Toggle => false,
                    OptionKind::Answer | OptionKind::Irreversible => self.allow_note,
                })
    }

    fn wants_note(&self, index: usize) -> bool {
        self.allow_note
            && self
                .options
                .get(index)
                .is_some_and(|option| option.asks_note)
    }

    fn move_to(&mut self, index: usize) {
        self.selected = index;
        self.armed = None;
        // An open note follows the selection, so Tab then ↓ puts it on the next option. A typed
        // answer belongs to its own option, so it closes instead.
        let free_text = |question: &Self, index: usize| {
            question
                .options
                .get(index)
                .is_some_and(|option| option.kind == OptionKind::FreeText)
        };
        let Some(option) = self.note.as_ref().map(|note| note.option) else {
            return;
        };
        if free_text(self, option) || free_text(self, index) {
            self.note = None;
        } else if let Some(note) = &mut self.note {
            note.option = index;
        }
    }

    /// A view leaves the question as it was, pointer and note included, so coming back finds it
    /// ready.
    fn pick(&mut self, index: usize, confirmed: bool) -> QuestionEvent {
        if let Some(sheet) = self.sheet.as_mut().filter(|sheet| sheet.option == index) {
            sheet.open = true;
            self.move_to(index);
            self.note = None;
            return QuestionEvent::Moved;
        }
        let kind = self.options[index].kind;
        match kind {
            OptionKind::View => return QuestionEvent::OpenView { index },
            OptionKind::Toggle => {
                let option = &mut self.options[index];
                option.on = !option.on;
                return QuestionEvent::OpenView { index };
            }
            OptionKind::Irreversible if !confirmed => {
                self.move_to(index);
                self.armed = Some(index);
                return QuestionEvent::Moved;
            }
            OptionKind::FreeText => {
                let typed = self
                    .note
                    .as_ref()
                    .filter(|note| note.option == index)
                    .map(|note| note.text().trim().to_owned());
                return match typed {
                    Some(text) if !text.is_empty() => {
                        self.note = None;
                        QuestionEvent::Answered {
                            index,
                            note: Some(text),
                            checked: Vec::new(),
                        }
                    }
                    Some(_) => QuestionEvent::Moved,
                    None => {
                        self.move_to(index);
                        self.note = Some(QuestionNote::new(index));
                        QuestionEvent::Moved
                    }
                };
            }
            OptionKind::Checklist => {
                self.move_to(index);
                self.note = None;
                self.checklist = Some(Checklist {
                    option: index,
                    checked: vec![false; self.options[index].items.len()],
                    cursor: 0,
                });
                return QuestionEvent::Moved;
            }
            OptionKind::Answer | OptionKind::Irreversible => {}
        }
        self.armed = None;
        // An empty note still answers, so a second Enter sends it without words.
        let noted = self.note.as_ref().is_some_and(|note| note.option == index);
        if self.wants_note(index) && !noted {
            self.move_to(index);
            self.note = Some(QuestionNote::new(index));
            return QuestionEvent::Moved;
        }
        QuestionEvent::Answered {
            index,
            note: self.take_note(),
            checked: Vec::new(),
        }
    }

    /// The pointer and any open note stay, so ↑ from the input finds the question as it was. An
    /// armed option doesn't, since its next key would answer.
    fn back_to_input(&mut self) -> QuestionEvent {
        self.focused = false;
        self.armed = None;
        QuestionEvent::BackToInput
    }

    /// Esc: it leaves the screen with nothing picked. An answer typed into the answer field
    /// stays, so opening it again finds it.
    pub fn dismiss(&mut self) -> QuestionEvent {
        self.dismissed = true;
        self.focused = false;
        if !self.is_answer_field() {
            self.note = None;
        }
        self.armed = None;
        self.checklist = None;
        QuestionEvent::Dismissed
    }

    /// Options answer to the numbers shown beside them, 1 to 9, and never to letters.
    /// On a keys-only question, the option whose label starts with `c`, when one alone does.
    fn option_for_letter(&self, c: char) -> Option<usize> {
        if !self.keys_only || !c.is_alphabetic() {
            return None;
        }
        let starts = |option: &QuestionOption| {
            option
                .label
                .chars()
                .next()
                .is_some_and(|first| first.to_lowercase().eq(c.to_lowercase()))
        };
        let mut matching = self.options.iter().enumerate().filter(|(_, o)| starts(o));
        let (index, _) = matching.next()?;
        matching.next().is_none().then_some(index)
    }

    fn option_for_digit(&self, c: char) -> Option<usize> {
        let index = c.to_digit(10).filter(|digit| *digit >= 1)? as usize - 1;
        (index < self.options.len()).then_some(index)
    }
}

pub(crate) mod answered;
/// Drops the last grapheme, so an accent or a flag goes whole.
#[cfg(test)]
mod tests;
