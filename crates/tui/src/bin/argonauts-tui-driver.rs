//! Runs the screen on a scripted app, with no controller. The pty tests drive it, and it's the
//! quickest way to look at a screen by hand.
//!
//! `argonauts-tui-driver <script.jsonl>` plays the script's lines in order:
//! - `{"at_ms": N, "event": <AppEvent>}` sends the event N ms after the script started, or after
//!   the last wait below ended;
//! - `{"await_command": "<type>"}` waits for a `UiCommand` of that type, such as `submit`. Each
//!   wait takes the oldest such command not taken yet, even one sent before the wait began;
//! - `{"await_signal": "<name>"}` waits until a file called `<name>` exists in
//!   `$DRIVER_SIGNALS`, so a test can hold an event back until it has typed something;
//! - `{"panic": "<message>"}` panics, so a test can see the terminal given back after a crash.
//!
//! `$DRIVER_UI` holds `[ui]` settings as TOML, bare or under a `[ui]` table.
//! `$DRIVER_DOUBLE_PRESS_MS` sets how long a first Ctrl+C, Ctrl+D or Esc waits for the second,
//! a second unless set. Every command the screen sends goes to `$DRIVER_COMMANDS` as one JSON
//! line. The driver exits when the screen does. Without `ARGONAUTS_HOME` the prompt history
//! stays in memory, so a run by hand never touches `~/.argonauts`.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, bail};
use argonauts_core::NoteLevel;
use argonauts_core::app::{AppEvent, UiCommand};
use argonauts_core::ids::RepoId;
use argonauts_core::settings::ui::UiSettings;
use argonauts_tui::{AppIo, UiOptions, history_file_in, run_app, run_screen_reader};
use serde_json::Value;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

const UI_ENV: &str = "DRIVER_UI";
const COMMANDS_ENV: &str = "DRIVER_COMMANDS";
const SIGNALS_ENV: &str = "DRIVER_SIGNALS";
const DOUBLE_PRESS_ENV: &str = "DRIVER_DOUBLE_PRESS_MS";
const SIGNAL_POLL: Duration = Duration::from_millis(10);
const RECORDER_DRAIN: Duration = Duration::from_secs(1);

fn main() -> anyhow::Result<()> {
    let script = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .context("usage: argonauts-tui-driver <script.jsonl>")?;
    let steps = load_script(&script)?;
    let settings = ui_settings()?;
    let mut options = UiOptions::from_settings(&settings);
    if let Some(window) = double_press()? {
        options.double_press = window;
    }
    // Only a test that gave the driver a home of its own keeps history on disk.
    if std::env::var_os(argonauts_core::paths::HOME_ENV).is_some_and(|home| !home.is_empty()) {
        options.history_file = history_here();
    }
    let recorder = Recorder::open(std::env::var_os(COMMANDS_ENV).map(PathBuf::from))?;
    let signals = std::env::var_os(SIGNALS_ENV).map(PathBuf::from);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    let result = runtime.block_on(run(steps, options, recorder, signals));
    // As in the app: a screen can be blocked reading stdin, and nothing is left to wait for.
    runtime.shutdown_background();
    result
}

async fn run(
    steps: Vec<Step>,
    options: UiOptions,
    mut recorder: Recorder,
    signals: Option<PathBuf>,
) -> anyhow::Result<()> {
    let (command_tx, mut command_rx) = mpsc::channel::<UiCommand>(256);
    let (event_tx, event_rx) = mpsc::channel::<AppEvent>(1024);
    let (seen_tx, seen_rx) = watch::channel(Vec::<String>::new());

    let recording = tokio::spawn(async move {
        while let Some(command) = command_rx.recv().await {
            let kind = recorder.append(&command)?;
            seen_tx.send_modify(|seen| seen.push(kind));
        }
        anyhow::Ok(())
    });
    let player = tokio::spawn(play(steps, event_tx, seen_rx, signals));

    let io = AppIo {
        commands: command_tx,
        events: event_rx,
    };
    let screen = if options.screen_reader {
        run_screen_reader(io, options).await
    } else {
        run_app(io, options).await
    };
    player.abort();
    screen?;
    // Once the screen drops its sender the recorder ends. Each command was flushed as it came,
    // so a task of the screen's still holding a sender only costs the wait.
    match tokio::time::timeout(RECORDER_DRAIN, recording).await {
        Ok(recorded) => recorded.context("the command recorder stopped")?,
        Err(_) => Ok(()),
    }
}

enum Step {
    Event { at: Duration, event: Box<AppEvent> },
    AwaitCommand(String),
    AwaitSignal(String),
    Panic(String),
}

/// The history file of the folder the driver runs in, as the app finds it.
fn history_here() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let repo = RepoId::for_repo(&cwd).ok()?;
    let state_dir = argonauts_core::paths::state_dir(&repo).ok()?;
    Some(history_file_in(&state_dir))
}

fn load_script(path: &Path) -> anyhow::Result<Vec<Step>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading the script {}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            parse_step(line).with_context(|| format!("{} line {}", path.display(), index + 1))
        })
        .collect()
}

fn parse_step(line: &str) -> anyhow::Result<Step> {
    let value: Value = serde_json::from_str(line).context("not JSON")?;
    if let Some(kind) = value.get("await_command") {
        let kind = kind
            .as_str()
            .context("await_command takes a command type")?;
        return Ok(Step::AwaitCommand(kind.to_owned()));
    }
    if let Some(name) = value.get("await_signal") {
        let name = name.as_str().context("await_signal takes a name")?;
        if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
            bail!("a signal name is a plain file name, not {name:?}");
        }
        return Ok(Step::AwaitSignal(name.to_owned()));
    }
    if let Some(message) = value.get("panic") {
        let message = message.as_str().context("panic takes a message")?;
        return Ok(Step::Panic(message.to_owned()));
    }
    let event = value
        .get("event")
        .context("a line has an event, await_command, await_signal, or panic")?;
    let event: AppEvent = serde_json::from_value(event.clone()).context("not an AppEvent")?;
    let at_ms = match value.get("at_ms") {
        None => 0,
        Some(ms) => ms.as_u64().context("at_ms is whole milliseconds")?,
    };
    Ok(Step::Event {
        at: Duration::from_millis(at_ms),
        event: Box::new(event),
    })
}

/// Plays the script, then keeps the event channel open so the screen stays up until it exits
/// on its own. A step that can't be played shows as an error notice on the screen.
async fn play(
    steps: Vec<Step>,
    events: mpsc::Sender<AppEvent>,
    mut seen: watch::Receiver<Vec<String>>,
    signals: Option<PathBuf>,
) {
    let mut mark = Instant::now();
    let mut taken: Vec<bool> = Vec::new();
    for step in steps {
        let played = match step {
            Step::Event { at, event } => {
                tokio::time::sleep_until(mark + at).await;
                events.send(*event).await.map_err(|_| Stop)
            }
            Step::AwaitCommand(kind) => {
                let found = take_command(&mut seen, &mut taken, &kind).await;
                mark = Instant::now();
                found
            }
            Step::AwaitSignal(name) => match &signals {
                Some(dir) => {
                    wait_for_file(&dir.join(&name)).await;
                    mark = Instant::now();
                    Ok(())
                }
                None => {
                    let text = format!("driver: await_signal {name:?} needs ${SIGNALS_ENV}");
                    let notice = AppEvent::Notice {
                        level: NoteLevel::Error,
                        text,
                    };
                    let _ = events.send(notice).await;
                    Err(Stop)
                }
            },
            // Unwinding drops the event sender, so the screen closes after the panic hook.
            Step::Panic(message) => panic!("{message}"),
        };
        if played.is_err() {
            break;
        }
    }
    std::future::pending::<()>().await;
    drop(events);
}

/// The screen is gone, so there's nothing left to play to.
struct Stop;

async fn take_command(
    seen: &mut watch::Receiver<Vec<String>>,
    taken: &mut Vec<bool>,
    kind: &str,
) -> Result<(), Stop> {
    loop {
        {
            let seen = seen.borrow_and_update();
            taken.resize(seen.len(), false);
            let next = seen
                .iter()
                .zip(taken.iter())
                .position(|(seen, taken)| !taken && seen == kind);
            if let Some(index) = next {
                taken[index] = true;
                return Ok(());
            }
        }
        seen.changed().await.map_err(|_| Stop)?;
    }
}

async fn wait_for_file(path: &Path) {
    while !tokio::fs::try_exists(path).await.unwrap_or(false) {
        tokio::time::sleep(SIGNAL_POLL).await;
    }
}

/// Appends commands as JSON lines, flushed one by one so a test sees each as soon as it's sent.
struct Recorder {
    file: Option<File>,
}

impl Recorder {
    fn open(path: Option<PathBuf>) -> anyhow::Result<Self> {
        let file = path
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .with_context(|| format!("opening {}", path.display()))
            })
            .transpose()?;
        Ok(Self { file })
    }

    /// Returns the command's `type`.
    fn append(&mut self, command: &UiCommand) -> anyhow::Result<String> {
        let value = serde_json::to_value(command).context("writing a command as JSON")?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if let Some(file) = &mut self.file {
            let mut line = value.to_string();
            line.push('\n');
            file.write_all(line.as_bytes())
                .and_then(|()| file.flush())
                .context("recording a command")?;
        }
        Ok(kind)
    }
}

fn double_press() -> anyhow::Result<Option<Duration>> {
    let Some(text) = std::env::var_os(DOUBLE_PRESS_ENV) else {
        return Ok(None);
    };
    let ms = text
        .to_str()
        .and_then(|text| text.parse::<u64>().ok())
        .with_context(|| format!("${DOUBLE_PRESS_ENV} isn't a number of milliseconds"))?;
    Ok(Some(Duration::from_millis(ms)))
}

fn ui_settings() -> anyhow::Result<UiSettings> {
    let Some(text) = std::env::var_os(UI_ENV).filter(|text| !text.is_empty()) else {
        return Ok(UiSettings::default());
    };
    let text = text
        .into_string()
        .map_err(|_| anyhow::anyhow!("${UI_ENV} isn't UTF-8"))?;
    let mut table: toml::Table =
        toml::from_str(&text).with_context(|| format!("${UI_ENV} isn't TOML"))?;
    if table.len() == 1
        && let Some(toml::Value::Table(ui)) = table.remove("ui")
    {
        table = ui;
    }
    let given = toml::Value::Table(table);
    let settings: UiSettings = given
        .clone()
        .try_into()
        .with_context(|| format!("${UI_ENV} isn't a [ui] section"))?;
    // Unknown keys deserialize silently, so a typo would test the defaults instead.
    let kept = toml::Value::try_from(&settings).context("reading the settings back")?;
    if let Some(key) = first_unknown_key(&given, &kept, "") {
        bail!("${UI_ENV} has an unknown key {key:?}");
    }
    Ok(settings)
}

fn first_unknown_key(given: &toml::Value, kept: &toml::Value, prefix: &str) -> Option<String> {
    let (toml::Value::Table(given), toml::Value::Table(kept)) = (given, kept) else {
        return None;
    };
    given.iter().find_map(|(key, value)| {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match kept.get(key) {
            None => Some(path),
            Some(kept) => first_unknown_key(value, kept, &path),
        }
    })
}
