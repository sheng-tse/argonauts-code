//! `list`: each run's journal ends and lock, newest first. A run whose lock is free and whose
//! last lifecycle event isn't `Parked` or `Finished` crashed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use argonauts_core::api::{ApiError, RunState, RunSummary};
use argonauts_core::events::card::IntegrationEvent;
use argonauts_core::events::checkpoint::{CheckpointEvent, CheckpointKind, CheckpointPacket};
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
            name: None,
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
        name: tally.name.filter(|name| !name.trim().is_empty()),
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
    /// Your files in the way while the run waits to merge.
    waiting_to_merge: Vec<String>,
    /// The run's newest name, such as `fix-slugify-median`.
    name: Option<String>,
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
                    let waiting = snapshot
                        .open_checkpoints
                        .iter()
                        .find_map(|opened| match &opened.packet {
                            CheckpointPacket::Merge(merge)
                                if !merge.in_the_way.is_empty() || !merge.edited.is_empty() =>
                            {
                                Some(
                                    merge
                                        .in_the_way
                                        .iter()
                                        .chain(&merge.edited)
                                        .cloned()
                                        .collect(),
                                )
                            }
                            _ => None,
                        });
                    if let Some(waiting) = waiting {
                        tally.waiting_to_merge = waiting;
                    }
                    // The engine's state keeps the name a run took before the snapshot.
                    if let Some(name) = snapshot
                        .engine
                        .pointer("/state/run/named/name")
                        .and_then(serde_json::Value::as_str)
                    {
                        tally.name = Some(name.to_owned());
                    }
                }
                RunEventKind::Run(lifecycle) => {
                    if let RunLifecycle::Named(named) = lifecycle {
                        tally.name = Some(named.name.clone());
                    }
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
                RunEventKind::Integration(IntegrationEvent::WaitingToMerge {
                    in_the_way, ..
                }) => tally.waiting_to_merge = in_the_way.clone(),
                RunEventKind::Integration(IntegrationEvent::Landed { .. }) => {
                    tally.waiting_to_merge.clear();
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
        // Parked until the log says it resumed, even while a supervisor holds it, so the run
        // never reads as running above its own `Parked` milestone.
        Some((_, RunLifecycle::Parked { reason, waiting_on })) => {
            (RunState::Parked, parked_words(tally, reason, waiting_on))
        }
        Some((_, RunLifecycle::PauseRequested { .. })) if live => (
            RunState::Pausing,
            "pausing at the next safe point".to_owned(),
        ),
        Some((_, RunLifecycle::StopRequested { .. })) if live => {
            (RunState::Pausing, "stopping".to_owned())
        }
        _ if live && !tally.waiting_to_merge.is_empty() => {
            (RunState::Running, waiting_to_merge_words(tally))
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

/// A run that waits to merge hasn't failed: it waits for your files to move.
fn waiting_to_merge_words(tally: &Tally) -> String {
    let files = tally.waiting_to_merge.len();
    let files = if files == 1 {
        "1 file of yours".to_owned()
    } else {
        format!("{files} files of yours")
    };
    format!("waiting to merge, {files} in the way")
}

fn parked_words(tally: &Tally, reason: &ParkReason, waiting_on: &[CheckpointId]) -> String {
    let at_merge = matches!(reason, ParkReason::Checkpoint | ParkReason::Paused);
    if at_merge && !tally.waiting_to_merge.is_empty() {
        return waiting_to_merge_words(tally);
    }
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
        // It tries again by itself at `until`, when there is one.
        ParkReason::NoStandIn { detail, until } => match until {
            Some(at) => format!("{detail} · tries again {}", utc_minute(*at)),
            None => detail.clone(),
        },
    }
}

fn with_until(what: &str, until: Option<OffsetDateTime>) -> String {
    match until {
        Some(until) => format!("{what} until {}", utc_minute(until)),
        None => what.to_owned(),
    }
}

/// `2026-10-04 20:30 UTC`.
fn utc_minute(at: OffsetDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} UTC",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute()
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn event(seq: u64, kind: RunEventKind) -> RunEvent {
        RunEvent {
            seq,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: RunId::new("run-01").unwrap(),
            card_id: None,
            session: None,
            source: "conductor".to_owned(),
            kind,
        }
    }

    #[test]
    fn a_run_your_files_hold_up_waits_to_merge_instead_of_failing() {
        let waiting = event(
            1,
            RunEventKind::Integration(IntegrationEvent::WaitingToMerge {
                target: "main".to_owned(),
                in_the_way: vec!["a.pyc".to_owned(), "b.pyc".to_owned()],
            }),
        );
        let parked = event(
            2,
            RunEventKind::Run(RunLifecycle::Parked {
                reason: ParkReason::Paused,
                waiting_on: Vec::new(),
            }),
        );
        let words = "waiting to merge, 2 files of yours in the way".to_owned();
        let tally = Tally::of(&[waiting.clone(), parked]);
        assert_eq!(state_of(&tally, false), (RunState::Parked, words.clone()));
        // A supervisor that holds it before it resumes doesn't make it running.
        assert_eq!(state_of(&tally, true), (RunState::Parked, words.clone()));
        let tally = Tally::of(std::slice::from_ref(&waiting));
        assert_eq!(state_of(&tally, true), (RunState::Running, words));

        let landed = event(
            3,
            RunEventKind::Integration(IntegrationEvent::Landed {
                target: "main".to_owned(),
                sha: "abc".to_owned(),
                fast_forward: true,
            }),
        );
        assert!(Tally::of(&[waiting, landed]).waiting_to_merge.is_empty());
    }

    /// A run nobody could take a seat in says who couldn't be replaced, and when it tries again
    /// by itself only when it will.
    #[test]
    fn a_run_nobody_could_take_a_seat_in_says_when_it_tries_again() {
        let parked = |until| {
            event(
                1,
                RunEventKind::Run(RunLifecycle::Parked {
                    reason: ParkReason::NoStandIn {
                        detail: "nobody could take over as the lead".to_owned(),
                        until,
                    },
                    waiting_on: Vec::new(),
                }),
            )
        };
        let at = OffsetDateTime::UNIX_EPOCH + time::Duration::minutes(5);
        let tally = Tally::of(&[parked(Some(at))]);
        assert_eq!(
            state_of(&tally, false),
            (
                RunState::Parked,
                "nobody could take over as the lead · tries again 1970-01-01 00:05 UTC".to_owned()
            )
        );
        let tally = Tally::of(&[parked(None)]);
        assert_eq!(
            state_of(&tally, false).1,
            "nobody could take over as the lead"
        );
    }

    /// A run goes by its newest name, from a `Named` or the engine's snapshot, so the panel
    /// names a run it doesn't hold as the run is named once it's open.
    #[test]
    fn a_run_goes_by_its_newest_name() {
        use argonauts_core::api::StatusSummary;
        use argonauts_core::events::run::{RunNamed, RunSnapshot};
        use argonauts_core::settings::workflows::RunStyle;

        let named = |seq, name: &str| {
            event(
                seq,
                RunEventKind::Run(RunLifecycle::Named(RunNamed {
                    name: name.to_owned(),
                    description: String::new(),
                    style: RunStyle::Workflow,
                    based_on: None,
                    done_when: Vec::new(),
                })),
            )
        };
        let tally = Tally::of(&[named(1, "greet-visitors"), named(2, "greet-by-name")]);
        assert_eq!(tally.name.as_deref(), Some("greet-by-name"));

        let snapshot = event(
            3,
            RunEventKind::Run(RunLifecycle::Snapshot(Box::new(RunSnapshot {
                upto: 2,
                engine: serde_json::json!({
                    "version": 1,
                    "state": { "run": { "named": { "name": "fix-slugify-median" } } }
                }),
                summary: StatusSummary::default(),
                open_checkpoints: Vec::new(),
                running: Vec::new(),
            }))),
        );
        assert_eq!(
            Tally::of(&[snapshot]).name.as_deref(),
            Some("fix-slugify-median")
        );
        assert_eq!(Tally::of(&[]).name, None);
    }
}
