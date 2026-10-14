//! Telemetry, off until you say yes: once a day, when Argonauts Code starts, one anonymous event
//! with the day's counts goes to PostHog. `argonauts telemetry show` prints it first, `off`
//! stops it, and `DO_NOT_TRACK=1` always wins. A failed send waits for the next day, and none
//! ever slows the app or shows an error.

mod event;
mod send;
mod uses;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use argonauts_core::Paths;
use argonauts_core::api::day_format;
use argonauts_core::paths::write_private_file;
use argonauts_core::settings::Settings;
use serde::{Deserialize, Serialize};
use time::{Date, Duration, OffsetDateTime};

use argonauts_conductor::{DoctorReport, doctor};

use crate::adapters::builtin_adapters;
use crate::env::AppEnv;
use crate::usage_ledger::{UsageLedger, local_day};
use crate::work_ledger::WorkLedger;
pub(crate) use event::Found;
use event::{DailyUsage, EVENT};
use send::Sender;
pub(crate) use uses::{Use, used};

const STATE_FILE: &str = "telemetry.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryAction {
    Show,
    On,
    Off,
}

/// `argonauts telemetry show|on|off`: what it says back.
pub async fn telemetry(action: TelemetryAction, overrides: Vec<String>) -> anyhow::Result<String> {
    let paths = Paths::from_env().context("finding your Argonauts Code folder")?;
    let now = OffsetDateTime::now_utc();
    match action {
        TelemetryAction::Show => {
            let settings = crate::commands::current_settings(None, overrides)?;
            let report = doctor(&builtin_adapters(), &settings).await;
            let env = AppEnv::discover(None).ok();
            let found = found(env.as_ref(), &report);
            Ok(show(&paths, &settings, &found, do_not_track(), now))
        }
        TelemetryAction::On => turn_on(&paths, now),
        TelemetryAction::Off => turn_off(&paths),
    }
}

/// What the event counts besides the ledgers: each installed CLI with its version, and the mod
/// files loaded.
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
    Found { clis, mods }
}

/// At start: sends the event in the background when a day has passed since the last one.
pub(crate) fn send_when_due(paths: &Paths, settings: &Settings, found: Found) {
    let Some(sender) = Sender::built_in() else {
        return;
    };
    if !settings.telemetry.enabled || do_not_track() {
        return;
    }
    let paths = paths.clone();
    let settings = settings.clone();
    tokio::spawn(async move {
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

/// The event that's due now, after marking today as tried so a failed send waits a day.
/// `None` when it isn't due.
fn due_event(
    paths: &Paths,
    settings: &Settings,
    found: &Found,
    now: OffsetDateTime,
) -> Option<(DailyUsage, State)> {
    let today = local_day(now);
    let file = state_file(paths);
    let mut state = State::load_or_start(&file, today);
    if state.tried >= today || state.counted_from >= today {
        // A first start saves the state, so counting begins today.
        let _ = state.save(&file);
        return None;
    }
    state.tried = today;
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
    event::count(&state.install, &usage, &work, &uses, settings, found, days)
}

/// The days sent are counted, so the next event starts where this one ended.
fn sent(file: &Path, mut state: State, until: Date) {
    state.counted_from = until;
    if let Err(err) = state.save(file) {
        tracing::debug!("couldn't keep the telemetry state: {err:#}");
    }
}

fn show(
    paths: &Paths,
    settings: &Settings,
    found: &Found,
    dnt: bool,
    now: OffsetDateTime,
) -> String {
    let today = local_day(now);
    let file = state_file(paths);
    let state = State::load(&file).unwrap_or_else(|| State::new(today));
    let mut lines = Vec::new();
    lines.push(if settings.telemetry.enabled {
        "Telemetry is on: one anonymous event a day, sent when Argonauts Code starts. argonauts \
         telemetry off stops it."
            .to_owned()
    } else {
        "Telemetry is off, so nothing is sent. argonauts telemetry on turns it on.".to_owned()
    });
    if dnt {
        lines.push("DO_NOT_TRACK is set, so nothing is sent while it is.".to_owned());
    }
    match Sender::built_in() {
        Some(sender) => lines.push(format!(
            "It goes to PostHog's EU capture address, {}, marked anonymous.",
            sender.address
        )),
        None => lines.push("This build has no PostHog key, so it sends nothing.".to_owned()),
    }
    lines.push(String::new());
    lines.push("The next event, as it stands now:".to_owned());
    let days = (state.counted_from.min(today), today + Duration::days(1));
    let usage = usage_between(paths, settings, found, &state, days);
    lines.push(shown(&usage));
    lines.join("\n")
}

/// The event as `show` prints it: its name, then what it carries.
fn shown(usage: &DailyUsage) -> String {
    #[derive(Serialize)]
    struct Shown<'a> {
        event: &'static str,
        #[serde(flatten)]
        usage: &'a DailyUsage,
    }
    serde_json::to_string_pretty(&Shown {
        event: EVENT,
        usage,
    })
    .unwrap_or_default()
}

fn turn_on(paths: &Paths, now: OffsetDateTime) -> anyhow::Result<String> {
    // Saying yes is yours to do, never an agent's working in a session for you.
    if std::env::var_os("ARGONAUTS_SESSION").is_some() {
        bail!("telemetry can't be turned on from an agent's session; run it in your own terminal");
    }
    argonauts_registry::write_setting(&paths.global_config(), "telemetry.enabled", "true")?;
    let today = local_day(now);
    let file = state_file(paths);
    let mut state = State::load_or_start(&file, today);
    state.counted_from = today;
    state.save(&file)?;
    let mut said = vec![
        "Telemetry is on: Argonauts Code sends one anonymous event a day, from tomorrow. argonauts \
         telemetry show prints it, and off stops it."
            .to_owned(),
    ];
    if Sender::built_in().is_none() {
        said.push("This build has no PostHog key, so it sends nothing.".to_owned());
    }
    if do_not_track() {
        said.push("DO_NOT_TRACK is set, so nothing is sent while it is.".to_owned());
    }
    Ok(said.join("\n"))
}

fn turn_off(paths: &Paths) -> anyhow::Result<String> {
    argonauts_registry::write_setting(&paths.global_config(), "telemetry.enabled", "false")?;
    Ok("Telemetry is off. Nothing is sent.".to_owned())
}

/// `DO_NOT_TRACK` set to anything but empty, `0` or `false`.
fn do_not_track() -> bool {
    std::env::var("DO_NOT_TRACK")
        .is_ok_and(|value| !matches!(value.trim(), "" | "0" | "false" | "FALSE" | "False"))
}

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
}

impl State {
    fn new(today: Date) -> Self {
        Self {
            install: uuid::Uuid::new_v4().to_string(),
            counted_from: today,
            tried: today - Duration::days(1),
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
}

#[cfg(test)]
mod tests;
