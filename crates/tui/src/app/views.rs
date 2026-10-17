//! The app's full-screen views sit on a stack above the conversation. The top one gets every key
//! and every controller answer first; what it asks for comes back as [`UiAction`]s for the bridge.

use std::io::Write;
use std::time::Instant;

use argonauts_core::app::{UiCommand, ViewData};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind};

use super::{Ui, UiAction};
use crate::cell::{HistoryCell, ResultLine, Tone, UserMessage};
use crate::keymap::{Action, Layer};
use crate::state::AppState;
use crate::views::{Follow, ViewCtx, ViewOutcome};

impl<W: Write> Ui<W> {
    /// Like [`handle_event`](Self::handle_event), with the top view seeing `state`.
    pub fn handle_event_in(
        &mut self,
        event: Event,
        now: Instant,
        state: &AppState,
    ) -> Vec<UiAction> {
        self.settle_question_keys(now);
        if let Some(actions) = self.selection_key(&event) {
            return actions;
        }
        let view_has_keys = !self.views.is_empty() && !self.question_over_view();
        match event {
            Event::Key(key) if view_has_keys => {
                if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                    self.view_key(key, now, state)
                } else {
                    Vec::new()
                }
            }
            // The hidden prompt never gets a paste meant for the view on top.
            Event::Paste(text) if view_has_keys => self.view_paste(&text, now, state),
            Event::Mouse(mouse) => self.mouse_event(mouse, now, state),
            event => self.handle_conversation_event(event, now),
        }
    }

    /// After an event, the top view moves on with its run when it follows one: it's replaced,
    /// or it closes and what follows is asked for.
    pub fn follow_views(&mut self, now: Instant, state: &AppState) -> Vec<UiAction> {
        let Some(view) = self.views.last_mut() else {
            return Vec::new();
        };
        let ctx = ViewCtx {
            theme: &self.theme,
            state,
            now,
        };
        let Some(next) = view.follow(&ctx) else {
            return Vec::new();
        };
        self.dirty = true;
        self.views.pop();
        match next {
            Follow::Replace(view) => {
                self.stack_view(view);
                Vec::new()
            }
            Follow::Request(request) => vec![UiAction::Request(request)],
        }
    }

    /// A controller answer for the view that asked. False when no open view takes it.
    pub fn offer_view_data(&mut self, data: &ViewData) -> bool {
        if let ViewData::Mods { view } = data {
            self.model.mod_chords = crate::mods::keys::Chords::of(view);
        }
        let taken = self.views.last_mut().is_some_and(|view| view.accept(data));
        if taken {
            self.dirty = true;
        }
        taken
    }

    fn view_key(&mut self, key: KeyEvent, now: Instant, state: &AppState) -> Vec<UiAction> {
        let Some(view) = self.views.last_mut() else {
            return Vec::new();
        };
        let ctx = ViewCtx {
            theme: &self.theme,
            state,
            now,
        };
        let outcome = view.handle_key(key, &ctx);
        let mut actions = Vec::new();
        if let ViewOutcome::Ignored = outcome {
            self.unused_view_key(key, now, &mut actions);
        }
        self.view_outcome(outcome, &mut actions);
        actions
    }

    fn view_paste(&mut self, text: &str, now: Instant, state: &AppState) -> Vec<UiAction> {
        let Some(view) = self.views.last_mut() else {
            return Vec::new();
        };
        let ctx = ViewCtx {
            theme: &self.theme,
            state,
            now,
        };
        let outcome = view.handle_paste(text, &ctx);
        let mut actions = Vec::new();
        self.view_outcome(outcome, &mut actions);
        actions
    }

    fn view_outcome(&mut self, outcome: ViewOutcome, actions: &mut Vec<UiAction>) {
        self.dirty = true;
        match outcome {
            ViewOutcome::Consumed | ViewOutcome::Ignored => {}
            ViewOutcome::Close => {
                self.views.pop();
            }
            ViewOutcome::CloseAll => self.views.clear(),
            ViewOutcome::Open(view) => self.stack_view(view),
            ViewOutcome::Replace(view) => {
                self.views.pop();
                self.stack_view(view);
            }
            ViewOutcome::Send(command) => actions.push(UiAction::Command(command)),
            ViewOutcome::SendToConversation(command) => {
                self.views.clear();
                actions.push(UiAction::Command(command));
            }
            ViewOutcome::Request(request) => actions.push(UiAction::Request(request)),
            ViewOutcome::AnswerQuestion(target) => actions.push(UiAction::AnswerQuestion(target)),
            ViewOutcome::ConfirmTakeOver { run, task } => {
                actions.push(UiAction::ConfirmTakeOver { run, task });
            }
            ViewOutcome::Notice(text) => actions.push(UiAction::Notice(text)),
            ViewOutcome::Copy(text) => actions.push(UiAction::Copy(text)),
            ViewOutcome::OpenLink { url, name } => actions.push(UiAction::OpenLink { url, name }),
            ViewOutcome::CloseSaying { command, line } => self.close_saying(command, line, actions),
            ViewOutcome::CloseSayingThen {
                command,
                line,
                then,
            } => {
                self.close_saying(command, line, actions);
                actions.push(UiAction::Command(then));
            }
            ViewOutcome::SaveQuietly(command) => {
                if let UiCommand::SetSetting { key, .. } = &command {
                    self.model.said_by_view.insert(key.clone());
                }
                actions.push(UiAction::Command(command));
            }
        }
    }

    /// Back to the conversation, leaving `line` under an echo of `command`.
    fn close_saying(&mut self, command: String, line: String, actions: &mut Vec<UiAction>) {
        self.views.clear();
        let history = &mut self.model.history;
        history.push(UserMessage::command(command));
        let line = ResultLine::new(Tone::Dim, line);
        // The conversation's record keeps it after the command that opened the view.
        actions.push(UiAction::Replied(line.reply()));
        let line: Box<dyn HistoryCell> = Box::new(line);
        history.push(line);
    }

    /// Esc always goes back, and the quit keys work from any view. The prompt behind the view
    /// never sees a key.
    fn unused_view_key(&mut self, key: KeyEvent, now: Instant, actions: &mut Vec<UiAction>) {
        if key.code == KeyCode::Esc && key.modifiers.is_empty() {
            self.views.pop();
            return;
        }
        for action in self.keymap.actions_for(Layer::Global, &key) {
            if matches!(action, Action::ClearOrExit | Action::Exit) {
                self.arm_quit(action, &key, now, actions);
                return;
            }
        }
    }
}
