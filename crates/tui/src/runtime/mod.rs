//! The screen's event loop: app events, terminal events, and frame deadlines in one `select!`.
//! Redraws are requested, never ticked, so an idle screen does no work.

mod editor;
mod keys;
mod screen;
mod takeover;

use std::io::{self, BufWriter, Write};
use std::time::Duration;

use argonauts_core::app::{AppEvent, UiCommand};
use crossterm::event::Event;
use futures::Stream;
use ratatui::layout::Position;
use tokio::sync::mpsc;

use crate::UiOptions;
use crate::format::WallClock;
use crate::state::TerminalInfo;
use crate::terminal::InlineTerminal;
use crate::terminal::setup::{TerminalModes, install_panic_hook, probe_background, probe_cursor};
use crate::text::set_ambiguous_width;
use crate::theme::ColorEnv;

pub use editor::{EditorError, edit_in_editor, edit_with, editor_command};
pub use takeover::{
    ENDPOINT_REFUSAL, TakeOverPlan, TermEnv, plan_take_over, plan_take_over_in, run_in_tmux,
    write_spec_files,
};

use keys::{GivenKeys, TerminalKeys};
use screen::{Screen, Stops, stopped};

/// How long the terminal gets to say its background color at start.
const BACKGROUND_WAIT: Duration = Duration::from_millis(250);

pub struct AppIo {
    pub commands: mpsc::Sender<UiCommand>,
    pub events: mpsc::Receiver<AppEvent>,
}

/// The screen laid out like Claude Code's: history under the header at the top, scrolling up
/// into scrollback, and the live region pinned to the bottom rows.
/// Returns after `/exit`, a second Ctrl+C or Ctrl+D, or `AppEvent::Exit`.
pub async fn run_app(io: AppIo, options: UiOptions) -> anyhow::Result<()> {
    if options.screen_reader {
        return crate::a11y::run_screen_reader(io, options).await;
    }
    // Listening before raw mode goes on means a stop signal during the probes below waits for
    // the screen, which gives the terminal back, instead of ending the app in raw mode.
    let stops = Stops::listen();
    let mut modes = TerminalModes::enable()?;
    install_panic_hook();
    let background = probe_background(BACKGROUND_WAIT);
    let (screen, cursor) = match probe_cursor() {
        Ok(found) => found,
        Err(error) => {
            let _ = modes.restore();
            return Err(error.into());
        }
    };
    // Without an answer, the whole screen goes up into scrollback, so no shell output is lost.
    let origin = cursor.unwrap_or(Position::new(0, screen.height));
    let terminal = InlineTerminal::new(BufWriter::new(io::stdout()), screen, origin);
    set_ambiguous_width(options.settings.ambiguous_width);
    let mut screen = Screen::new(io, &options, terminal, terminal_info(background));
    screen.use_real_terminal(modes, stops);
    screen.run(TerminalKeys::new()).await
}

/// The next Ctrl+C, terminate, or hangup, for what the app still waits on after [`run_app`]
/// closed, such as the controller's last call. The screen's listening took over what these
/// signals do, so without this one they would do nothing until the app exits.
pub async fn stop_signal() {
    stopped(&mut Stops::listen()).await;
}

/// [`run_app`] on any terminal and any source of terminal events, such as a `vt100` parser and
/// a test's script. Nothing here touches the process's own terminal.
pub async fn run_with<W: Write>(
    io: AppIo,
    options: UiOptions,
    terminal: InlineTerminal<W>,
    keys: impl Stream<Item = io::Result<Event>>,
) -> anyhow::Result<()> {
    set_ambiguous_width(options.settings.ambiguous_width);
    Screen::new(io, &options, terminal, terminal_info(None))
        .run(GivenKeys::new(keys))
        .await
}

/// What the terminal and the machine said at start. Every palette is built from it, so a
/// settings change later keeps the background the terminal reported.
fn terminal_info(background_is_light: Option<bool>) -> TerminalInfo {
    TerminalInfo {
        color_env: Some(ColorEnv::from_process()),
        background_is_light,
        utc_offset: Some(WallClock::local().now().offset()),
    }
}

#[cfg(test)]
mod tests;
