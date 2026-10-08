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
use argonauts_core::crew_stats::{CrewRole, CrewStats, Fallback, ModelDay, TaskRecord, WorkRow};
use argonauts_core::events::card::{CardEvent, CardKind, CardState, ReviewEvent};
use argonauts_core::events::vendor::{HandoffEvent, VendorEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::limits::QuotaStatus;
use argonauts_core::names::{VendorId, WorkerRef};
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_core::{CardId, RunId, Spend, create_private_dir_all};
use serde::{Deserialize, Serialize};
use time::{Date, Duration, OffsetDateTime};

use crate::usage_ledger::{LedgerLine, local_day};
use crate::views::sessions::{RunFacts, add_spend};

/// The days the Stats tab charts, whatever its period.
const CHART_DAYS: i64 = 7;

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
    },
    Run {
        /// The orchestrator that planned it, when one did.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<WorkerRef>,
        /// Seconds from its start to its end.
        took: u64,
    },
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
                let what = Work::Review {
                    by: reviewer.clone(),
                    task: card.cloned(),
                    judge: false,
                    sent_back: back,
                };
                lines.push(line(local_day(event.ts), what));
            }
            RunEventKind::Review(ReviewEvent::Adjudicated {
                adjudicator,
                ruling,
            }) => {
                let what = Work::Review {
                    by: adjudicator.clone(),
                    task: card.cloned(),
                    judge: true,
                    sent_back: ruling.verdict != ReviewVerdict::Approve,
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

    // A review names the reviewer as its step asked; its session knows the model it ran.
    let ran: BTreeMap<&WorkerRef, &WorkerRef> = facts
        .sessions
        .iter()
        .map(|session| (&session.started.worker, &session.worker))
        .collect();
    for review in &mut lines {
        if let Work::Review { by, .. } = &mut review.what
            && let Some(worker) = ran.get(&*by)
        {
            *by = (*worker).clone();
        }
    }

    let mut builders: BTreeMap<CardId, WorkerRef> = BTreeMap::new();
    let mut orchestrator = None;
    for session in &facts.sessions {
        let role = session.started.role.as_str();
        let crew_role = CrewRole::of(role);
        if crew_role == CrewRole::Orchestrator && orchestrator.is_none() {
            orchestrator = Some(session.worker.clone());
        }
        let kind = session.card.as_ref().and_then(|card| kinds.get(card));
        if crew_role == CrewRole::Worker
            && let Some(card) = &session.card
        {
            builders.insert(card.clone(), session.worker.clone());
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
            by: session.worker.clone(),
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
    lines.push(line(
        ended,
        Work::Run {
            by: orchestrator,
            took,
        },
    ));
    lines
}

/// Stats for the days from `first` on, or every day: the work ledger's lines with the usage
/// ledger's chat turns, whose answers the main role gave. A run the work ledger missed still
/// counts its tokens, as work no role is known for. The chart's seven days are counted
/// whatever the period, so `usage` holds them too.
pub(crate) fn stats(
    ledger: &WorkLedger,
    first: Option<Date>,
    today: Date,
    usage: &[LedgerLine],
) -> CrewStats {
    let chart_from = today.saturating_sub(Duration::days(CHART_DAYS - 1));
    let earliest = first.map(|first| first.min(chart_from));
    let lines = ledger.read_since(earliest);
    let in_period = |day: Date| first.is_none_or(|first| day >= first);
    let recorded: BTreeSet<&RunId> = lines.iter().map(|line| &line.run).collect();

    let mut stats = CrewStats::default();
    let mut rows: BTreeMap<RowKey, WorkRow> = BTreeMap::new();
    let mut days: BTreeMap<(Date, String, Option<String>), Spend> = BTreeMap::new();
    let mut chart = |day: Date, by: &WorkerRef, spent: &Spend| {
        if day >= chart_from && spent.total_tokens() > 0 {
            let key = (day, by.worker.clone(), by.model.clone());
            add_spend(days.entry(key).or_default(), spent);
        }
    };

    for line in &lines {
        if let Work::Session { by, spent, .. } = &line.what {
            chart(line.day, by, spent);
        }
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
                ..
            } => {
                let role = if *judge {
                    CrewRole::Judge
                } else {
                    CrewRole::Reviewer
                };
                let row = row_of(&mut rows, by, role, None);
                row.done += 1;
                row.sent_back += u32::from(*sent_back);
            }
            Work::Run { by, took } => {
                stats.workflows += 1;
                stats.took_secs += took;
                if let Some(by) = by {
                    row_of(&mut rows, by, CrewRole::Orchestrator, None).done += 1;
                }
            }
        }
    }

    for line in usage {
        let unrecorded = line.run.as_ref().is_some_and(|run| !recorded.contains(run));
        if line.run.is_some() && !unrecorded {
            continue;
        }
        let by = WorkerRef::new(line.agent.clone(), line.model.clone());
        chart(line.day, &by, &line.spent());
        if !in_period(line.day) {
            continue;
        }
        let role = if unrecorded {
            CrewRole::Worker
        } else {
            CrewRole::Main
        };
        let row = row_of(&mut rows, &by, role, None);
        add_spend(&mut row.spent, &line.spent());
        row.done += u32::from(!unrecorded);
    }

    stats.work = rows.into_values().collect();
    stats.days = days
        .into_iter()
        .map(|((day, agent, model), spent)| ModelDay {
            day,
            agent,
            model,
            tokens: spent.total_tokens(),
            cost_usd: spent.cost_usd,
        })
        .collect();
    stats
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
    })
}

#[cfg(test)]
#[path = "work_ledger_tests.rs"]
mod tests;
