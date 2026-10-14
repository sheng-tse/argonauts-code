//! What the run's reviews and checks decided, in the words the workflow view puts after a
//! row's counts: `approved`, `sent it back · 2 findings`, `passed`, and for a session held
//! where it was, `paused · work so far kept`. An opened agent gets the findings themselves: a
//! reviewer the ones it raised, a worker the ones it was sent back with.

use argonauts_core::TaskId;
use argonauts_core::events::session::EndReason;
use argonauts_core::events::task::{ReviewEvent, TaskEvent, TryCause};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{RoleName, SessionKey};
use argonauts_core::session_runner::Access;
use argonauts_core::steps::StepName;
use argonauts_core::steps::judgment::{ReviewOutput, ReviewVerdict};
use argonauts_core::views::{Finding, Findings, ReviewGiven, RowNote, Tone};
use argonauts_crew::claude_codes::review_of;
use argonauts_crew::merge::merger::COMMIT_MESSAGE;
use time::OffsetDateTime;

use super::sessions::{RunFacts, SessionFacts};

/// One review as the log has it.
struct Reviewed {
    task: Option<TaskId>,
    session: Option<SessionKey>,
    round: u32,
    at: OffsetDateTime,
    review: ReviewOutput,
}

/// A task's checks since its worker last reported.
#[derive(Default)]
struct Checked {
    ran: bool,
    failed: bool,
}

pub(crate) struct Verdicts {
    reviews: Vec<Reviewed>,
    /// Each task's checks after its latest try started.
    checks: Vec<(TaskId, Checked)>,
    /// Each task's tries: the try number, why it started, and when.
    tries: Vec<(TaskId, u32, TryCause, OffsetDateTime)>,
    /// The tasks that have a worktree of their own.
    worktrees: Vec<TaskId>,
}

impl Verdicts {
    pub(crate) fn of(events: &[RunEvent]) -> Self {
        let mut verdicts = Self {
            reviews: Vec::new(),
            checks: Vec::new(),
            tries: Vec::new(),
            worktrees: Vec::new(),
        };
        for event in events {
            match (&event.kind, &event.task_id) {
                (RunEventKind::Review(ReviewEvent::Reviewed { round, review, .. }), task) => {
                    verdicts.reviews.push(Reviewed {
                        task: task.clone(),
                        session: event.session.clone(),
                        round: *round,
                        at: event.ts,
                        review: review.clone(),
                    });
                }
                (RunEventKind::Check(check), Some(task)) if !check.skipped => {
                    let checked = verdicts.checked_mut(task);
                    checked.ran = true;
                    checked.failed |= !check.passed;
                }
                (
                    RunEventKind::Task(TaskEvent::Try {
                        try_number, cause, ..
                    }),
                    Some(task),
                ) => {
                    // A new try's checks start over.
                    *verdicts.checked_mut(task) = Checked::default();
                    verdicts
                        .tries
                        .push((task.clone(), *try_number, *cause, event.ts));
                }
                (RunEventKind::Task(TaskEvent::Worktree { .. }), Some(task)) => {
                    verdicts.worktrees.push(task.clone());
                }
                _ => {}
            }
        }
        verdicts
    }

    fn checked_mut(&mut self, task: &TaskId) -> &mut Checked {
        let at = match self.checks.iter().position(|(known, _)| known == task) {
            Some(at) => at,
            None => {
                self.checks.push((task.clone(), Checked::default()));
                self.checks.len() - 1
            }
        };
        &mut self.checks[at].1
    }

    /// The review `session` gave: the one logged for it, else, for a role asked through
    /// Argonauts Code with no task of its own, the answer it ended with.
    fn review_by(&self, session: &SessionFacts) -> Option<ReviewOutput> {
        let logged = self
            .reviews
            .iter()
            .rev()
            .find(|review| review.session.as_ref() == Some(&session.key));
        if let Some(logged) = logged {
            return Some(logged.review.clone());
        }
        let asked = matches!(session.started.role, RoleName::Reviewer | RoleName::Judge)
            && session.task.is_none();
        let answer = session.ended.as_ref()?.structured.as_ref()?;
        asked.then(|| review_of(answer))
    }

    /// What `session`'s row says after its counts, when there is something to say.
    pub(crate) fn note(&self, facts: &RunFacts, session: &SessionFacts) -> Option<RowNote> {
        if let Some(end) = &session.ended {
            match end.reason {
                EndReason::Paused => {
                    return Some(RowNote::new("paused · work so far kept", Tone::Warn));
                }
                EndReason::Stopped => return Some(RowNote::new("stopped", Tone::Plain)),
                _ => {}
            }
        }
        if let Some(review) = self.review_by(session) {
            let (verdict, tone) = verdict_words(review.verdict);
            let text = match review.comments.len() {
                0 => verdict.to_owned(),
                1 => format!("{verdict} · 1 finding"),
                count => format!("{verdict} · {count} findings"),
            };
            return Some(RowNote::new(text, tone));
        }
        if wrote_only_the_message(facts, session) {
            return Some(RowNote::new(GIVEN_THE_DIFF, Tone::Plain));
        }
        if let Some(did) = merger_did(session) {
            return Some(did);
        }
        // A worker whose latest try passed its checks.
        let task = session.task.as_ref()?;
        let latest = facts
            .sessions
            .iter()
            .rev()
            .find(|other| other.task.as_ref() == Some(task) && builds(other))?;
        let checked = self.checks.iter().find(|(known, _)| known == task)?;
        let passed = latest.key == session.key
            && builds(session)
            && session.ended.is_some()
            && checked.1.ran
            && !checked.1.failed;
        passed.then(|| RowNote::new("passed", Tone::Good))
    }

    /// A reviewer's verdict and findings, which its opened agent shows as its outcome.
    pub(crate) fn given(&self, session: &SessionFacts) -> Option<ReviewGiven> {
        let review = self.review_by(session)?;
        let (verdict, tone) = verdict_words(review.verdict);
        Some(ReviewGiven {
            verdict: verdict.to_owned(),
            tone,
            findings: findings(&review),
        })
    }

    /// The findings the latest review sent `session`'s task back with, while the session works
    /// on them: it started after that review asked for changes.
    pub(crate) fn to_fix(&self, facts: &RunFacts, session: &SessionFacts) -> Option<Findings> {
        let task = session.task.as_ref()?;
        if !builds(session) {
            return None;
        }
        let sent_back = self.reviews.iter().rev().find(|review| {
            review.task.as_ref() == Some(task)
                && review.at <= session.started_at
                && review.review.verdict == ReviewVerdict::RequestChanges
        })?;
        let from = sent_back
            .session
            .as_ref()
            .and_then(|key| facts.session(key))
            .map_or_else(|| "its review".to_owned(), SessionFacts::label);
        Some(Findings {
            from,
            round: sent_back.round,
            items: findings(&sent_back.review),
        })
    }

    /// What an opened agent says after its model: what it is in the run, what it may touch,
    /// and the round or try it is on.
    pub(crate) fn facts(&self, session: &SessionFacts) -> Vec<String> {
        let mut said = vec![acts_as(session).as_str().to_owned()];
        let own_worktree = session
            .task
            .as_ref()
            .is_some_and(|task| self.worktrees.contains(task));
        // What it may touch is how it was started, whatever role it has: a worker asked to
        // review reads only.
        match session.started.access {
            Access::ReadOnly => said.push("read-only".to_owned()),
            Access::NoTools => said.push("no tools".to_owned()),
            _ if builds(session) && own_worktree => said.push("worktree".to_owned()),
            _ => {}
        }
        if let Some(task) = session.task.as_ref().filter(|_| builds(session)) {
            let this_try = self
                .tries
                .iter()
                .rev()
                .find(|(known, _, _, at)| known == task && *at <= session.started_at);
            let sent_back = self
                .tries
                .iter()
                .filter(|(known, _, cause, at)| {
                    known == task && *cause == TryCause::ReviewChanges && *at <= session.started_at
                })
                .count();
            match this_try {
                Some((_, _, TryCause::ReviewChanges, _)) => {
                    said.push(format!("round {} (sent back)", sent_back + 1));
                }
                Some((_, number, TryCause::Restarted, _)) => {
                    said.push(format!("try {number} (restarted)"));
                }
                Some((_, number, _, _)) if *number > 1 => said.push(format!("try {number}")),
                _ => {}
            }
        }
        said
    }

    /// `build:due-flag is on round 2 with the findings · its review runs again after`, for the
    /// phase that holds `reviewer`'s row while the work it sent back is under way.
    pub(crate) fn waits_on(&self, facts: &RunFacts, reviewer: &SessionFacts) -> Option<String> {
        let sent_back = self
            .reviews
            .iter()
            .rev()
            .find(|review| review.session.as_ref() == Some(&reviewer.key))
            .filter(|review| review.review.verdict == ReviewVerdict::RequestChanges)?;
        let task = sent_back.task.as_ref()?;
        let worker = facts.sessions.iter().rev().find(|session| {
            session.task.as_ref() == Some(task)
                && builds(session)
                && session.started_at >= sent_back.at
                && session.ended.is_none()
        })?;
        Some(format!(
            "{} is on round {} with the findings · its review runs again after",
            worker.label(),
            sent_back.round + 1
        ))
    }
}

/// Said of the merger's session on a clean merge: the change was in its prompt, so it read
/// nothing and ran nothing.
const GIVEN_THE_DIFF: &str = "given the diff, no tools";

/// Whether `session` wrote the commit message from the diff alone, and is the only agent the
/// merge needed. One that also settled a conflict or fixed a check has rows that say more.
fn wrote_only_the_message(facts: &RunFacts, session: &SessionFacts) -> bool {
    let started = &session.started;
    started.label.as_deref() == Some(COMMIT_MESSAGE)
        && started.access == Access::NoTools
        && started.workflow_step.is_some()
        && !facts.sessions.iter().any(|other| {
            other.key != session.key && other.started.workflow_step == started.workflow_step
        })
}

/// How the crew's labels start for the session that fixes what broke once your branch came
/// in, `fix after main moved`.
const FIX_AFTER: &str = "fix after ";

/// What a session the merge needed says after its counts, from the report it ended with: one
/// that settled a conflict, what it did in its own words, and one that fixed what broke once
/// your branch came in, how many files it changed.
fn merger_did(session: &SessionFacts) -> Option<RowNote> {
    let started = &session.started;
    let report = session.ended.as_ref()?.structured.as_ref()?;
    if started.step == Some(StepName::ResolveConflict) {
        let did = report.get("summary")?.as_str()?.lines().next()?.trim();
        return (!did.is_empty()).then(|| RowNote::new(did, Tone::Plain));
    }
    let fixed = started.step.is_none()
        && started
            .label
            .as_deref()
            .is_some_and(|label| label.starts_with(FIX_AFTER));
    if !fixed {
        return None;
    }
    match report.get("files_changed")?.as_array()?.len() {
        0 => None,
        1 => Some(RowNote::new("1 file", Tone::Plain)),
        files => Some(RowNote::new(format!("{files} files"), Tone::Plain)),
    }
}

/// What `session` is in the run. Its role says, but for a worker a workflow has review or
/// judge another's work, as council's workers review each other: its step tells, and for one
/// a script's own step started, which has none, the phase its label names.
fn acts_as(session: &SessionFacts) -> RoleName {
    let started = &session.started;
    if started.role != RoleName::Worker {
        return started.role;
    }
    let labelled = started
        .label
        .as_deref()
        .and_then(|label| label.split_once(':'))
        .map(|(phase, _)| phase.trim());
    match (started.step, labelled) {
        (Some(StepName::Review), _) | (None, Some("review")) => RoleName::Reviewer,
        (Some(StepName::Judge | StepName::ModelJudge), _) | (None, Some("judge" | "sign-off")) => {
            RoleName::Judge
        }
        _ => RoleName::Worker,
    }
}

/// Whether `session` is a worker at its own work, which has tries, rounds and a worktree.
fn builds(session: &SessionFacts) -> bool {
    session.started.role == RoleName::Worker && acts_as(session) == RoleName::Worker
}

fn verdict_words(verdict: ReviewVerdict) -> (&'static str, Tone) {
    match verdict {
        ReviewVerdict::Approve => ("approved", Tone::Good),
        ReviewVerdict::RequestChanges => ("sent it back", Tone::Warn),
        ReviewVerdict::Reject => ("rejected", Tone::Bad),
    }
}

fn findings(review: &ReviewOutput) -> Vec<Finding> {
    review
        .comments
        .iter()
        .map(|comment| {
            let path = comment.path.trim();
            let place = match comment.line {
                Some(line) if !path.is_empty() => format!("{path}:{line}"),
                _ => path.to_owned(),
            };
            Finding {
                severity: comment.severity,
                place,
                text: comment.text.trim().to_owned(),
            }
        })
        .collect()
}
