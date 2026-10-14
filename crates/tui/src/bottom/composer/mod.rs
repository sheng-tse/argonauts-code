mod edit;
mod history;
mod layout;
mod motion;
mod paste_burst;
mod picture;
mod undo;
mod view;
mod vim;
mod workflow_word;

use std::cell::Cell;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;

use argonauts_core::cli_commands::CliCommandSet;
use argonauts_core::input::{DraftCommand, draft_command};
use argonauts_core::keywords::Keyword;
use argonauts_core::sanitize::sanitize_for_terminal;

use super::VimMode;
use crate::keywords;
use crate::render::{Clock, RenderCtx};
use crate::text::sanitize_line;
use crate::theme::Theme;

use edit::{Clip, Draft};
use paste_burst::PasteBurst;
pub(super) use paste_burst::{CHAR_INTERVAL, ENTER_WINDOW, MIN_CHARS};

pub use view::{SHELL_PLACEHOLDER, SHELL_PROMPT};
pub use workflow_word::{ignored_notice, requested_notice};

/// Longer drafts scroll to keep the cursor in view.
pub const MAX_INPUT_ROWS: usize = 10;

/// A paste past either limit collapses to a label.
const PASTE_CHARS: usize = 800;
const PASTE_LINES: usize = 3;

/// The example a fresh session offers until one is built from the repo's recent commit
/// subjects. It suits any repo, and it only reads.
pub const BUILT_IN_EXAMPLE: &str = "review the last commit";

#[derive(Debug, Clone, Default)]
pub struct Composer {
    draft: Draft,
    pub label: Option<String>,
    /// Ultracode is on for the session, so the rule carries its tag at the far right.
    pub ultracode: bool,
    /// Takes the place of the example.
    pub placeholder: Option<String>,
    /// Built from recent commit subjects, never with a model. Without one,
    /// [`BUILT_IN_EXAMPLE`] shows.
    pub example: Option<String>,
    /// The main agent CLI's keywords, drawn as that CLI draws them.
    pub keywords: Vec<Keyword>,
    /// The main agent CLI's own commands, which are real commands beside Argonauts Code's.
    pub cli_commands: Option<CliCommandSet>,
    /// The names of your own mods, each of which runs as a command.
    pub mod_commands: Vec<String>,
    /// Set by the first prompt. An example is a way to start a fresh session, so it goes then.
    started: bool,
    history: Vec<String>,
    /// `!` at the start of an empty input: the text is a command for your shell.
    shell: bool,
    /// The history index shown while walking history.
    browsing: Option<usize>,
    /// The draft from before history walking started, restored when walking past the newest.
    stash: Option<Saved>,
    /// The column Up and Down aim for, kept across a run of vertical moves.
    preferred_col: Option<usize>,
    /// For numbering paste labels.
    pastes: usize,
    /// What Ctrl+K, Ctrl+U, or Ctrl+W removed last, for Ctrl+Y.
    kill: Clip,
    /// The kill holds whole lines, from vim's `dd`, `yy`, or VISUAL LINE, so `p` puts it on a
    /// line of its own.
    kill_linewise: bool,
    /// The input width and scroll of the last render, for moving by visual rows.
    view: Cell<ViewState>,
    /// `None` without vim keys.
    vim: Option<VimMode>,
    vim_pending: vim::Pending,
    visual: Option<vim::Visual>,
    burst: PasteBurst,
    /// A field inside a question, such as a question's answer: `!` is a letter there, and nothing
    /// typed in it joins the input's history.
    field: bool,
    /// Opt+W ignored the workflow word in this draft. Reading it settles it, since the word
    /// leaving the draft forgets the ignore.
    workflow_ignored: Cell<bool>,
    /// The prompt just sent asked for a workflow.
    workflow_sent: bool,
    /// What undo and redo step through.
    edits: undo::EditHistory,
    /// The pictures made in this conversation. A chip in a draft, or in a message brought back
    /// from history or the queue, names one of them.
    pictures: Vec<argonauts_core::pictures::Picture>,
    /// The pictures of the prompt just sent, until the screen takes them.
    sent_pictures: Vec<argonauts_core::pictures::Picture>,
    /// The last chip's number. Chips count on across the conversation, as in Claude Code.
    next_picture: u32,
    clipboard: picture::Clipboard,
    arriving: picture::Arriving,
}

/// A draft put aside while history shows, with the mode it was typed in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Saved {
    draft: Draft,
    shell: bool,
}

/// A draft out of the input while a question holds the keys, to come back as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftAside {
    saved: Saved,
    browsing: Option<usize>,
    stash: Option<Saved>,
    pastes: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ViewState {
    /// Columns for text on each input row, or zero before the first render.
    width: usize,
    scroll: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerEvent {
    /// `text` has its pastes expanded, and `shown` keeps them as their labels for the echo.
    Submitted {
        text: String,
        shown: String,
    },
    /// Enter in shell mode, with pastes expanded.
    Shell {
        command: String,
    },
    Edited,
    Moved,
    /// Up on the first row with no older history.
    AtTop,
    /// Ctrl+R, or `/` in vim's NORMAL mode: open the prompt history search.
    SearchHistory,
    Unhandled,
}

impl ComposerEvent {
    /// A burst that went in before this key changed the text, whatever the key did.
    fn or_edited(self) -> Self {
        match self {
            Self::Moved | Self::AtTop | Self::Unhandled => Self::Edited,
            event => event,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ComposerState<'a> {
    pub focused: bool,
    pub busy: bool,
    pub offer_example: bool,
    /// Wins over the composer's own placeholder and example.
    pub placeholder: Option<&'a str>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ComposerView {
    pub lines: Vec<Line<'static>>,
    /// (column, row) within `lines`.
    pub cursor: (u16, u16),
}

/// Whether `typed`, a `/name` that is no command, is something on this computer, such as
/// `/tmp`. A prompt may start with one: Claude Code 2.1.296 looks for the path before it calls
/// the name an unknown command, and sends the prompt to the model as text.
pub fn is_on_disk(typed: &str) -> bool {
    std::path::Path::new(typed).exists()
}

impl Composer {
    pub fn new() -> Self {
        Self::default()
    }

    /// The input's editor for a field inside a question: new lines, pastes, and the cursor keys
    /// work as in the input, with no shell mode and no history.
    pub fn field() -> Self {
        Self {
            field: true,
            ..Self::default()
        }
    }

    /// With pasted blocks as their labels.
    /// What the draft is as a command: a real one, with the arguments still to come, or none
    /// at all, with the nearest real one. `None` for anything else, and in shell mode.
    pub fn command(&self) -> Option<DraftCommand> {
        if self.shell || self.field {
            return None;
        }
        let command = draft_command(
            &self.draft.text,
            self.cli_commands.as_ref(),
            &self.mod_commands,
        )?;
        // Until the CLI's own commands have arrived, a name Argonauts Code doesn't know may
        // still be one of them.
        let judged = self.cli_commands.is_some() || matches!(command, DraftCommand::Known { .. });
        let a_path = matches!(&command, DraftCommand::Unknown { typed, .. } if is_on_disk(typed));
        (judged && !a_path).then_some(command)
    }

    /// What the bottom line says Enter does with a draft that is a command. Where the draft's
    /// row, on a screen `width` cells wide, has no room for how to fix a mistyped one, the fix
    /// follows here.
    pub fn enter_does(&self, width: usize) -> Option<String> {
        let command = self.command()?;
        Some(if view::fix_goes_below(&command, &self.draft.text, width) {
            command.enter_does_and_fix()
        } else {
            command.enter_does()
        })
    }

    /// Tab on a mistyped command puts the nearest real one in its place, and leaves the
    /// cursor where it was in the words after it.
    fn fix_command(&mut self) -> Option<ComposerEvent> {
        let Some(DraftCommand::Unknown {
            typed,
            nearest: Some(nearest),
        }) = self.command()
        else {
            return None;
        };
        let cursor = self.draft.cursor;
        self.draft.delete(0..typed.len());
        self.draft.cursor = 0;
        self.draft.insert(&nearest);
        self.draft.cursor = if cursor <= typed.len() {
            nearest.len()
        } else {
            cursor - typed.len() + nearest.len()
        };
        Some(self.edited())
    }

    pub fn text(&self) -> &str {
        &self.draft.text
    }

    pub fn expanded_text(&self) -> String {
        self.draft.expanded()
    }

    /// A byte offset into [`text`](Self::text).
    pub fn cursor(&self) -> usize {
        self.draft.cursor
    }

    pub fn placeholder_text(&self, offer_example: bool) -> Option<String> {
        self.placeholder_for(ComposerState {
            offer_example,
            ..ComposerState::default()
        })
    }

    fn placeholder_for(&self, state: ComposerState<'_>) -> Option<String> {
        let clean = |text: &str| sanitize_line(text).trim().to_owned();
        let placeholder = [state.placeholder, self.placeholder.as_deref()]
            .into_iter()
            .flatten()
            .map(clean)
            .find(|placeholder| !placeholder.is_empty());
        if placeholder.is_some() {
            return placeholder;
        }
        if !state.offer_example || self.started {
            return None;
        }
        let example = self
            .example
            .as_deref()
            .map(clean)
            .filter(|example| !example.is_empty());
        let example = example.as_deref().unwrap_or(BUILT_IN_EXAMPLE);
        Some(format!("Try \"{example}\""))
    }

    /// No example shows from here on, as after the first prompt.
    pub fn end_example(&mut self) {
        self.started = true;
        self.example = None;
    }

    pub fn is_empty(&self) -> bool {
        self.draft.text.is_empty()
    }

    /// A paste typed out as keys is held back until the keys stop, so the draft can look
    /// empty while it comes in.
    pub fn holds_paste(&self) -> bool {
        self.burst.holds()
    }

    /// In shell mode, Enter runs the text as a command rather than sending it.
    pub fn is_shell(&self) -> bool {
        self.shell
    }

    /// Esc leaves shell mode, keeping what was typed. True when it was on.
    pub fn leave_shell(&mut self) -> bool {
        std::mem::take(&mut self.shell)
    }

    /// Escapes and controls are removed.
    pub fn set_text(&mut self, text: &str) {
        self.draft = Draft::from_text(&clean(text));
        self.browsing = None;
        self.stash = None;
        self.preferred_col = None;
        self.pastes = 0;
        self.visual = None;
        self.vim_pending = vim::Pending::default();
        if self.vim == Some(VimMode::Visual) {
            self.vim = Some(VimMode::Normal);
        }
    }

    /// A prompt picked in the history search, with the cursor at its end, on its last letter
    /// in vim's NORMAL mode. A `!` command comes back in shell mode, as ↑ brings it.
    pub fn fill(&mut self, entry: &str) {
        let (text, shell) = shell_entry(entry);
        self.set_text(text);
        self.shell = shell;
        if self.vim == Some(VimMode::Normal) {
            let at = self.draft.cursor;
            if at > self.draft.line_start(at) {
                self.draft.cursor = self.draft.prev_stop(at);
            }
        }
    }

    /// The draft back from `$EDITOR`, which was given `sent`, the draft with its pastes
    /// expanded. Left unchanged there, the draft keeps its pastes collapsed.
    pub fn set_edited_text(&mut self, sent: &str, edited: &str) {
        if edited != sent {
            let before = self.saved();
            self.set_text(edited);
            self.edited_from(before, undo::EditKind::Other, false);
        }
    }

    /// Messages taken back from the queue go in front of the draft, oldest first, each on its
    /// own line, with the cursor after them.
    pub fn restore(&mut self, texts: &[String]) {
        let restored: Vec<String> = texts
            .iter()
            .map(|text| clean(text))
            .filter(|text| !text.trim().is_empty())
            .collect();
        if restored.is_empty() {
            return;
        }
        let mut block = restored.join("\n");
        let end = block.len();
        if !self.is_empty() {
            block.push('\n');
        }
        self.draft.cursor = 0;
        self.draft.insert(&block);
        self.draft.cursor = end;
        self.edited();
    }

    /// Shell mode goes too, and so do the draft's steps for undo.
    pub fn clear(&mut self) {
        self.set_text("");
        self.shell = false;
        self.forget_edits();
    }

    /// Takes the draft out whole, with its pastes, its mode, and where you were in history,
    /// leaving the input empty. `None` when there's nothing to keep.
    pub fn set_aside(&mut self) -> Option<DraftAside> {
        if self.is_empty() && !self.shell && self.browsing.is_none() {
            return None;
        }
        let aside = DraftAside {
            saved: Saved {
                draft: self.draft.clone(),
                shell: self.shell,
            },
            browsing: self.browsing,
            stash: self.stash.clone(),
            pastes: self.pastes,
        };
        self.clear();
        Some(aside)
    }

    /// Puts a draft taken by [`set_aside`](Self::set_aside) back as it was. Text that reached
    /// the input meanwhile stays, after it.
    pub fn bring_back(&mut self, aside: DraftAside) {
        if !self.is_empty() || self.shell {
            self.restore(&[aside.saved.draft.expanded()]);
            return;
        }
        self.set_text("");
        self.draft = aside.saved.draft;
        self.shell = aside.saved.shell;
        self.browsing = aside.browsing;
        self.stash = aside.stash;
        self.pastes = aside.pastes;
    }

    pub fn is_browsing_history(&self) -> bool {
        self.browsing.is_some()
    }

    pub fn cursor_on_first_row(&self) -> bool {
        let (rows, width) = self.rows();
        layout::cursor_position(&self.draft.text, &rows, self.draft.cursor, width).0 == 0
    }

    pub fn cursor_on_last_row(&self) -> bool {
        let (rows, width) = self.rows();
        let (row, _) = layout::cursor_position(&self.draft.text, &rows, self.draft.cursor, width);
        row + 1 >= rows.len()
    }

    /// A key without its arrival time, so it never counts toward a paste burst. Use
    /// [`handle_key_at`](Self::handle_key_at) for keys from the terminal.
    pub fn handle_key(&mut self, key: KeyEvent) -> ComposerEvent {
        let flushed = self.flush_burst();
        self.burst.end_run();
        let before = self.saved();
        let event = self.dispatch(key);
        self.note_edit(before, &key, &event);
        // Settles the ignore, so a word typed again after it left counts afresh.
        self.workflow_requested();
        if flushed { event.or_edited() } else { event }
    }

    /// What a key just did to the draft, for undo. Vim's own commands keep their steps, and
    /// sending starts a new draft with none.
    fn note_edit(&mut self, before: Saved, key: &KeyEvent, event: &ComposerEvent) {
        let vim_command = matches!(self.vim, Some(VimMode::Normal | VimMode::Visual));
        if vim_command || undo::is_undo_key(key) {
            return;
        }
        match event {
            ComposerEvent::Submitted { .. } | ComposerEvent::Shell { .. } => {}
            _ if !same_text(&before, &self.saved()) => {
                let ends_word = matches!(key.code, KeyCode::Char(c) if c.is_whitespace());
                self.edited_from(before, undo::EditKind::of(key), ends_word);
            }
            ComposerEvent::Moved => self.edit_step_ends(),
            _ => {}
        }
    }

    fn dispatch(&mut self, key: KeyEvent) -> ComposerEvent {
        if let Some(event) = self.vim_key(key) {
            return event;
        }
        if undo::is_undo_key(&key) {
            return self.undo();
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let word = ctrl || alt;
        if key.code == KeyCode::Tab
            && !(ctrl || alt || shift)
            && let Some(event) = self.fix_command()
        {
            return event;
        }
        let draft = &self.draft;
        let at = draft.cursor;
        match key.code {
            KeyCode::Enter if shift || alt => self.insert("\n"),
            KeyCode::Enter if ctrl => ComposerEvent::Unhandled,
            KeyCode::Enter => self.enter(),
            KeyCode::Char('!') if ctrl == alt && self.is_empty() && !self.shell && !self.field => {
                self.shell = true;
                self.edited()
            }
            KeyCode::Backspace if self.shell && self.is_empty() => {
                self.shell = false;
                self.edited()
            }
            // AltGr arrives as Ctrl+Alt on some systems and types a character.
            KeyCode::Char(c) if ctrl == alt => self.insert(c.encode_utf8(&mut [0; 4])),
            KeyCode::Char('j') if ctrl => self.insert("\n"),
            KeyCode::Char('r') if ctrl => ComposerEvent::SearchHistory,
            KeyCode::Char('v') if ctrl && !self.field => self.paste_clipboard(),
            KeyCode::Char('a') if ctrl => self.move_to(draft.line_start(at)),
            KeyCode::Char('e') if ctrl => self.move_to(draft.line_end(at)),
            KeyCode::Char('b') if ctrl => self.move_to(draft.prev_stop(at)),
            KeyCode::Char('f') if ctrl => self.move_to(draft.next_stop(at)),
            KeyCode::Char('b') if alt => self.move_to(draft.word_start_before(at)),
            KeyCode::Char('f') if alt => self.move_to(draft.word_end_after(at)),
            KeyCode::Char('w') if ctrl => self.kill_back(draft.word_start_before(at)),
            KeyCode::Char('u') if ctrl => self.kill_back(draft.line_start(at)),
            KeyCode::Char('k') if ctrl => self.kill_forward(draft.line_end(at)),
            KeyCode::Char('y') if ctrl => self.yank(),
            KeyCode::Char('h') if ctrl => self.delete_back(),
            KeyCode::Char('d') if ctrl && !self.is_empty() => self.delete_forward(),
            KeyCode::Backspace if word => self.kill_back(draft.word_start_before(at)),
            KeyCode::Backspace => self.delete_back(),
            KeyCode::Delete if word => self.kill_forward(draft.word_end_after(at)),
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Left if word => self.move_to(draft.word_start_before(at)),
            KeyCode::Right if word => self.move_to(draft.word_end_after(at)),
            KeyCode::Left => self.move_to(draft.prev_stop(at)),
            KeyCode::Right => self.move_to(draft.next_stop(at)),
            KeyCode::Home => self.move_to(draft.line_start(at)),
            KeyCode::End => self.move_to(draft.line_end(at)),
            KeyCode::Up => self.up(),
            KeyCode::Down => self.down(),
            _ => ComposerEvent::Unhandled,
        }
    }

    /// A bracketed paste. A burst still held goes in first, so the text keeps its order.
    pub fn handle_paste(&mut self, text: &str) {
        self.flush_burst();
        self.insert_paste(text);
    }

    fn insert_paste(&mut self, text: &str) {
        let mut text = clean(text);
        if text.is_empty() || self.paste_pictures(&text) {
            return;
        }
        let before = self.saved();
        // A pasted `!git status` reads as typed: shell mode, then the command. A paste of more
        // lines, such as a notebook cell or Markdown that opens with `![image]`, stays text.
        if self.is_empty()
            && !self.shell
            && !self.field
            && !text.contains('\n')
            && let Some(command) = text.strip_prefix('!')
        {
            self.shell = true;
            text = command.to_owned();
        }
        // What Enter runs as a command always shows in full, never behind a label.
        let breaks = text.matches('\n').count();
        if !self.shell && (text.chars().count() > PASTE_CHARS || breaks + 1 > PASTE_LINES) {
            self.pastes += 1;
            let label = if breaks > 0 {
                format!("[Pasted text #{} +{breaks} lines]", self.pastes)
            } else {
                format!("[Pasted text #{}]", self.pastes)
            };
            self.draft.insert_element(&label, text);
        } else {
            self.draft.insert(&text);
        }
        self.edited_from(before, undo::EditKind::Other, false);
        self.edited();
    }

    /// When a keyword's shimmer next moves. Shell mode sends nothing to the agent, so it draws
    /// no keywords.
    pub fn next_change(&self, clock: &Clock, theme: &Theme) -> Option<Instant> {
        if self.shell {
            return None;
        }
        // The workflow word holds still, so only the CLI's own keywords can move.
        keywords::next_change(&self.draft.text, &self.keywords, theme, clock)
    }

    pub fn render(&self, ctx: &RenderCtx<'_>, focused: bool, busy: bool) -> ComposerView {
        self.render_with(
            ctx,
            ComposerState {
                focused,
                busy,
                offer_example: !busy,
                placeholder: None,
            },
        )
    }

    pub fn render_with(&self, ctx: &RenderCtx<'_>, state: ComposerState<'_>) -> ComposerView {
        view::render(self, ctx, state)
    }

    fn enter(&mut self) -> ComposerEvent {
        let cursor = self.draft.cursor;
        if self.draft.text[..cursor].ends_with('\\') && self.draft.is_stop(cursor - 1) {
            self.draft.delete(cursor - 1..cursor);
            return self.insert("\n");
        }
        let expanded = self.draft.expanded();
        let text = expanded.trim_end_matches('\n');
        if text.trim().is_empty() {
            return ComposerEvent::Unhandled;
        }
        // What isn't a command goes nowhere, so the draft and its pictures stay as they are
        // for you to fix.
        if matches!(self.command(), Some(DraftCommand::Unknown { .. })) {
            return ComposerEvent::Moved;
        }
        let text = text.to_owned();
        if self.shell {
            // Kept with its `!`, so ↑ brings it back in shell mode.
            self.remember(format!("!{text}"));
            self.clear();
            return ComposerEvent::Shell { command: text };
        }
        // The echo keeps a long paste as its label, so it doesn't push the conversation away.
        let shown = self.draft.text.trim_end_matches('\n').to_owned();
        self.sent_pictures = self.pictures_in(&text);
        self.workflow_sent = self.workflow_requested();
        self.remember(text.clone());
        self.clear();
        self.end_example();
        ComposerEvent::Submitted { text, shown }
    }

    fn insert(&mut self, text: &str) -> ComposerEvent {
        self.draft.insert(text);
        self.edited()
    }

    fn delete_back(&mut self) -> ComposerEvent {
        let cursor = self.draft.cursor;
        if cursor == 0 {
            return ComposerEvent::Unhandled;
        }
        self.draft.delete(self.draft.prev_stop(cursor)..cursor);
        self.edited()
    }

    fn delete_forward(&mut self) -> ComposerEvent {
        let cursor = self.draft.cursor;
        if cursor >= self.draft.text.len() {
            return ComposerEvent::Unhandled;
        }
        self.draft.delete(cursor..self.draft.next_stop(cursor));
        self.edited()
    }

    fn kill_back(&mut self, to: usize) -> ComposerEvent {
        let cursor = self.draft.cursor;
        // At the start of a line, Ctrl+U takes the newline before it.
        let to = if to == cursor {
            self.draft.prev_stop(cursor)
        } else {
            to
        };
        if to == cursor {
            return ComposerEvent::Unhandled;
        }
        self.kill = self.draft.delete(to..cursor);
        self.kill_linewise = false;
        self.edited()
    }

    fn kill_forward(&mut self, to: usize) -> ComposerEvent {
        let cursor = self.draft.cursor;
        let to = if to == cursor {
            self.draft.next_stop(cursor)
        } else {
            to
        };
        if to == cursor {
            return ComposerEvent::Unhandled;
        }
        self.kill = self.draft.delete(cursor..to);
        self.kill_linewise = false;
        self.edited()
    }

    fn yank(&mut self) -> ComposerEvent {
        if self.kill.is_empty() {
            return ComposerEvent::Unhandled;
        }
        self.draft.insert_clip(&self.kill);
        self.edited()
    }

    fn move_to(&mut self, pos: usize) -> ComposerEvent {
        self.preferred_col = None;
        if pos == self.draft.cursor {
            return ComposerEvent::Unhandled;
        }
        self.draft.cursor = pos;
        ComposerEvent::Moved
    }

    /// Editing a recalled entry makes it the draft: history walking ends there.
    fn edited(&mut self) -> ComposerEvent {
        self.browsing = None;
        self.stash = None;
        self.preferred_col = None;
        ComposerEvent::Edited
    }

    fn up(&mut self) -> ComposerEvent {
        if self.browsing.is_some() && self.at_either_end() {
            return self.older();
        }
        let (rows, width) = self.rows();
        let (row, col) = layout::cursor_position(&self.draft.text, &rows, self.draft.cursor, width);
        if row == 0 {
            return self.older();
        }
        self.vertical(&rows, width, row - 1, col)
    }

    fn down(&mut self) -> ComposerEvent {
        if self.browsing.is_some() && self.at_either_end() {
            return self.newer();
        }
        let (rows, width) = self.rows();
        let (row, col) = layout::cursor_position(&self.draft.text, &rows, self.draft.cursor, width);
        if row + 1 < rows.len() {
            return self.vertical(&rows, width, row + 1, col);
        }
        if self.browsing.is_some() {
            return self.newer();
        }
        self.move_to(self.draft.text.len())
    }

    fn vertical(
        &mut self,
        rows: &[layout::Row],
        width: usize,
        row: usize,
        col: usize,
    ) -> ComposerEvent {
        let target = *self.preferred_col.get_or_insert(col);
        let draft = &self.draft;
        let offset = layout::offset_at(&draft.text, rows, row, target, width, |pos| {
            draft.is_stop(pos)
        });
        self.draft.cursor = offset;
        ComposerEvent::Moved
    }

    fn saved(&self) -> Saved {
        Saved {
            draft: self.draft.clone(),
            shell: self.shell,
        }
    }

    fn put_back(&mut self, saved: Saved) {
        self.draft = saved.draft;
        self.shell = saved.shell;
    }

    fn show_entry(&mut self, index: usize) {
        let (text, shell) = shell_entry(&self.history[index]);
        self.draft = Draft::from_text(text);
        self.shell = shell;
    }

    fn at_either_end(&self) -> bool {
        self.draft.cursor == 0 || self.draft.cursor == self.draft.text.len()
    }

    /// Unwrapped before the first render.
    fn rows(&self) -> (Vec<layout::Row>, usize) {
        let width = match self.view.get().width {
            0 => usize::MAX / 2,
            width => width,
        };
        (layout::rows(&self.draft.text, width), width)
    }
}

/// Pasted or restored text made safe to keep and send: line endings unified, and escapes and
/// controls removed. Tabs and joined emoji stay as they came, since this is what the agent
/// gets; only drawing lays them out. Line by line, so an unterminated OSC takes only the rest
/// of its own line with it.
fn clean(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace(['\r', '\u{2028}', '\u{2029}'], "\n")
        .split('\n')
        .map(sanitize_for_terminal)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The same draft, wherever the cursor is.
fn same_text(a: &Saved, b: &Saved) -> bool {
    a.shell == b.shell && a.draft.text == b.draft.text && a.draft.elements == b.draft.elements
}

/// A history entry that starts with `!` was a shell command: its text, and true.
fn shell_entry(entry: &str) -> (&str, bool) {
    match entry.strip_prefix('!') {
        Some(command) => (command, true),
        None => (entry, false),
    }
}

#[cfg(test)]
mod tests;
