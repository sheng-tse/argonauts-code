//! Prompt history search, as in Claude Code: `/` in vim's NORMAL mode or Ctrl+R opens it under
//! the conversation in place of the input. Typing filters past prompts, newest at the bottom
//! with the pointer on it; the highlighted one shows whole in a box beside the list. Enter puts
//! it in the input and never sends it, so nothing reaches an agent from here.

mod fuzzy;
mod render;

use std::cell::RefCell;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::composer::{CHAR_INTERVAL, ENTER_WINDOW, MIN_CHARS};
use crate::prompt_history::PromptEntry;
use crate::text::{sanitize, sanitize_line};

use fuzzy::Query;

/// List rows on show at most, as in Claude Code.
pub const LIST_ROWS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    /// This repo's prompts.
    Project,
    /// Every repo's.
    Everywhere,
}

impl SearchScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Project => "this project",
            Self::Everywhere => "everywhere",
        }
    }

    fn other(self) -> Self {
        match self {
            Self::Project => Self::Everywhere,
            Self::Everywhere => Self::Project,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchEvent {
    Moved,
    /// Enter: this prompt goes in the input, unsent, and the search closes.
    Use(String),
    /// Esc: the search closes and the input stays as it was.
    Cancel,
    /// Esc and the next key came together, which terminals report as Alt and the key: the
    /// search closes, and the key goes on as if pressed after Esc.
    CancelThen(KeyEvent),
    Unhandled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Prompt {
    text: String,
    at: Option<u64>,
    /// One row: escapes gone and line breaks folded, which is also what the filter reads.
    line: String,
    /// The length of `line`'s start that is the prompt's first line, all its list row shows,
    /// as in Claude Code, where `Follow-ups:` stands for the list under it.
    first: usize,
}

impl Prompt {
    fn new(entry: PromptEntry) -> Self {
        // Folded as `sanitize_line` folds, keeping where the first line ends.
        let cleaned = sanitize(&entry.text);
        let mut lines = cleaned
            .split('\n')
            .map(str::trim)
            .filter(|line| !line.is_empty());
        let mut line = lines.next().unwrap_or_default().to_owned();
        let first = line.len();
        for rest in lines {
            line.push(' ');
            line.push_str(rest);
        }
        Self {
            text: entry.text,
            at: entry.at,
            line,
            first,
        }
    }

    fn first_line(&self) -> &str {
        &self.line[..self.first]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Shown {
    /// Into the scope's prompts.
    prompt: usize,
    /// Char indices into the prompt's line.
    letters: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistorySearch {
    project: Vec<Prompt>,
    everywhere: Vec<Prompt>,
    scope: SearchScope,
    filter: String,
    /// Top to bottom: oldest first with no filter, else the best match last.
    shown: Vec<Shown>,
    /// Into `shown`.
    selected: usize,
    /// The first row in view.
    top: usize,
    /// Set at open from the larger scope, so the panel keeps its height while you filter.
    rows: usize,
    /// Unix seconds at open, which the ages count back from.
    now: u64,
    /// Opened from vim's NORMAL mode, where Esc i / reaches the `/` commands, which the footer
    /// then says.
    vim: bool,
    typing: Typing,
    /// The selected prompt's preview rows, kept until the selection or the box changes.
    preview: RefCell<Option<Preview>>,
}

/// When keys came, so a paste that a terminal types out key by key can be told from typing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Typing {
    last: Option<Instant>,
    /// Keys in the current fast run.
    run: usize,
    /// Enter until then still belongs to the paste.
    paste_until: Option<Instant>,
}

impl Typing {
    /// True when a plain key at `now` is part of a paste.
    fn key(&mut self, now: Instant) -> bool {
        let fast = self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) <= CHAR_INTERVAL);
        self.last = Some(now);
        self.run = if fast { self.run + 1 } else { 1 };
        let in_paste = self.paste_until.is_some_and(|until| now <= until);
        if self.run >= MIN_CHARS || in_paste {
            self.paste_until = Some(now + ENTER_WINDOW);
        }
        fast || in_paste
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Preview {
    scope: SearchScope,
    prompt: usize,
    width: usize,
    room: usize,
    rows: Vec<String>,
}

impl HistorySearch {
    /// Both lists oldest first. It opens on everywhere, as Claude Code's does.
    pub fn new(project: Vec<PromptEntry>, everywhere: Vec<PromptEntry>, now: u64) -> Self {
        let prompts = |entries: Vec<PromptEntry>| -> Vec<Prompt> {
            entries
                .into_iter()
                .map(Prompt::new)
                .filter(|prompt| !prompt.line.is_empty())
                .collect()
        };
        let project = prompts(project);
        let everywhere = prompts(everywhere);
        let rows = project.len().max(everywhere.len()).clamp(3, LIST_ROWS);
        let mut search = Self {
            project,
            everywhere,
            scope: SearchScope::Everywhere,
            filter: String::new(),
            shown: Vec::new(),
            selected: 0,
            top: 0,
            rows,
            now,
            vim: false,
            typing: Typing::default(),
            preview: RefCell::new(None),
        };
        search.refilter();
        search
    }

    pub fn with_vim(mut self, vim: bool) -> Self {
        self.vim = vim;
        self
    }

    #[cfg(test)]
    pub fn shows_slash_hint(&self) -> bool {
        self.vim
    }

    pub fn scope(&self) -> SearchScope {
        self.scope
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// What the rows show, top to bottom.
    pub fn shown(&self) -> Vec<&str> {
        let prompts = self.prompts();
        self.shown
            .iter()
            .map(|shown| prompts[shown.prompt].text.as_str())
            .collect()
    }

    /// The prompt under the pointer.
    pub fn selected(&self) -> Option<&str> {
        let shown = self.shown.get(self.selected)?;
        Some(self.prompts()[shown.prompt].text.as_str())
    }

    /// A key from the terminal with the time it came. Enter right behind other keys is a line
    /// break in a paste typed out key by key, so it goes in the filter as a space and never
    /// picks a prompt.
    pub fn handle_key_at(&mut self, key: KeyEvent, now: Instant) -> SearchEvent {
        let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        let enter = key.code == KeyCode::Enter && key.modifiers.is_empty();
        if !(enter || plain && matches!(key.code, KeyCode::Char(_))) {
            self.typing = Typing::default();
            return self.handle_key(key);
        }
        if self.typing.key(now) && enter {
            if self.filter.ends_with(char::is_whitespace) {
                return SearchEvent::Moved;
            }
            let filter = format!("{} ", self.filter);
            return self.set_filter(filter);
        }
        self.handle_key(key)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> SearchEvent {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => SearchEvent::Cancel,
            KeyCode::Char('c' | 'g') if ctrl => SearchEvent::Cancel,
            KeyCode::Enter => match self.selected() {
                Some(text) => SearchEvent::Use(text.to_owned()),
                None => SearchEvent::Moved,
            },
            KeyCode::Up => self.step(-1),
            KeyCode::Char('p' | 'r') if ctrl => self.step(-1),
            KeyCode::Down => self.step(1),
            KeyCode::Char('n') if ctrl => self.step(1),
            KeyCode::PageUp => self.step(-(self.rows as isize)),
            KeyCode::PageDown => self.step(self.rows as isize),
            KeyCode::Char('s') if ctrl => {
                self.scope = self.scope.other();
                self.refilter();
                SearchEvent::Moved
            }
            KeyCode::Char('u') if ctrl => self.set_filter(String::new()),
            KeyCode::Char('w') if ctrl => {
                let kept = self.filter.trim_end();
                // After the whole space, which can be more than one byte, as U+3000 is.
                let cut = kept
                    .char_indices()
                    .rev()
                    .find(|(_, c)| c.is_whitespace())
                    .map_or(0, |(at, space)| at + space.len_utf8());
                self.set_filter(kept[..cut].to_owned())
            }
            KeyCode::Backspace => {
                let mut filter = self.filter.clone();
                filter.pop();
                self.set_filter(filter)
            }
            // AltGr arrives as Ctrl+Alt on some systems and types a character.
            KeyCode::Char(c) if ctrl == alt => {
                let filter = format!("{}{c}", self.filter);
                self.set_filter(filter)
            }
            KeyCode::Char(c) if alt => SearchEvent::CancelThen(KeyEvent::new(
                KeyCode::Char(c),
                key.modifiers.difference(KeyModifiers::ALT),
            )),
            _ => SearchEvent::Unhandled,
        }
    }

    /// A paste filters, as typing does, on one line. Terminals send a pasted line break as CR,
    /// which parts words as LF does.
    pub fn handle_paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let pasted = sanitize_line(&text);
        let filter = format!("{}{}", self.filter, pasted.trim_end());
        self.set_filter(filter);
    }

    fn prompts(&self) -> &[Prompt] {
        match self.scope {
            SearchScope::Project => &self.project,
            SearchScope::Everywhere => &self.everywhere,
        }
    }

    fn set_filter(&mut self, filter: String) -> SearchEvent {
        if filter == self.filter {
            return SearchEvent::Moved;
        }
        // Typing on only narrows the matches, since every letter must still be there in order,
        // so only the prompts shown now need matching again.
        let was = self.filter.trim();
        let narrows = !was.is_empty() && filter.trim().starts_with(was);
        let within = narrows.then(|| {
            let mut within: Vec<usize> = self.shown.iter().map(|shown| shown.prompt).collect();
            within.sort_unstable();
            within
        });
        self.filter = filter;
        self.refilter_within(within);
        SearchEvent::Moved
    }

    /// The pointer goes back to the bottom row, the newest or best match.
    fn refilter(&mut self) {
        self.refilter_within(None);
    }

    /// Matches the prompts at `within`, oldest first, or all of them.
    fn refilter_within(&mut self, within: Option<Vec<usize>>) {
        let query = Query::new(self.filter.trim());
        let prompts = self.prompts();
        let candidates = within.unwrap_or_else(|| (0..prompts.len()).collect());
        let mut found: Vec<(i64, Shown)> = candidates
            .into_iter()
            .filter_map(|prompt| {
                let found = query.find(&prompts[prompt].line)?;
                Some((
                    found.score,
                    Shown {
                        prompt,
                        letters: found.positions,
                    },
                ))
            })
            .collect();
        // Stable, so equal scores stay oldest first and the newest of them ends up lowest.
        found.sort_by_key(|(score, _)| *score);
        self.shown = found.into_iter().map(|(_, shown)| shown).collect();
        self.selected = self.shown.len().saturating_sub(1);
        self.top = self.shown.len().saturating_sub(self.rows);
    }

    fn step(&mut self, by: isize) -> SearchEvent {
        let Some(last) = self.shown.len().checked_sub(1) else {
            return SearchEvent::Moved;
        };
        self.selected = self.selected.saturating_add_signed(by).min(last);
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + self.rows {
            self.top = self.selected + 1 - self.rows;
        }
        SearchEvent::Moved
    }

    /// The rows in view for a list `height` rows tall, keeping the pointer in it.
    fn window(&self, height: usize) -> std::ops::Range<usize> {
        let height = height.max(1);
        let top = self
            .top
            .min(self.selected)
            .max((self.selected + 1).saturating_sub(height));
        top..(top + height).min(self.shown.len())
    }
}

/// How long ago, as Claude Code writes it: `37m ago`, `2h ago`, `3d ago`.
pub fn age(at: u64, now: u64) -> String {
    let seconds = now.saturating_sub(at);
    let (amount, unit) = match seconds {
        0 => return "now".to_owned(),
        s if s < 60 => (s, "s"),
        s if s < 3_600 => (s / 60, "m"),
        s if s < 86_400 => (s / 3_600, "h"),
        s if s < 7 * 86_400 => (s / 86_400, "d"),
        s if s < 30 * 86_400 => (s / (7 * 86_400), "w"),
        s if s < 365 * 86_400 => (s / (30 * 86_400), "mo"),
        s => (s / (365 * 86_400), "y"),
    };
    format!("{amount}{unit} ago")
}

#[cfg(test)]
mod tests;
