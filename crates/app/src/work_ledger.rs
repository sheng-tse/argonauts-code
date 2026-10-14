//! The work ledger, `~/.argonauts/usage/work/<yyyy-mm>.jsonl`, beside the usage ledger. When a
//! run ends it gets one line for each session (its role, task, tokens and time), one for each
//! task (who built it and how review took it), one for each review, and one for the run. Stats
//! reads it with the usage ledger's chat turns, so it says who did what per CLI, model and
//! role long after the runs are gone. Reading it costs nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use argonauts_core::api::day_format;
use argonauts_core::crew_stats::{
    CrewRole, CrewStats, Fallback, ModelDay, ReviewTally, TaskRecord, WorkRow,
};
use argonauts_core::events::card::{CardEvent, CardKind, CardState, GateKind, ReviewEvent};
use argonauts_core::events::checkpoint::CheckpointEvent;
use argonauts_core::events::goal::GoalEvent;
use argonauts_core::events::run::{Party, RunLifecycle};
use argonauts_core::events::vendor::{HandoffEvent, VendorEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::limits::QuotaStatus;
use argonauts_core::names::{SessionKey, VendorId, WorkerRef};
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_core::{AgentEvent, CardId, RunId, Spend, create_private_dir_all};
use serde::{Deserialize, Serialize};
use time::{Date, Duration, OffsetDateTime};

use crate::usage_ledger::{LedgerLine, local_day};
use crate::views::sessions::{RunFacts, SessionFacts, add_spend};

/// The triage rule that sends a request to a workflow because ultracode is on.
const ULTRACODE_RULE: &str = "ultracode";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorkLine {
    /// The local day it happened: a session's start, or the run's end.
    #[serde(with = "day_format")]
    pub day: Date,
    pub repo: String,
    pub run: RunId,
    #[serde(flatten)]
    pub what: Work,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "line", rename_all = "snake_case")]
pub(crate) enum Work {
    Session {
        /// The role its start named, such as `worker` or `adjudicator`.
        role: String,
        by: WorkerRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<CardId>,
        /// What its task was, such as `build` or `tests`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        spent: Spend,
        /// Seconds of wall time.
        took: u64,
        /// The one it took the task over from, and why.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stood_in_for: Option<WorkerRef>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        why: String,
        /// The plan window that had run out on the one it took over from.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<String>,
    },
    Task {
        task: CardId,
        /// The worker whose work it ended with.
        by: WorkerRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        /// Review rounds that sent it back.
        sent_back: u32,
        merged: bool,
        /// A reviewer or the judge looked at it, so it fared in review one way or another.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        reviewed: bool,
    },
    Review {
        by: WorkerRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<CardId>,
        /// A judge's ruling rather than a reviewer's review.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        judge: bool,
        sent_back: bool,
        /// Why it sent the work back, in the words Stats shows.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// The check commands it was given with their results.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        checks: Vec<String>,
        /// One of those checks had failed. `None` when it was given none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checks_failed: Option<bool>,
        /// It ran commands of its own.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        ran_own: bool,
    },
    Run {
        /// The orchestrator that planned it, when one did.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<WorkerRef>,
        /// Seconds from its start to its end.
        took: u64,
        /// The workflow it ran, such as `council`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workflow: Option<String>,
        /// Goals it set: its own, and each of its tasks'.
        #[serde(default, skip_serializing_if = "is_zero")]
        goals: u32,
        /// Triage sent it to a workflow because ultracode is on.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        ultracode: bool,
        /// Questions it settled on autopilot, in your place.
        #[serde(default, skip_serializing_if = "is_zero")]
        answered: u32,
    },
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

pub(crate) struct WorkLedger {
    dir: PathBuf,
}

impl WorkLedger {
    /// The ledger in the usage ledger's folder `usage`.
    pub fn beside(usage: &Path) -> Self {
        Self {
            dir: usage.join("work"),
        }
    }

    /// A run ended: its lines, from its whole log. A run already in this month's file isn't
    /// written again.
    pub fn run_ended(&self, repo: &Path, run: &RunId, events: &[RunEvent], at: OffsetDateTime) {
        let day = local_day(at);
        let seen = read_lines(&self.month_file(day))
            .iter()
            .any(|line| line.run == *run && matches!(line.what, Work::Run { .. }));
        if seen {
            return;
        }
        let lines = lines_of(repo, run, events, day);
        if let Err(err) = self.append(&lines) {
            tracing::warn!("couldn't add run {run} to the work ledger: {err}");
        }
    }

    /// Each line goes to the file of its day's month in one write, so two processes appending
    /// at once never interleave within a line.
    pub fn append(&self, lines: &[WorkLine]) -> io::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        create_private_dir_all(&self.dir)?;
        for line in lines {
            let mut text = serde_json::to_string(line).map_err(io::Error::other)?;
            text.push('\n');
            OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(self.month_file(line.day))?
                .write_all(text.as_bytes())?;
        }
        Ok(())
    }

    /// Every line from `first` on, or every line at all. Lines that don't read are skipped.
    pub fn read_since(&self, first: Option<Date>) -> Vec<WorkLine> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let first_month = first.map(month_name);
        let mut months: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                let month = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_suffix(".jsonl"));
                month.is_some_and(|month| first_month.as_deref().is_none_or(|first| month >= first))
            })
            .collect();
        months.sort();
        months
            .iter()
            .flat_map(|path| read_lines(path))
            .filter(|line| first.is_none_or(|first| line.day >= first))
            .collect()
    }

    fn month_file(&self, day: Date) -> PathBuf {
        self.dir.join(format!("{}.jsonl", month_name(day)))
    }
}

fn read_lines(path: &Path) -> Vec<WorkLine> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn month_name(day: Date) -> String {
    format!("{:04}-{:02}", day.year(), u8::from(day.month()))
}

/// What a task was, in the words Stats shows: `build`, `tests`, `docs`.
fn kind_word(kind: &CardKind) -> &'static str {
    match kind {
        CardKind::Solo | CardKind::Build => "build",
        CardKind::Interface => "interfaces",
        CardKind::AcceptanceTests { .. } => "tests",
        CardKind::Chore => "chores",
        CardKind::Docs => "docs",
        CardKind::Spike { .. } => "experiments",
    }
}

/// The run's lines, from its log. `ended` is the local day it ended.
/// The workflow a run's first event names.
fn workflow_of(events: &[RunEvent]) -> Option<String> {
    events.iter().find_map(|event| match &event.kind {
        RunEventKind::Run(RunLifecycle::Started(started)) => Some(started.workflow.clone()),
        _ => None,
    })
}

/// The goals a run set, one for the run and one for each task that had one. A goal set again,
/// as when its lines change, is still one goal.
fn goals_of(events: &[RunEvent]) -> u32 {
    let set: BTreeSet<Option<&CardId>> = events
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Goal(GoalEvent::Set(goal)) => Some(goal.task.as_ref()),
            _ => None,
        })
        .collect();
    set.len() as u32
}

/// Whether triage sent the run to a workflow because ultracode is on.
fn ultracode_of(events: &[RunEvent]) -> bool {
    events.iter().any(|event| match &event.kind {
        RunEventKind::Run(RunLifecycle::Started(started)) => started
            .triage
            .as_ref()
            .is_some_and(|triage| triage.rule == ULTRACODE_RULE),
        _ => false,
    })
}

fn lines_of(repo: &Path, run: &RunId, events: &[RunEvent], ended: Date) -> Vec<WorkLine> {
    let facts = RunFacts::of(events);
    let mut kinds: BTreeMap<CardId, &'static str> = BTreeMap::new();
    let mut merged: BTreeSet<CardId> = BTreeSet::new();
    // Each task's review rounds that sent it back.
    let mut sent_back: BTreeMap<CardId, BTreeSet<u32>> = BTreeMap::new();
    // Who stopped on a task, and why, until the one taking over starts.
    let mut stopped: BTreeMap<CardId, (String, Option<String>)> = BTreeMap::new();
    // Each CLI's plan window that has run out, by its last reading.
    let mut out_of: BTreeMap<VendorId, String> = BTreeMap::new();
    // Each task's check commands as they last ran, and whether each passed.
    let mut checks: BTreeMap<CardId, BTreeMap<String, bool>> = BTreeMap::new();
    // The model each session's CLI said it runs, which the line is filed under.
    let mut reported: BTreeMap<&SessionKey, &str> = BTreeMap::new();
    let line = |day: Date, what: Work| WorkLine {
        day,
        repo: repo.display().to_string(),
        run: run.clone(),
        what,
    };
    let mut lines = Vec::new();
    for event in events {
        let card = event.card_id.as_ref();
        match &event.kind {
            RunEventKind::Card(CardEvent::Created(spec)) => {
                kinds.insert(spec.id.clone(), kind_word(&spec.kind));
            }
            RunEventKind::Card(CardEvent::State {
                to: CardState::Integrated,
                ..
            }) => {
                merged.extend(card.cloned());
            }
            RunEventKind::Gate(gate) if gate.kind == GateKind::Command && !gate.skipped => {
                if let (Some(card), Some(command)) = (card, &gate.command) {
                    checks
                        .entry(card.clone())
                        .or_default()
                        .insert(command.join(" "), gate.passed);
                }
            }
            RunEventKind::Agent(AgentEvent::Init {
                model: Some(model), ..
            }) => {
                if let Some(session) = &event.session {
                    reported.insert(session, model);
                }
            }
            RunEventKind::Review(ReviewEvent::Reviewed {
                reviewer,
                round,
                review,
                ..
            }) => {
                let back = review.verdict != ReviewVerdict::Approve;
                if back && let Some(card) = card {
                    sent_back.entry(card.clone()).or_default().insert(*round);
                }
                let given = card.and_then(|card| checks.get(card));
                let failed = given.map(|given| given.values().any(|passed| !passed));
                let test_named = review.comments.iter().any(|comment| {
                    comment.text.to_lowercase().contains("test") || comment.path.contains("test")
                });
                let what = Work::Review {
                    by: reviewer.clone(),
                    task: card.cloned(),
                    judge: false,
                    sent_back: back,
                    reason: back.then(|| sent_back_for(review.verdict, failed, test_named)),
                    checks: given
                        .map(|given| given.keys().cloned().collect())
                        .unwrap_or_default(),
                    checks_failed: failed,
                    ran_own: false,
                };
                lines.push(line(local_day(event.ts), what));
            }
            RunEventKind::Review(ReviewEvent::Adjudicated {
                adjudicator,
                ruling,
            }) => {
                let back = ruling.verdict != ReviewVerdict::Approve;
                let given = card.and_then(|card| checks.get(card));
                let failed = given.map(|given| given.values().any(|passed| !passed));
                let what = Work::Review {
                    by: adjudicator.clone(),
                    task: card.cloned(),
                    judge: true,
                    sent_back: back,
                    reason: back.then(|| sent_back_for(ruling.verdict, failed, false)),
                    checks: given
                        .map(|given| given.keys().cloned().collect())
                        .unwrap_or_default(),
                    checks_failed: failed,
                    ran_own: false,
                };
                lines.push(line(local_day(event.ts), what));
            }
            RunEventKind::Vendor(VendorEvent::Quota(reading)) => match &reading.window {
                Some(window) if reading.status == QuotaStatus::Rejected => {
                    out_of.insert(reading.vendor.clone(), window.clone());
                }
                _ if reading.status == QuotaStatus::Allowed => {
                    out_of.remove(&reading.vendor);
                }
                _ => {}
            },
            RunEventKind::Handoff(HandoffEvent::Needed(plan)) => {
                let window = out_of.get(&plan.stopped.vendor()).cloned();
                stopped.insert(plan.card.clone(), (plan.reason.clone(), window));
            }
            _ => {}
        }
    }

    // A line is filed under the model the session's CLI said it ran, which an alias in the
    // settings, such as `haiku`, may not name the way the catalog does.
    let ran_on = |session: &SessionFacts| -> WorkerRef {
        match reported.get(&session.key) {
            Some(model) => WorkerRef::new(session.worker.worker.clone(), Some((*model).to_owned())),
            None => session.worker.clone(),
        }
    };
    // A review names the reviewer as its step asked; its session knows the model it ran, and
    // whether it ran commands of its own.
    let ran: BTreeMap<&WorkerRef, WorkerRef> = facts
        .sessions
        .iter()
        .map(|session| (&session.started.worker, ran_on(session)))
        .collect();
    let own_commands: BTreeSet<(&CardId, &WorkerRef)> = facts
        .sessions
        .iter()
        .filter(|session| {
            let role = CrewRole::of(session.started.role.as_str());
            matches!(role, CrewRole::Reviewer | CrewRole::Judge)
                && session.tools.iter().any(|tool| is_shell(&tool.name))
        })
        .filter_map(|session| Some((session.card.as_ref()?, &session.started.worker)))
        .collect();
    for review in &mut lines {
        if let Work::Review {
            by, task, ran_own, ..
        } = &mut review.what
        {
            *ran_own = task
                .as_ref()
                .is_some_and(|task| own_commands.contains(&(task, &*by)));
            if let Some(worker) = ran.get(&*by) {
                *by = worker.clone();
            }
        }
    }

    let mut builders: BTreeMap<CardId, WorkerRef> = BTreeMap::new();
    let mut orchestrator = None;
    for session in &facts.sessions {
        let role = session.started.role.as_str();
        let crew_role = CrewRole::of(role);
        let worker = ran_on(session);
        if crew_role == CrewRole::Orchestrator && orchestrator.is_none() {
            orchestrator = Some(worker.clone());
        }
        let kind = session.card.as_ref().and_then(|card| kinds.get(card));
        if crew_role == CrewRole::Worker
            && let Some(card) = &session.card
        {
            builders.insert(card.clone(), worker.clone());
        }
        let ended_at = session.ended.as_ref().map(|end| {
            session.started_at + Duration::milliseconds(end.duration_ms.min(i64::MAX as u64) as i64)
        });
        let until = ended_at.or(facts.finished_at).or(facts.last_ts);
        let took = until.map_or(0, |until| {
            (until - session.started_at).whole_seconds().max(0)
        });
        let (why, window) = match (&session.standing_in_for, &session.card) {
            (Some(_), Some(card)) => stopped.get(card).cloned().unwrap_or_default(),
            _ => Default::default(),
        };
        let what = Work::Session {
            role: role.to_owned(),
            by: worker,
            task: session.card.clone(),
            kind: kind.map(|kind| (*kind).to_owned()),
            spent: session.spent(),
            took: took as u64,
            stood_in_for: session.standing_in_for.clone(),
            why,
            window,
        };
        lines.push(line(local_day(session.started_at), what));
    }

    let reviewed: BTreeSet<CardId> = lines
        .iter()
        .filter_map(|line| match &line.what {
            Work::Review {
                task: Some(task), ..
            } => Some(task.clone()),
            _ => None,
        })
        .collect();
    for (card, by) in builders {
        let what = Work::Task {
            kind: kinds.get(&card).map(|kind| (*kind).to_owned()),
            sent_back: sent_back.get(&card).map_or(0, |rounds| rounds.len() as u32),
            merged: merged.contains(&card),
            reviewed: reviewed.contains(&card),
            task: card,
            by,
        };
        lines.push(line(ended, what));
    }

    let started = events.first().map(|event| event.ts);
    let finished = facts.finished_at.or(facts.last_ts);
    let took = match (started, finished) {
        (Some(started), Some(finished)) => (finished - started).whole_seconds().max(0) as u64,
        _ => 0,
    };
    let answered = events
        .iter()
        .filter(|event| {
            matches!(
                &event.kind,
                RunEventKind::Checkpoint(CheckpointEvent::Decided(decided))
                    if decided.by == Party::Autopilot
            )
        })
        .count() as u32;
    lines.push(line(
        ended,
        Work::Run {
            by: orchestrator,
            took,
            workflow: workflow_of(events).filter(|name| !name.is_empty()),
            goals: goals_of(events),
            ultracode: ultracode_of(events),
            answered,
        },
    ));
    lines
}

/// Why a review sent the work back, in the words Stats shows: a check it was given had
/// failed, it found the work wrong, or it asked for a test; anything else is changes asked for.
fn sent_back_for(verdict: ReviewVerdict, checks_failed: Option<bool>, test_named: bool) -> String {
    let why = if checks_failed == Some(true) {
        "a check failed"
    } else if verdict == ReviewVerdict::Reject {
        "didn't do the task"
    } else if test_named {
        "a test missing"
    } else {
        "changes asked for"
    };
    why.to_owned()
}

/// A tool that runs commands in a shell, whichever CLI calls it.
fn is_shell(tool: &str) -> bool {
    let tool = tool.to_ascii_lowercase();
    ["bash", "shell", "exec", "command"]
        .iter()
        .any(|word| tool.contains(word))
}

/// Stats for the days from `first` on, or every day: the work ledger's lines with the usage
/// ledger's chat turns, whose answers the main role gave. A run the work ledger missed still
/// counts its tokens, as work no role is known for. Each day of the period keeps what each
/// model did and used in each role, for the charts.
pub(crate) fn stats(ledger: &WorkLedger, first: Option<Date>, usage: &[LedgerLine]) -> CrewStats {
    let lines = ledger.read_since(first);
    let in_period = |day: Date| first.is_none_or(|first| day >= first);
    let recorded: BTreeSet<&RunId> = lines.iter().map(|line| &line.run).collect();

    let mut stats = CrewStats::default();
    let mut rows: BTreeMap<RowKey, WorkRow> = BTreeMap::new();
    let mut days: BTreeMap<DayKey, (Spend, u32)> = BTreeMap::new();

    for line in &lines {
        if !in_period(line.day) {
            continue;
        }
        match &line.what {
            Work::Session {
                role,
                by,
                kind,
                spent,
                took,
                stood_in_for,
                why,
                window,
                ..
            } => {
                let crew_role = CrewRole::of(role);
                add_spend(
                    &mut day_of(&mut days, line.day, by, Some(crew_role)).0,
                    spent,
                );
                let kind = kind.clone().filter(|_| crew_role == CrewRole::Worker);
                let row = row_of(&mut rows, by, crew_role, kind);
                add_spend(&mut row.spent, spent);
                row.took_secs += took;
                if let Some(from) = stood_in_for {
                    stats.fallbacks.push(Fallback {
                        agent: by.worker.clone(),
                        model: by.model.clone(),
                        from_agent: from.worker.clone(),
                        from_model: from.model.clone(),
                        why: why.clone(),
                        window: window.clone(),
                    });
                }
            }
            Work::Task {
                by,
                kind,
                sent_back,
                merged,
                reviewed,
                ..
            } => {
                day_of(&mut days, line.day, by, Some(CrewRole::Worker)).1 += 1;
                row_of(&mut rows, by, CrewRole::Worker, kind.clone()).done += 1;
                stats.tasks.push(TaskRecord {
                    agent: by.worker.clone(),
                    model: by.model.clone(),
                    kind: kind.clone(),
                    sent_back: *sent_back,
                    merged: *merged,
                    reviewed: *reviewed,
                });
            }
            Work::Review {
                by,
                judge,
                sent_back,
                reason,
                checks,
                checks_failed,
                ran_own,
                ..
            } => {
                let role = if *judge {
                    CrewRole::Judge
                } else {
                    CrewRole::Reviewer
                };
                day_of(&mut days, line.day, by, Some(role)).1 += 1;
                let row = row_of(&mut rows, by, role, None);
                row.done += 1;
                row.sent_back += u32::from(*sent_back);
                let mut tally = ReviewTally {
                    checks: checks.clone(),
                    ran_own: u32::from(*ran_own),
                    ..ReviewTally::default()
                };
                match checks_failed {
                    Some(true) => tally.failed = 1,
                    Some(false) => tally.passed = 1,
                    None => {}
                }
                if let Some(why) = reason.as_ref().filter(|_| *sent_back) {
                    tally.reasons.insert(why.clone(), 1);
                }
                row.review.add(&tally);
            }
            Work::Run { by, took, .. } => {
                stats.workflows += 1;
                stats.took_secs += took;
                if let Some(by) = by {
                    day_of(&mut days, line.day, by, Some(CrewRole::Orchestrator)).1 += 1;
                    row_of(&mut rows, by, CrewRole::Orchestrator, None).done += 1;
                }
            }
        }
    }

    for line in usage {
        let unrecorded = line.run.as_ref().is_some_and(|run| !recorded.contains(run));
        if line.run.is_some() && !unrecorded || !in_period(line.day) {
            continue;
        }
        let by = WorkerRef::new(line.agent.clone(), line.model.clone());
        let role = if unrecorded {
            CrewRole::Worker
        } else {
            CrewRole::Main
        };
        let day = day_of(&mut days, line.day, &by, (!unrecorded).then_some(role));
        add_spend(&mut day.0, &line.spent());
        day.1 += u32::from(!unrecorded);
        let row = row_of(&mut rows, &by, role, None);
        add_spend(&mut row.spent, &line.spent());
        row.done += u32::from(!unrecorded);
    }

    stats.work = rows.into_values().collect();
    stats.days = days
        .into_iter()
        .filter(|(_, (spent, done))| spent.total_tokens() > 0 || *done > 0)
        .map(|((day, agent, model, role), (spent, done))| ModelDay {
            day,
            agent,
            model,
            role,
            tokens: spent.total_tokens(),
            cost_usd: spent.cost_usd,
            done,
        })
        .collect();
    stats
}

/// Files each record that names no model under the model its CLI runs when none is named,
/// so a turn recorded without one counts as that model's rather than as the CLI's.
pub(crate) fn name_unnamed_models(stats: &mut CrewStats, defaults: &BTreeMap<String, String>) {
    let named = |agent: &str, model: &mut Option<String>| {
        if model.is_none() {
            model.clone_from(&defaults.get(agent).cloned());
        }
    };
    for row in &mut stats.work {
        named(&row.agent, &mut row.model);
    }
    for task in &mut stats.tasks {
        named(&task.agent, &mut task.model);
    }
    for day in &mut stats.days {
        named(&day.agent, &mut day.model);
    }
    for fallback in &mut stats.fallbacks {
        named(&fallback.agent, &mut fallback.model);
        named(&fallback.from_agent, &mut fallback.from_model);
    }
}

type DayKey = (Date, String, Option<String>, Option<CrewRole>);

/// The tokens and the pieces of work of one model in one role on one day.
fn day_of<'a>(
    days: &'a mut BTreeMap<DayKey, (Spend, u32)>,
    day: Date,
    by: &WorkerRef,
    role: Option<CrewRole>,
) -> &'a mut (Spend, u32) {
    let key = (day, by.worker.clone(), by.model.clone(), role);
    days.entry(key).or_default()
}

type RowKey = (String, Option<String>, CrewRole, Option<String>);

fn row_of<'a>(
    rows: &'a mut BTreeMap<RowKey, WorkRow>,
    by: &WorkerRef,
    role: CrewRole,
    kind: Option<String>,
) -> &'a mut WorkRow {
    let key = (by.worker.clone(), by.model.clone(), role, kind.clone());
    rows.entry(key).or_insert_with(|| WorkRow {
        agent: by.worker.clone(),
        model: by.model.clone(),
        role,
        kind,
        spent: Spend::default(),
        done: 0,
        sent_back: 0,
        took_secs: 0,
        review: ReviewTally::default(),
    })
}

#[cfg(test)]
#[path = "work_ledger_tests.rs"]
mod tests;
