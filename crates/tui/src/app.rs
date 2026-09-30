//! Screen state and key handling for the live main screen, kept free of terminal I/O.

use argonauts_core::{KeyScheme, MergeDecision, Verbosity};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::oneshot;

use crate::UiOptions;
use crate::channel::{UiCommand, UiEvent};
use crate::model::RunView;
use crate::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputMode {
    Prompt,
    /// The note for "request changes".
    Note,
}

pub(crate) struct App {
    pub view: RunView,
    pub verbosity: Verbosity,
    configured_verbosity: Verbosity,
    keys: KeyScheme,
    pub theme: Theme,
    pub tick: u64,
    pub input: String,
    pub mode: InputMode,
    /// `Some` while the diff view is open, holding its scroll position.
    pub diff_scroll: Option<usize>,
    /// Lines the transcript is scrolled up from the live end.
    pub scroll: usize,
    pub page: usize,
    pub hint: Option<String>,
    /// Set after `a` on a merge card, until `y` or Enter confirms it or another key cancels.
    confirm_merge: bool,
    pub stopping: bool,
    pub quit: bool,
    ctrl_c_presses: u8,
    reply: Option<oneshot::Sender<MergeDecision>>,
    bell: bool,
    bell_pending: bool,
}

impl App {
    pub(crate) fn new(options: &UiOptions) -> Self {
        Self {
            view: RunView::new(options.home_dir.clone()),
            verbosity: options.verbosity,
            configured_verbosity: options.verbosity,
            keys: options.keys,
            theme: Theme::new(options.color),
            tick: 0,
            input: String::new(),
            mode: InputMode::Prompt,
            diff_scroll: None,
            scroll: 0,
            page: 10,
            hint: None,
            confirm_merge: false,
            stopping: false,
            quit: false,
            ctrl_c_presses: 0,
            reply: None,
            bell: options.bell,
            bell_pending: false,
        }
    }

    pub(crate) fn on_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::Run(event) => self.view.apply(&event),
            UiEvent::MergeCardShown(prompt) => {
                self.view
                    .show_merge_prompt(&prompt.card_id, &prompt.card, prompt.diff.as_deref());
                self.reply = Some(prompt.reply);
                self.bell_pending = self.bell;
                self.scroll = 0;
                self.ctrl_c_presses = 0;
                self.confirm_merge = false;
                // Whatever was typed before the card appeared must not answer it.
                self.input.clear();
            }
        }
    }

    pub(crate) fn on_tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    pub(crate) fn take_bell(&mut self) -> bool {
        std::mem::take(&mut self.bell_pending)
    }

    pub(crate) fn on_paste(&mut self, text: &str) {
        if self.diff_scroll.is_none() {
            self.input
                .extend(text.chars().filter(|c| !c.is_control()).take(4_000));
        }
    }

    pub(crate) fn on_key(&mut self, key: KeyEvent) -> Option<UiCommand> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            return self.ctrl_c();
        }
        self.ctrl_c_presses = 0;
        if self.diff_scroll.is_some() {
            self.diff_key(key);
            return None;
        }
        match self.mode {
            InputMode::Note => {
                self.note_key(key);
                None
            }
            InputMode::Prompt => self.prompt_key(key, ctrl),
        }
    }

    fn ctrl_c(&mut self) -> Option<UiCommand> {
        if self.view.finished.is_some() || self.stopping {
            self.quit = true;
            return None;
        }
        self.ctrl_c_presses += 1;
        if self.ctrl_c_presses == 1 {
            self.hint = Some("Press Ctrl+C again to stop the run.".to_owned());
            return None;
        }
        self.ctrl_c_presses = 0;
        self.request_stop()
    }

    fn request_stop(&mut self) -> Option<UiCommand> {
        if self.view.pending_merge().is_some() {
            self.decide(MergeDecision::Leave);
            self.hint = Some("Left the branch as it is.".to_owned());
            return None;
        }
        self.stopping = true;
        self.hint = Some(
            "Stopping the run. The agent's work stays on its branch. Ctrl+C again closes this screen."
                .to_owned(),
        );
        Some(UiCommand::StopRun)
    }

    pub(crate) fn confirming_merge(&self) -> bool {
        self.confirm_merge && self.view.pending_merge().is_some()
    }

    fn prompt_key(&mut self, key: KeyEvent, ctrl: bool) -> Option<UiCommand> {
        if self.confirming_merge() {
            self.confirm_merge = false;
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Enter) && !ctrl {
                self.decide(MergeDecision::Approve);
            } else {
                self.hint = Some("Not merged. The card is still open.".to_owned());
            }
            return None;
        }
        let vim = self.keys == KeyScheme::Vim;
        match key.code {
            KeyCode::Char('u') if ctrl && vim => self.scroll_up(self.page / 2),
            KeyCode::Char('d') if ctrl && vim => self.scroll_down(self.page / 2),
            KeyCode::Char('G') if vim && self.input.is_empty() => self.scroll = 0,
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                if self.input.is_empty() && self.merge_key(c) {
                    return None;
                }
                self.input.push(c);
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Enter => return self.submit(),
            KeyCode::Esc => {
                if self.input.is_empty() {
                    self.hint = None;
                } else {
                    self.input.clear();
                }
            }
            KeyCode::PageUp => self.scroll_up(self.page),
            KeyCode::PageDown => self.scroll_down(self.page),
            KeyCode::Up => self.scroll_up(1),
            KeyCode::Down => self.scroll_down(1),
            KeyCode::End => self.scroll = 0,
            KeyCode::Home => self.scroll = usize::MAX,
            _ => {}
        }
        None
    }

    /// Returns false when `c` isn't a merge key, so it gets typed instead.
    fn merge_key(&mut self, c: char) -> bool {
        if self.view.pending_merge().is_none() {
            return false;
        }
        match c {
            // Two keys, so a stray or injected keystroke can't merge.
            'a' => {
                let target = self
                    .view
                    .pending_merge()
                    .and_then(|merge| merge.card.target_branch.clone())
                    .unwrap_or_else(|| "the starting branch".to_owned());
                self.confirm_merge = true;
                self.hint = Some(format!(
                    "Merge into {target}? Press y or Enter to confirm, any other key to cancel."
                ));
            }
            'p' => self.decide(MergeDecision::OpenPr),
            'l' => self.decide(MergeDecision::Leave),
            'c' => {
                self.mode = InputMode::Note;
                self.hint =
                    Some("Say what should change, then press Enter. Esc cancels.".to_owned());
            }
            'd' => {
                if self.view.merge_diff().is_some() {
                    self.diff_scroll = Some(0);
                } else {
                    self.hint = Some("No diff came with this card.".to_owned());
                }
            }
            _ => return false,
        }
        true
    }

    fn submit(&mut self) -> Option<UiCommand> {
        let text = std::mem::take(&mut self.input);
        let text = text.trim();
        match text {
            "" => {}
            "/verbose" => self.toggle_verbosity(Verbosity::Full),
            "/quiet" => self.toggle_verbosity(Verbosity::Quiet),
            "/stop" => return self.request_stop(),
            "/help" | "?" => {
                self.hint = Some(
                    "Commands: /verbose, /quiet, /stop. On a merge card: a, d, c, p, l.".to_owned(),
                );
            }
            command if command.starts_with('/') => {
                self.hint = Some(format!("Unknown command {command}. Try /help."));
            }
            note if self.view.pending_merge().is_some() => {
                self.decide(MergeDecision::RequestChanges {
                    note: Some(note.to_owned()),
                });
            }
            _ => {
                self.hint = Some(
                    "Messages to a running agent aren't supported yet. /stop stops the run."
                        .to_owned(),
                );
            }
        }
        None
    }

    fn toggle_verbosity(&mut self, level: Verbosity) {
        self.verbosity = if self.verbosity == level {
            if self.configured_verbosity == level {
                Verbosity::Normal
            } else {
                self.configured_verbosity
            }
        } else {
            level
        };
        let name = match self.verbosity {
            Verbosity::Full => "full",
            Verbosity::Normal => "normal",
            Verbosity::Quiet => "quiet",
        };
        self.hint = Some(format!("Verbosity: {name}."));
        self.scroll = 0;
    }

    fn note_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char(c) => self.input.push(c),
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Enter => {
                let note = std::mem::take(&mut self.input).trim().to_owned();
                let note = (!note.is_empty()).then_some(note);
                self.decide(MergeDecision::RequestChanges { note });
            }
            KeyCode::Esc => {
                self.mode = InputMode::Prompt;
                self.input.clear();
                self.hint = None;
            }
            _ => {}
        }
    }

    fn diff_key(&mut self, key: KeyEvent) {
        let Some(scroll) = self.diff_scroll.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('d') => self.diff_scroll = None,
            KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => *scroll = scroll.saturating_add(1),
            KeyCode::PageUp => *scroll = scroll.saturating_sub(self.page),
            KeyCode::PageDown | KeyCode::Char(' ') => *scroll = scroll.saturating_add(self.page),
            KeyCode::Home | KeyCode::Char('g') => *scroll = 0,
            KeyCode::End | KeyCode::Char('G') => *scroll = usize::MAX,
            _ => {}
        }
    }

    fn scroll_up(&mut self, lines: usize) {
        self.scroll = self.scroll.saturating_add(lines.max(1));
    }

    fn scroll_down(&mut self, lines: usize) {
        self.scroll = self.scroll.saturating_sub(lines.max(1));
    }

    pub(crate) fn clamp_scroll(&mut self, transcript_max: usize, diff_max: usize) {
        self.scroll = self.scroll.min(transcript_max);
        if let Some(scroll) = self.diff_scroll.as_mut() {
            *scroll = (*scroll).min(diff_max);
        }
    }

    fn decide(&mut self, decision: MergeDecision) {
        if let Some(reply) = self.reply.take() {
            // The conductor may have stopped waiting; then there is nothing left to tell it.
            let _ = reply.send(decision.clone());
        }
        self.view.mark_chosen(decision);
        self.mode = InputMode::Prompt;
        self.input.clear();
        self.diff_scroll = None;
        self.hint = None;
        self.confirm_merge = false;
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::{CardId, RunOutcome};

    use super::*;
    use crate::channel::MergePrompt;
    use crate::testing::{DIFF, Script, merge_card};

    fn options() -> UiOptions {
        UiOptions {
            verbosity: Verbosity::Normal,
            keys: KeyScheme::Standard,
            color: false,
            bell: true,
            home_dir: None,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_text(app: &mut App, text: &str) -> Option<UiCommand> {
        for c in text.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter))
    }

    fn app_with_card() -> (App, oneshot::Receiver<MergeDecision>) {
        let mut app = App::new(&options());
        for event in Script::solo_finished().events {
            app.on_event(event.into());
        }
        let (prompt, answer) =
            MergePrompt::new(CardId::numbered(1), merge_card(), Some(DIFF.into()));
        app.on_event(UiEvent::MergeCardShown(Box::new(prompt)));
        (app, answer)
    }

    #[test]
    fn merge_keys_send_decisions() {
        for (c, expected) in [('p', MergeDecision::OpenPr), ('l', MergeDecision::Leave)] {
            let (mut app, mut answer) = app_with_card();
            assert!(app.take_bell());
            assert!(!app.take_bell());
            app.on_key(key(KeyCode::Char(c)));
            assert_eq!(answer.try_recv().unwrap(), expected);
            assert!(app.view.pending_merge().is_none());
        }
    }

    #[test]
    fn approving_takes_a_confirming_key() {
        for confirm in [KeyCode::Char('y'), KeyCode::Enter] {
            let (mut app, mut answer) = app_with_card();
            app.on_key(key(KeyCode::Char('a')));
            assert!(answer.try_recv().is_err(), "one key must not merge");
            assert!(app.confirming_merge());
            assert!(app.hint.as_deref().unwrap().contains("Merge into main?"));
            app.on_key(key(confirm));
            assert_eq!(answer.try_recv().unwrap(), MergeDecision::Approve);
            assert!(!app.confirming_merge());
        }
    }

    #[test]
    fn any_other_key_cancels_an_approval() {
        let (mut app, mut answer) = app_with_card();
        app.on_key(key(KeyCode::Char('a')));
        app.on_key(key(KeyCode::Char('a')));
        assert!(answer.try_recv().is_err());
        assert!(!app.confirming_merge());
        assert_eq!(
            app.hint.as_deref(),
            Some("Not merged. The card is still open.")
        );
        assert!(app.view.pending_merge().is_some());
        assert!(app.input.is_empty(), "the cancelling key isn't typed");
    }

    #[test]
    fn request_changes_takes_a_note() {
        let (mut app, mut answer) = app_with_card();
        app.on_key(key(KeyCode::Char('c')));
        assert_eq!(app.mode, InputMode::Note);
        type_text(&mut app, "add a test");
        assert_eq!(
            answer.try_recv().unwrap(),
            MergeDecision::RequestChanges {
                note: Some("add a test".into())
            }
        );
        assert_eq!(app.mode, InputMode::Prompt);
    }

    #[test]
    fn request_changes_note_can_be_cancelled_or_empty() {
        let (mut app, mut answer) = app_with_card();
        app.on_key(key(KeyCode::Char('c')));
        app.on_key(key(KeyCode::Char('x')));
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.mode, InputMode::Prompt);
        assert!(answer.try_recv().is_err());

        app.on_key(key(KeyCode::Char('c')));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(
            answer.try_recv().unwrap(),
            MergeDecision::RequestChanges { note: None }
        );
    }

    #[test]
    fn typed_text_on_an_open_card_is_a_change_request() {
        let (mut app, mut answer) = app_with_card();
        app.on_key(key(KeyCode::Char('/')));
        app.on_key(key(KeyCode::Backspace));
        type_text(&mut app, "rename the file please");
        assert_eq!(
            answer.try_recv().unwrap(),
            MergeDecision::RequestChanges {
                note: Some("rename the file please".into())
            }
        );
    }

    #[test]
    fn diff_view_opens_scrolls_and_closes() {
        let (mut app, mut answer) = app_with_card();
        app.on_key(key(KeyCode::Char('d')));
        assert_eq!(app.diff_scroll, Some(0));
        app.on_key(key(KeyCode::Char('j')));
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.diff_scroll, Some(2));
        app.on_key(key(KeyCode::Char('a')));
        assert!(
            answer.try_recv().is_err(),
            "keys in the diff view don't decide"
        );
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.diff_scroll, None);
        app.clamp_scroll(0, 3);
    }

    #[test]
    fn ctrl_c_twice_stops_the_run() {
        let mut app = App::new(&options());
        for event in Script::solo_until_working().events {
            app.on_event(event.into());
        }
        assert_eq!(app.on_key(ctrl('c')), None);
        assert!(app.hint.as_deref().unwrap().contains("again"));
        assert_eq!(app.on_key(ctrl('c')), Some(UiCommand::StopRun));
        assert!(app.stopping);
        assert!(!app.quit);
        assert_eq!(app.on_key(ctrl('c')), None);
        assert!(app.quit);
    }

    #[test]
    fn ctrl_c_on_an_open_card_leaves_the_branch() {
        let (mut app, mut answer) = app_with_card();
        app.on_key(ctrl('c'));
        assert_eq!(app.on_key(ctrl('c')), None);
        assert_eq!(answer.try_recv().unwrap(), MergeDecision::Leave);
    }

    #[test]
    fn other_keys_reset_ctrl_c() {
        let mut app = App::new(&options());
        app.on_key(ctrl('c'));
        app.on_key(key(KeyCode::Char('x')));
        assert_eq!(app.on_key(ctrl('c')), None);
    }

    #[test]
    fn ctrl_c_after_the_run_finished_closes_the_screen() {
        let mut app = App::new(&options());
        let mut s = Script::new();
        s.finished(RunOutcome::Merged, None);
        app.on_event(s.events.remove(0).into());
        app.on_key(ctrl('c'));
        assert!(app.quit);
    }

    #[test]
    fn slash_commands() {
        let mut app = App::new(&options());
        type_text(&mut app, "/verbose");
        assert_eq!(app.verbosity, Verbosity::Full);
        type_text(&mut app, "/verbose");
        assert_eq!(app.verbosity, Verbosity::Normal);
        type_text(&mut app, "/quiet");
        assert_eq!(app.verbosity, Verbosity::Quiet);
        type_text(&mut app, "/nope");
        assert!(
            app.hint
                .as_deref()
                .unwrap()
                .contains("Unknown command /nope")
        );
        assert_eq!(type_text(&mut app, "/stop"), Some(UiCommand::StopRun));
        type_text(&mut app, "hello agent");
        assert!(app.hint.is_some());
    }

    #[test]
    fn verbosity_toggle_returns_to_the_configured_level() {
        let mut app = App::new(&UiOptions {
            verbosity: Verbosity::Quiet,
            ..options()
        });
        type_text(&mut app, "/verbose");
        assert_eq!(app.verbosity, Verbosity::Full);
        type_text(&mut app, "/verbose");
        assert_eq!(app.verbosity, Verbosity::Quiet);
    }

    #[test]
    fn scrolling_keys() {
        let mut app = App::new(&options());
        app.page = 10;
        app.on_key(key(KeyCode::PageUp));
        assert_eq!(app.scroll, 10);
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.scroll, 9);
        app.on_key(key(KeyCode::End));
        assert_eq!(app.scroll, 0);
        app.on_key(key(KeyCode::Home));
        app.clamp_scroll(25, 0);
        assert_eq!(app.scroll, 25);
    }

    #[test]
    fn vim_keys_scroll_half_pages() {
        let mut app = App::new(&UiOptions {
            keys: KeyScheme::Vim,
            ..options()
        });
        app.page = 10;
        app.on_key(ctrl('u'));
        assert_eq!(app.scroll, 5);
        app.on_key(key(KeyCode::Char('G')));
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn letters_type_normally_without_a_card() {
        let mut app = App::new(&options());
        app.on_key(key(KeyCode::Char('a')));
        assert_eq!(app.input, "a");
    }

    #[test]
    fn paste_drops_control_characters() {
        let mut app = App::new(&options());
        app.on_paste("fix\x1b[31m it\n");
        assert_eq!(app.input, "fix[31m it");
    }
}
