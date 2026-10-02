//! `list`: each run's journal ends and lock, newest first. A run whose lock is free and whose
//! last lifecycle event isn't `Parked` or `Finished` crashed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use argonauts_core::api::{ApiError, RunState, RunSummary};
use argonauts_core::events::checkpoint::{CheckpointEvent, CheckpointKind};
use argonauts_core::events::run::{ParkReason, RunLifecycle, RunMode, RunOutcome, RunPhase};
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::vendor::BudgetEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::journal::{self, RunLock};
use argonauts_core::layout::run_files as names;
use argonauts_core::names::{CheckpointId, SessionKey};
use argonauts_core::settings::Settings;
use argonauts_core::{RunId, Spend};
use argonauts_registry::resolve;
use time::OffsetDateTime;

use super::Client;
use super::journal_ends;
use super::start::WELCOME_TIMEOUT;
use crate::app_settings;
use crate::run_files;
use crate::trust::TrustStore;
use crate::views::add_spend;

pub(crate) fn list(client: &Client) -> Result<Vec<RunSummary>, ApiError> {
    let trust = TrustStore::load(&client.env.state_dir);
    let sources = app_settings::sources(&client.env, &trust, Vec::new(), client.interactive);
    let settings = resolve(&sources)
        .map(|resolved| resolved.settings)
        .map_err(|err| ApiError::Refused(err.to_string()))?;
    let runs_dir = client.env.runs_dir();
    let entries = match std::fs::read_dir(&runs_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(ApiError::Supervisor(format!(
                "reading {}: {err}",
                runs_dir.display()
            )));
        }
    };
    let mut runs: Vec<RunSummary> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| summarize(&entry.path(), &settings))
        .collect();
    runs.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| b.run.cmp(&a.run)));
    Ok(runs)
}

/// One run's summary from its directory, or `None` when it holds no run. Other repos' rows in
/// the "All repos" view come from here too.
pub(crate) fn summarize(run_dir: &Path, settings: &Settings) -> Option<RunSummary> {
    let name = run_dir.file_name()?.to_str()?;
    let run = RunId::new(name).ok()?;
    let events_path = run_dir.join(names::EVENTS);
    let start = run_files::read_start(run_dir).ok();
    let live = RunLock::is_held(run_dir);
    let now = OffsetDateTime::now_utc();

    let Some(first) = journal_ends::first_event(&events_path) else {
        let start = start?;
        let created = start.created;
        let starting = live || now - created < WELCOME_TIMEOUT;
        return Some(RunSummary {
            run,
            title: first_line(&start.request.request),
            mode: start.request.mode,
            preset: String::new(),
            state: if starting {
                RunState::Running
            } else {
                RunState::Crashed
            },
            stopped_at: if starting {
                "starting".to_owned()
            } else {
                "never started".to_owned()
            },
            updated: created,
            spent: Spend::default(),
            sessions_to_resume: 0,
            waiting: Vec::new(),
            live,
            remind: false,
        });
    };
    let recent = journal::read_since_snapshot(&events_path)
        .map(|back| back.events)
        .unwrap_or_default();
    let tally = Tally::of(&recent);
    let (title, mode, preset) = match &first.kind {
        RunEventKind::Run(RunLifecycle::Started(started)) => {
            (started.title.clone(), started.mode, started.preset.clone())
        }
        _ => match &start {
            Some(start) => (
                first_line(&start.request.request),
                start.request.mode,
                String::new(),
            ),
            None => (run.to_string(), RunMode::Solo, String::new()),
        },
    };
    let (state, stopped_at) = state_of(&tally, live);
    let waiting = match &tally.last_lifecycle {
        Some((_, RunLifecycle::Parked { waiting_on, .. })) if !waiting_on.is_empty() => {
            waiting_on.clone()
        }
        _ => tally.open_needing_you(),
    };
    let remind = match &tally.last_lifecycle {
        Some((parked_at, RunLifecycle::Parked { .. })) if !live => {
            now - *parked_at > time::Duration::days(i64::from(settings.runs.remind_parked_days))
        }
        _ => false,
    };
    Some(RunSummary {
        run,
        title,
        mode,
        preset,
        state,
        stopped_at,
        updated: recent.last().map_or(first.ts, |event| event.ts),
        spent: tally.spent(),
        sessions_to_resume: u32::try_from(tally.running.len()).unwrap_or(u32::MAX),
        waiting,
        live,
        remind,
    })
}

/// What the events since the latest snapshot say about the run's state.
#[derive(Default)]
struct Tally {
    last_lifecycle: Option<(OffsetDateTime, RunLifecycle)>,
    phase: Option<String>,
    open: BTreeMap<CheckpointId, (CheckpointKind, bool)>,
    running: BTreeSet<SessionKey>,
    budget: Option<Spend>,
    snapshot_spend: Option<Spend>,
    session_spend: Spend,
}

impl Tally {
    fn of(events: &[RunEvent]) -> Self {
        let mut tally = Self::default();
        for event in events {
            match &event.kind {
                RunEventKind::Run(RunLifecycle::Snapshot(snapshot)) => {
                    tally.running = snapshot.running.iter().cloned().collect();
                    tally.open = snapshot
                        .open_checkpoints
                        .iter()
                        .map(|opened| (opened.id.clone(), (opened.kind, opened.needs_you)))
                        .collect();
                    tally.snapshot_spend =
                        snapshot.summary.budget.as_ref().map(|budget| budget.spent);
                }
                RunEventKind::Run(lifecycle) => {
                    tally.last_lifecycle = Some((event.ts, lifecycle.clone()));
                }
                RunEventKind::Phase(change) => {
                    tally.phase = Some(phase_words(change.phase).to_owned());
                }
                RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) => {
                    tally
                        .open
                        .insert(opened.id.clone(), (opened.kind, opened.needs_you));
                }
                RunEventKind::Checkpoint(CheckpointEvent::Decided(decided)) => {
                    tally.open.remove(&decided.id);
                }
                RunEventKind::Checkpoint(CheckpointEvent::Withdrawn { id, .. }) => {
                    tally.open.remove(id);
                }
                RunEventKind::Session(SessionEvent::Started(_)) => {
                    if let Some(session) = &event.session {
                        tally.running.insert(session.clone());
                    }
                }
                RunEventKind::Session(SessionEvent::Ended(end)) => {
                    if let Some(session) = &event.session {
                        tally.running.remove(session);
                    }
                    add_spend(&mut tally.session_spend, &end.spend);
                }
                RunEventKind::Budget(BudgetEvent::Run(budget)) => {
                    tally.budget = Some(budget.spent);
                }
                _ => {}
            }
        }
        tally
    }

    fn open_needing_you(&self) -> Vec<CheckpointId> {
        self.open
            .iter()
            .filter(|(_, (_, needs_you))| *needs_you)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The run's own meter, else the snapshot's plus what sessions spent since.
    fn spent(&self) -> Spend {
        if let Some(spent) = self.budget {
            return spent;
        }
        let mut total = self.snapshot_spend.unwrap_or_default();
        add_spend(&mut total, &self.session_spend);
        total
    }

    fn kind_of(&self, id: &CheckpointId) -> Option<CheckpointKind> {
        self.open.get(id).map(|(kind, _)| *kind)
    }
}

fn state_of(tally: &Tally, live: bool) -> (RunState, String) {
    match &tally.last_lifecycle {
        Some((_, RunLifecycle::Finished { outcome, detail })) => (
            finished_state(*outcome),
            detail
                .clone()
                .unwrap_or_else(|| outcome_words(*outcome).to_owned()),
        ),
        Some((_, RunLifecycle::Parked { reason, waiting_on })) if !live => {
            (RunState::Parked, parked_words(tally, reason, waiting_on))
        }
        Some((_, RunLifecycle::PauseRequested { .. })) if live => (
            RunState::Pausing,
            "pausing at the next safe point".to_owned(),
        ),
        Some((_, RunLifecycle::StopRequested { .. })) if live => {
            (RunState::Pausing, "stopping".to_owned())
        }
        _ if live => {
            let waiting = tally.open_needing_you();
            let words = match waiting.first().and_then(|id| tally.kind_of(id)) {
                Some(kind) => format!("{} checkpoint, waiting for you", kind_words(kind)),
                None => match &tally.phase {
                    Some(phase) => format!("working on {phase}"),
                    None => "working".to_owned(),
                },
            };
            (RunState::Running, words)
        }
        _ => {
            let words = match &tally.phase {
                Some(phase) => format!("stopped without parking during {phase}"),
                None => "stopped without parking".to_owned(),
            };
            (RunState::Crashed, words)
        }
    }
}

fn finished_state(outcome: RunOutcome) -> RunState {
    match outcome {
        RunOutcome::Merged => RunState::Merged,
        RunOutcome::PullRequest => RunState::PullRequest,
        RunOutcome::BranchLeft => RunState::BranchLeft,
        RunOutcome::Abandoned => RunState::Abandoned,
        RunOutcome::Stopped => RunState::Stopped,
        RunOutcome::Failed => RunState::Failed,
    }
}

fn outcome_words(outcome: RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Merged => "merged",
        RunOutcome::PullRequest => "pull request",
        RunOutcome::BranchLeft => "branches kept",
        RunOutcome::Abandoned => "abandoned",
        RunOutcome::Stopped => "stopped",
        RunOutcome::Failed => "failed",
    }
}

fn parked_words(tally: &Tally, reason: &ParkReason, waiting_on: &[CheckpointId]) -> String {
    match reason {
        ParkReason::Checkpoint => match waiting_on.first().and_then(|id| tally.kind_of(id)) {
            Some(kind) => format!("{} checkpoint, waiting for you", kind_words(kind)),
            None => "waiting for you".to_owned(),
        },
        ParkReason::Paused => "paused".to_owned(),
        ParkReason::AllVendorsOut { until } => with_until("every agent is out", *until),
        ParkReason::WaitingForVendor { vendor, until } => {
            with_until(&format!("waiting for {vendor}"), *until)
        }
    }
}

fn with_until(what: &str, until: Option<OffsetDateTime>) -> String {
    match until {
        Some(until) => format!(
            "{what} until {:04}-{:02}-{:02} {:02}:{:02} UTC",
            until.year(),
            u8::from(until.month()),
            until.day(),
            until.hour(),
            until.minute()
        ),
        None => what.to_owned(),
    }
}

fn kind_words(kind: CheckpointKind) -> &'static str {
    match kind {
        CheckpointKind::Goal => "goal",
        CheckpointKind::Plan => "plan",
        CheckpointKind::Merge => "merge",
        CheckpointKind::Handoff => "handoff",
        CheckpointKind::VendorDown => "agent down",
        CheckpointKind::Escalation => "escalation",
        CheckpointKind::Budget => "budget",
        CheckpointKind::ChangeRequest => "change request",
        CheckpointKind::GoalChange => "goal change",
        CheckpointKind::RecipePromotion => "recipe",
        CheckpointKind::TakeOverReturn => "take-over",
        CheckpointKind::Question => "question",
        CheckpointKind::OriginalReturned => "original agent back",
    }
}

fn phase_words(phase: RunPhase) -> &'static str {
    match phase {
        RunPhase::Goal => "the goal",
        RunPhase::Planning => "planning",
        RunPhase::Dividing => "dividing",
        RunPhase::Contracts => "contracts",
        RunPhase::Build => "the build",
        RunPhase::FinalCheck => "the final check",
        RunPhase::Merge => "the merge",
        RunPhase::Retro => "the retro",
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_owned()
}
