//! Terminals without bracketed paste deliver a paste as a fast run of keys. Following Codex's
//! `paste_burst.rs` (Apache-2.0), plain characters closer together than anyone types become
//! one paste, Enter inside the run is a newline rather than a send, and the run goes in as a
//! paste once the keys stop.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{Composer, ComposerEvent, VimMode};

/// Keys closer than this come from a paste; a person's fastest key repeat is several times
/// slower.
pub(crate) const CHAR_INTERVAL: Duration = Duration::from_millis(8);
/// The run's third fast character starts a burst.
pub(crate) const MIN_CHARS: usize = 3;
/// Some terminals hand a paste over in chunks, so Enter this soon after a burst still belongs
/// to it.
pub(crate) const ENTER_WINDOW: Duration = Duration::from_millis(120);

#[derive(Debug, Clone, Default)]
pub(super) struct PasteBurst {
    /// Held back from the draft until the burst ends.
    buffer: String,
    active: bool,
    last_char: Option<Instant>,
    /// Characters in the current fast run.
    run: usize,
    /// The run's characters that went into the draft as typed, and the cursor after them, so
    /// a burst that starts late can take them back.
    typed: String,
    typed_end: usize,
    enter_until: Option<Instant>,
}

enum Step {
    Type,
    Begin,
    Buffer,
}

impl PasteBurst {
    fn on_char(&mut self, now: Instant) -> Step {
        let fast = self.is_fast(now);
        self.last_char = Some(now);
        self.run = if fast { self.run + 1 } else { 1 };
        if !fast {
            self.typed.clear();
        }
        if self.active {
            self.enter_until = Some(now + ENTER_WINDOW);
            Step::Buffer
        } else if self.run >= MIN_CHARS {
            Step::Begin
        } else {
            Step::Type
        }
    }

    fn is_fast(&self, now: Instant) -> bool {
        self.last_char
            .is_some_and(|last| now.saturating_duration_since(last) <= CHAR_INTERVAL)
    }

    fn enter_joins(&self, now: Instant) -> bool {
        self.active || self.is_fast(now) || self.enter_until.is_some_and(|until| now <= until)
    }

    fn start(&mut self, buffer: String, now: Instant) {
        self.buffer = buffer;
        self.active = true;
        self.typed.clear();
        self.enter_until = Some(now + ENTER_WINDOW);
    }

    /// The buffer, once no key has come for longer than a paste's gap.
    fn take_if_idle(&mut self, now: Instant) -> Option<String> {
        if self.active && !self.is_fast(now) {
            return self.take();
        }
        None
    }

    fn take(&mut self) -> Option<String> {
        if !self.active {
            return None;
        }
        self.active = false;
        self.run = 0;
        Some(std::mem::take(&mut self.buffer))
    }

    /// Text of a paste is held back, not yet in the draft.
    pub(super) fn holds(&self) -> bool {
        self.active
    }

    /// Any other key ends the run of fast characters.
    pub(super) fn end_run(&mut self) {
        self.run = 0;
        self.typed.clear();
        self.last_char = None;
    }

    fn due(&self) -> Option<Instant> {
        let last = self.last_char.filter(|_| self.active)?;
        Some(last + CHAR_INTERVAL + Duration::from_millis(1))
    }
}

impl Composer {
    /// A key from the terminal with the time it arrived, so a paste typed out by a terminal
    /// without bracketed paste arrives as one paste. Call
    /// [`flush_paste_burst`](Self::flush_paste_burst) at
    /// [`paste_burst_due`](Self::paste_burst_due).
    pub fn handle_key_at(&mut self, key: KeyEvent, now: Instant) -> ComposerEvent {
        let flushed = self.flush_paste_burst(now);
        let event = match self.burst_key(key, now) {
            Some(event) => event,
            None => self.handle_key(key),
        };
        if flushed { event.or_edited() } else { event }
    }

    /// Puts a burst in as a paste once its keys have stopped. True when the text changed.
    pub fn flush_paste_burst(&mut self, now: Instant) -> bool {
        match self.burst.take_if_idle(now) {
            Some(text) => {
                self.insert_paste(&text);
                true
            }
            None => false,
        }
    }

    /// Puts a held burst in now, before a key that can't be part of it. True when the text
    /// changed.
    pub fn end_paste_burst(&mut self) -> bool {
        let flushed = self.flush_burst();
        self.burst.end_run();
        flushed
    }

    /// When a held burst should go in, if one is held.
    pub fn paste_burst_due(&self) -> Option<Instant> {
        self.burst.due()
    }

    pub(super) fn flush_burst(&mut self) -> bool {
        match self.burst.take() {
            Some(text) => {
                self.insert_paste(&text);
                true
            }
            None => false,
        }
    }

    /// Only text typed into the draft can be a paste: vim's NORMAL letters are commands.
    fn burst_key(&mut self, key: KeyEvent, now: Instant) -> Option<ComposerEvent> {
        let typing = !matches!(self.vim, Some(VimMode::Normal | VimMode::Visual));
        if !typing {
            return None;
        }
        let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        // `!` into an empty input switches to shell mode, which typing it as text would skip.
        let enters_shell = !self.burst.active && !self.shell && self.is_empty();
        match key.code {
            KeyCode::Char('!') if plain && enters_shell => None,
            KeyCode::Char(c) if plain => Some(match self.burst.on_char(now) {
                Step::Buffer => {
                    self.burst.buffer.push(c);
                    ComposerEvent::Edited
                }
                Step::Begin => {
                    self.begin_burst(c, now);
                    ComposerEvent::Edited
                }
                Step::Type => {
                    let event = self.insert(c.encode_utf8(&mut [0; 4]));
                    self.burst.typed.push(c);
                    self.burst.typed_end = self.draft.cursor;
                    event
                }
            }),
            KeyCode::Enter if key.modifiers.is_empty() && self.burst.enter_joins(now) => {
                if self.burst.active {
                    self.burst.buffer.push('\n');
                } else {
                    self.begin_burst('\n', now);
                }
                Some(ComposerEvent::Edited)
            }
            _ => None,
        }
    }

    /// The characters of the run already typed into the draft come back out to start the
    /// burst, so the paste goes in whole.
    fn begin_burst(&mut self, c: char, now: Instant) {
        let typed = std::mem::take(&mut self.burst.typed);
        let end = self.burst.typed_end;
        let mut buffer = String::new();
        let start = end.checked_sub(typed.len());
        if let Some(start) = start
            && !typed.is_empty()
            && end == self.draft.cursor
            && self.draft.text.get(start..end) == Some(typed.as_str())
            && self.draft.is_stop(start)
        {
            self.draft.delete(start..end);
            buffer = typed;
        }
        buffer.push(c);
        self.burst.start(buffer, now);
    }
}
