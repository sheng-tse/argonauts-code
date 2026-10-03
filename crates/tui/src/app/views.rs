//! The app's full-screen views sit on a stack above the chat. The top one gets every key and
//! every controller answer first; what it asks for comes back as [`UiAction`]s for the bridge.

use std::io::Write;
use std::time::Instant;

use argonauts_core::app::ViewData;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind};

use super::{Ui, UiAction};
use crate::keymap::{Action, Layer};
use crate::state::AppState;
use crate::views::{ViewCtx, ViewOutcome};

impl<W: Write> Ui<W> {
    /// Like [`handle_event`](Self::handle_event), with the top view seeing `state`.
    pub fn handle_event_in(
        &mut self,
        event: Event,
        now: Instant,
        state: &AppState,
    ) -> Vec<UiAction> {
        if let Some(actions) = self.selection_key(&event) {
            return actions;
        }
        match event {
            Event::Key(key) if !self.views.is_empty() => {
                if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                    self.view_key(key, now, state)
                } else {
                    Vec::new()
                }
            }
            // The hidden prompt never gets a paste meant for the view on top.
            Event::Paste(text) if !self.views.is_empty() => self.view_paste(&text, now, state),
            Event::Mouse(mouse) => self.mouse_event(mouse, now, state),
            event => self.handle_chat_event(event, now),
        }
    }

    /// A controller answer for the view that asked. False when no open view takes it.
    pub fn offer_view_data(&mut self, data: &ViewData) -> bool {
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
            ViewOutcome::Open(view) => self.views.push(view),
            ViewOutcome::Replace(view) => {
                self.views.pop();
                self.views.push(view);
            }
            ViewOutcome::Send(command) => actions.push(UiAction::Command(command)),
            ViewOutcome::SendToChat(command) => {
                self.views.clear();
                actions.push(UiAction::Command(command));
            }
            ViewOutcome::Request(request) => actions.push(UiAction::Request(request)),
            ViewOutcome::AnswerCard(target) => actions.push(UiAction::AnswerCard(target)),
            ViewOutcome::ConfirmTakeOver { run, card } => {
                actions.push(UiAction::ConfirmTakeOver { run, card });
            }
            ViewOutcome::Notice(text) => actions.push(UiAction::Notice(text)),
        }
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
