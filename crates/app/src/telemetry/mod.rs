//! Usage reports, on unless you turn them off: once a day, when Argonauts Code starts, one
//! anonymous event with the day's counts goes to PostHog. Nothing goes until the line that says
//! so has shown once ([`notice`]), and nothing ever goes in CI, with `DO_NOT_TRACK`, or from an
//! agent's own session. `argonauts telemetry show` prints the next report first and `off` stops
//! them. A failed send waits for the next day, and none ever slows the app or shows an error.
//! Only optimized builds send; a debug build never does ([`send`]).

mod event;
pub(crate) mod notice;
mod send;
mod show;
mod uses;

use std::collections::BTreeMap;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use argonauts_core::Paths;
use argonauts_core::base::paths::write_private_file;
use argonauts_core::runs::api::day_format;
use argonauts_core::settings::Settings;
use argonauts_tui::base::theme::{ColorEnv, Theme};
use serde::{Deserialize, Serialize};
use time::{Date, Duration, OffsetDateTime};

use argonauts_conductor::boards::cli_versions::CliVersions;
use argonauts_conductor::{DoctorReport, doctor};

use crate::agents::adapters::builtin_adapters;
use crate::behaviors::{self, records::Records};
use crate::startup::env::{AppEnv, FolderGit};
use crate::usage::usage_ledger::{UsageLedger, local_day};
use crate::usage::work_ledger::WorkLedger;
pub(crate) use event::Found;
use event::{DailyUsage, Machine};
use send::Sender;
use show::{Due, Held};
pub use uses::count_crashes;
pub(crate) use uses::{Use, count, used};

const STATE_FILE: &str = "telemetry.json";

/// A report goes at most once in this long, whatever the days in between.
const BETWEEN_REPORTS: Duration = Duration::hours(24);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryAction {
    Show,
    On,
    Off,
}

/// `argonauts telemetry show|on|off`: what it says back, in colour on a terminal.
pub async fn telemetry(action: TelemetryAction, overrides: Vec<String>) -> anyhow::Result<String> {
    let paths = Paths::from_env().context("finding your Argonauts Code folder")?;
    let now = OffsetDateTime::now_utc();
    match action {
        TelemetryAction::Show => {
            let settings = crate::commands::current_settings(None, overrides)?;
            let report = doctor(&builtin_adapters(), &settings).await;
            let env = AppEnv::discover(None).ok();
            let mut found = found(env.as_ref(), &report);
            found.machine = machine().await;
            let var = |name: &str| std::env::var(name).ok();
            let held = held(&settings, &var);
            let colour = std::io::stdout().is_terminal();
            Ok(show(&paths, &settings, &found, &held, now, colour))
        }
        TelemetryAction::On => turn_on(&paths, now),
        TelemetryAction::Off => turn_off(&paths),
    }
}

/// What the event counts besides the ledgers: each installed CLI with its version, the mod
/// files loaded, whether this folder is a git repository, and how many kinds of output the CLIs
/// printed that Argonauts Code doesn't read.
pub(crate) fn found(env: Option<&AppEnv>, report: &DoctorReport) -> Found {
    let clis: BTreeMap<String, String> = report
        .agents
        .iter()
        .filter_map(|agent| Some((agent.id.clone(), agent.version.clone()?)))
        .collect();
    let mods = env.map_or(0, |env| {
        crate::mods::ModDirs::of(env)
            .load()
            .mods
            .iter()
            .filter(|loaded| loaded.path.is_some())
            .count()
    });
    let not_understood = env.map_or(0, |env| {
        let seen = CliVersions::load(&env.paths.cli_versions());
        clis.iter()
            .map(|(cli, version)| seen.unread(cli, version).len() as u64)
            .sum()
    });
    Found {
        clis,
        mods,
        machine: Machine::default(),
        git_repo: env.is_some_and(|env| env.git() == FolderGit::Repository),
        not_understood,
    }
}

/// This machine in brackets: its chip, memory, cores and terminal.
pub(crate) async fn machine() -> Machine {
    let ram = crate::host::memory::machine(0)
        .await
        .map(|memory| memory.total);
    let cores = std::thread::available_parallelism().map_or(0, |cores| cores.get() as u64);
    Machine {
        chip: event::chip(),
        ram_gb: ram.and_then(|bytes| event::bracket(bytes / (1024 * 1024 * 1024))),
        cores: event::bracket(cores),
        terminal: event::terminal_kind(|name| std::env::var(name).ok()),
    }
}

/// At start: sends the event in the background when one is due. Nothing goes before the usage
/// line has shown, nor while anything asks for quiet.
pub(crate) fn send_when_due(paths: &Paths, settings: &Settings, found: Found) {
    let Ok(sender) = Sender::built_in() else {
        return;
    };
    if !may_send(paths, settings, &|name| std::env::var(name).ok()) {
        return;
    }
    let paths = paths.clone();
    let settings = settings.clone();
    tokio::spawn(async move {
        let mut found = found;
        found.machine = machine().await;
        send_due(
            &paths,
            &settings,
            &found,
            &sender,
            OffsetDateTime::now_utc(),
        )
        .await;
    });
}

/// After `argonauts run` without the app: the day's report, when one is due. The CLIs are
/// asked their versions only then.
pub(crate) async fn send_after_run(env: &AppEnv, settings: &Settings) {
    let due = Sender::built_in().is_ok()
        && may_send(&env.paths, settings, &|name| std::env::var(name).ok())
        && State::load(&state_file(&env.paths))
            .is_some_and(|state| state.due(OffsetDateTime::now_utc()) == Due::NextStart);
    if !due {
        return;
    }
    let report = doctor(&builtin_adapters(), settings).await;
    send_when_due(&env.paths, settings, found(Some(env), &report));
}

/// Reports are on, nothing asks for quiet, and the usage line has shown.
fn may_send(paths: &Paths, settings: &Settings, var: &dyn Fn(&str) -> Option<String>) -> bool {
    settings.telemetry.enabled && !notice::quiet(var) && notice::shown(paths)
}

/// Sends the event when it's due, and says whether one went.
async fn send_due(
    paths: &Paths,
    settings: &Settings,
    found: &Found,
    sender: &Sender,
    now: OffsetDateTime,
) -> bool {
    let Some((usage, state)) = due_event(paths, settings, found, now) else {
        return false;
    };
    match sender.send(&usage, now).await {
        Ok(()) => {
            sent(&state_file(paths), state, local_day(now));
            true
        }
        Err(err) => {
            tracing::debug!("the day's usage event wasn't sent: {err:#}");
            false
        }
    }
}

/// The event that's due now, after marking it tried so a failed send waits a day. `None` when
/// it isn't due.
fn due_event(
    paths: &Paths,
    settings: &Settings,
    found: &Found,
    now: OffsetDateTime,
) -> Option<(DailyUsage, State)> {
    let today = local_day(now);
    let file = state_file(paths);
    let mut state = State::load_or_start(&file, today);
    if state.due(now) != Due::NextStart {
        // A first start saves the state, so counting begins today.
        let _ = state.save(&file);
        return None;
    }
    state.tried = today;
    state.tried_at = Some(now);
    if let Err(err) = state.save(&file) {
        tracing::debug!("couldn't keep the telemetry state: {err:#}");
        return None;
    }
    let usage = usage_between(paths, settings, found, &state, (state.counted_from, today));
    Some((usage, state))
}

fn usage_between(
    paths: &Paths,
    settings: &Settings,
    found: &Found,
    state: &State,
    days: (Date, Date),
) -> DailyUsage {
    let ledger = UsageLedger::new(paths);
    let usage = ledger.read_since(Some(days.0));
    let work = WorkLedger::beside(&paths.usage_dir()).read_since(Some(days.0));
    let uses = uses::read_since(paths, days.0);
    let mut event = event::count(&state.install, &usage, &work, &uses, settings, found, days);
    let actions = Records::of(paths).actions(None);
    event.behaviors = behaviors::report::count(&actions, days, |cli, model| {
        event::model_name(cli, model, settings)
    });
    event
}

/// The days sent are counted, so the next event starts where this one ended.
fn sent(file: &Path, mut state: State, until: Date) {
    state.counted_from = until;
    if let Err(err) = state.save(file) {
        tracing::debug!("couldn't keep the telemetry state: {err:#}");
    }
}

/// Why nothing would go now, besides when.
fn held(settings: &Settings, var: &dyn Fn(&str) -> Option<String>) -> Vec<Held> {
    let set = |name: &str| notice::quiet(&|asked| (asked == name).then(|| var(name)).flatten());
    let mut held = Vec::new();
    if !settings.telemetry.enabled {
        held.push(Held::Off);
    }
    if set("DO_NOT_TRACK") {
        held.push(Held::DoNotTrack);
    }
    if set("CI") {
        held.push(Held::Ci);
    }
    if let Err(why) = Sender::built_in() {
        held.push(why);
    }
    held
}

/// The next report as it stands now, with when it goes and why it wouldn't.
fn show(
    paths: &Paths,
    settings: &Settings,
    found: &Found,
    held: &[Held],
    now: OffsetDateTime,
    colour: bool,
) -> String {
    let today = local_day(now);
    let state = State::load(&state_file(paths)).unwrap_or_else(|| State::new(today));
    let days = (state.counted_from.min(today), today + Duration::days(1));
    let usage = usage_between(paths, settings, found, &state, days);
    let theme = if colour {
        Theme::detect(&ColorEnv::from_process())
    } else {
        Theme::dark()
    };
    let lines = show::lines(&usage, &state.due(now), held, &theme);
    argonauts_tui::cells::welcome::printed(&lines, colour && theme.has_color())
}

fn turn_on(paths: &Paths, now: OffsetDateTime) -> anyhow::Result<String> {
    // Saying yes is yours to do, never an agent's working in a session for you.
    if std::env::var_os("ARGONAUTS_SESSION").is_some() {
        bail!(
            "usage reports can't be turned on from an agent's session; run it in your own \
             terminal"
        );
    }
    argonauts_registry::write_setting(&paths.global_config(), "telemetry.enabled", "true")?;
    let today = local_day(now);
    let file = state_file(paths);
    let mut state = State::load_or_start(&file, today);
    state.counted_from = today;
    state.save(&file)?;
    let mut said = vec![
        "Usage reports are on: one anonymous report a day, from tomorrow. argonauts telemetry \
         show prints it, and off stops them."
            .to_owned(),
    ];
    if let Err(why) = Sender::built_in() {
        said.push(show::held_line(why).to_owned());
    }
    if notice::quiet(&|name| std::env::var(name).ok()) {
        said.push("DO_NOT_TRACK or CI is set, so nothing is sent while it is.".to_owned());
    }
    Ok(said.join("\n"))
}

fn turn_off(paths: &Paths) -> anyhow::Result<String> {
    argonauts_registry::write_setting(&paths.global_config(), "telemetry.enabled", "false")?;
    Ok(OFF.to_owned())
}

/// What `argonauts telemetry off` says.
const OFF: &str =
    "Usage reports are off. Nothing more is sent; argonauts telemetry on turns them back on.";

fn state_file(paths: &Paths) -> PathBuf {
    paths.home().join(STATE_FILE)
}

/// `~/.argonauts/telemetry.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct State {
    /// A random id made on this computer, which nothing links to you.
    install: String,
    /// The first day no event has counted yet.
    #[serde(with = "day_format")]
    counted_from: Date,
    /// The last day a send was tried, so a failed one waits for the next day.
    #[serde(with = "day_format")]
    tried: Date,
    /// When that was, kept only here, so the next waits a whole day after it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    tried_at: Option<OffsetDateTime>,
}

impl State {
    fn new(today: Date) -> Self {
        Self {
            install: uuid::Uuid::new_v4().to_string(),
            counted_from: today,
            tried: today - Duration::days(1),
            tried_at: None,
        }
    }

    fn load(file: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(file).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn load_or_start(file: &Path, today: Date) -> Self {
        Self::load(file).unwrap_or_else(|| Self::new(today))
    }

    fn save(&self, file: &Path) -> anyhow::Result<()> {
        let text = serde_json::to_string_pretty(self)?;
        write_private_file(file, text.as_bytes())
            .with_context(|| format!("writing {}", file.display()))
    }

    /// When the next report goes: never on the day counting began, never on a day one was
    /// tried, and never within a day of the last try.
    fn due(&self, now: OffsetDateTime) -> Due {
        let today = local_day(now);
        if self.counted_from >= today {
            return Due::Tomorrow;
        }
        match self.tried_at.map(|at| at + BETWEEN_REPORTS) {
            Some(next) if next > now => Due::After {
                today: local_day(next) == today,
                clock: clock(next),
            },
            _ if self.tried >= today => Due::Tomorrow,
            _ => Due::NextStart,
        }
    }
}

/// `14:00`, in this computer's time.
fn clock(at: OffsetDateTime) -> String {
    let local = argonauts_tui::base::format::WallClock::default().to_local(at);
    format!("{:02}:{:02}", local.hour(), local.minute())
}

#[cfg(test)]
mod tests;
