//! The screen's event loop: app events, terminal events, and frame deadlines in one `select!`.
//! Redraws are requested, never ticked, so an idle screen does no work.

mod editor;
mod gone;
mod keys;
mod screen;
mod take_over;

use std::io::{self, BufWriter, Write};
use std::time::Duration;

use argonauts_core::app::{AppEvent, UiCommand};
use argonauts_core::settings::ui::Logos;
use crossterm::event::Event;
use futures::Stream;
use ratatui::layout::Position;
use tokio::sync::mpsc;

use crate::UiOptions;
use crate::format::WallClock;
use crate::kit::logo;
use crate::mascot::{Graphics, placeholder};
use crate::state::TerminalInfo;
use crate::terminal::InlineTerminal;
use crate::terminal::setup::{
    TerminalModes, install_panic_hook, probe_background, probe_cell_size, probe_cursor,
    probe_graphics, probe_pixel_mouse,
};
use crate::text::set_ambiguous_width;
use crate::theme::ColorEnv;

pub use editor::{EditorError, edit_in_editor, edit_with, editor_command};
pub use gone::TerminalGone;
pub use take_over::{
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
    // `behaviors.view = "world"` asks a terminal its variables don't name whether it takes
    // pictures; one that left the background's question unanswered isn't asked.
    let graphics = Graphics::detect_or_ask(
        |name| std::env::var(name).ok(),
        options.ask_for_pictures && background.is_some(),
        || probe_graphics(BACKGROUND_WAIT),
    );
    // Only a terminal that draws pictures is asked how big its cells are, for the mascot's.
    let cell = graphics.and_then(|_| probe_cell_size(BACKGROUND_WAIT));
    // Only a terminal that takes the kitty protocol's pictures may report the mouse in pixels,
    // and it's asked, since its variables may have come from another terminal. One that left
    // the background's question unanswered isn't asked again.
    let pixel_mouse = matches!(graphics, Some(Graphics::Kitty))
        && background.is_some()
        && probe_pixel_mouse(BACKGROUND_WAIT);
    // Logos are pictures only in a terminal that draws placeholders and says it takes pictures.
    // One that does gets them on each screen before the first frame drawn there, and the palette
    // built next draws them. With `ui.logos = "off"` the terminal isn't asked and nothing goes
    // out, as one that takes the question badly needs; turning logos on later sends them then.
    let takes_logos =
        placeholder::terminal_draws_them(|name| std::env::var(name).ok()) && background.is_some();
    match logos_at_start(options.settings.logos, takes_logos) {
        LogosAtStart::Ask if probe_graphics(BACKGROUND_WAIT) => logo::set_takes_logos(true),
        LogosAtStart::Hold => logo::hold_for_later(),
        _ => {}
    }
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
    screen.use_pictures(graphics, cell);
    screen.use_pixel_mouse(pixel_mouse);
    screen.run(TerminalKeys::new()).await
}

/// What becomes of the logos' pictures as the app starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogosAtStart {
    /// Ask the terminal whether it takes pictures, and give them to it if it does.
    Ask,
    /// `ui.logos` is off: ask nothing and send nothing, until logos are turned on.
    Hold,
    /// The terminal can't draw them.
    Nothing,
}

/// [`LogosAtStart`] for `ui.logos` set to `logos`, in a terminal that `takes` the logos'
/// placeholders and answered the background's question.
fn logos_at_start(logos: Logos, takes: bool) -> LogosAtStart {
    match (logos, takes) {
        (_, false) => LogosAtStart::Nothing,
        (Logos::Auto, true) => LogosAtStart::Ask,
        (Logos::Off, true) => LogosAtStart::Hold,
    }
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
