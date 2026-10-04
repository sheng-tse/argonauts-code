//! One screen session: the loop that feeds the bridge, does what it asks for, and draws.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use argonauts_core::KeyScheme;
use argonauts_core::app::{AppEvent, UiCommand};
use argonauts_core::settings::ui::{ScreenMode, TakeoverWhere};
use crossterm::event::Event;
use futures::StreamExt;
use ratatui::layout::Size;
use tokio::sync::mpsc;
use tokio::time::{Interval, MissedTickBehavior};

use super::AppIo;
use super::editor::edit_in_editor;
use super::gone::{self, TerminalGone};
use super::keys::KeySource;
use super::takeover::{TakeOverPlan, TermEnv, plan_take_over, run_in_tmux, write_spec_files};
use crate::UiOptions;
use crate::app::Ui;
use crate::bottom::{ModeNotice, VimMode};
use crate::bridge::keys::{exit_lines, watch_line};
use crate::bridge::settings::theme_for;
use crate::bridge::{Bridge, Effects, TakeOverJob, TerminalJob};
use crate::cell::{Notice, Tone};
use crate::keymap::Keymap;
use crate::model::{VerbPicker, ViewModel};
use crate::state::TerminalInfo;
use crate::terminal::InlineTerminal;
use crate::terminal::setup::TerminalModes;
use crate::theme::ColorEnv;
use crate::views::crew::CrewView;

/// How long a notice from the loop stays on the mode line.
const NOTICE_FOR: Duration = Duration::from_secs(5);
/// tmux doesn't always pass a resize on, so inside it the size is checked this often.
const SIZE_POLL: Duration = Duration::from_secs(1);
/// With nothing scheduled the loop sleeps until input or an app event.
const IDLE_WAIT: Duration = Duration::from_secs(3600);
/// App events handled before a frame, so a burst costs one frame and keys still get a turn.
const EVENT_BURST: usize = 256;

/// Work that takes the terminal away, done between events.
enum Handover {
    EditDraft,
    TakeOver(Box<TakeOverJob>),
    Run(TerminalJob),
}

/// A take-over in another tmux pane or window came back, or never opened.
struct Returned {
    job: TakeOverJob,
    opened: io::Result<()>,
}

pub(super) struct Screen<W: Write> {
    io: AppIo,
    ui: Ui<W>,
    bridge: Bridge,
    handovers: VecDeque<Handover>,
    returns_tx: mpsc::UnboundedSender<Returned>,
    returns: mpsc::UnboundedReceiver<Returned>,
    /// Set once the screen should close, with the lines to leave in scrollback.
    exit: Option<Vec<String>>,
    /// Only on the process's own terminal, inside tmux.
    size_poll: Option<Interval>,
    /// The `ui.keymap` the keymap was built from.
    keymap_source: Option<BTreeMap<String, String>>,
    /// Only on the process's own terminal.
    stops: Option<Stops>,
    /// Only on the process's own terminal: when to check it's still there.
    gone_check: Option<Interval>,
    /// The terminal went away, so nothing is written to it again.
    gone: bool,
}

impl<W: Write> Screen<W> {
    pub(super) fn new(
        io: AppIo,
        options: &UiOptions,
        terminal: InlineTerminal<W>,
        info: TerminalInfo,
    ) -> Self {
        let env = info
            .color_env
            .clone()
            .unwrap_or_else(ColorEnv::from_process);
        let theme = theme_for(&options.settings, &env, info.background_is_light);
        let mut model = ViewModel {
            verb_picker: VerbPicker::random(),
            ..ViewModel::default()
        };
        if options.keys == KeyScheme::Vim {
            model.bottom.mode.vim = Some(VimMode::Insert);
        }
        let mut bridge = Bridge::new(options);
        bridge.state.terminal = info;
        let (returns_tx, returns) = mpsc::unbounded_channel();
        let mut ui = Ui::new(terminal, theme, model, Instant::now());
        ui.time_keys(true);
        if options.settings.screen == ScreenMode::Fullscreen {
            ui.use_fullscreen();
        }
        let mut screen = Self {
            io,
            ui,
            bridge,
            handovers: VecDeque::new(),
            returns_tx,
            returns,
            exit: None,
            size_poll: None,
            keymap_source: None,
            stops: None,
            gone_check: None,
            gone: false,
        };
        screen.refresh_keymap();
        screen
    }

    /// The process's own terminal: its modes go back on after a child program, stop signals
    /// close the screen, the screen closes when the terminal goes away, and inside tmux its
    /// size is polled.
    pub(super) fn use_real_terminal(&mut self, modes: TerminalModes, stops: Option<Stops>) {
        self.ui.set_terminal_modes(modes);
        self.stops = stops;
        // Only from here: during the start-up probes, a terminal that doesn't answer may
        // rightly take longer than the backstop waits.
        gone::arm_backstop();
        let mut check = tokio::time::interval(gone::CHECK_EVERY);
        check.set_missed_tick_behavior(MissedTickBehavior::Delay);
        self.gone_check = Some(check);
        if TermEnv::from_process().in_tmux {
            let start = tokio::time::Instant::now() + SIZE_POLL;
            let mut poll = tokio::time::interval_at(start, SIZE_POLL);
            poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
            self.size_poll = Some(poll);
        }
    }

    /// Leaves what was live and the exit lines in scrollback, and gives the terminal back
    /// even when the loop failed. The fullscreen app leaves the main screen as it was, with one
    /// line under it only while runs keep going.
    pub(super) async fn run<K: KeySource>(mut self, mut keys: K) -> anyhow::Result<()> {
        let result = self.run_loop(&mut keys).await;
        if self.gone {
            // Restoring modes on a terminal that's gone could only fail, or wait.
            if let Some(modes) = self.ui.take_terminal_modes() {
                modes.abandon();
            }
            return Err(TerminalGone.into());
        }
        let lines = self.exit.take().unwrap_or_default();
        let lines = if self.ui.is_fullscreen() {
            watch_line(&self.bridge.state).into_iter().collect()
        } else {
            lines
        };
        let finished = self.ui.finish_with(lines);
        let restored = self
            .ui
            .take_terminal_modes()
            .map_or(Ok(()), |mut modes| modes.restore());
        result?;
        finished?;
        restored?;
        Ok(())
    }

    async fn run_loop<K: KeySource>(&mut self, keys: &mut K) -> anyhow::Result<()> {
        let mut keys_open = true;
        loop {
            self.run_handovers(keys).await?;
            if self.exit.is_some() {
                return Ok(());
            }
            if let Err(error) = self.ui.frame_in(Instant::now(), &self.bridge.state) {
                self.gone = self.gone_check.is_some();
                return Err(error.into());
            }
            let now = Instant::now();
            let wake = self.ui.next_deadline(now);
            let sleep_until = tokio::time::Instant::from_std(wake.unwrap_or(now + IDLE_WAIT));
            tokio::select! {
                event = self.io.events.recv() => match event {
                    Some(event) => self.on_app_events(event).await,
                    // The controller is gone, so nothing more can happen here.
                    None => {
                        self.exit.get_or_insert_with(Vec::new);
                    }
                },
                key = keys.next(), if keys_open => match key {
                    Some(Ok(event)) => self.on_terminal_event(event).await,
                    Some(Err(error)) => {
                        self.gone = self.gone_check.is_some();
                        return Err(error.into());
                    }
                    // A test's keys can run out while the screen goes on; the terminal's only
                    // end when it goes away.
                    None if self.gone_check.is_some() => {
                        self.gone = true;
                        return Ok(());
                    }
                    None => keys_open = false,
                },
                Some(returned) = self.returns.recv() => self.on_returned(returned).await,
                () = tokio::time::sleep_until(sleep_until), if wake.is_some() => {}
                () = tick(&mut self.size_poll), if self.size_poll.is_some() => self.check_size(),
                () = tick(&mut self.gone_check), if self.gone_check.is_some() => {
                    if gone::terminal_gone() {
                        self.gone = true;
                        return Ok(());
                    }
                }
                () = stopped(&mut self.stops), if self.stops.is_some() => {
                    self.exit = Some(exit_lines(&self.bridge.state));
                }
            }
        }
    }

    async fn on_app_events(&mut self, first: AppEvent) {
        self.on_app_event(first).await;
        for _ in 0..EVENT_BURST {
            // A handover runs before the events after it.
            if self.exit.is_some() || !self.handovers.is_empty() {
                break;
            }
            match self.io.events.try_recv() {
                Ok(event) => self.on_app_event(event).await,
                Err(_) => break,
            }
        }
    }

    async fn on_app_event(&mut self, event: AppEvent) {
        // The view that asked gets its answer first.
        if let AppEvent::View { data } = &event
            && self.ui.offer_view_data(data)
        {
            return;
        }
        let effects = self
            .bridge
            .on_event(self.ui.model_mut(), &event, Instant::now());
        if matches!(event, AppEvent::UiSettings { .. }) {
            self.refresh_keymap();
        }
        self.apply(effects).await;
        if matches!(event, AppEvent::Exit) && self.exit.is_none() {
            self.exit = Some(exit_lines(&self.bridge.state));
        }
    }

    async fn on_terminal_event(&mut self, event: Event) {
        if let Event::Resize(columns, rows) = event {
            // Asking where the cursor went would race the event stream for stdin.
            self.ui.resize(Size::new(columns, rows), None);
            return;
        }
        let now = Instant::now();
        let actions = self.ui.handle_event_in(event, now, &self.bridge.state);
        for action in actions {
            let effects = self.bridge.on_action(self.ui.model_mut(), &action, now);
            self.apply(effects).await;
            if self.exit.is_some() {
                break;
            }
        }
    }

    async fn apply(&mut self, effects: Effects) {
        let Effects {
            commands,
            bell,
            views,
            close_views,
            take_over,
            run_in_terminal,
            clipboard,
            edit_draft,
            theme,
            vim,
            exit,
            exit_lines,
            ..
        } = effects;
        for command in commands {
            self.send(command).await;
        }
        // The bell is a courtesy; a terminal that can't take it fails the next frame anyway.
        if bell && self.bridge.settings().bell {
            let _ = self.ui.terminal_mut().bell();
        }
        if let Some(text) = clipboard {
            copy_to_clipboard(self.ui.terminal_mut(), &text);
        }
        if close_views {
            self.ui.close_views();
        }
        for view in views {
            self.ui.push_view(view);
        }
        if let Some(theme) = theme {
            self.ui.set_theme(theme);
        }
        // Vim keys turned on start in INSERT, as the prompt was being typed in.
        if let Some(on) = vim {
            let mode = &mut self.ui.model_mut().bottom.mode.vim;
            match (on, *mode) {
                (true, None) => *mode = Some(VimMode::Insert),
                (false, Some(_)) => *mode = None,
                _ => {}
            }
        }
        if edit_draft {
            self.handovers.push_back(Handover::EditDraft);
        }
        if let Some(job) = take_over {
            self.handovers.push_back(Handover::TakeOver(Box::new(job)));
        }
        if let Some(job) = run_in_terminal {
            self.handovers.push_back(Handover::Run(job));
        }
        if exit {
            self.exit = Some(exit_lines);
        }
    }

    async fn send(&mut self, command: UiCommand) {
        if self.io.commands.send(command).await.is_err() {
            self.exit.get_or_insert_with(Vec::new);
        }
    }

    async fn run_handovers<K: KeySource>(&mut self, keys: &mut K) -> anyhow::Result<()> {
        while self.exit.is_none() {
            let Some(handover) = self.handovers.pop_front() else {
                break;
            };
            match handover {
                Handover::EditDraft => self.edit_draft(keys).await?,
                Handover::TakeOver(job) => self.take_over(*job, keys).await?,
                Handover::Run(job) => self.run_in_terminal(job, keys).await?,
            }
        }
        Ok(())
    }

    /// Ctrl+G: the draft goes through `$VISUAL` or `$EDITOR` and comes back into the prompt.
    /// Pastes go out expanded, since a label means nothing outside the prompt.
    async fn edit_draft<K: KeySource>(&mut self, keys: &mut K) -> anyhow::Result<()> {
        let sent = self.ui.model().bottom.composer.expanded_text();
        let draft = sent.clone();
        let editing = tokio::task::spawn_blocking(move || edit_in_editor(&draft));
        match self.handed_over(keys, editing).await? {
            Ok(Ok(text)) => self
                .ui
                .model_mut()
                .bottom
                .composer
                .set_edited_text(&sent, &text),
            Ok(Err(error)) => self.notice(error.to_string(), Tone::Warning),
            Err(error) => self.notice(format!("the editor failed: {error}"), Tone::Warning),
        }
        Ok(())
    }

    async fn take_over<K: KeySource>(
        &mut self,
        job: TakeOverJob,
        keys: &mut K,
    ) -> anyhow::Result<()> {
        if let Some(spec) = &job.ticket.command
            && let Err(error) = write_spec_files(spec)
        {
            self.report(
                format!("Couldn't write the session's files: {error}"),
                Tone::Warning,
            );
        }
        match plan_take_over(&job.ticket, job.place) {
            TakeOverPlan::Refused(reason) => {
                self.report(reason, Tone::Warning);
                self.take_over_ended(&job, None).await;
            }
            TakeOverPlan::Here { argv, cwd, notice } => {
                let program = argv.first().cloned().unwrap_or_default();
                let exit_code = match self.handed_over(keys, run_here(argv, cwd)).await? {
                    Ok(status) => status.code(),
                    Err(error) => {
                        self.report(format!("Couldn't start {program}: {error}"), Tone::Error);
                        None
                    }
                };
                if let Some(notice) = notice {
                    self.report(notice, Tone::Dim);
                }
                self.take_over_ended(&job, exit_code).await;
                self.show_what_waits();
            }
            TakeOverPlan::TmuxSplit { argv, sentinel }
            | TakeOverPlan::NewTab { argv, sentinel } => {
                let returns = self.returns_tx.clone();
                tokio::spawn(async move {
                    let opened = run_in_tmux(&argv, &sentinel).await;
                    let _ = returns.send(Returned { job, opened });
                });
                self.report("The session opened in a tmux pane.".to_owned(), Tone::Dim);
            }
        }
        Ok(())
    }

    /// An agent CLI's own command, such as its login, with the screen out of the way.
    async fn run_in_terminal<K: KeySource>(
        &mut self,
        job: TerminalJob,
        keys: &mut K,
    ) -> anyhow::Result<()> {
        let shown = job.argv.join(" ");
        match self.handed_over(keys, run_here(job.argv, job.cwd)).await? {
            Ok(status) if status.success() => {
                self.report(format!("Back from `{shown}`."), Tone::Dim);
            }
            Ok(status) => self.report(format!("`{shown}` ended with {status}."), Tone::Warning),
            Err(error) => {
                self.report(format!("Couldn't start `{shown}`: {error}"), Tone::Error);
            }
        }
        Ok(())
    }

    async fn on_returned(&mut self, returned: Returned) {
        let Returned { mut job, opened } = returned;
        match opened {
            Ok(()) => {
                self.take_over_ended(&job, None).await;
                self.show_what_waits();
            }
            Err(error) => {
                self.report(
                    format!("tmux couldn't open the session ({error}), so it runs here."),
                    Tone::Warning,
                );
                job.place = TakeoverWhere::Here;
                self.handovers.push_back(Handover::TakeOver(Box::new(job)));
            }
        }
    }

    async fn take_over_ended(&mut self, job: &TakeOverJob, exit_code: Option<i32>) {
        self.send(UiCommand::TakeOverEnded {
            run: job.run.clone(),
            card: job.ticket.card.clone(),
            exit_code,
        })
        .await;
    }

    /// Back from a take-over, anything waiting on you shows first: the card here, open, or the
    /// crew view when cards wait elsewhere.
    fn show_what_waits(&mut self) {
        let bottom = &mut self.ui.model_mut().bottom;
        if let Some(card) = bottom.card.as_mut() {
            card.pinned = false;
            self.ui.close_views();
        } else if bottom.mode.needs_you > 0 {
            self.ui.close_views();
            self.ui
                .push_view(Box::new(CrewView::new(&self.bridge.state)));
        }
    }

    /// Runs `child` with the terminal handed over. App events keep being folded in meanwhile
    /// and are drawn once the screen is back.
    async fn handed_over<K: KeySource, F: Future>(
        &mut self,
        keys: &mut K,
        child: F,
    ) -> anyhow::Result<F::Output> {
        keys.pause();
        if let Err(error) = self.ui.suspend() {
            keys.resume();
            return Err(error.into());
        }
        let output = self.while_away(child).await;
        let resumed = self.ui.resume();
        keys.resume();
        resumed?;
        Ok(output)
    }

    async fn while_away<F: Future>(&mut self, child: F) -> F::Output {
        tokio::pin!(child);
        let mut events_open = true;
        loop {
            // Checked first, since the child's Ctrl+C can land in the same wake as its exit.
            tokio::select! {
                biased;
                () = interrupted(&mut self.stops), if self.stops.is_some() => {}
                output = &mut child => return output,
                event = self.io.events.recv(), if events_open => match event {
                    Some(event) => self.on_app_event(event).await,
                    None => events_open = false,
                },
            }
        }
    }

    fn refresh_keymap(&mut self) {
        let wanted = &self.bridge.settings().keymap;
        if self.keymap_source.as_ref() == Some(wanted) {
            return;
        }
        let (keymap, errors) = Keymap::with_overrides(wanted);
        self.keymap_source = Some(wanted.clone());
        self.ui.set_keymap(keymap);
        if let Some(error) = errors.first() {
            self.notice(error.to_string(), Tone::Warning);
        }
    }

    fn check_size(&mut self) {
        if let Ok((columns, rows)) = crossterm::terminal::size() {
            let size = Size::new(columns, rows);
            if size != self.ui.terminal().screen_size() {
                self.ui.resize(size, None);
            }
        }
    }

    /// A line in the transcript, for what should stay on record.
    fn report(&mut self, text: String, tone: Tone) {
        self.ui.model_mut().history.push(Notice::new(tone, text));
    }

    /// A short line at the mode line's right end that goes away by itself.
    fn notice(&mut self, text: String, tone: Tone) {
        self.ui.model_mut().bottom.mode.notice = Some(ModeNotice {
            text,
            tone,
            until: Instant::now() + NOTICE_FOR,
        });
    }
}

/// OSC 52 reaches the clipboard through most terminals, over ssh and tmux too. Terminals that
/// ignore it, such as macOS Terminal, get the system's own copy command as well.
fn copy_to_clipboard<W: Write>(terminal: &mut InlineTerminal<W>, text: &str) {
    let _ = terminal.set_clipboard(text);
    let native: &[&[&str]] = if cfg!(target_os = "macos") {
        &[&["pbcopy"]]
    } else {
        &[&["wl-copy"], &["xclip", "-selection", "clipboard"]]
    };
    for argv in native {
        let Some((program, args)) = argv.split_first() else {
            continue;
        };
        let child = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        let Ok(mut child) = child else {
            continue;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        if child.wait().is_ok_and(|status| status.success()) {
            return;
        }
    }
}

async fn run_here(argv: Vec<String>, cwd: PathBuf) -> io::Result<ExitStatus> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::other("the session has no command"))?;
    tokio::process::Command::new(program)
        .args(args)
        .current_dir(cwd)
        .status()
        .await
}

/// SIGTERM, SIGHUP, and SIGINT close the app the way `/exit` does, so the terminal gets its
/// modes back and, in fullscreen, its main screen. In raw mode Ctrl+C is a key, so these come
/// from elsewhere.
pub(super) struct Stops {
    #[cfg(unix)]
    ends: Vec<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
}

impl Stops {
    #[cfg(unix)]
    pub(super) fn listen() -> Option<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        let ends = [SignalKind::terminate(), SignalKind::hangup()]
            .into_iter()
            .map(signal)
            .collect::<io::Result<_>>()
            .ok()?;
        let interrupt = signal(SignalKind::interrupt()).ok()?;
        Some(Self { ends, interrupt })
    }

    #[cfg(not(unix))]
    pub(super) fn listen() -> Option<Self> {
        None
    }
}

pub(super) async fn stopped(stops: &mut Option<Stops>) {
    #[cfg(unix)]
    if let Some(Stops { ends, interrupt }) = stops {
        let waits = ends
            .iter_mut()
            .chain(std::iter::once(interrupt))
            .map(|signal| Box::pin(signal.recv()));
        futures::future::select_all(waits).await;
        return;
    }
    let _ = stops;
    std::future::pending::<()>().await;
}

/// While a child program has the terminal, Ctrl+C there reaches this process too, since both
/// are in the terminal's foreground group. It was meant for the child, so it stops nothing here.
async fn interrupted(stops: &mut Option<Stops>) {
    #[cfg(unix)]
    if let Some(stops) = stops
        && stops.interrupt.recv().await.is_some()
    {
        return;
    }
    let _ = stops;
    std::future::pending::<()>().await;
}

async fn tick(poll: &mut Option<Interval>) {
    match poll {
        Some(interval) => {
            interval.tick().await;
        }
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::Verbosity;
    use argonauts_core::settings::ui::UiSettings;
    use ratatui::layout::Position;

    use super::*;
    use crate::theme::Brightness;

    #[tokio::test]
    async fn a_settings_change_keeps_the_background_the_terminal_reported() {
        let (commands, _commands) = mpsc::channel(16);
        let (_events, events) = mpsc::channel(16);
        let terminal = InlineTerminal::new(Vec::new(), Size::new(80, 24), Position::new(0, 0));
        // No COLORFGBG, as in macOS Terminal, so only the probe knows the background is light.
        let env = ColorEnv {
            no_color: false,
            colorterm: Some("truecolor".into()),
            term: Some("xterm-256color".into()),
            colorfgbg: None,
        };
        let info = TerminalInfo {
            color_env: Some(env),
            background_is_light: Some(true),
            utc_offset: None,
        };
        let io = AppIo { commands, events };
        let mut screen = Screen::new(io, &UiOptions::default(), terminal, info);
        assert_eq!(screen.ui.theme().brightness, Brightness::Light);

        let settings = AppEvent::UiSettings {
            vim: true,
            verbosity: Verbosity::default(),
            ui: Some(Box::new(UiSettings::default())),
        };
        screen.on_app_event(settings).await;
        assert_eq!(screen.ui.theme().brightness, Brightness::Light);
    }
}
