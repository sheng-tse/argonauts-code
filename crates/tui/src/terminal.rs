//! Runs the live main screen on the real terminal.

use std::io::{self, IsTerminal, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::Context;
use crossterm::event::{self, Event};
use ratatui::Terminal;
use ratatui::backend::Backend;
use time::OffsetDateTime;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

use crate::UiOptions;
use crate::app::App;
use crate::channel::{UiCommand, UiEvent};
use crate::view::{self, ScrollLimits};

const FRAME: Duration = Duration::from_millis(100);
const FALLBACK_WIDTH: usize = 100;
const INPUT_POLL: Duration = Duration::from_millis(50);
/// Most events folded in before the next redraw, so a fast stream can't starve the keyboard.
const EVENT_BATCH: usize = 512;

/// Shows the run until the event channel closes or the user quits. On exit it restores the
/// terminal and prints the transcript so it stays in the scrollback.
pub async fn run_terminal(
    mut events: mpsc::UnboundedReceiver<UiEvent>,
    commands: mpsc::UnboundedSender<UiCommand>,
    options: UiOptions,
) -> anyhow::Result<()> {
    let mut app = App::new(&options);
    let guard = ScreenGuard;
    let mut terminal = ratatui::try_init().context("could not open the terminal screen")?;
    let stop_input = Arc::new(AtomicBool::new(false));
    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    let reader = spawn_input_reader(input_tx, Arc::clone(&stop_input));

    let result = event_loop(
        &mut terminal,
        &mut app,
        &mut events,
        &mut input_rx,
        &commands,
    )
    .await;

    let width = terminal
        .size()
        .ok()
        .map(|size| size.width as usize)
        .filter(|&w| w > 0)
        .unwrap_or(FALLBACK_WIDTH);
    stop_input.store(true, Ordering::Relaxed);
    drop(guard);
    // The reader wakes up within one poll interval and sees the stop flag.
    let _ = reader.join();
    print_summary(&app, width);
    result
}

/// Restores the terminal even when the loop returns early with an error.
struct ScreenGuard;

impl Drop for ScreenGuard {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

async fn event_loop<B>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    events: &mut mpsc::UnboundedReceiver<UiEvent>,
    input: &mut mpsc::UnboundedReceiver<Event>,
    commands: &mpsc::UnboundedSender<UiCommand>,
) -> anyhow::Result<()>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let mut ticker = tokio::time::interval(FRAME);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut input_open = true;
    let mut dirty = true;
    loop {
        if dirty {
            let now = OffsetDateTime::now_utc();
            let mut limits = ScrollLimits::default();
            terminal.draw(|frame| limits = view::draw(frame, app, now))?;
            app.page = limits.page;
            app.clamp_scroll(limits.transcript, limits.diff);
        }
        if app.take_bell() {
            ring_bell();
        }
        if app.quit {
            return Ok(());
        }
        // Anything but an idle tick changes the screen; a tick matters only while something runs.
        dirty = true;
        tokio::select! {
            event = events.recv() => {
                let Some(event) = event else { return Ok(()) };
                if apply_event_batch(app, event, events) {
                    discard_typed_ahead(input);
                }
            }
            key = input.recv(), if input_open => match key {
                Some(Event::Key(key)) => {
                    if let Some(command) = app.on_key(key) {
                        // The conductor may already be gone; the screen keeps showing what it has.
                        let _ = commands.send(command);
                    }
                }
                Some(Event::Paste(text)) => app.on_paste(&text),
                Some(_) => {}
                None => input_open = false,
            },
            _ = ticker.tick() => {
                app.on_tick();
                dirty = app.view.is_live();
            }
        }
    }
}

/// Folds in `first` and up to [`EVENT_BATCH`] more, returning whether any showed a merge card.
fn apply_event_batch(
    app: &mut App,
    first: UiEvent,
    events: &mut mpsc::UnboundedReceiver<UiEvent>,
) -> bool {
    let mut card_shown = is_merge_card(&first);
    app.on_event(first);
    for _ in 0..EVENT_BATCH {
        let Ok(event) = events.try_recv() else { break };
        card_shown |= is_merge_card(&event);
        app.on_event(event);
    }
    card_shown
}

fn is_merge_card(event: &UiEvent) -> bool {
    matches!(event, UiEvent::MergeCardShown(_))
}

/// Drops keys typed before a merge card appeared, so none of them answers it.
fn discard_typed_ahead(input: &mut mpsc::UnboundedReceiver<Event>) {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        let _ = nix::sys::termios::tcflush(&stdin, nix::sys::termios::FlushArg::TCIFLUSH);
    }
    while input.try_recv().is_ok() {}
}

fn spawn_input_reader(
    tx: mpsc::UnboundedSender<Event>,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            match event::poll(INPUT_POLL) {
                Ok(true) => match event::read() {
                    Ok(event) => {
                        if tx.send(event).is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                },
                Ok(false) => {}
                Err(_) => return,
            }
        }
    })
}

fn ring_bell() {
    let mut out = io::stdout();
    // A bell that fails to ring isn't worth stopping the screen for.
    let _ = out.write_all(b"\x07").and_then(|()| out.flush());
}

fn print_summary(app: &App, width: usize) {
    let mut out = io::stdout().lock();
    for line in view::summary_text(&app.view, app.verbosity, width) {
        if writeln!(out, "{line}").is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::{CardId, KeyScheme, MergeDecision, Verbosity};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::channel::MergePrompt;
    use crate::testing::{DIFF, Script, merge_card};

    fn options() -> UiOptions {
        UiOptions {
            verbosity: Verbosity::Normal,
            keys: KeyScheme::Standard,
            color: false,
            bell: false,
            home_dir: None,
        }
    }

    fn press(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    }

    #[tokio::test]
    async fn loop_draws_answers_the_card_and_ends_with_the_events() {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut app = App::new(&options());
        let (events_tx, mut events) = mpsc::unbounded_channel();
        let (input_tx, mut input) = mpsc::unbounded_channel();
        let (commands, _commands_rx) = mpsc::unbounded_channel();

        for event in Script::solo_finished().events {
            events_tx.send(event.into()).unwrap();
        }
        let (prompt, answer) =
            MergePrompt::new(CardId::numbered(1), merge_card(), Some(DIFF.into()));
        events_tx
            .send(UiEvent::MergeCardShown(Box::new(prompt)))
            .unwrap();
        // Typed before the card arrived, so it must not count.
        input_tx.send(press('l')).unwrap();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            input_tx.send(press('a')).unwrap();
            input_tx.send(press('y')).unwrap();
        });

        let conductor = tokio::spawn(async move {
            let decision = answer.await.unwrap();
            drop(events_tx);
            decision
        });
        let run = event_loop(&mut terminal, &mut app, &mut events, &mut input, &commands);
        tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .expect("the loop ends when the events end")
            .unwrap();
        assert_eq!(conductor.await.unwrap(), MergeDecision::Approve);
        let screen = terminal.backend().to_string();
        assert!(screen.contains("you chose: approve and merge"), "{screen}");
    }

    #[tokio::test]
    async fn loop_sends_stop_and_quits_on_ctrl_c() {
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        let mut app = App::new(&options());
        let (_events_tx, mut events) = mpsc::unbounded_channel::<UiEvent>();
        let (input_tx, mut input) = mpsc::unbounded_channel();
        let (commands, mut commands_rx) = mpsc::unbounded_channel();
        let ctrl_c = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        for _ in 0..3 {
            input_tx.send(ctrl_c.clone()).unwrap();
        }
        let run = event_loop(&mut terminal, &mut app, &mut events, &mut input, &commands);
        tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .expect("the third Ctrl+C closes the screen")
            .unwrap();
        assert_eq!(commands_rx.try_recv().unwrap(), UiCommand::StopRun);
    }
}
