//! Keys go to the transcript overlay when it is open, then to the prompt history search, an
//! open settings picker, a focused question, the answered fallback pick's line when ↑ picked it, a
//! second Esc that clears the input, the focused agent panel, the open menu, vim's modes, the
//! global keys, and last the composer. The search, a picker, or a focused question lets only the
//! quit keys past it, so while a question has the keys, Ctrl+R and `/` don't open the search and
//! `!` doesn't start shell mode. A question you set off yourself opens with the keys, and one
//! work waits on takes them the moment it shows. A draft in the input goes aside meanwhile and
//! comes back after, and keys typed in a burst as a question shows are dropped. An open search
//! or shell mode counts as a draft. Esc sets a run's question aside and gives the keys
//! back, and ↑ or Tab on an empty input hands them to a waiting question again.
//! What the named keys do comes from the [`Keymap`].
//!
//! [`Keymap`]: crate::keymap::Keymap

use std::io::Write;
use std::time::{Duration, Instant};

use argonauts_core::app::{UiCommand, ViewRequest};
use argonauts_core::input::{ModPlaceChange, ModsAction, SlashCommand};
use argonauts_core::settings::ui::Place;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Position, Size};

use super::{OverlayEvent, QuitArm, TranscriptOverlay, Ui, UiAction};
use crate::bottom::{
    BandEvent, ComposerEvent, Hint, MenuEvent, Menus, NoticeEvent, PanelEvent, PickerEvent,
    QuestionEvent, REMEMBER_KEY, SearchEvent, WORD_IGNORED_NOTICE, ignored_notice,
};
use crate::keymap::{Action, CHORD_WAIT, KeySpec, Layer};
use crate::views::list;

/// How long a first Ctrl+C, Ctrl+D, or Esc waits for the second.
pub const QUIT_WINDOW: Duration = Duration::from_secs(1);

/// Shown after the first Esc on a draft.
pub const CLEAR_HINT: &str = "Esc again to clear";

impl<W: Write> Ui<W> {
    /// Keys go to the top view while one is open, drawn against an empty app state; the
    /// runtime passes its state through [`handle_event_in`](Self::handle_event_in).
    pub fn handle_event(&mut self, event: Event, now: Instant) -> Vec<UiAction> {
        self.settle_question_keys(now);
        if self.views.is_empty() {
            if let Some(actions) = self.selection_key(&event) {
                return actions;
            }
            return self.handle_conversation_event(event, now);
        }
        let state = crate::state::AppState::default();
        self.handle_event_in(event, now, &state)
    }

    pub(super) fn handle_conversation_event(
        &mut self,
        event: Event,
        now: Instant,
    ) -> Vec<UiAction> {
        let mut actions = Vec::new();
        let bottom = &mut self.model.bottom;
        bottom.composer.adopt_vim_mode(bottom.mode.vim);
        match event {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                self.handle_key(key, now, &mut actions);
            }
            Event::Mouse(mouse) => {
                let state = crate::state::AppState::default();
                actions = self.mouse_event(mouse, now, &state);
            }
            Event::Key(_) => {}
            Event::Paste(text) => self.handle_paste(&text, now, &mut actions),
            Event::Resize(columns, rows) => self.resize(Size::new(columns, rows), None),
            Event::FocusGained => actions.push(UiAction::Focus(true)),
            Event::FocusLost => actions.push(UiAction::Focus(false)),
        }
        self.model.bottom.settle_workflow_word(now);
        self.show_vim_mode();
        self.settle_question_keys(now);
        if actions
            .iter()
            .any(|action| matches!(action, UiAction::Submit { .. } | UiAction::Shell { .. }))
        {
            self.follow_conversation();
        }
        actions
    }

    /// A question that asks takes the keys the moment it shows, from wherever they were, so an
    /// approval never waits unseen. A view on top would keep them, so the views close as it
    /// takes them, down to one that stays up over a question, which the question draws under.
    /// A view the question opens itself, such as its diff, stays, but no other docked one,
    /// which draws where the question does. A draft goes aside while the question holds the
    /// keys and comes back once it's answered or passed on.
    pub(super) fn settle_question_keys(&mut self, now: Instant) {
        let bottom = &mut self.model.bottom;
        let took = bottom.hand_keys_to_question(now);
        let mut changed = bottom.settle_draft() || took;
        if took {
            while self
                .views
                .last()
                .is_some_and(|view| !view.stays_over_question())
            {
                self.views.pop();
            }
        }
        // The transcript on the alternate screen would hide the question and keep its keys.
        if took && self.overlay.is_some() {
            self.overlay = None;
            changed = true;
        }
        if bottom.focused_question().is_some() {
            while self
                .views
                .last()
                .is_some_and(|view| view.docked() && !view.stays_over_question())
            {
                self.views.pop();
                changed = true;
            }
        }
        if changed {
            self.dirty = true;
        }
    }

    /// A question holds the keys over the docked view it draws under, so they go to it and
    /// not to the view.
    pub(super) fn question_over_view(&self) -> bool {
        self.model.bottom.focused_question().is_some()
            && self
                .views
                .last()
                .is_some_and(|view| view.stays_over_question())
    }

    /// The composer keeps vim's mode, and the bottom line draws `mode.vim`, which settings also
    /// set to turn vim keys on or off. The composer takes that before each event and hands its
    /// own mode back after, so the bottom line always shows the mode the next key meets.
    fn show_vim_mode(&mut self) {
        let bottom = &mut self.model.bottom;
        let mode = bottom.composer.vim_mode();
        if bottom.mode.vim != mode {
            bottom.mode.vim = mode;
            self.dirty = true;
        }
    }

    /// `cursor` is where the terminal reports its cursor now, if the host could ask. Terminals
    /// that rewrap scrollback move the live region with it, and the cursor tells by how much.
    pub fn resize(&mut self, size: Size, cursor: Option<Position>) {
        // Resizing only records the new size, so it has nothing to fail on.
        let _ = self.terminal.resize(size, cursor);
        self.measure_cells();
        self.fit_mascot_to_width();
        self.resend_map();
        self.clear_selection();
        self.dirty = true;
    }

    fn handle_key(&mut self, key: KeyEvent, now: Instant, actions: &mut Vec<UiAction>) {
        if self.model.bottom.updated.take().is_some() {
            self.dirty = true;
        }
        if self.overlay.is_some() {
            if self.keymap.action_for(Layer::Overlay, &key) == Some(Action::Transcript) {
                self.close_overlay();
                return;
            }
            let key = list::standard(key, self.model.bottom.mode.vim.is_some(), KeyCode::Esc);
            match self.overlay.as_mut().map(|overlay| overlay.handle_key(key)) {
                Some(OverlayEvent::Close) => self.close_overlay(),
                Some(OverlayEvent::Scrolled) => self.dirty = true,
                Some(OverlayEvent::Ignored) | None => {}
            }
            return;
        }
        if self.key_times {
            self.settle_paste_burst(Some(&key), now);
        }
        // A question that holds the keys took them from the search or the picker too, which wait
        // under it.
        if self.model.bottom.focused_question().is_some() {
            if !self.cycle_key(key, actions) && !self.question_key(key, now, actions) {
                self.quit_key(key, now, actions);
            }
            return;
        }
        if self.model.bottom.history_search.is_some() {
            if !self.search_key(key, now, actions) {
                self.quit_key(key, now, actions);
            }
            return;
        }
        if self.model.bottom.picker.is_some() {
            if !self.picker_key(key, actions) {
                self.quit_key(key, now, actions);
            }
            return;
        }
        if self.chord_key(key, now, actions)
            || self.band_key(key, now, actions)
            || self.notice_key(key, now, actions)
        {
            return;
        }
        if self.scroll_key(&key) {
            return;
        }
        if self.model.bottom.pick_selected() && self.answered_pick_key(key, now, actions) {
            return;
        }
        if self.clear_key(key, now)
            || self.panel_key(key, actions)
            || self.menu_key(key, now, actions)
        {
            return;
        }
        let key = self.as_vim_arrow(key);
        if self.vim_key(key, actions)
            || self.shell_key(key)
            || self.global_key(key, now, actions)
            || self.raw_failure_key(&key)
        {
            return;
        }
        self.composer_key(key, now, actions);
    }

    /// `r` in an empty input opens the text behind the newest failure, once, as the failure's
    /// hint says. After that, or after any other key for the input, `r` is a letter again until
    /// the next failure. Typing in vim's INSERT is always letters.
    fn raw_failure_key(&mut self, key: &KeyEvent) -> bool {
        let raw = self.model.raw_failure.take();
        let bottom = &self.model.bottom;
        let opens = key.code == KeyCode::Char('r')
            && key.modifiers.is_empty()
            && bottom.composer_has_focus()
            && bottom.composer.is_empty()
            && !bottom.composer.is_shell()
            && !bottom.composer.holds_paste()
            && bottom.composer.vim_mode() != Some(crate::bottom::VimMode::Insert);
        match raw {
            Some(raw) if opens => {
                self.open_view(raw.title.clone(), raw.cells());
                true
            }
            _ => false,
        }
    }

    /// Two keys in a row, such as Ctrl+X Ctrl+S. After the first, the next key within
    /// [`CHORD_WAIT`] completes the binding. Any other key, or one that comes too late, does
    /// what it does alone, and Ctrl+X alone does nothing, as in Claude Code.
    fn chord_key(&mut self, key: KeyEvent, now: Instant, actions: &mut Vec<UiAction>) -> bool {
        if let Some((first, at)) = self.chord.take()
            && now.saturating_duration_since(at) <= CHORD_WAIT
        {
            let bound = self.keymap.chord_actions(Layer::Global, &first, &key);
            // A mod's own chord hides it or shows it again.
            if bound.is_empty()
                && let Some(name) = self.model.mod_chords.find(&first, &key)
            {
                actions.push(UiAction::Command(UiCommand::Slash {
                    command: SlashCommand::ModFile {
                        name: name.to_owned(),
                        words: vec!["toggle".to_owned()],
                    },
                }));
                self.dirty = true;
                return true;
            }
            if bound.is_empty() {
                return false;
            }
            for action in bound {
                if self.global_action(action, &key, now, actions) {
                    break;
                }
            }
            self.dirty = true;
            return true;
        }
        let alone = !self.keymap.actions_for(Layer::Global, &key).is_empty();
        let starts =
            self.keymap.starts_chord(Layer::Global, &key) || self.model.mod_chords.starts(&key);
        if !alone && starts {
            self.chord = Some((KeySpec::from_event(&key).to_string(), now));
            return true;
        }
        false
    }

    /// The mods above the input, while they have the keys: Tab moves between them and on to
    /// the notice, `-` folds, Enter opens the mod's tab, `p` unpins, `[` and `]` move it,
    /// `,` opens its settings, and Esc gives the keys back to the input. Each change is one
    /// setting, which the controller saves and sends back.
    fn band_key(&mut self, key: KeyEvent, now: Instant, actions: &mut Vec<UiAction>) -> bool {
        let bottom = &mut self.model.bottom;
        if !bottom.band.has_focus() {
            return false;
        }
        let above = Place::AboveInput;
        let change = |the_mod, change| {
            UiAction::Command(UiCommand::Slash {
                command: SlashCommand::Mods {
                    action: ModsAction::Change { the_mod, change },
                    scope: None,
                },
            })
        };
        match bottom.band.handle_key(key) {
            BandEvent::Moved | BandEvent::Leave => {}
            BandEvent::Past => {
                // The notice's buttons come next, and with none Tab comes back around.
                bottom.band.blur();
                if !bottom.notice.focus() {
                    bottom.band.focus();
                }
            }
            BandEvent::Fold { name, open: true } => {
                actions.push(change(name, ModPlaceChange::Open))
            }
            BandEvent::Fold { name, open: false } => {
                actions.push(change(name, ModPlaceChange::Fold))
            }
            BandEvent::Open(name) => actions.push(UiAction::OpenTab(name)),
            BandEvent::Unpin(name) => {
                actions.push(change(name, ModPlaceChange::Hide { place: above }));
            }
            BandEvent::Move { name, up: true } => {
                actions.push(change(name, ModPlaceChange::Up { place: Some(above) }));
            }
            BandEvent::Move { name, up: false } => {
                actions.push(change(name, ModPlaceChange::Down { place: Some(above) }));
            }
            BandEvent::Settings(the_mod) => actions.push(UiAction::Command(UiCommand::Slash {
                command: SlashCommand::Mods {
                    action: ModsAction::Page { the_mod },
                    scope: None,
                },
            })),
            BandEvent::Unhandled => self.quit_key(key, now, actions),
        }
        self.dirty = true;
        true
    }

    /// The notice above the input, while it has the keys: a number presses its button,
    /// ←/→ and Tab move, Enter or Space presses, Esc gives the keys back to the input.
    fn notice_key(&mut self, key: KeyEvent, now: Instant, actions: &mut Vec<UiAction>) -> bool {
        let notice = &mut self.model.bottom.notice;
        if !notice.has_focus() {
            return false;
        }
        match notice.handle_key(key) {
            NoticeEvent::Press(at) => {
                if let Some(button) = notice.drawn.buttons.get(at) {
                    actions.push(UiAction::Command(UiCommand::PressNotice {
                        plugin: button.plugin.clone(),
                        handle: button.handle,
                    }));
                }
            }
            NoticeEvent::Leave | NoticeEvent::Moved => {}
            NoticeEvent::Unhandled => self.quit_key(key, now, actions),
        }
        self.dirty = true;
        true
    }

    /// Vim's modes, when vim keys are on. Esc in INSERT only leaves INSERT, as in Claude Code,
    /// so a habit of pressing Esc twice never stops a run. Esc in VISUAL, or after a command's
    /// first key such as `d`, only goes back to NORMAL. In NORMAL and VISUAL, letters are
    /// commands and never typed; Enter, arrows, and modified keys go on as usual.
    fn vim_key(&mut self, key: KeyEvent, actions: &mut Vec<UiAction>) -> bool {
        let bottom = &mut self.model.bottom;
        if bottom.mode.vim.is_none() || !bottom.composer_has_focus() {
            return false;
        }
        match bottom.composer.vim_key(key) {
            None => return false,
            Some(ComposerEvent::Edited) => bottom.menus.text_changed(),
            Some(ComposerEvent::SearchHistory) => actions.push(UiAction::SearchHistory),
            Some(ComposerEvent::Submitted { text, shown }) => {
                actions.push(UiAction::Submit { text, shown });
            }
            Some(ComposerEvent::Shell { command }) => actions.push(UiAction::Shell { command }),
            Some(_) => {}
        }
        self.dirty = true;
        true
    }

    /// In vim's NORMAL, `j` and `k` in the input are ↓ and ↑ from here on.
    fn as_vim_arrow(&self, key: KeyEvent) -> KeyEvent {
        let bottom = &self.model.bottom;
        match bottom.composer.vim_arrow(&key) {
            Some(code) if bottom.composer_has_focus() => KeyEvent::new(code, KeyModifiers::NONE),
            _ => key,
        }
    }

    /// Esc leaves shell mode and keeps what was typed, before it could interrupt anything.
    /// With vim keys, Esc in INSERT first goes back to NORMAL, as anywhere else.
    fn shell_key(&mut self, key: KeyEvent) -> bool {
        let bottom = &mut self.model.bottom;
        let esc = key.code == KeyCode::Esc && key.modifiers.is_empty();
        if !esc || !bottom.composer_has_focus() || !bottom.composer.leave_shell() {
            return false;
        }
        bottom.menus.text_changed();
        self.dirty = true;
        true
    }

    /// A second Esc soon after the first clears the draft into history, like Claude Code's
    /// double tap. Any other key lets the first one go.
    fn clear_key(&mut self, key: KeyEvent, now: Instant) -> bool {
        let Some(until) = self.disarm_clear() else {
            return false;
        };
        let bottom = &mut self.model.bottom;
        let esc = key.code == KeyCode::Esc && key.modifiers.is_empty();
        if !esc || now >= until || !bottom.composer_has_focus() || bottom.composer.is_empty() {
            return false;
        }
        let draft = bottom.composer.text().to_owned();
        bottom.composer.remember(draft);
        bottom.composer.clear();
        bottom.menus.text_changed();
        self.dirty = true;
        true
    }

    fn disarm_clear(&mut self) -> Option<Instant> {
        let until = self.clear_armed.take()?;
        let bottom = &mut self.model.bottom;
        if bottom
            .hint
            .as_ref()
            .is_some_and(|hint| hint.text == CLEAR_HINT)
        {
            bottom.hint = None;
            self.dirty = true;
        }
        Some(until)
    }

    fn arm_clear(&mut self, now: Instant) {
        let until = now + QUIT_WINDOW;
        self.clear_armed = Some(until);
        self.model.bottom.hint = Some(Hint {
            text: CLEAR_HINT.to_owned(),
            until,
        });
    }

    /// `?` on an empty prompt opens the shortcuts panel until the next key. Esc hides the `/`
    /// or `@` menu, and counts as the first press of the double tap that clears the input.
    fn menu_key(&mut self, key: KeyEvent, now: Instant, actions: &mut Vec<UiAction>) -> bool {
        let bottom = &mut self.model.bottom;
        if !bottom.composer_has_focus() {
            return false;
        }
        if bottom.menus.shortcuts_open {
            bottom.menus.shortcuts_open = false;
            self.dirty = true;
            if key.code == KeyCode::Esc {
                return true;
            }
        }
        // In shell mode `?`, `/`, and `@` are a command's own characters.
        if bottom.composer.is_shell() {
            return false;
        }
        // A `?` inside a paste typed out as keys is part of the paste.
        if self.keymap.is(Action::Shortcuts, &key)
            && bottom.composer.is_empty()
            && !bottom.composer.holds_paste()
            && !bottom.menus.shortcuts.is_empty()
        {
            bottom.menus.shortcuts_open = true;
            self.dirty = true;
            return true;
        }
        let text = bottom.composer.text().to_owned();
        match bottom.menus.handle_key(key, &text) {
            MenuEvent::Moved if key.code == KeyCode::Esc => self.arm_clear(now),
            MenuEvent::Moved => {}
            MenuEvent::Complete(text) => bottom.composer.set_text(&text),
            MenuEvent::Run(command) => {
                bottom.composer.clear();
                bottom.composer.remember(command.clone());
                bottom.composer.end_example();
                actions.push(UiAction::submit(command));
            }
            MenuEvent::Unhandled => return false,
        }
        self.dirty = true;
        true
    }

    /// A paste typed out as keys goes in once they stop, or before a key that can't be part of
    /// it, so the text keeps its order.
    pub(super) fn settle_paste_burst(&mut self, key: Option<&KeyEvent>, now: Instant) {
        let bottom = &mut self.model.bottom;
        let part_of_it = key.is_none_or(|key| {
            matches!(key.code, KeyCode::Char(_) | KeyCode::Enter)
                && key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
        });
        let changed = if part_of_it {
            bottom.composer.flush_paste_burst(now)
        } else {
            bottom.composer.end_paste_burst()
        };
        let changed = bottom.composer.take_arrived_pictures() || changed;
        if changed {
            bottom.menus.text_changed();
            self.dirty = true;
        }
    }

    /// The search takes every key it knows, so typing filters it. Enter only fills the input,
    /// never sends, and Esc leaves the input as it was.
    fn search_key(&mut self, key: KeyEvent, now: Instant, actions: &mut Vec<UiAction>) -> bool {
        let bottom = &mut self.model.bottom;
        let Some(search) = bottom.history_search.as_mut() else {
            return false;
        };
        let event = if self.key_times {
            search.handle_key_at(key, now)
        } else {
            search.handle_key(key)
        };
        match event {
            SearchEvent::Unhandled => return false,
            SearchEvent::Moved => {}
            SearchEvent::Cancel => bottom.history_search = None,
            SearchEvent::CancelThen(next) => {
                bottom.history_search = None;
                self.dirty = true;
                self.handle_key(next, now, actions);
                return true;
            }
            SearchEvent::Use(text) => {
                bottom.history_search = None;
                bottom.composer.fill(&text);
                bottom.menus.text_changed();
            }
        }
        self.dirty = true;
        true
    }

    /// An open picker takes the keys it knows, so typing filters it and never reaches the
    /// composer under it.
    fn picker_key(&mut self, key: KeyEvent, actions: &mut Vec<UiAction>) -> bool {
        let Some(picker) = self.model.bottom.picker.as_mut() else {
            return false;
        };
        match picker.handle_key(key) {
            PickerEvent::Unhandled => return false,
            event => actions.push(UiAction::Picker(event)),
        }
        self.dirty = true;
        true
    }

    /// Shift+Tab cycles the approval mode while a question has the keys too, as at Claude Code's
    /// prompts, so you can stop being asked: autopilot settles the question. A note being typed on
    /// the question keeps its keys, and so does a question's answer field once you've typed in it.
    fn cycle_key(&mut self, key: KeyEvent, actions: &mut Vec<UiAction>) -> bool {
        let bottom = &mut self.model.bottom;
        let typing_a_note = bottom.question.as_ref().is_some_and(|question| {
            question
                .note
                .as_ref()
                .is_some_and(|note| !(question.is_answer_field() && note.is_empty()))
        });
        if typing_a_note
            || !self
                .keymap
                .actions_for(Layer::Global, &key)
                .contains(&Action::CycleApprovalMode)
        {
            return false;
        }
        bottom.mode.approval = bottom.mode.approval.next();
        actions.push(UiAction::ModeChanged(bottom.mode.approval));
        self.dirty = true;
        true
    }

    fn question_key(&mut self, key: KeyEvent, now: Instant, actions: &mut Vec<UiAction>) -> bool {
        let vim = self.model.bottom.mode.vim.is_some();
        let Some(question) = self
            .model
            .bottom
            .question
            .as_mut()
            .filter(|question| question.focused)
        else {
            return false;
        };
        let id = question.id;
        let key = question.read_key(key, vim);
        if question.held_off(&key, now) {
            return true;
        }
        let event = question.handle_key(key);
        match event {
            QuestionEvent::Answered {
                index,
                note,
                checked,
            } => {
                self.model.bottom.question = None;
                actions.push(UiAction::Answer {
                    question: id,
                    option: index,
                    note,
                    checked,
                });
            }
            QuestionEvent::OpenView { index } => actions.push(UiAction::OpenView {
                question: id,
                option: index,
            }),
            QuestionEvent::Dismissed => actions.push(UiAction::Dismiss { question: id }),
            QuestionEvent::BackToInput | QuestionEvent::Moved => {}
            QuestionEvent::Unhandled => return false,
        }
        self.dirty = true;
        true
    }

    /// On the answered fallback pick's line, `m` or Enter saves the pick as a rule, Esc lets it
    /// go, and ↓ goes back to the input. ↑ goes on to the prompt history, and any other key
    /// leaves the line and does what it does in the input.
    fn answered_pick_key(
        &mut self,
        key: KeyEvent,
        now: Instant,
        actions: &mut Vec<UiAction>,
    ) -> bool {
        let bottom = &mut self.model.bottom;
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let Some(pick) = bottom.answered_pick.as_mut() else {
            return false;
        };
        self.dirty = true;
        match key.code {
            KeyCode::Char(REMEMBER_KEY) | KeyCode::Enter if plain => {
                let (run, id) = (pick.run.clone(), pick.id.clone());
                bottom.answered_pick = None;
                actions.push(UiAction::Command(UiCommand::RememberPick { run, id }));
                true
            }
            KeyCode::Esc => {
                bottom.answered_pick = None;
                true
            }
            KeyCode::Down => {
                pick.selected = false;
                true
            }
            KeyCode::Up => {
                pick.selected = false;
                self.composer_key(key, now, actions);
                true
            }
            _ => {
                pick.selected = false;
                false
            }
        }
    }

    fn panel_key(&mut self, key: KeyEvent, actions: &mut Vec<UiAction>) -> bool {
        let bottom = &mut self.model.bottom;
        if !bottom.agents.has_focus() && bottom.workflows.selected.is_none() {
            return false;
        }
        if !bottom.composer.is_empty() {
            return self.message_key(key, actions);
        }
        let key = list::standard(key, bottom.mode.vim.is_some(), KeyCode::Esc);
        match bottom.panel_key(key) {
            PanelEvent::Open(agent) => actions.push(UiAction::OpenAgent(agent)),
            PanelEvent::Stop { agent, .. } => actions.push(UiAction::StopAgent(agent)),
            PanelEvent::StopRun(run) => actions.push(UiAction::StopRun(run)),
            PanelEvent::PauseRun(run) => actions.push(UiAction::PauseRun(run)),
            PanelEvent::OpenWorkflow(run) => actions.push(UiAction::OpenWorkflow(run)),
            PanelEvent::PastEnd => actions.push(UiAction::OpenTree),
            PanelEvent::Leave | PanelEvent::Moved | PanelEvent::StopAsked => {}
            PanelEvent::Unhandled => return false,
        }
        self.dirty = true;
        true
    }

    /// A key while a draft sits in the input and the list has a row selected. The draft is a
    /// message for the selected agent, as in Claude Code: Enter sends it there, Esc lets the
    /// list go and leaves the draft for the main agent, and every other key edits it. A
    /// command, a shell line, or a draft with a run's row selected is the main agent's as
    /// always, so the list lets go and the key does what it does in the input.
    fn message_key(&mut self, key: KeyEvent, actions: &mut Vec<UiAction>) -> bool {
        let bottom = &mut self.model.bottom;
        let plain = key.modifiers.is_empty();
        let first = bottom.composer.text().trim_start().chars().next();
        let for_main = bottom.composer.is_shell() || matches!(first, Some('/' | '!'));
        let target = bottom
            .messaging()
            .filter(|_| !for_main)
            .map(|row| (row.agent_ref(), row.display_name()));
        match (key.code, target) {
            (KeyCode::Enter, Some((agent, name))) if plain => {
                let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
                if let ComposerEvent::Submitted { text, .. } = bottom.composer.handle_key(enter) {
                    actions.push(UiAction::MessageAgent { agent, name, text });
                }
                self.dirty = true;
                true
            }
            (KeyCode::Esc, Some(_)) if plain => {
                bottom.agents.blur();
                bottom.workflows.blur();
                self.dirty = true;
                true
            }
            (_, Some(_)) => false,
            (_, None) => {
                bottom.agents.blur();
                bottom.workflows.blur();
                false
            }
        }
    }

    /// Past an open picker or a focused question, only the quit keys do anything.
    fn quit_key(&mut self, key: KeyEvent, now: Instant, actions: &mut Vec<UiAction>) {
        for action in self.keymap.actions_for(Layer::Global, &key) {
            if matches!(action, Action::ClearOrExit | Action::Exit)
                && self.global_action(action, &key, now, actions)
            {
                self.dirty = true;
                return;
            }
        }
    }

    /// Tries each action bound to `key` until one applies here, such as ↑ taking queued
    /// messages back, or else giving a waiting question the keys.
    fn global_key(&mut self, key: KeyEvent, now: Instant, actions: &mut Vec<UiAction>) -> bool {
        let bottom = &self.model.bottom;
        let panel_focused = bottom.agents.has_focus() || bottom.workflows.selected.is_some();
        // A digit in a message you're typing to the selected agent is part of the message.
        let following = panel_focused && bottom.composer.is_empty();
        if let Some(n) = follow_digit(&key).filter(|_| following) {
            actions.push(UiAction::Follow(n));
            self.dirty = true;
            return true;
        }
        // Where Option isn't Meta, macOS sends `∑` for Opt+W. It takes the workflow word's key
        // only while the draft holds the word, and is a letter everywhere else.
        if key.code == KeyCode::Char('∑')
            && self.global_action(Action::WorkflowKeywordToggle, &key, now, actions)
        {
            self.dirty = true;
            return true;
        }
        for action in self.keymap.actions_for(Layer::Global, &key) {
            if self.global_action(action, &key, now, actions) {
                self.dirty = true;
                return true;
            }
        }
        false
    }

    fn global_action(
        &mut self,
        action: Action,
        key: &KeyEvent,
        now: Instant,
        actions: &mut Vec<UiAction>,
    ) -> bool {
        let bottom = &mut self.model.bottom;
        // Shell mode with nothing typed still has something to clear: the mode.
        let empty = bottom.composer.is_empty() && !bottom.composer.is_shell();
        match action {
            Action::ClearOrExit if bottom.shell_running => {
                actions.push(UiAction::Command(UiCommand::StopShell));
            }
            Action::ClearOrExit if empty => self.arm_quit(action, key, now, actions),
            Action::ClearOrExit => {
                bottom.composer.clear();
                // Clearing arms the exit too, so the second press leaves. It
                // always arms afresh, so text typed while armed is cleared, never a way out.
                self.quit = None;
                self.arm_quit(action, key, now, actions);
            }
            Action::Exit if empty => self.arm_quit(action, key, now, actions),
            Action::Transcript => self.open_overlay(),
            Action::WorkflowView => actions.push(UiAction::OpenTree),
            Action::Editor => actions.push(UiAction::EditDraft),
            Action::CycleApprovalMode => {
                bottom.mode.approval = bottom.mode.approval.next();
                actions.push(UiAction::ModeChanged(bottom.mode.approval));
            }
            // A question that waits without the keys takes the first press, which dismisses it. The
            // next interrupts whatever works. A question that Esc would answer waits for its
            // focus, so Esc in the input never answers one for you.
            Action::Interrupt => {
                if let Some(question) = bottom
                    .question
                    .as_mut()
                    .filter(|question| !question.dismissed && !question.esc_answers)
                {
                    question.dismiss();
                    actions.push(UiAction::Dismiss {
                        question: question.id,
                    });
                } else if bottom.working.is_some() {
                    actions.push(UiAction::Interrupt);
                } else {
                    return false;
                }
            }
            Action::AgentPanelView if empty => actions.push(UiAction::OpenAgentPanel),
            // With text and nothing waiting, the text goes first, then the request.
            Action::SendNow if bottom.send_now => {
                if bottom.queued.is_empty() && !bottom.composer.is_empty() {
                    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
                    if let ComposerEvent::Submitted { text, shown } =
                        bottom.composer.handle_key(enter)
                    {
                        actions.push(UiAction::Submit { text, shown });
                    }
                }
                actions.push(UiAction::Command(UiCommand::SendNow));
            }
            Action::WorkflowKeywordToggle => {
                let Some(ignored) = bottom.composer.toggle_workflow_word() else {
                    return false;
                };
                bottom.word_notice = ignored.then(|| Hint {
                    text: ignored_notice(&bottom.workflow_key),
                    until: now + WORD_IGNORED_NOTICE,
                });
            }
            Action::StopAgents => self.arm_stop_all(now, actions),
            Action::OpenArtifact => actions.push(UiAction::OpenArtifact),
            // The mods first, since they sit on top, then the notice's buttons.
            Action::AboveInputFocus => {
                if !bottom.band.focus() && !bottom.notice.focus() {
                    return false;
                }
                bottom.agents.blur();
                bottom.workflows.blur();
            }
            Action::AboveInputToggle if bottom.band.shows() || !bottom.notice.drawn.is_empty() => {
                bottom.band.toggle_hidden();
                if bottom.notice.hidden != bottom.band.hidden || !bottom.band.shows() {
                    bottom.notice.toggle();
                }
            }
            // The controller saves the change where the list came from, and the screen
            // follows the settings it sends back.
            Action::Mod(the_mod) => {
                let change = ModPlaceChange::Toggle {
                    place: Place::AboveInput,
                };
                actions.push(UiAction::Command(UiCommand::Slash {
                    command: SlashCommand::Mods {
                        action: ModsAction::Change { the_mod, change },
                        scope: None,
                    },
                }));
            }
            Action::TakeBack
                if bottom.composer.cursor_on_first_row() && !bottom.queued.is_empty() =>
            {
                actions.push(UiAction::TakeBack(bottom.queued.clone()));
            }
            // A waiting question gets the keys from an empty input.
            Action::FocusQuestion if empty && bottom.expanded_question().is_some() => {
                if let Some(question) = bottom.question.as_mut() {
                    question.focus();
                }
            }
            // With no question, the last fallback pick's line gets them, before the history.
            Action::FocusQuestion
                if empty
                    && !bottom.composer.is_browsing_history()
                    && bottom.answered_pick.is_some() =>
            {
                if let Some(pick) = bottom.answered_pick.as_mut() {
                    pick.selected = true;
                }
            }
            Action::AgentPanel
                if empty
                    && !bottom.composer.is_browsing_history()
                    && !(bottom.agents.is_empty() && bottom.workflows.rows.is_empty()) =>
            {
                bottom.focus_panel();
            }
            _ => return false,
        }
        true
    }

    fn composer_key(&mut self, key: KeyEvent, now: Instant, actions: &mut Vec<UiAction>) {
        // Rebound keys reach the composer as the key it knows for their action, and a key whose
        // action moved elsewhere does nothing.
        let key = match self.keymap.action_for(Layer::Composer, &key) {
            Some(action) => native_composer_key(action).unwrap_or(key),
            None if native_composer_action(&key).is_some() => return,
            None => key,
        };
        let bottom = &mut self.model.bottom;
        let event = if self.key_times {
            bottom.composer.handle_key_at(key, now)
        } else {
            bottom.composer.handle_key(key)
        };
        match event {
            ComposerEvent::Submitted { text, shown } => {
                bottom.answered_pick = None;
                actions.push(UiAction::Submit { text, shown });
            }
            ComposerEvent::Shell { command } => {
                bottom.answered_pick = None;
                actions.push(UiAction::Shell { command });
            }
            ComposerEvent::Edited => {
                bottom.menus.text_changed();
                // Files come and go while you work, so a fresh `@` reads them again.
                if !bottom.composer.is_shell() && Menus::starts_mention(bottom.composer.text()) {
                    actions.push(UiAction::Request(ViewRequest::RepoFiles));
                }
            }
            ComposerEvent::Moved => {}
            ComposerEvent::SearchHistory => actions.push(UiAction::SearchHistory),
            // With vim keys, Esc is how you get back to NORMAL, so pressing it twice is a
            // habit there and never clears; `cc` or `dd` does.
            ComposerEvent::Unhandled
                if key.code == KeyCode::Esc
                    && key.modifiers.is_empty()
                    && bottom.mode.vim.is_none()
                    && !bottom.composer.is_empty() =>
            {
                self.arm_clear(now);
                self.dirty = true;
                return;
            }
            // Enter on an empty prompt still means something while a run pauses or waits.
            ComposerEvent::Unhandled
                if key.code == KeyCode::Enter
                    && key.modifiers.is_empty()
                    && bottom.composer.is_empty() =>
            {
                bottom.answered_pick = None;
                actions.push(UiAction::submit(""));
                return;
            }
            ComposerEvent::AtTop | ComposerEvent::Unhandled => return,
        }
        // Typing goes back to the prompt, so a focused panel lets go, unless an agent's row is
        // selected: then what you type is a message for that agent.
        if bottom.messaging().is_none() {
            bottom.agents.blur();
            bottom.workflows.blur();
        }
        self.dirty = true;
    }

    /// A paste goes where typing would: a focused question's note, the history search's filter, an
    /// open picker, or the composer. A focused question with no note open takes no text, nor one
    /// that only just took the keys, so the paste goes nowhere, as typing does, and the question
    /// waits for a key meant for it before anything answers it.
    fn handle_paste(&mut self, text: &str, now: Instant, actions: &mut Vec<UiAction>) {
        self.disarm_clear();
        let bottom = &mut self.model.bottom;
        if let Some(question) = bottom
            .question
            .as_mut()
            .filter(|question| question.focused && !question.dismissed)
        {
            // A paste a question can't take goes nowhere, as typing does.
            if !question.guarded(now) {
                question.handle_paste(text);
            }
            self.dirty = true;
            return;
        }
        if let Some(search) = bottom.history_search.as_mut() {
            search.handle_paste(text);
            self.dirty = true;
            return;
        }
        if let Some(picker) = bottom.picker.as_mut() {
            if picker.handle_paste(text) == PickerEvent::Moved {
                actions.push(UiAction::Picker(PickerEvent::Moved));
            }
            self.dirty = true;
            return;
        }
        bottom.composer.handle_paste(text);
        bottom.menus.text_changed();
        // A paste with an agent's row selected is part of a message for that agent.
        if bottom.messaging().is_none() {
            bottom.agents.blur();
        }
        self.dirty = true;
    }

    pub(super) fn arm_quit(
        &mut self,
        action: Action,
        key: &KeyEvent,
        now: Instant,
        actions: &mut Vec<UiAction>,
    ) {
        if self
            .quit
            .is_some_and(|quit| quit.action == action && now < quit.until)
        {
            self.quit = None;
            self.model.bottom.hint = None;
            actions.push(UiAction::Quit);
            return;
        }
        let until = now + QUIT_WINDOW;
        self.quit = Some(QuitArm { action, until });
        let mut text = format!("Press {} again to exit", hint_name(key));
        if let Some(note) = self
            .model
            .bottom
            .exit_note
            .as_deref()
            .filter(|n| !n.is_empty())
        {
            text.push_str(" · ");
            text.push_str(note);
        }
        self.model.bottom.hint = Some(Hint { text, until });
    }

    /// The stop-all chord. The first press only asks, in Claude Code's words, and a second
    /// within [`QUIT_WINDOW`] stops every agent running in the background. With none running
    /// it says so.
    fn arm_stop_all(&mut self, now: Instant, actions: &mut Vec<UiAction>) {
        let bottom = &mut self.model.bottom;
        // The list holds what runs in the background: runs, their agents, and the main
        // agent's own subagents.
        let running = bottom.agents.rows.iter().any(|row| row.stops());
        if !running && bottom.workflows.rows.is_empty() {
            self.stop_all_armed = None;
            bottom.hint = Some(Hint {
                text: "No background agents running".to_owned(),
                until: now + QUIT_WINDOW,
            });
            return;
        }
        if self.stop_all_armed.take().is_some_and(|until| now < until) {
            bottom.hint = None;
            actions.push(UiAction::StopAllAgents);
            return;
        }
        let until = now + QUIT_WINDOW;
        self.stop_all_armed = Some(until);
        let chord = self
            .keymap
            .keys(Action::StopAgents)
            .first()
            .cloned()
            .unwrap_or_default();
        bottom.hint = Some(Hint {
            text: format!("Press {chord} again to stop background agents"),
            until,
        });
    }

    fn open_overlay(&mut self) {
        self.overlay = Some(TranscriptOverlay::new());
        self.dirty = true;
    }

    fn close_overlay(&mut self) {
        self.overlay = None;
        self.dirty = true;
    }
}

/// 1 to 9 follow an agent from the focused panel.
fn follow_digit(key: &KeyEvent) -> Option<u8> {
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
        return None;
    }
    c.to_digit(10)
        .and_then(|digit| u8::try_from(digit).ok())
        .filter(|digit| (1..=9).contains(digit))
}

/// The key the composer itself uses for a composer action.
fn native_composer_key(action: Action) -> Option<KeyEvent> {
    match action {
        Action::Submit => Some(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        Action::Newline => Some(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)),
        Action::HistorySearch => Some(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
        _ => None,
    }
}

/// What the composer would do with `key` on its own, for the keys the keymap can move.
fn native_composer_action(key: &KeyEvent) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift_or_alt = key
        .modifiers
        .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT);
    match key.code {
        KeyCode::Enter if shift_or_alt => Some(Action::Newline),
        KeyCode::Enter if !ctrl => Some(Action::Submit),
        KeyCode::Char('j') if ctrl => Some(Action::Newline),
        KeyCode::Char('r') if ctrl => Some(Action::HistorySearch),
        _ => None,
    }
}

/// The key as Claude Code names it in the exit hint, such as `Ctrl-C`.
fn hint_name(key: &KeyEvent) -> String {
    let spec = KeySpec::from_event(key).to_string();
    let parts: Vec<String> = spec
        .split('+')
        .map(|part| {
            let mut chars = part.chars();
            match (chars.next(), chars.as_str()) {
                (Some(first), rest) => first.to_uppercase().chain(rest.chars()).collect(),
                (None, _) => String::new(),
            }
        })
        .collect();
    parts.join("-")
}
