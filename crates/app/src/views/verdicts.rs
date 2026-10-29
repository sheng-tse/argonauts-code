//! What the run's reviews and checks decided, in the words the workflow view puts after a
//! row's counts: `approved`, `sent it back · 2 findings`, `passed`, and for a session held
//! where it was, `paused · work so far kept` or what else the run waits for. An opened agent gets the findings themselves: a
//! reviewer the ones it raised, a worker the ones it was sent back with.

use argonauts_core::TaskId;
use argonauts_core::display_names::builtin_cli_name;
use argonauts_core::events::run::{RunLifecycle, WaitReason};
use argonauts_core::events::session::EndReason;
use argonauts_core::events::task::{ReviewEvent, TaskEvent, TaskState, TryCause};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::sanitize::sanitize_for_terminal;
use argonauts_core::session_runner::Access;
use argonauts_core::steps::StepName;
use argonauts_core::steps::judgment::{ReviewOutput, ReviewVerdict};
use argonauts_core::steps::plan::Severity;
use argonauts_core::views::{
    EarlierFindings, EarlierVerdict, Finding, Findings, ReviewGiven, RowNote, Tone,
    writes_the_message,
};
use argonauts_crew::claude_codes::review_of;
use serde_json::Value;
use time::OffsetDateTime;

use super::sessions::{RunFacts, SessionFacts};

/// One review as the log has it.
struct Reviewed {
    task: Option<TaskId>,
    session: Option<SessionKey>,
    reviewer: ModelRef,
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
    /// When each task went on without another round of work: to you, to an end, or merged.
    went_on: Vec<(TaskId, OffsetDateTime)>,
    /// The tasks that merged.
    merged: Vec<TaskId>,
    /// The run is a council's, whose agents that only read propose and challenge.
    council: bool,
}

impl Verdicts {
    pub(crate) fn of(events: &[RunEvent]) -> Self {
        let mut verdicts = Self {
            reviews: Vec::new(),
            checks: Vec::new(),
            tries: Vec::new(),
            worktrees: Vec::new(),
            went_on: Vec::new(),
            merged: Vec::new(),
            council: false,
        };
        for event in events {
            match (&event.kind, &event.task_id) {
                (RunEventKind::Run(RunLifecycle::Started(started)), _) => {
                    verdicts.council = started.workflow == COUNCIL;
                }
                (
                    RunEventKind::Review(ReviewEvent::Reviewed {
                        reviewer,
                        round,
                        review,
                        ..
                    }),
                    task,
                ) => {
                    verdicts.reviews.push(Reviewed {
                        task: task.clone(),
                        session: event.session.clone(),
                        reviewer: reviewer.clone(),
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
                (RunEventKind::Task(TaskEvent::State { to, .. }), Some(task))
                    if goes_on_without_a_round(*to) =>
                {
                    verdicts.went_on.push((task.clone(), event.ts));
                    if *to == TaskState::Merged {
                        verdicts.merged.push(task.clone());
                    }
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
        // A script asks its reviews of read-only agents with no task, which answer the same way.
        let asked_by_script = session.started.role == RoleName::Worker
            && session.started.access == Access::ReadOnly
            && session.task.is_none();
        let answer = session.ended.as_ref()?.structured.as_ref()?;
        // Only an answer with a yes or no is a verdict: a judge that decides answers with a plan.
        let verdict = answer.get("accept").is_some_and(Value::is_boolean);
        ((asked || asked_by_script) && verdict).then(|| review_of(answer))
    }

    /// What one of a council's agents gave, by its answer: the proposal a judge chose, what a
    /// challenge argued against, or a proposal. A judge's choice reads so in any workflow.
    pub(crate) fn gave(&self, session: &SessionFacts) -> Option<RowNote> {
        let end = session.ended.as_ref()?;
        if end.reason != EndReason::Completed {
            return None;
        }
        let answer = end.structured.as_ref();
        let text = |key: &str| {
            answer?
                .get(key)?
                .as_str()
                .map(str::trim)
                .filter(|text| !text.is_empty())
        };
        if let Some(chose) = text("chose") {
            return Some(RowNote::new(format!("chose {chose}"), Tone::Text));
        }
        let started = &session.started;
        let proposes = self.council
            && started.role == RoleName::Worker
            && started.access == Access::ReadOnly
            && session.task.is_none();
        if !proposes {
            return None;
        }
        // A challenge names what it argues against, none at all among them; a proposal doesn't.
        let Some(against) = answer.and_then(|answer| answer.get("against")?.as_array()) else {
            return Some(RowNote::new("proposal ready", Tone::Good));
        };
        let against: Vec<&str> = against.iter().filter_map(Value::as_str).collect();
        let points = answer
            .and_then(|answer| answer.get("points")?.as_array())
            .map_or(0, Vec::len);
        let points = match points {
            1 => "1 point".to_owned(),
            points => format!("{points} points"),
        };
        let named = if against.is_empty() {
            "none".to_owned()
        } else {
            and_list(&against)
        };
        Some(RowNote::new(
            format!("against {named} · {points}"),
            Tone::Plain,
        ))
    }

    /// What the row of a review handed over through a CLI's own workflow says: the verdict,
    /// with what blocks it in the verdict's colour and its notes dim after. While the agent that
    /// asked still has the answer, the verdict alone, and that it is passing it on.
    pub(crate) fn handed_back(&self, session: &SessionFacts, passing_on: bool) -> Option<RowNote> {
        let review = self.review_by(session)?;
        let (verdict, tone) = verdict_words(review.verdict);
        if passing_on {
            return Some(RowNote::new(verdict, tone).then("passing it on"));
        }
        let blocking = review
            .comments
            .iter()
            .filter(|comment| comment.severity == Severity::High)
            .count();
        let text = match blocking {
            0 => verdict.to_owned(),
            count => format!("{verdict} · {count} blocking"),
        };
        let note = RowNote::new(text, tone);
        Some(match review.comments.len() - blocking {
            0 => note,
            1 => note.then("1 note"),
            count => note.then(format!("{count} notes")),
        })
    }

    /// What `session`'s row says after its counts, when there is something to say.
    pub(crate) fn note(&self, facts: &RunFacts, session: &SessionFacts) -> Option<RowNote> {
        if let Some(end) = &session.ended {
            match end.reason {
                // A run that ended after it cut the session short stopped with it.
                EndReason::Paused if facts.finished_at.is_some() => {
                    return Some(RowNote::new(
                        "stopped with the run · work so far kept",
                        Tone::Warn,
                    ));
                }
                EndReason::Paused => return Some(cut_short(facts.held.as_ref(), &session.worker)),
                EndReason::Stopped => return Some(RowNote::new("stopped", Tone::Plain)),
                _ => {}
            }
        }
        if let Some(review) = self.review_by(session) {
            let (verdict, tone) = verdict_words(review.verdict);
            if self.no_round_followed(facts, session) {
                return Some(RowNote::new(format!("{verdict} · no rounds left"), tone));
            }
            // A sign-off about no one task read the change in a folder that holds it.
            let signs_off = session.started.role == RoleName::Judge && session.task.is_none();
            if signs_off && review.verdict == ReviewVerdict::Approve && review.comments.is_empty() {
                let read = if self.worktrees.len() > 1 {
                    "read every task's change"
                } else {
                    "read the task's change"
                };
                return Some(RowNote::new(read, Tone::Good));
            }
            let text = match review.comments.len() {
                0 => verdict.to_owned(),
                1 => format!("{verdict} · 1 finding"),
                count => format!("{verdict} · {count} findings"),
            };
            return Some(RowNote::new(text, tone));
        }
        if let Some(gave) = self.gave(session) {
            return Some(gave);
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

    /// Whether `session` is one of the run's own reviewers whose latest word sent its task's
    /// work back: its next round is coming, so it isn't done yet. A review a CLI's own workflow
    /// asked for is done once given, since that workflow asks again with a new agent.
    pub(crate) fn sent_back(&self, session: &SessionFacts) -> bool {
        session.task.is_some()
            && matches!(session.started.role, RoleName::Reviewer | RoleName::Judge)
            && self
                .review_by(session)
                .is_some_and(|review| review.verdict == ReviewVerdict::RequestChanges)
    }

    /// Whether the latest review `session` gave sent its task's work back and no round followed
    /// it: no worker took the task up again, and the task went on without one or the run ended.
    pub(crate) fn no_round_followed(&self, facts: &RunFacts, session: &SessionFacts) -> bool {
        let Some(task) = session.task.as_ref() else {
            return false;
        };
        let Some(sent_back) = self
            .reviews
            .iter()
            .rev()
            .find(|review| review.session.as_ref() == Some(&session.key))
            .filter(|review| review.review.verdict == ReviewVerdict::RequestChanges)
        else {
            return false;
        };
        let taken_up = facts.sessions.iter().any(|other| {
            other.task.as_ref() == Some(task) && builds(other) && other.started_at >= sent_back.at
        });
        let went_on = facts.finished_at.is_some()
            || self
                .went_on
                .iter()
                .any(|(known, at)| known == task && *at >= sent_back.at);
        matches!(session.started.role, RoleName::Reviewer | RoleName::Judge) && !taken_up && went_on
    }

    /// Whether `task` merged.
    pub(crate) fn merged(&self, task: &TaskId) -> bool {
        self.merged.contains(task)
    }

    /// For a reviewer, the latest earlier review of the same work that sent it back, whose
    /// findings come with the change it is given. A judge weighs the work with the latest
    /// review of its task, whatever that review said. `task` is the one the session works on,
    /// which for a session a CLI's workflow asked for is that workflow's.
    pub(crate) fn earlier(
        &self,
        facts: &RunFacts,
        session: &SessionFacts,
        task: Option<&TaskId>,
    ) -> Option<EarlierFindings> {
        let any_verdict = match session.started.role {
            RoleName::Judge => task.is_some(),
            RoleName::Reviewer => false,
            _ => return None,
        };
        let latest = self.reviews.iter().rev().find(|review| {
            review.task.as_ref() == task
                && review.at <= session.started_at
                && review.session.as_ref() != Some(&session.key)
                && (any_verdict || review.review.verdict == ReviewVerdict::RequestChanges)
        })?;
        let reviewer = latest.session.as_ref().and_then(|key| facts.session(key));
        let from = reviewer.map_or_else(|| "its review".to_owned(), SessionFacts::label);
        // A review a judge is given may have found nothing, so what it said comes with it.
        let verdict = any_verdict.then(|| {
            let (said, tone) = verdict_words(latest.review.verdict);
            let summary = sanitize_for_terminal(latest.review.summary.trim());
            EarlierVerdict {
                said: said.to_owned(),
                tone,
                by: reviewer.map_or_else(|| latest.reviewer.clone(), |by| by.worker.clone()),
                summary: (!summary.is_empty()).then_some(summary),
            }
        });
        Some(EarlierFindings {
            from,
            round: latest.round,
            answered_by: None,
            items: findings(&latest.review),
            answers: Vec::new(),
            verdict,
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

/// Whether a task in state `to` goes on without another round of work: it came to you, ended,
/// or merged.
fn goes_on_without_a_round(to: TaskState) -> bool {
    matches!(
        to,
        TaskState::NeedsDecision
            | TaskState::NeedsYou
            | TaskState::Failed
            | TaskState::Deferred
            | TaskState::Cancelled
            | TaskState::TakenOver
            | TaskState::Merged
    )
}

/// Said of the merger's session on a clean merge: the change was in its prompt, so it read
/// nothing and ran nothing.
const GIVEN_THE_DIFF: &str = "given the diff, no tools";

/// Whether `session` wrote the commit message from the diff alone, and is the only agent the
/// merge needed. One that also settled a conflict or fixed a check has rows that say more.
fn wrote_only_the_message(facts: &RunFacts, session: &SessionFacts) -> bool {
    let started = &session.started;
    writes_the_message(started.step)
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

/// The built-in workflow whose agents propose, challenge and decide.
const COUNCIL: &str = "council";

/// `p2`, `p2 and p3`, `p1, p2 and p3`.
fn and_list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
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

/// The row of a session the run's wait cut short, by what the run waits for: only a pause you
/// asked for reads "paused".
fn cut_short(held: Option<&WaitReason>, worker: &ModelRef) -> RowNote {
    match held {
        Some(WaitReason::NoFallback { .. }) => {
            RowNote::new("stopped with the run · work so far kept", Tone::Warn)
        }
        Some(WaitReason::AllAgentsOut { .. } | WaitReason::WaitingForAgent { .. }) => {
            let cli = builtin_cli_name(&worker.agent).unwrap_or(worker.agent.as_str());
            RowNote::new(format!("waits for {cli} · work so far kept"), Tone::Warn)
        }
        _ => RowNote::new("paused · work so far kept", Tone::Warn),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::names::ModelRef;
    use argonauts_core::steps::judgment::ReviewComment;

    use super::*;
    use crate::client::test_support::{event, session_started, task_event};
    use argonauts_core::events::run::{RunLifecycle, RunOutcome};

    fn sent_back(seq: u64, session: &SessionKey, task: Option<&TaskId>) -> RunEvent {
        let review = ReviewOutput {
            verdict: ReviewVerdict::RequestChanges,
            summary: "Due dates can be in the past".to_owned(),
            comments: vec![ReviewComment {
                path: "src/cli.js".to_owned(),
                line: Some(42),
                severity: Severity::High,
                text: "--due with no date isn't rejected".to_owned(),
            }],
            tamper_suspected: false,
        };
        let mut reviewed = event(
            seq,
            RunEventKind::Review(ReviewEvent::Reviewed {
                reviewer: ModelRef::new("codex", None),
                blind: false,
                round: 1,
                review,
            }),
        );
        reviewed.session = Some(session.clone());
        reviewed.task_id = task.cloned();
        reviewed
    }

    /// A session the run's wait cut short reads "paused" only when you paused the run; one the
    /// run's full stop cut reads "stopped with the run", and one an outage cut waits for its CLI.
    #[test]
    fn a_session_cut_short_says_what_the_run_waits_for() {
        use argonauts_core::events::run::WaitReason;
        use argonauts_core::events::session::EndReason;

        let worker = SessionKey::numbered(1);
        let said = |reason: WaitReason| {
            let events = vec![
                session_started(0, &worker, None, RoleName::Worker, "claude-code"),
                event(
                    1,
                    RunEventKind::Run(RunLifecycle::Waiting {
                        reason,
                        waiting_on: Vec::new(),
                    }),
                ),
                crate::client::test_support::session_ended(2, &worker, EndReason::Paused, 1000),
            ];
            let facts = RunFacts::of(&events);
            let session = facts.session(&worker).unwrap();
            facts.verdicts.note(&facts, session).map(|note| note.text)
        };
        assert_eq!(
            said(WaitReason::Paused).as_deref(),
            Some("paused · work so far kept")
        );
        let stopped = WaitReason::NoFallback {
            detail: "nobody to fall back to as the task's worker".to_owned(),
            until: None,
            stop: None,
        };
        assert_eq!(
            said(stopped).as_deref(),
            Some("stopped with the run · work so far kept")
        );
        assert_eq!(
            said(WaitReason::AllAgentsOut { until: None }).as_deref(),
            Some("waits for Claude Code · work so far kept")
        );
    }

    /// A review that sent the work back says no rounds are left once its task went on without
    /// another round, or the run ended without one; once its worker takes the task up again,
    /// it says what it found.
    #[test]
    fn a_send_back_that_no_round_follows_says_no_rounds_left() {
        let task = TaskId::new("task-001").unwrap();
        let (worker, reviewer) = (SessionKey::numbered(1), SessionKey::numbered(2));
        let events = vec![
            session_started(0, &worker, Some(&task), RoleName::Worker, "claude-code"),
            session_started(1, &reviewer, Some(&task), RoleName::Reviewer, "codex"),
            sent_back(2, &reviewer, Some(&task)),
        ];
        let said = |events: &[RunEvent]| {
            let facts = RunFacts::of(events);
            let session = facts.session(&reviewer).unwrap();
            facts.verdicts.note(&facts, session).map(|note| note.text)
        };
        assert_eq!(said(&events).as_deref(), Some("sent it back · 1 finding"));

        let mut to_you = events.clone();
        to_you.push(task_event(
            3,
            &task,
            TaskEvent::State {
                from: Some(TaskState::InReview),
                to: TaskState::NeedsYou,
                reason: "review asked for changes again after 2 rounds".to_owned(),
            },
        ));
        assert_eq!(
            said(&to_you).as_deref(),
            Some("sent it back · no rounds left")
        );

        let mut ended = events.clone();
        ended.push(event(
            3,
            RunEventKind::Run(RunLifecycle::Finished {
                outcome: RunOutcome::BranchLeft,
                detail: None,
            }),
        ));
        assert_eq!(
            said(&ended).as_deref(),
            Some("sent it back · no rounds left")
        );

        let mut again = events;
        again.push(session_started(
            3,
            &SessionKey::numbered(3),
            Some(&task),
            RoleName::Worker,
            "claude-code",
        ));
        again.extend(ended.pop());
        assert_eq!(said(&again).as_deref(), Some("sent it back · 1 finding"));
    }

    /// One of the run's own reviewers that sent its task's work back has a round to come; one
    /// a CLI's own workflow asked, with no task of its own, is done once it answered.
    #[test]
    fn only_the_runs_own_reviewer_waits_for_its_next_round() {
        let task = TaskId::new("task-001").unwrap();
        let (own, asked) = (SessionKey::numbered(1), SessionKey::numbered(2));
        let events = vec![
            session_started(0, &own, Some(&task), RoleName::Reviewer, "codex"),
            sent_back(1, &own, Some(&task)),
            session_started(2, &asked, None, RoleName::Reviewer, "codex"),
            sent_back(3, &asked, None),
        ];
        let facts = RunFacts::of(&events);
        let waits = |key: &SessionKey| facts.verdicts.sent_back(facts.session(key).unwrap());
        assert!(waits(&own));
        assert!(!waits(&asked));
    }
}
