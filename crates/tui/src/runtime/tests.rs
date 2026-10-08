use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use argonauts_core::api::{RawView, TakeOverTicket};
use argonauts_core::app::{AppEvent, UiCommand, ViewData};
use argonauts_core::names::SessionKey;
use argonauts_core::session::CommandSpec;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::ui::{ScreenMode, TakeoverWhere};
use argonauts_core::{CardId, RunId};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use futures::channel::mpsc::{UnboundedSender, unbounded};
use ratatui::layout::{Position, Size};
use tokio::sync::mpsc;
use tokio::time::timeout;

use super::*;

const COLUMNS: u16 = 80;
const ROWS: u16 = 24;
/// Longer than a frame, so what was sent is on screen.
const SETTLE: Duration = Duration::from_millis(60);
const PATIENCE: Duration = Duration::from_secs(5);

/// A terminal the test can look at while the loop owns it.
#[derive(Clone)]
struct SharedScreen(Arc<Mutex<vt100::Parser>>);

impl SharedScreen {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLUMNS, 0))))
    }

    fn parser(&self) -> MutexGuard<'_, vt100::Parser> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn rows(&self) -> Vec<String> {
        self.parser()
            .screen()
            .rows(0, COLUMNS)
            .map(|row| row.trim_end().to_owned())
            .collect()
    }

    fn shows(&self, text: &str) -> bool {
        self.rows().iter().any(|row| row.contains(text))
    }

    fn alternate(&self) -> bool {
        self.parser().screen().alternate_screen()
    }
}

impl Write for SharedScreen {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.parser().process(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Script {
    keys: UnboundedSender<io::Result<Event>>,
    events: mpsc::Sender<AppEvent>,
    commands: mpsc::Receiver<UiCommand>,
    screen: SharedScreen,
}

impl Script {
    fn key(&self, code: KeyCode, modifiers: KeyModifiers) {
        self.send_key(Event::Key(KeyEvent::new(code, modifiers)));
    }

    fn send_key(&self, event: Event) {
        self.keys.unbounded_send(Ok(event)).unwrap();
    }

    async fn event(&self, event: AppEvent) {
        self.events.send(event).await.unwrap();
    }

    async fn command(&mut self) -> UiCommand {
        timeout(PATIENCE, self.commands.recv())
            .await
            .expect("a command in time")
            .expect("the loop still running")
    }

    async fn settle(&self) {
        tokio::time::sleep(SETTLE).await;
    }
}

fn session() -> (
    Script,
    AppIo,
    InlineTerminal<SharedScreen>,
    impl Stream<Item = io::Result<Event>>,
) {
    crossterm::style::force_color_output(true);
    let (keys, key_stream) = unbounded();
    let (events_tx, events) = mpsc::channel(64);
    let (commands, commands_rx) = mpsc::channel(64);
    let screen = SharedScreen::new();
    let terminal = InlineTerminal::new(
        screen.clone(),
        Size::new(COLUMNS, ROWS),
        Position::new(0, 0),
    );
    let script = Script {
        keys,
        events: events_tx,
        commands: commands_rx,
        screen,
    };
    (script, AppIo { commands, events }, terminal, key_stream)
}

fn options() -> UiOptions {
    UiOptions::from_settings(&argonauts_core::settings::ui::UiSettings::default())
}

fn inline() -> UiOptions {
    UiOptions::from_settings(&argonauts_core::settings::ui::UiSettings {
        screen: ScreenMode::Inline,
        ..Default::default()
    })
}

fn raw_view() -> AppEvent {
    let raw = RawView {
        session: SessionKey::numbered(1),
        lines: vec!["raw line".into()],
        truncated: false,
        path: PathBuf::from("raw.jsonl"),
        vendor_transcript: None,
    };
    AppEvent::View {
        data: Box::new(ViewData::Raw { raw }),
    }
}

#[tokio::test]
async fn a_scripted_session_runs_end_to_end() {
    let (mut script, io, terminal, keys) = session();
    let run = run_with(io, inline(), terminal, keys);
    let drive = async move {
        script.settle().await;
        assert!(script.screen.shows("❯"), "{:#?}", script.screen.rows());

        script.key(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(
            script.command().await,
            UiCommand::ApprovalMode {
                mode: ApprovalMode::Auto
            }
        );
        script.send_key(Event::FocusLost);
        assert_eq!(script.command().await, UiCommand::Focus { focused: false });

        // A controller answer opens its view on the alternate screen, and Esc comes back.
        script.event(raw_view()).await;
        script.settle().await;
        assert!(script.screen.alternate());
        script.key(KeyCode::Esc, KeyModifiers::NONE);
        script.settle().await;
        assert!(!script.screen.alternate());

        // An endpoint has nothing to hand over, and the card resumes.
        let run = RunId::new("run-01").unwrap();
        let card = CardId::new("task-001").unwrap();
        script
            .event(AppEvent::TakeOver {
                ticket: TakeOverTicket {
                    card: card.clone(),
                    command: None,
                    worktree: PathBuf::from("."),
                    vendor: "ollama".into(),
                },
                place: TakeoverWhere::Here,
                run: run.clone(),
            })
            .await;
        assert_eq!(
            script.command().await,
            UiCommand::TakeOverEnded {
                run: run.clone(),
                card: card.clone(),
                exit_code: None
            }
        );
        script.settle().await;
        assert!(
            script
                .screen
                .shows("A direct endpoint has no UI to hand over."),
            "{:#?}",
            script.screen.rows()
        );

        // Here: the screen steps aside while the program runs, then comes back.
        let dir = tempfile::tempdir().unwrap();
        let spec = CommandSpec::new("sh", dir.path()).args(["-c", "exit 3"]);
        script
            .event(AppEvent::TakeOver {
                ticket: TakeOverTicket {
                    card: card.clone(),
                    command: Some(spec),
                    worktree: dir.path().to_owned(),
                    vendor: "codex".into(),
                },
                place: TakeoverWhere::Here,
                run: run.clone(),
            })
            .await;
        assert_eq!(
            script.command().await,
            UiCommand::TakeOverEnded {
                run,
                card,
                exit_code: Some(3)
            }
        );
        script.settle().await;
        assert!(script.screen.shows("❯"), "{:#?}", script.screen.rows());

        // Ctrl+C twice leaves.
        script.key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        script.settle().await;
        assert!(
            script.screen.shows("Press Ctrl-C again to exit"),
            "{:#?}",
            script.screen.rows()
        );
        script.key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(script.command().await, UiCommand::Exit);
        script
    };
    let (ran, script) = tokio::join!(timeout(PATIENCE, run), drive);
    ran.expect("the loop ends").unwrap();
    // The live region is gone and the shell gets a clean screen back.
    assert!(!script.screen.alternate());
    assert!(
        !script.screen.shows("Press Ctrl-C"),
        "{:#?}",
        script.screen.rows()
    );
}

#[tokio::test]
async fn fullscreen_keeps_the_alternate_screen_through_views_and_handovers() {
    let (mut script, io, terminal, keys) = session();
    script.screen.write_all(b"$ argonauts\r\n").unwrap();
    let run = run_with(io, options(), terminal, keys);
    let drive = async move {
        script.settle().await;
        assert!(script.screen.alternate());
        assert!(script.screen.shows("❯"), "{:#?}", script.screen.rows());

        script.event(raw_view()).await;
        script.settle().await;
        assert!(
            script.screen.shows("raw line"),
            "{:#?}",
            script.screen.rows()
        );
        script.key(KeyCode::Esc, KeyModifiers::NONE);
        script.settle().await;
        assert!(script.screen.alternate());
        assert!(
            !script.screen.shows("raw line"),
            "{:#?}",
            script.screen.rows()
        );

        // The program gets the main screen, and the app takes the alternate one back after.
        let dir = tempfile::tempdir().unwrap();
        let spec = CommandSpec::new("sh", dir.path()).args(["-c", "exit 0"]);
        let run = RunId::new("run-01").unwrap();
        let card = CardId::new("task-001").unwrap();
        script
            .event(AppEvent::TakeOver {
                ticket: TakeOverTicket {
                    card: card.clone(),
                    command: Some(spec),
                    worktree: dir.path().to_owned(),
                    vendor: "codex".into(),
                },
                place: TakeoverWhere::Here,
                run: run.clone(),
            })
            .await;
        assert!(matches!(
            script.command().await,
            UiCommand::TakeOverEnded { .. }
        ));
        script.settle().await;
        assert!(script.screen.alternate());
        assert!(script.screen.shows("❯"), "{:#?}", script.screen.rows());

        script.key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        script.key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(script.command().await, UiCommand::Exit);
        script
    };
    let (ran, script) = tokio::join!(timeout(PATIENCE, run), drive);
    ran.expect("the loop ends").unwrap();
    assert!(!script.screen.alternate());
    let rows = script.screen.rows();
    assert_eq!(rows[0], "$ argonauts");
    assert!(rows[1..].iter().all(String::is_empty), "{rows:#?}");
}

#[tokio::test]
async fn the_controller_can_close_the_screen() {
    let (script, io, terminal, keys) = session();
    let run = run_with(io, options(), terminal, keys);
    let drive = async {
        script.settle().await;
        script.event(AppEvent::Exit).await;
    };
    let (ran, ()) = tokio::join!(timeout(PATIENCE, run), drive);
    ran.expect("the loop ends").unwrap();

    // Dropping the controller's side ends it too.
    let (script, io, terminal, keys) = session();
    drop(script.events);
    timeout(PATIENCE, run_with(io, options(), terminal, keys))
        .await
        .expect("the loop ends")
        .unwrap();
}

#[test]
fn the_app_can_run_the_screen_on_its_own_task() {
    fn is_send<T: Send>(_: &T) {}
    let (_script, io, _, _) = session();
    let screen = run_app(io, options());
    is_send(&screen);
}
