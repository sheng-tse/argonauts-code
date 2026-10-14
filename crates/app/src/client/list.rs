//! `list`: each run's journal ends and lock, newest first. A run whose lock is free and whose
//! last lifecycle event isn't `Waiting` or `Finished` crashed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use argonauts_core::api::{ApiError, RunState, RunSummary};
use argonauts_core::events::agent_status::SpendEvent;
use argonauts_core::events::approval::{ApprovalEvent, ApprovalKind, ApprovalPacket};
use argonauts_core::events::run::{RunLifecycle, RunMode, RunOutcome, RunPhase, WaitReason};
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::task::MergeEvent;
use argonauts_core::events::{RunEvent, RunEventKind, WorkflowEvent};
use argonauts_core::hidden_git::{self, MergedInto};
use argonauts_core::journal::{self, RunLock, WrittenBy};
use argonauts_core::layout::run_files as names;
use argonauts_core::names::{ApprovalId, SessionKey};
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
            return Err(ApiError::RunHost(format!(
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

    // A journal another Argonauts Code wrote can't be read, so nothing below is asked of it.
    if matches!(
        journal::written_by(&events_path),
        WrittenBy::AnOlderBuild | WrittenBy::ANewerBuild
    ) {
        return Some(from_another_build(run, run_dir, &events_path));
    }
    let Some(first) = journal_ends::first_event(&events_path) else {
        let start = start?;
        let created = start.created;
        let starting = live || now - created < WELCOME_TIMEOUT;
        return Some(RunSummary {
            run,
            title: first_line(&start.request.request),
            mode: start.request.mode,
            workflow: String::new(),
            state: if starting {
                RunState::Running
            } else {
                RunState::NeverStarted
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
    let (title, mode, chosen) = match &first.kind {
        RunEventKind::Run(RunLifecycle::Started(started)) => (
            started.title.clone(),
            started.mode,
            started.workflow.clone(),
        ),
        _ => match &start {
            Some(start) => (
                first_line(&start.request.request),
                start.request.mode,
                String::new(),
            ),
            None => (run.to_string(), RunMode::Captain, String::new()),
        },
    };
    let (state, mut stopped_at) = state_of(&tally, live);
    if state == RunState::Crashed {
        let last = recent.last().map_or(first.ts, |event| event.ts);
        let phase = tally.workflow_phase.as_deref().or(tally.phase.as_deref());
        stopped_at = stopped_words(last - first.ts, phase);
    }
    if written_into_folder(&first, &tally) {
        stopped_at = MergedInto::YourFolder.ended();
    }
    let waiting = match &tally.last_lifecycle {
        Some((_, RunLifecycle::Waiting { waiting_on, .. })) if !waiting_on.is_empty() => {
            waiting_on.clone()
        }
        _ => tally.open_needing_you(),
    };
    let remind = match &tally.last_lifecycle {
        Some((waiting_at, RunLifecycle::Waiting { .. })) if !live => {
            now - *waiting_at > time::Duration::days(i64::from(settings.runs.remind_waiting_days))
        }
        _ => false,
    };
    Some(RunSummary {
        run,
        title,
        mode,
        workflow: chosen,
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

/// What Status says of a run another Argonauts Code wrote.
pub(crate) const FROM_AN_OLDER_BUILD: &str = "from an older Argonauts, can't be resumed";

/// The summary of a run whose journal this build can't read. Its title and its day come from
/// whatever of its start record and its first line still reads as plain JSON, since their
/// shapes are the older build's.
fn from_another_build(run: RunId, run_dir: &Path, events_path: &Path) -> RunSummary {
    let json = |path: &Path, whole: bool| {
        let text = std::fs::read_to_string(path).ok()?;
        let text = if whole {
            text.as_str()
        } else {
            text.lines().next()?
        };
        serde_json::from_str::<serde_json::Value>(text).ok()
    };
    let start = json(&run_dir.join(names::START), true);
    let first = json(events_path, false);
    let text = |value: &Option<serde_json::Value>, pointer: &str| {
        value
            .as_ref()
            .and_then(|value| value.pointer(pointer))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let title = text(&first, "/kind/data/title")
        .or_else(|| text(&start, "/request/request").map(|request| first_line(&request)))
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| run.to_string());
    let rfc3339 = &time::format_description::well_known::Rfc3339;
    let updated = text(&first, "/ts")
        .or_else(|| text(&start, "/created"))
        .and_then(|at| OffsetDateTime::parse(&at, rfc3339).ok())
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    RunSummary {
        run,
        title,
        mode: RunMode::Captain,
        workflow: String::new(),
        state: RunState::FromAnotherBuild,
        stopped_at: FROM_AN_OLDER_BUILD.to_owned(),
        updated,
        spent: Spend::default(),
        sessions_to_resume: 0,
        waiting: Vec::new(),
        live: false,
        remind: false,
        name: None,
    }
}

/// What is said of a run another build wrote, with what it `cant` be: `"hi" (Oct 4) was written
/// by an older Argonauts and can't be opened · /runs archive moves it out of the way`.
pub(crate) fn from_another_build_line(run: &RunSummary, cant: &str) -> String {
    let title = run.name.as_deref().unwrap_or(&run.title);
    format!(
        "\"{title}\" ({}) was written by an older Argonauts and can't be {cant} · /runs archive \
         moves it out of the way",
        day(run.updated)
    )
}

/// Why the run in `run_dir` can't be resumed, when another build wrote its journal: no run
/// host of this build can read it, so none is started for it.
pub(crate) fn cant_be_resumed(run_dir: &Path) -> Option<String> {
    let events_path = run_dir.join(names::EVENTS);
    let unreadable = matches!(
        journal::written_by(&events_path),
        WrittenBy::AnOlderBuild | WrittenBy::ANewerBuild
    );
    let run = RunId::new(run_dir.file_name()?.to_str()?).ok()?;
    unreadable.then(|| {
        from_another_build_line(&from_another_build(run, run_dir, &events_path), "resumed")
    })
}

/// `Oct 4`.
pub(crate) fn day(at: OffsetDateTime) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = MONTHS[usize::from(u8::from(at.month())) - 1];
    format!("{month} {}", at.day())
}

/// `stopped at 3m10s when its run host exited · /resume carries it on from the review`: what
/// a run whose run host died says, once at the next start and then in Status.
fn stopped_words(ran: time::Duration, phase: Option<&str>) -> String {
    let seconds = ran.whole_seconds().max(0);
    let ran = match (seconds / 3600, seconds / 60 % 60, seconds % 60) {
        (0, 0, seconds) => format!("{seconds}s"),
        (0, minutes, seconds) => format!("{minutes}m{seconds:02}s"),
        (hours, minutes, _) => format!("{hours}h{minutes:02}m"),
    };
    let from = phase.map_or_else(String::new, |phase| format!(" from {phase}"));
    format!("stopped at {ran} when its run host exited · /resume carries it on{from}")
}

/// What the events since the latest snapshot say about the run's state.
#[derive(Default)]
struct Tally {
    last_lifecycle: Option<(OffsetDateTime, RunLifecycle)>,
    phase: Option<String>,
    /// The workflow phase that started last, such as `Review`.
    workflow_phase: Option<String>,
    open: BTreeMap<ApprovalId, (ApprovalKind, bool)>,
    running: BTreeSet<SessionKey>,
    limit: Option<Spend>,
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
                        .open_approvals
                        .iter()
                        .map(|opened| (opened.id.clone(), (opened.kind, opened.needs_you)))
                        .collect();
                    tally.snapshot_spend = snapshot.summary.spend.as_ref().map(|spend| spend.spent);
                    let waiting =
                        snapshot
                            .open_approvals
                            .iter()
                            .find_map(|opened| match &opened.packet {
                                ApprovalPacket::Merge(merge)
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
                // The workflow's own name for where it is, as its view shows it.
                RunEventKind::Workflow(WorkflowEvent::PhaseStarted(phase)) => {
                    tally.workflow_phase = Some(phase.name.clone());
                }
                RunEventKind::Approval(ApprovalEvent::Opened(opened)) => {
                    tally
                        .open
                        .insert(opened.id.clone(), (opened.kind, opened.needs_you));
                }
                RunEventKind::Approval(ApprovalEvent::Decided(decided)) => {
                    tally.open.remove(&decided.id);
                }
                RunEventKind::Approval(ApprovalEvent::Withdrawn { id, .. }) => {
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
                RunEventKind::Spend(SpendEvent::Run(limit)) => {
                    tally.limit = Some(limit.spent);
                }
                RunEventKind::Merge(MergeEvent::WaitingToMerge { in_the_way, .. }) => {
                    tally.waiting_to_merge = in_the_way.clone()
                }
                RunEventKind::Merge(MergeEvent::RunMerged { .. }) => {
                    tally.waiting_to_merge.clear();
                }
                _ => {}
            }
        }
        tally
    }

    fn open_needing_you(&self) -> Vec<ApprovalId> {
        self.open
            .iter()
            .filter(|(_, (_, needs_you))| *needs_you)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The run's own meter, else the snapshot's plus what sessions spent since.
    fn spent(&self) -> Spend {
        if let Some(spent) = self.limit {
            return spent;
        }
        let mut total = self.snapshot_spend.unwrap_or_default();
        add_spend(&mut total, &self.session_spend);
        total
    }

    fn kind_of(&self, id: &ApprovalId) -> Option<ApprovalKind> {
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
        // Waiting until the log says it resumed, even while a run host holds it, so the run
        // never reads as running above its own `Waiting` milestone.
        Some((_, RunLifecycle::Waiting { reason, waiting_on })) => {
            (RunState::Waiting, waiting_words(tally, reason, waiting_on))
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
                Some(kind) => format!("{}, waiting for you", kind_words(kind)),
                None => match &tally.phase {
                    Some(phase) => format!("working on {phase}"),
                    None => "working".to_owned(),
                },
            };
            (RunState::Running, words)
        }
        _ => {
            let words = match &tally.phase {
                Some(phase) => format!("stopped without waiting during {phase}"),
                None => "stopped without waiting".to_owned(),
            };
            (RunState::Crashed, words)
        }
    }
}

/// Whether the run ended merged in a folder without git, where its work was written into your
/// files and no branch of yours took a merge. A run that said how it ended keeps its words.
fn written_into_folder(first: &RunEvent, tally: &Tally) -> bool {
    let merged = matches!(
        &tally.last_lifecycle,
        Some((
            _,
            RunLifecycle::Finished {
                outcome: RunOutcome::Merged,
                detail: None
            }
        ))
    );
    let on_hidden_history = matches!(
        &first.kind,
        RunEventKind::Run(RunLifecycle::Started(started))
            if started.base_branch.as_deref() == Some(hidden_git::BRANCH)
    );
    merged && on_hidden_history
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

fn waiting_words(tally: &Tally, reason: &WaitReason, waiting_on: &[ApprovalId]) -> String {
    let at_merge = matches!(reason, WaitReason::Approval | WaitReason::Paused);
    if at_merge && !tally.waiting_to_merge.is_empty() {
        return waiting_to_merge_words(tally);
    }
    match reason {
        WaitReason::Approval => match waiting_on.first().and_then(|id| tally.kind_of(id)) {
            Some(kind) => format!("{}, waiting for you", kind_words(kind)),
            None => "waiting for you".to_owned(),
        },
        WaitReason::Paused => "paused".to_owned(),
        WaitReason::AllAgentsOut { until } => with_until("every agent is out", *until),
        WaitReason::WaitingForAgent { agent_id, until } => {
            with_until(&format!("waiting for {agent_id}"), *until)
        }
        // It tries again by itself at `until`, when there is one.
        WaitReason::NoFallback { detail, until } => match until {
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

fn kind_words(kind: ApprovalKind) -> &'static str {
    match kind {
        ApprovalKind::Understand => "Understand approval",
        ApprovalKind::Plan => "plan approval",
        ApprovalKind::Merge => "merge approval",
        ApprovalKind::Fallback => "fallback question",
        ApprovalKind::AgentDown => "agent down question",
        ApprovalKind::Stuck => "stuck task question",
        ApprovalKind::Limit => "limit approval",
        ApprovalKind::ChangeRequest => "change request approval",
        ApprovalKind::GoalChange => "goal change approval",
        ApprovalKind::WorkflowPromotion => "workflow approval",
        ApprovalKind::TakeOverReturn => "take-over question",
        ApprovalKind::Question => "question",
        ApprovalKind::OriginalReturned => "original agent back question",
    }
}

fn phase_words(phase: RunPhase) -> &'static str {
    match phase {
        RunPhase::Understand => "Understand",
        RunPhase::Planning => "planning",
        RunPhase::Dividing => "dividing",
        RunPhase::Contracts => "contracts",
        RunPhase::Build => "the build",
        RunPhase::FinalCheck => "the final check",
        RunPhase::Merge => "the merge",
        RunPhase::RunReview => "the run review",
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run an older Argonauts Code wrote, as it left it: its start record and its journal,
    /// in a run folder of its own.
    fn older_run() -> (tempfile::TempDir, std::path::PathBuf) {
        let runs = tempfile::tempdir().unwrap();
        let run_dir = runs.path().join("run-20261004-072329-63c3");
        std::fs::create_dir_all(&run_dir).unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/older-run");
        for file in [names::START, names::EVENTS] {
            std::fs::copy(fixture.join(file), run_dir.join(file)).unwrap();
        }
        (runs, run_dir)
    }

    #[test]
    fn a_run_from_an_older_build_says_why_it_cant_be_resumed() {
        let (_runs, run_dir) = older_run();
        assert_eq!(
            cant_be_resumed(&run_dir).as_deref(),
            Some(
                "\"hi\" (Oct 4) was written by an older Argonauts and can't be resumed · /runs \
                 archive moves it out of the way"
            )
        );
        let ours = tempfile::tempdir().unwrap();
        let run_dir = ours.path().join("run-20261013-072329-63c3");
        std::fs::create_dir_all(&run_dir).unwrap();
        assert_eq!(cant_be_resumed(&run_dir), None, "a run with no journal yet");
    }

    #[test]
    fn a_run_from_an_older_build_says_so_instead_of_never_started() {
        let (_runs, run_dir) = older_run();
        let events = run_dir.join(names::EVENTS);
        // It did run: its journal holds every event, in a format this build can't read.
        let lines = std::fs::read_to_string(&events).unwrap().lines().count();
        assert_eq!(lines, 90);
        assert_eq!(journal::written_by(&events), WrittenBy::AnOlderBuild);

        let summary = summarize(&run_dir, &Settings::default()).unwrap();
        assert_eq!(summary.state, RunState::FromAnotherBuild);
        assert_eq!(summary.title, "hi");
        assert_eq!(
            summary.stopped_at,
            "from an older Argonauts, can't be resumed"
        );
        assert_eq!(summary.updated.date(), time::macros::date!(2026 - 10 - 04));
        assert!(!summary.live && summary.waiting.is_empty());
        // Reading it changed nothing in its folder.
        assert_eq!(
            std::fs::read_to_string(&events).unwrap().lines().count(),
            90
        );
    }

    #[test]
    fn a_run_merged_in_a_folder_without_git_was_written_into_it() {
        let started_on = |branch: &str| {
            let mut first = crate::client::test_support::run_started(1, "add due dates", None);
            if let RunEventKind::Run(RunLifecycle::Started(started)) = &mut first.kind {
                started.base_branch = Some(branch.to_owned());
            }
            first
        };
        let ended = |outcome, detail: Option<&str>| {
            Tally::of(&[event(
                2,
                RunEventKind::Run(RunLifecycle::Finished {
                    outcome,
                    detail: detail.map(str::to_owned),
                }),
            )])
        };
        let folder = started_on(hidden_git::BRANCH);
        let merged = ended(RunOutcome::Merged, None);
        assert!(written_into_folder(&folder, &merged));
        assert_eq!(state_of(&merged, false).1, "merged");

        // Its row in every list of runs says so.
        let runs = tempfile::tempdir().unwrap();
        let run_dir = runs.path().join("run-20261010-091503-3fa2");
        std::fs::create_dir_all(&run_dir).unwrap();
        let finished = RunEventKind::Run(RunLifecycle::Finished {
            outcome: RunOutcome::Merged,
            detail: None,
        });
        crate::client::test_support::write_journal(
            &run_dir.join(names::EVENTS),
            &[folder.clone(), event(2, finished)],
        );
        let summary = summarize(&run_dir, &Settings::default()).unwrap();
        assert_eq!(summary.state, RunState::Merged);
        assert_eq!(summary.stopped_at, "written into your folder");

        // A branch of yours took the merge, the run ended another way, or it said how.
        assert!(!written_into_folder(&started_on("main"), &merged));
        assert!(!written_into_folder(
            &folder,
            &ended(RunOutcome::Abandoned, None)
        ));
        assert!(!written_into_folder(
            &folder,
            &ended(RunOutcome::Merged, Some("nothing to write"))
        ));
    }

    #[test]
    fn a_stopped_run_says_how_far_it_got_and_what_resume_does() {
        assert_eq!(
            stopped_words(time::Duration::seconds(190), Some("Review")),
            "stopped at 3m10s when its run host exited · /resume carries it on from Review"
        );
        assert_eq!(
            stopped_words(time::Duration::seconds(42), None),
            "stopped at 42s when its run host exited · /resume carries it on"
        );
        assert_eq!(
            stopped_words(time::Duration::seconds(3725), Some("the build")),
            "stopped at 1h02m when its run host exited · /resume carries it on from the build"
        );
    }

    fn event(seq: u64, kind: RunEventKind) -> RunEvent {
        RunEvent {
            seq,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: RunId::new("run-01").unwrap(),
            task_id: None,
            session: None,
            source: "argonauts-code".to_owned(),
            kind,
        }
    }

    #[test]
    fn a_run_your_files_hold_up_waits_to_merge_instead_of_failing() {
        let waiting = event(
            1,
            RunEventKind::Merge(MergeEvent::WaitingToMerge {
                target: "main".to_owned(),
                in_the_way: vec!["a.pyc".to_owned(), "b.pyc".to_owned()],
            }),
        );
        let held = event(
            2,
            RunEventKind::Run(RunLifecycle::Waiting {
                reason: WaitReason::Paused,
                waiting_on: Vec::new(),
            }),
        );
        let words = "waiting to merge, 2 files of yours in the way".to_owned();
        let tally = Tally::of(&[waiting.clone(), held]);
        assert_eq!(state_of(&tally, false), (RunState::Waiting, words.clone()));
        // A run host that holds it before it resumes doesn't make it running.
        assert_eq!(state_of(&tally, true), (RunState::Waiting, words.clone()));
        let tally = Tally::of(std::slice::from_ref(&waiting));
        assert_eq!(state_of(&tally, true), (RunState::Running, words));

        let merged = event(
            3,
            RunEventKind::Merge(MergeEvent::RunMerged {
                target: "main".to_owned(),
                sha: "abc".to_owned(),
                fast_forward: true,
            }),
        );
        assert!(Tally::of(&[waiting, merged]).waiting_to_merge.is_empty());
    }

    /// A run with no fallback for its jobs says who has none, and when it tries again by
    /// itself only when it will.
    #[test]
    fn a_run_with_no_fallback_says_when_it_tries_again() {
        let waiting = |until| {
            event(
                1,
                RunEventKind::Run(RunLifecycle::Waiting {
                    reason: WaitReason::NoFallback {
                        detail: "no fallback for the main agent".to_owned(),
                        until,
                    },
                    waiting_on: Vec::new(),
                }),
            )
        };
        let at = OffsetDateTime::UNIX_EPOCH + time::Duration::minutes(5);
        let tally = Tally::of(&[waiting(Some(at))]);
        assert_eq!(
            state_of(&tally, false),
            (
                RunState::Waiting,
                "no fallback for the main agent · tries again 1970-01-01 00:05 UTC".to_owned()
            )
        );
        let tally = Tally::of(&[waiting(None)]);
        assert_eq!(state_of(&tally, false).1, "no fallback for the main agent");
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
                open_approvals: Vec::new(),
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
