//! A blocking event loop for hosts that don't bring their own. It sleeps until the next event
//! or deadline, so an idle screen uses no CPU. Hosts with an async runtime drive [`Ui`]
//! directly instead.

use std::io::{self, BufWriter, Stdout, Write};
use std::time::{Duration, Instant};

use crossterm::cursor;
use crossterm::event::{self, Event};
use ratatui::layout::{Position, Size};

use super::{Ui, UiAction};
use crate::model::{VerbPicker, ViewModel};
use crate::terminal::InlineTerminal;
use crate::terminal::setup::{TerminalModes, install_panic_hook, probe_cursor};
use crate::theme::Theme;

/// Flushed once per frame so a frame reaches the terminal in one write.
pub type StdoutWriter = BufWriter<Stdout>;

/// How long the loop sleeps when nothing at all is scheduled. Input wakes it sooner.
const IDLE_WAIT: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
}

pub trait Host<W: Write> {
    /// Called once before the first frame.
    fn on_start(&mut self, _ui: &mut Ui<W>, _now: Instant) {}

    /// The next time the host wants [`on_deadline`](Self::on_deadline), such as a scripted step.
    fn next_deadline(&self) -> Option<Instant> {
        None
    }

    fn on_deadline(&mut self, _ui: &mut Ui<W>, _now: Instant) -> Flow {
        Flow::Continue
    }

    fn on_action(&mut self, ui: &mut Ui<W>, action: UiAction, now: Instant) -> Flow;

    /// Dim lines left in scrollback on exit, such as what keeps going and how to come back.
    fn exit_lines(&self, _ui: &Ui<W>) -> Vec<String> {
        Vec::new()
    }
}

/// Runs `host` on the real terminal until it or the user quits, leaving the transcript in
/// scrollback.
pub fn run<H: Host<StdoutWriter>>(host: &mut H, theme: Theme) -> io::Result<()> {
    let mut modes = TerminalModes::enable()?;
    install_panic_hook();
    let (screen, cursor) = match probe_cursor() {
        Ok(found) => found,
        Err(error) => {
            let _ = modes.restore();
            return Err(error);
        }
    };
    // Without an answer, the whole screen goes up into scrollback, so no shell output is lost.
    let origin = cursor.unwrap_or(Position::new(0, screen.height));
    let terminal = InlineTerminal::new(BufWriter::new(io::stdout()), screen, origin);
    let now = Instant::now();
    let model = ViewModel {
        verb_picker: VerbPicker::random(),
        ..ViewModel::default()
    };
    let mut ui = Ui::new(terminal, theme, model, now);
    ui.set_terminal_modes(modes);
    host.on_start(&mut ui, now);

    let mut looper = Looper {
        host,
        ui: &mut ui,
        // A terminal that didn't answer at startup would stall every resize for two seconds.
        cursor_answers: cursor.is_some(),
    };
    let result = looper.run();
    let lines = host.exit_lines(&ui);
    let finished = ui.finish_with(lines);
    let restored = ui
        .take_terminal_modes()
        .map_or(Ok(()), |mut modes| modes.restore());
    result.and(finished).and(restored)
}

struct Looper<'a, H> {
    host: &'a mut H,
    ui: &'a mut Ui<StdoutWriter>,
    cursor_answers: bool,
}

impl<H: Host<StdoutWriter>> Looper<'_, H> {
    fn run(&mut self) -> io::Result<()> {
        loop {
            let now = Instant::now();
            let wake = [self.ui.next_deadline(now), self.host.next_deadline()]
                .into_iter()
                .flatten()
                .min();
            let wait = wake.map_or(IDLE_WAIT, |at| at.saturating_duration_since(now));
            if event::poll(wait)? {
                // Take every event that is ready before drawing, so a burst costs one frame.
                loop {
                    let event = event::read()?;
                    if self.dispatch(event) == Flow::Quit {
                        return Ok(());
                    }
                    if !event::poll(Duration::ZERO)? {
                        break;
                    }
                }
            }

            let now = Instant::now();
            if self.host.next_deadline().is_some_and(|at| at <= now)
                && self.host.on_deadline(self.ui, now) == Flow::Quit
            {
                return Ok(());
            }
            self.ui.frame(Instant::now())?;
        }
    }

    fn dispatch(&mut self, event: Event) -> Flow {
        if let Event::Resize(columns, rows) = event {
            // Where the cursor went tells how far a rewrapping terminal moved the live region.
            let cursor = if self.cursor_answers {
                cursor::position().ok().map(|(x, y)| Position::new(x, y))
            } else {
                None
            };
            self.ui.resize(Size::new(columns, rows), cursor);
            return Flow::Continue;
        }
        let now = Instant::now();
        for action in self.ui.handle_event(event, now) {
            if self.host.on_action(self.ui, action, now) == Flow::Quit {
                return Flow::Quit;
            }
        }
        Flow::Continue
    }
}
