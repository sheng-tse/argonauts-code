//! One screen session: the loop that feeds the bridge, does what it asks for, and draws.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::io::{self, Write};
use std::process::ExitStatus;
use std::task::Poll;
use std::time::{Duration, Instant};

use argonauts_core::KeyScheme;
use argonauts_core::app::{AppEvent, UiCommand};
use argonauts_core::settings::ui::{ScreenMode, TakeOverWhere};
use crossterm::event::Event;
use futures::StreamExt;
use ratatui::layout::Size;
use tokio::sync::{mpsc, watch};
use tokio::time::{Interval, MissedTickBehavior};

use super::AppIo;
use super::editor::edit_in_editor;
use super::gone::{self, TerminalGone};
use super::keys::KeySource;
use super::take_over::{TakeOverPlan, TermEnv, plan_take_over, run_in_tmux, write_spec_files};
use crate::UiOptions;
use crate::app::Ui;
use crate::app::model::{VerbPicker, ViewModel};
use crate::base::input::keymap::Keymap;
use crate::base::mascot::Graphics;
use crate::base::theme::ColorEnv;
use crate::bottom::{ModeNotice, VimMode};
use crate::bridge::keys::{exit_lines, watch_line};
use crate::bridge::settings::theme_for;
use crate::bridge::{Bridge, Effects, TakeOverJob, TerminalJob};
use crate::cells::cell::{Cell, Notice, Tone};
use crate::frame::state::TerminalInfo;
use crate::terminal::InlineTerminal;
use crate::terminal::setup::TerminalModes;
use crate::views::crew::AgentPanelView;
use crate::world::deliver::worker::arrivals;

/// How long a notice from the loop stays on the bottom line.
const NOTICE_FOR: Duration = Duration::from_secs(5);
/// tmux doesn't always pass a resize on, so inside it the size is checked this often.
const SIZE_POLL: Duration = Duration::from_secs(1);
/// With nothing scheduled the loop sleeps until input or an app event.
const IDLE_WAIT: Duration = Duration::from_secs(3600);
/// App events, or keys, handled before a frame, so a burst costs one frame and the other side
/// still gets a turn.
const EVENT_BURST: usize = 256;

/// How long after a child program gives the terminal back a Ctrl+C still counts as the
/// child's. Linux tells of the child's exit apart from signals, so the Ctrl+C that ended it
/// can arrive here after its exit was seen.
const CHILDS_LATE_INTERRUPT: Duration = Duration::from_secs(1);

/// How long the first frame waits for the header, so the input never shows alone. The header
/// needs only the settings and a few files, a few milliseconds.
pub(super) const HEADER_WAIT: Duration = Duration::from_millis(250);

/// Work that takes the terminal away, done between events.
enum Handover {
    EditDraft,
    Suspend,
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
    /// The streams the check watches, as they were when the screen took the terminal.
    terminals: gone::Terminals,
    /// The terminal went away, so nothing is written to it again.
    gone: bool,
    /// Changes as each picture drawn or packed off the frame arrives, so the frame that shows it
    /// is drawn then, and none before.
    arrived: watch::Receiver<u64>,
    /// The controller sends a header before the first frame, unless the greeting stands on top.
    header_due: bool,
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
        crate::bridge::settings::habits(&options.settings, &mut model);
        model.bottom.composer.workflow_word_off = !options.ultracode_trigger;
        let mut bridge = Bridge::new(options);
        bridge.state.terminal = info;
        let (returns_tx, returns) = mpsc::unbounded_channel();
        if let Some(mut greeting) = options.greeting.clone() {
            // The screen places the argonaut's picture over its cells where it draws pictures.
            greeting.picture = true;
            model.history.push(Cell::Custom(Box::new(greeting)));
        }
        let mut ui = Ui::new(terminal, theme, model, Instant::now());
        ui.time_keys(true);
        ui.set_double_press(options.double_press);
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
            terminals: gone::Terminals::default(),
            gone: false,
            arrived: arrivals(),
            header_due: options.greeting.is_none(),
        };
        screen.refresh_keymap();
        screen
    }

    /// The terminal draws pictures through `graphics`, at `cell` pixels a cell.
    pub(super) fn use_pictures(&mut self, graphics: Option<Graphics>, cell: Option<(u16, u16)>) {
        self.ui.use_pictures(graphics, cell);
        let under_text = graphics == Some(Graphics::Kitty)
            && cell.is_some()
            && crate::council::draw::place::under_text(|name| std::env::var(name).ok());
        self.ui.draw_pictures_under_text(under_text);
        self.bridge.council.under_text = under_text;
    }

    /// The terminal said it reports the mouse in pixels when asked.
    pub(super) fn use_pixel_mouse(&mut self, yes: bool) {
        self.ui.use_pixel_mouse(yes);
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
        self.terminals = gone::Terminals::now();
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
        self.wait_for_the_header().await;
        loop {
            self.run_handovers(keys).await?;
            if self.exit.is_some() {
                return Ok(());
            }
            // What the views that just opened ask for, such as the agent panel's rows.
            for view in self.ui.take_asked() {
                if !self.bridge.state.replaying() {
                    self.send(UiCommand::Request { view }).await;
                }
            }
            if let Err(error) = self.ui.frame_in(Instant::now(), &self.bridge.state) {
                self.gone = self.gone_check.is_some();
                return Err(error.into());
            }
            let now = Instant::now();
            // Whoever draws usage numbers gets them again as they age.
            let watch = self.ui.model().bottom.band.watch.clone();
            if !self.bridge.state.replaying()
                && let Some(view) = watch.due(now)
            {
                self.send(UiCommand::Request { view }).await;
            }
            let wake = [self.ui.next_deadline(now), watch.next(now)]
                .into_iter()
                .flatten()
                .min();
            let sleep_until = tokio::time::Instant::from_std(wake.unwrap_or(now + IDLE_WAIT));
            tokio::select! {
                event = self.io.events.recv() => match event {
                    Some(event) => {
                        self.on_app_events(event).await;
                    }
                    // The controller is gone, so nothing more can happen here.
                    None => {
                        self.exit.get_or_insert_with(Vec::new);
                    }
                },
                key = keys.next(), if keys_open => {
                    // Keys the terminal already holds came in the same read as this one, as a
                    // paste typed out as keys does. They're all handled before the next frame
                    // and as having come when it did, so drawing between them can't space
                    // them out like typing.
                    let arrived = Instant::now();
                    let mut key = key;
                    let mut taken = 0;
                    loop {
                        match key {
                            Some(Ok(event)) => self.on_terminal_event(event, arrived).await,
                            Some(Err(error)) => {
                                self.gone = self.gone_check.is_some();
                                return Err(error.into());
                            }
                            // A test's keys can run out while the screen goes on; the
                            // terminal's only end when it goes away.
                            None if self.gone_check.is_some() => {
                                self.gone = true;
                                return Ok(());
                            }
                            None => {
                                keys_open = false;
                                break;
                            }
                        }
                        taken += 1;
                        // A handover runs before the keys after it.
                        if taken == EVENT_BURST || self.exit.is_some() || !self.handovers.is_empty()
                        {
                            break;
                        }
                        match futures::poll!(keys.next()) {
                            Poll::Ready(next) => key = next,
                            Poll::Pending => break,
                        }
                    }
                }
                Some(returned) = self.returns.recv() => self.on_returned(returned).await,
                Ok(()) = self.arrived.changed() => self.ui.pictures_arrived(),
                () = tokio::time::sleep_until(sleep_until), if wake.is_some() => {}
                () = tick(&mut self.size_poll), if self.size_poll.is_some() => self.check_size(),
                () = tick(&mut self.gone_check), if self.gone_check.is_some() => {
                    if self.terminals.gone() {
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

    /// Holds the first frame until the header and what came with it are in, at most
    /// [`HEADER_WAIT`], so the header, the notices and the input show together.
    async fn wait_for_the_header(&mut self) {
        if !std::mem::replace(&mut self.header_due, false) {
            return;
        }
        let until = tokio::time::Instant::now() + HEADER_WAIT;
        loop {
            match tokio::time::timeout_at(until, self.io.events.recv()).await {
                Ok(Some(event)) => {
                    let header = self.on_app_events(event).await;
                    if header || self.exit.is_some() || !self.handovers.is_empty() {
                        return;
                    }
                }
                Ok(None) => {
                    self.exit.get_or_insert_with(Vec::new);
                    return;
                }
                Err(_) => return,
            }
        }
    }

    /// Handles `first` and the events queued behind it, and says whether a header was among
    /// them.
    async fn on_app_events(&mut self, first: AppEvent) -> bool {
        let mut header = matches!(first, AppEvent::Welcome { .. });
        self.on_app_event(first).await;
        for _ in 0..EVENT_BURST {
            // A handover runs before the events after it.
            if self.exit.is_some() || !self.handovers.is_empty() {
                break;
            }
            match self.io.events.try_recv() {
                Ok(event) => {
                    header |= matches!(event, AppEvent::Welcome { .. });
                    self.on_app_event(event).await;
                }
                Err(_) => break,
            }
        }
        header
    }

    async fn on_app_event(&mut self, event: AppEvent) {
        // The view that asked gets its answer first.
        if let AppEvent::View { data } = &event
            && self.ui.offer_view_data(data)
        {
            return;
        }
        let now = Instant::now();
        let effects = self.bridge.on_event(self.ui.model_mut(), &event, now);
        if matches!(event, AppEvent::UiSettings { .. }) {
            self.refresh_keymap();
        }
        self.apply(effects).await;
        if !self.bridge.state.replaying() {
            for action in self.ui.follow_views(now, &self.bridge.state) {
                let effects = self.bridge.on_action(self.ui.model_mut(), &action, now);
                self.apply(effects).await;
            }
        }
        if matches!(event, AppEvent::Exit) && self.exit.is_none() {
            self.exit = Some(exit_lines(&self.bridge.state));
        }
    }

    /// `arrived` is when the terminal handed the event over, which the input's paste detection
    /// measures by.
    async fn on_terminal_event(&mut self, event: Event, arrived: Instant) {
        if let Event::Resize(columns, rows) = event {
            // Asking where the cursor went would race the event stream for stdin.
            self.ui.resize(Size::new(columns, rows), None);
            return;
        }
        let actions = self.ui.handle_event_in(event, arrived, &self.bridge.state);
        let now = Instant::now();
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
            typed,
            replies,
            bell,
            views,
            views_as_panel,
            close_views,
            views_make_way,
            take_over,
            run_in_terminal,
            clipboard,
            open_link,
            edit_draft,
            suspend,
            theme,
            vim,
            exit,
            exit_lines,
            ..
        } = effects;
        if let Some(text) = typed {
            self.send(UiCommand::CommandTyped { text }).await;
        }
        for command in commands {
            self.send(command).await;
        }
        for reply in replies {
            self.send(UiCommand::CommandReplied { reply }).await;
        }
        // The bell is a courtesy; a terminal that can't take it fails the next frame anyway.
        if bell && self.bridge.settings().bell {
            let _ = self.ui.terminal_mut().bell();
        }
        if let Some(text) = clipboard {
            copy_to_clipboard(self.ui.terminal_mut(), &text);
        }
        if let Some(url) = open_link
            && !open_in_browser(&url)
        {
            // Claude Code's words for it, with where the copy key is here.
            let text = "Couldn't open a browser. /artifacts, then c, copies the link".to_owned();
            self.notice(text, Tone::Warning);
        }
        if close_views {
            self.ui.close_views();
        } else if views_make_way {
            self.ui.make_way_for_question();
        }
        for view in views {
            if views_as_panel {
                self.ui.push_panel(view);
            } else {
                self.ui.push_view(view);
            }
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
        if suspend {
            self.handovers.push_back(Handover::Suspend);
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
                Handover::Suspend => self.suspend(keys).await?,
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

    /// Ctrl+Z: the terminal goes back to the shell and the app stops until `fg`, as Claude
    /// Code does. Agents keep running, but nothing reads their output meanwhile, so a long stop
    /// can hold them up. Coming back draws the screen again, logos included.
    async fn suspend<K: KeySource>(&mut self, keys: &mut K) -> anyhow::Result<()> {
        if let Err(error) = self
            .handed_over(keys, async { stop_until_continued() })
            .await?
        {
            self.notice(format!("Couldn't suspend: {error}"), Tone::Warning);
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
                let here = TerminalJob {
                    argv,
                    cwd,
                    env_remove: Vec::new(),
                    env_set: Vec::new(),
                    quiet: false,
                };
                let exit_code = match self.handed_over(keys, run_here(here)).await? {
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
        let argv = job.argv.clone();
        let quiet = job.quiet;
        let ran = self.handed_over(keys, run_here(job)).await?;
        self.send(UiCommand::TerminalReturned { argv }).await;
        match ran {
            Ok(status) if status.success() && quiet => {}
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
                job.place = TakeOverWhere::Here;
                self.handovers.push_back(Handover::TakeOver(Box::new(job)));
            }
        }
    }

    async fn take_over_ended(&mut self, job: &TakeOverJob, exit_code: Option<i32>) {
        self.send(UiCommand::TakeOverEnded {
            run: job.run.clone(),
            task: job.ticket.task.clone(),
            exit_code,
        })
        .await;
    }

    /// Back from a take-over, anything waiting on you shows first: the question here, open, or the
    /// agent panel when questions wait elsewhere.
    fn show_what_waits(&mut self) {
        let bottom = &mut self.ui.model_mut().bottom;
        if bottom.question.is_some() {
            self.ui.close_views();
        } else if bottom.mode.needs_you > 0 {
            self.ui.close_views();
            self.ui
                .push_view(Box::new(AgentPanelView::new(&self.bridge.state)));
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
                output = &mut child => {
                    if let Some(stops) = self.stops.as_mut() {
                        stops.mark_returned();
                    }
                    return output;
                }
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

    /// A short line at the bottom line's right end that goes away by itself.
    fn notice(&mut self, text: String, tone: Tone) {
        self.ui.model_mut().bottom.mode.notice = Some(ModeNotice {
            text,
            tone,
            until: Instant::now() + NOTICE_FOR,
        });
    }
}

/// OSC 52 reaches the clipboard through most terminals, over ssh and tmux too. Terminals that
/// ignore it, such as macOS Terminal, get the system's clipboard command as well.
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

/// Hands a claude.ai page to the system's opener, which picks the browser. The address is one
/// argument of its own and never a shell's, and anything but a claude.ai link is left alone.
/// False when there was nothing to hand it to.
fn open_in_browser(url: &str) -> bool {
    if !argonauts_core::app::artifacts::is_link(url) {
        return false;
    }
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

async fn run_here(job: TerminalJob) -> io::Result<ExitStatus> {
    let (program, args) = job
        .argv
        .split_first()
        .ok_or_else(|| io::Error::other("the session has no command"))?;
    let mut command = tokio::process::Command::new(program);
    command.args(args).current_dir(&job.cwd);
    for name in &job.env_remove {
        command.env_remove(name);
    }
    command.envs(job.env_set.iter().map(|(name, value)| (name, value)));
    command.status().await
}

/// SIGTERM, SIGHUP, and SIGINT close the app the way `/exit` does, so the terminal gets its
/// modes back and, in fullscreen, its main screen. In raw mode Ctrl+C is a key, so these come
/// from elsewhere, or from a child program's Ctrl+C, which closes nothing.
pub(super) struct Stops {
    #[cfg(unix)]
    ends: Vec<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    /// When a child program last gave the terminal back.
    returned: Option<Instant>,
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
        Some(Self {
            ends,
            interrupt,
            returned: None,
        })
    }

    #[cfg(not(unix))]
    pub(super) fn listen() -> Option<Self> {
        None
    }

    fn mark_returned(&mut self) {
        self.returned = Some(Instant::now());
    }
}

pub(super) async fn stopped(stops: &mut Option<Stops>) {
    #[cfg(unix)]
    if let Some(Stops {
        ends,
        interrupt,
        returned,
    }) = stops
    {
        let ends = ends.iter_mut().map(|signal| Box::pin(signal.recv()));
        let mut ended = futures::future::select_all(ends);
        loop {
            tokio::select! {
                _ = &mut ended => return,
                got = interrupt.recv() => {
                    let childs = returned.is_some_and(|at| at.elapsed() < CHILDS_LATE_INTERRUPT);
                    if got.is_none() || !childs {
                        return;
                    }
                }
            }
        }
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

/// Says how to come back, then stops this process, so the agents it started, each in a group
/// of its own, keep going. Returns once the shell's `fg` continues it. Where no shell keeps
/// jobs, the stop is dropped and this returns at once.
fn stop_until_continued() -> io::Result<()> {
    use nix::sys::signal::{Signal, killpg, raise};
    use nix::unistd::{getpgrp, getpid};

    let mut out = io::stdout();
    write!(
        out,
        "\x1b[2mArgonauts Code is suspended. Run fg to bring it back.\x1b[22m\r\n"
    )?;
    out.flush()?;
    // Under a parent that waits on it, as npm's launcher does, the shell's job is the
    // parent's group, and the shell takes the terminal back only once the whole group stops,
    // so this stops the group, as the terminal's own Ctrl+Z would.
    let group = getpgrp();
    let stopped = if group == getpid() {
        raise(Signal::SIGTSTP)
    } else {
        killpg(group, Signal::SIGTSTP)
    };
    stopped.map_err(io::Error::from)
}

#[cfg(test)]
mod tests {
    use argonauts_core::Verbosity;
    use argonauts_core::settings::ui::UiSettings;
    use ratatui::layout::Position;

    use super::*;
    use crate::base::theme::Brightness;

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
