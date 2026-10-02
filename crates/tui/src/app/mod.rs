//! The host feeds [`Ui`] terminal events and changes the model; the Ui decides what the keys
//! mean and when to draw. A frame prints newly committed history once, then redraws the live
//! region if anything in it changed, at most about 60 times a second and not at all while the
//! screen is still.

mod frames;
mod fullscreen;
mod input;
mod live;
mod overlay;
mod run;
mod views;

#[cfg(test)]
mod tests;

use std::io::{self, Write};
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;

use crate::bottom::ApprovalMode;
use crate::cell::BLINK_PERIOD;
use crate::keymap::{Action, Keymap};
use crate::model::ViewModel;
use crate::render::{Clock, RenderCtx};
use crate::state::AppState;
use crate::terminal::InlineTerminal;
use crate::terminal::setup::{self, TerminalModes};
use crate::terminal::title::{TitleStatus, title_text};
use crate::theme::Theme;
use crate::views::ViewCtx;

pub use frames::{FrameScheduler, MIN_FRAME_INTERVAL};
pub use fullscreen::{Scrollback, select_key};
pub use live::{LiveView, compose_live};
pub use overlay::{OverlayEvent, OverlayView, TranscriptOverlay};
pub use run::{Flow, Host, StdoutWriter, run};

/// Claude Code's title spinner period. Slow, so the title is rewritten rarely.
const TITLE_PERIOD: Duration = Duration::from_millis(960);

/// The title before a session has one.
pub const DEFAULT_TITLE: &str = "Argonauts";

#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    /// The composer is already cleared. `text` has long pastes expanded; `shown` keeps their
    /// labels, such as `[Pasted text #1 +39 lines]`, for the echo in the transcript.
    Submit {
        text: String,
        shown: String,
    },
    /// `note` is the text typed after Tab or into a free-text option; `checked` holds a
    /// checklist option's ticked rows. The card is already gone.
    Answer {
        card: u64,
        option: usize,
        note: Option<String>,
        checked: Vec<usize>,
    },
    /// An option that only shows something, such as Show the diff. The card stays and nothing
    /// is answered.
    OpenView {
        card: u64,
        option: usize,
    },
    /// Esc folded an expanded card into its pinned line.
    SetAside {
        card: u64,
    },
    /// Already shown in the mode line.
    ModeChanged(ApprovalMode),
    /// Esc while work runs. The host decides whether to interrupt a chat turn at once or pause a
    /// run at a safe point.
    Interrupt,
    /// ↑ on the first input row while messages are queued. The host puts them back in the
    /// composer once they are really taken back.
    TakeBack(Vec<String>),
    OpenCrewView,
    OpenAgent(crate::bottom::AgentRef),
    /// A second `x` on an agent's panel row. The first only asked on the mode line.
    StopAgent(crate::bottom::AgentRef),
    /// A second `x` on a run's panel row, with its run id.
    StopRun(String),
    OpenTree,
    /// Enter on a run's panel row, with its run id.
    OpenWorkflow(String),
    /// 1 to 9: that agent's session view.
    Follow(u8),
    /// `/help`.
    OpenHelp,
    /// A command a view sent, such as `Steer` or `StopAgent`.
    Command(argonauts_core::app::UiCommand),
    /// A view asked the controller for data.
    Request(argonauts_core::app::ViewRequest),
    /// Give this card the keys, such as Enter on a ◆ row in the crew view.
    AnswerCard(argonauts_core::app::AnswerTarget),
    /// A key the open settings picker took, for the settings flow to act on.
    Picker(crate::bottom::PickerEvent),
    /// `t` in a view: the local confirm card before a take-over.
    ConfirmTakeOver {
        run: argonauts_core::RunId,
        card: argonauts_core::CardId,
    },
    /// A short line for the mode line's notice slot.
    Notice(String),
    /// Edit the draft in `$EDITOR` through [`Ui::with_restored`].
    EditDraft,
    Focus(bool),
    Quit,
}

impl UiAction {
    /// A submission without pastes, so `shown` equals `text`.
    pub fn submit(text: impl Into<String>) -> Self {
        let text = text.into();
        UiAction::Submit {
            shown: text.clone(),
            text,
        }
    }
}

#[derive(Debug)]
pub struct Ui<W: Write> {
    terminal: InlineTerminal<W>,
    model: ViewModel,
    theme: Theme,
    frames: FrameScheduler,
    started: Instant,
    dirty: bool,
    overlay: Option<TranscriptOverlay>,
    /// The app's own views, the top one drawn. Esc pops one; an empty stack is the chat.
    views: Vec<Box<dyn crate::views::View>>,
    /// Whether the terminal shows the alternate screen now. Frames switch it to match what's
    /// open, so a switch's errors have a place to go.
    on_alternate: bool,
    /// Set when the app has the whole alternate screen (`ui.screen = "fullscreen"`), with the
    /// conversation the inline screen would have printed into the terminal's scrollback.
    scrollback: Option<Scrollback>,
    quit: Option<QuitArm>,
    /// Until when a second Esc clears the input.
    clear_armed: Option<Instant>,
    /// Only set on a real terminal, for handing it to child programs.
    modes: Option<TerminalModes>,
    next_animation: Option<Instant>,
    keymap: Keymap,
}

/// A second press of the same quit key before `until` quits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QuitArm {
    action: Action,
    until: Instant,
}

impl<W: Write> Ui<W> {
    pub fn new(
        terminal: InlineTerminal<W>,
        theme: Theme,
        mut model: ViewModel,
        now: Instant,
    ) -> Self {
        let keymap = Keymap::default();
        model.bottom.menus.shortcuts = keymap.shortcut_hints();
        Self {
            terminal,
            model,
            theme,
            frames: FrameScheduler::new(),
            started: now,
            dirty: true,
            overlay: None,
            views: Vec::new(),
            on_alternate: false,
            scrollback: None,
            quit: None,
            clear_armed: None,
            modes: None,
            next_animation: None,
            keymap,
        }
    }

    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    pub fn set_keymap(&mut self, keymap: Keymap) {
        self.model.bottom.menus.shortcuts = keymap.shortcut_hints();
        self.keymap = keymap;
    }

    pub fn model(&self) -> &ViewModel {
        &self.model
    }

    /// Marks the next frame to redraw.
    pub fn model_mut(&mut self) -> &mut ViewModel {
        self.dirty = true;
        &mut self.model
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.dirty = true;
    }

    pub fn terminal(&self) -> &InlineTerminal<W> {
        &self.terminal
    }

    pub fn terminal_mut(&mut self) -> &mut InlineTerminal<W> {
        &mut self.terminal
    }

    pub fn started(&self) -> Instant {
        self.started
    }

    pub fn push_view(&mut self, view: Box<dyn crate::views::View>) {
        self.views.push(view);
        self.dirty = true;
    }

    pub fn pop_view(&mut self) -> Option<Box<dyn crate::views::View>> {
        self.dirty = true;
        self.views.pop()
    }

    /// Back to the chat exactly where it was.
    pub fn close_views(&mut self) {
        self.dirty = true;
        self.views.clear();
    }

    pub fn top_view_mut(&mut self) -> Option<&mut (dyn crate::views::View + 'static)> {
        self.views.last_mut().map(|view| view.as_mut())
    }

    pub fn views_open(&self) -> usize {
        self.views.len()
    }

    /// Whether the transcript overlay, or a view in its place, is open.
    pub fn is_overlay_open(&self) -> bool {
        self.overlay.is_some()
    }

    /// Opens `cells` full screen, such as the diff behind a card's Show the diff. Esc returns to
    /// the inline chat exactly where it was, card and all.
    pub fn open_view(&mut self, title: impl Into<String>, cells: Vec<crate::cell::Cell>) {
        self.overlay = Some(TranscriptOverlay::view(title, cells));
        self.dirty = true;
    }

    /// Gives the Ui the real terminal's modes, so [`with_restored`](Self::with_restored) can
    /// turn them off and on around a child program.
    pub fn set_terminal_modes(&mut self, modes: TerminalModes) {
        self.modes = Some(modes);
    }

    pub fn take_terminal_modes(&mut self) -> Option<TerminalModes> {
        self.modes.take()
    }

    /// Runs one frame if one is due. Returns whether anything was written. Open views draw
    /// against an empty app state; the runtime uses [`frame_in`](Self::frame_in).
    pub fn frame(&mut self, now: Instant) -> io::Result<bool> {
        self.frame_with(now, None)
    }

    /// Runs one frame, drawing the top view from `state`.
    pub fn frame_in(&mut self, now: Instant, state: &AppState) -> io::Result<bool> {
        self.frame_with(now, Some(state))
    }

    /// Everything a frame writes, title and screen switches included, reaches the terminal as
    /// one synchronized update.
    fn frame_with(&mut self, now: Instant, state: Option<&AppState>) -> io::Result<bool> {
        self.terminal.begin_frame();
        let drawn = self.draw_frame(now, state);
        let sent = self.terminal.end_frame();
        let wrote = drawn?;
        sent?;
        Ok(wrote)
    }

    fn draw_frame(&mut self, now: Instant, state: Option<&AppState>) -> io::Result<bool> {
        // The main screen shows the panic now, and the screen is on its way out.
        if self.scrollback.is_some() && setup::given_back_after_panic() {
            return Ok(false);
        }
        // A panel whose rows went away lets go of the focus, so it can't come back unasked.
        if self.model.bottom.agents.rows.is_empty() {
            self.model.bottom.agents.blur();
        }
        let mut wrote = self.update_title(now)?;
        let wanted = self.wants_alternate_screen();
        if wanted != self.on_alternate {
            if self.scrollback.is_some() {
                // The fullscreen app leaves the alternate screen only to suspend or finish.
                self.terminal.enter_fullscreen()?;
            } else if wanted {
                // History waiting to print belongs to the inline transcript, so it goes first.
                self.draw_inline(now)?;
                self.terminal.enter_alternate_screen()?;
            } else {
                self.terminal.leave_alternate_screen()?;
            }
            self.on_alternate = wanted;
            self.dirty = true;
            wrote = true;
        }
        if now < self.frames.earliest(now) {
            return Ok(wrote);
        }
        let due = self.next_animation.is_some_and(|at| at <= now);

        let before = self.terminal.frame_bytes();
        if self.on_alternate {
            // On the inline screen, history waits while a view is up, and prints when it closes.
            let printed = self.scrollback.is_some() && self.drain_history(now);
            if !self.dirty && !due && !printed {
                return Ok(wrote);
            }
            if self.scrollback.is_some() && self.views.is_empty() && self.overlay.is_none() {
                self.draw_fullscreen(now)?;
            } else {
                self.draw_alternate(now, state)?;
            }
            self.frame_done(now);
            return Ok(wrote || self.terminal.frame_bytes() != before);
        }

        let printed = self.drain_history(now);
        if !self.dirty && !printed && !due && self.terminal.pending_history().is_empty() {
            return Ok(wrote);
        }
        self.draw_inline(now)?;
        self.frame_done(now);
        Ok(wrote || self.terminal.frame_bytes() != before)
    }

    /// The top view if one is open, else the transcript overlay.
    fn draw_alternate(&mut self, now: Instant, state: Option<&AppState>) -> io::Result<()> {
        let screen = self.terminal.screen_size();
        if let Some(view) = self.views.last_mut() {
            let empty;
            let state = match state {
                Some(state) => state,
                None => {
                    empty = AppState::default();
                    &empty
                }
            };
            let ctx = ViewCtx {
                theme: &self.theme,
                state,
                now,
            };
            let area = Rect::new(0, 0, screen.width, screen.height);
            let mut buffer = Buffer::empty(area);
            view.render(area, &mut buffer, &ctx);
            return self.terminal.draw_buffer(&buffer);
        }
        if let Some(overlay) = &self.overlay {
            let ctx = RenderCtx::new(screen.width, &self.theme, Clock::new(self.started, now));
            let lines = overlay.render(&self.model.history, &ctx, screen.height);
            self.terminal.draw_screen(&lines)?;
        }
        Ok(())
    }

    fn wants_alternate_screen(&self) -> bool {
        self.scrollback.is_some() || self.overlay.is_some() || !self.views.is_empty()
    }

    /// When [`frame`](Self::frame) next has work, or `None` while the screen is still.
    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let earliest = self.frames.earliest(now);
        let mut next: Option<Instant> = None;
        let mut sooner = |at: Instant| next = Some(next.map_or(at, |next| next.min(at)));
        if self.dirty
            || self.wants_alternate_screen() != self.on_alternate
            || !self.terminal.pending_history().is_empty()
        {
            sooner(earliest);
        }
        if let Some(at) = self.next_animation {
            sooner(at.max(earliest));
        }
        if self.is_busy() && !self.needs_you() && !self.theme.reduced_motion {
            sooner(Clock::new(self.started, now).next_frame_at(TITLE_PERIOD));
        }
        if let Some(notice) = self
            .model
            .bottom
            .mode
            .notice
            .as_ref()
            .filter(|notice| notice.until > now)
        {
            sooner(notice.until.max(earliest));
        }
        // Expiries that already passed have nothing left to do.
        if let Some(hint) = self
            .model
            .bottom
            .hint
            .as_ref()
            .filter(|hint| hint.until > now)
        {
            sooner(hint.until.max(earliest));
        }
        if let Some(quit) = self.quit.filter(|quit| quit.until > now) {
            sooner(quit.until);
        }
        next
    }

    /// Hands the terminal to a child program such as `$EDITOR`. The caller must stop reading
    /// terminal input while `child` runs, or the reader steals the child's keystrokes.
    pub fn with_restored<R>(&mut self, child: impl FnOnce() -> R) -> io::Result<R> {
        self.suspend()?;
        let result = child();
        self.resume()?;
        Ok(result)
    }

    /// The first half of [`with_restored`](Self::with_restored), for a caller that waits for
    /// its child without blocking: the live region clears and the terminal's modes come off.
    pub fn suspend(&mut self) -> io::Result<()> {
        self.dirty = true;
        // The child gets the main screen. An open view comes back on the next frame.
        if self.on_alternate {
            if self.scrollback.is_some() {
                self.terminal.leave_fullscreen()?;
            } else {
                self.terminal.leave_alternate_screen()?;
            }
            self.on_alternate = false;
        }
        if self.scrollback.is_none() {
            self.terminal.clear_live()?;
        }
        if let Some(modes) = self.modes.as_mut() {
            modes.restore()?;
        }
        Ok(())
    }

    /// Takes the terminal back after [`suspend`](Self::suspend); the next frame repaints.
    pub fn resume(&mut self) -> io::Result<()> {
        self.dirty = true;
        if let Some(modes) = self.modes.as_mut() {
            *modes = TerminalModes::enable()?;
            // Keys the child left unread would otherwise land in the prompt.
            setup::flush_stdin();
            // The child may have printed, or the window may have changed size meanwhile.
            if let Ok((screen, cursor)) = setup::probe() {
                if screen != self.terminal.screen_size() {
                    self.terminal.resize(screen, None)?;
                }
                self.terminal.set_origin(cursor);
            }
        }
        self.terminal.invalidate();
        Ok(())
    }

    pub fn finish(&mut self) -> io::Result<()> {
        self.finish_with(Vec::new())
    }

    /// What was still live prints as it stands, so an agent's block doesn't vanish from the
    /// screen, then `exit_lines` in dim text, and only then does the live region clear. The
    /// fullscreen app leaves the alternate screen instead, and prints only `exit_lines`.
    pub fn finish_with(&mut self, exit_lines: Vec<String>) -> io::Result<()> {
        if self.scrollback.is_some() {
            return self.finish_fullscreen(exit_lines);
        }
        if self.on_alternate {
            self.terminal.leave_alternate_screen()?;
            self.on_alternate = false;
        }
        self.overlay = None;
        self.views.clear();
        self.model.history.settle();
        if !exit_lines.is_empty() {
            self.model
                .history
                .push(crate::cell::ExitNote::new(exit_lines));
        }
        self.drain_history(Instant::now());
        if !self.terminal.pending_history().is_empty() {
            // An empty live region: the history prints where it was.
            self.terminal.draw(0, |_| {})?;
        }
        self.terminal.clear_live()
    }

    fn is_busy(&self) -> bool {
        self.model.is_working() || self.model.bottom.agents.any_working()
    }

    /// A card waits here, even set aside, or somewhere else.
    fn needs_you(&self) -> bool {
        self.model.bottom.card.is_some() || self.model.bottom.mode.needs_you > 0
    }

    fn update_title(&mut self, now: Instant) -> io::Result<bool> {
        let glyphs = &self.theme.glyphs;
        let (status, frame) = if self.needs_you() {
            (TitleStatus::NeedsYou, 0)
        } else if self.is_busy() {
            let frame = if self.theme.reduced_motion {
                0
            } else {
                Clock::new(self.started, now).frame(TITLE_PERIOD, glyphs.title_busy.len().max(1))
            };
            (TitleStatus::Working, frame)
        } else {
            (TitleStatus::Idle, 0)
        };
        let name = match self.model.title.trim() {
            "" => DEFAULT_TITLE,
            name => name,
        };
        let title = title_text(status, name, frame, glyphs);
        if self.terminal.title() == Some(title.as_str()) {
            return Ok(false);
        }
        self.terminal.set_title(&title)?;
        Ok(true)
    }

    fn drain_history(&mut self, now: Instant) -> bool {
        let width = self.terminal.screen_size().width;
        // History prints with a still clock, so its printed form never depends on frame timing.
        let ctx = RenderCtx::new(width, &self.theme, Clock::still(now));
        let rows = self.model.history.drain_printable(&ctx);
        let header = self.model.history.take_header_rows();
        let redrawn = self.model.history.take_header_redrawn();
        if rows.is_empty() && redrawn.is_none() {
            return false;
        }
        if let Some(scrollback) = self.scrollback.as_mut() {
            scrollback.push(rows, header);
            if let Some(lines) = redrawn {
                scrollback.replace_header(lines);
            }
            return true;
        }
        match header {
            Some(header) => self.terminal.insert_marked_history(rows, header),
            None => self.terminal.insert_history(rows),
        }
        if let Some(lines) = redrawn {
            self.terminal.redraw_marked(lines);
        }
        true
    }

    fn draw_inline(&mut self, now: Instant) -> io::Result<()> {
        self.drain_history(now);
        let screen = self.terminal.screen_size();
        let ctx = RenderCtx::new(screen.width, &self.theme, Clock::new(self.started, now));
        let blocks = self.model.history.live_blocks(&ctx);
        // Two rows stay above the live region, so some history always shows.
        let max_height = screen.height.saturating_sub(2);
        let view = compose_live(
            &blocks,
            &self.model.history,
            &self.model.bottom,
            &ctx,
            max_height,
        );
        let (trailing, pinned) = view.lines.split_at(view.trailing.min(view.lines.len()));
        let rows = |lines: &[Line<'_>]| u16::try_from(lines.len()).unwrap_or(u16::MAX);
        self.terminal
            .draw_live(rows(trailing), rows(pinned), |frame| {
                frame.paint_trailing(trailing);
                frame.paint_lines(pinned);
                // The cursor sits in the composer or a card, which are always pinned.
                if let Some((column, row)) = view.cursor
                    && let Some(row) = row.checked_sub(rows(trailing))
                {
                    frame.set_cursor(column, row);
                }
            })
    }

    fn frame_done(&mut self, now: Instant) {
        self.dirty = false;
        self.frames.frame_ran(now);
        self.next_animation = self.animation_deadline(now);
    }

    /// With reduced motion only elapsed seconds and expiries count.
    fn animation_deadline(&self, now: Instant) -> Option<Instant> {
        let screen = self.terminal.screen_size();
        let clock = Clock::new(self.started, now);
        let ctx = RenderCtx::new(screen.width, &self.theme, clock);
        let mut next = if let Some(view) = self.views.last() {
            view.next_change(now)
        } else if self.overlay.is_some() {
            None
        } else {
            self.model.bottom.next_change(&ctx)
        };
        if self.model.history.is_animating() {
            // A running call's bullet blinks on this beat; held still, only seconds tick.
            let period = if self.theme.reduced_motion {
                Duration::from_secs(1)
            } else {
                BLINK_PERIOD
            };
            let beat = clock.next_frame_at(period);
            next = Some(next.map_or(beat, |at| at.min(beat)));
        }
        next
    }
}
