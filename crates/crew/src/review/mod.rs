//! Review: each reviewer runs `Review` on Argonauts Code's diff, the check results, and the
//! acceptance tests, never the worker's notes; blind reviewers never see each other before
//! the judge settles them. Disagreement among two or more reviewers goes to a fresh judge.

use std::path::Path;

use futures::future::join_all;

use argonauts_core::TaskId;
use argonauts_core::agents::session_runner::{Access, RunnerKind};
use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::task::{PickedBy, ReviewEvent, TaskKind};
use argonauts_core::workflows::steps::Step;
use argonauts_core::workflows::steps::judgment::{
    Review, ReviewInput, ReviewOutput, ReviewVerdict,
};
use argonauts_core::workflows::workflow::StepModels;

use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::{Ask, Host};
use crate::engine::job_fallback::{self, Job};
use crate::plan::context::{self, agent_count, candidates_for, providers};
use crate::prompts::review::REVIEW;
use crate::state::RunState;
use crate::state::tasks::TaskEntry;
use crate::task::at_start;
use crate::workflow::kinds::item_task;
use crate::workflow::{StepCall, StepDone, authors, runner};

pub mod diff_cut;
pub mod endpoint_tests;
mod ladder_check;
pub mod reviewers;
pub mod settle;
mod together;

/// What a task's review came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Approved,
    /// The comments go back to the worker's session.
    ChangesRequested {
        comments: Vec<String>,
    },
    /// A reviewer or the judge rejected the approach, or review couldn't happen: you or
    /// your rules decide.
    NeedsYou {
        reasons: Vec<String>,
    },
}

impl Verdict {
    pub fn line(&self) -> String {
        match self {
            Self::Approved => "Approved".to_owned(),
            Self::ChangesRequested { comments } => {
                format!("Changes requested ({} comments)", comments.len())
            }
            Self::NeedsYou { reasons } => format!("Needs you: {}", reasons.join("; ")),
        }
    }

    /// What the reviewers said, one finding after another on one line, for the reason a task
    /// stops with. Empty for an approval.
    pub fn findings(&self) -> String {
        let said: &[String] = match self {
            Self::Approved => &[],
            Self::ChangesRequested { comments } => comments,
            Self::NeedsYou { reasons } => reasons,
        };
        said.iter()
            .map(|finding| finding.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|finding| !finding.is_empty())
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// `what`, then the reviewers' findings after a colon when they said any.
pub(crate) fn with_findings(what: String, verdict: &Verdict) -> String {
    match verdict.findings() {
        findings if findings.is_empty() => what,
        findings => format!("{what}: {findings}"),
    }
}

/// `review`: a task's review inside `build`, or a review of the step's input against criteria.
pub async fn review_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    match task_of(host, call) {
        Some(task) => {
            let excluded = excluded_providers(host, call);
            let round = host.read(|state| step_round(state, call, &task));
            let verdict = review_task_among(host, &task, round, &excluded).await?;
            Ok(StepDone::done(verdict.line()))
        }
        None => review_input(host, call).await,
    }
}

/// The task a step works on, when its `for_each` item is a task spec or a task's id.
pub(crate) fn task_of(host: &Host, call: &StepCall) -> Option<TaskId> {
    let task = item_task(call.item.as_ref()?)?;
    host.read(|state| state.tasks.get(&task).is_some())
        .then_some(task)
}

/// The task's own review round for a workflow `review` step: the round after its newest, or
/// that newest one again when this step round already began asking for it.
fn step_round(state: &RunState, call: &StepCall, task: &TaskId) -> u32 {
    let newest = state.tasks.get(task).map_or(0, |record| {
        record
            .reviews
            .iter()
            .filter_map(|event| match event {
                ReviewEvent::Reviewed { round, .. } => Some(*round),
                _ => None,
            })
            .max()
            .unwrap_or(0)
    });
    let began = state
        .workflows
        .by_id
        .get(call.workflow())
        .and_then(|record| record.round(&call.at.step, call.at.round))
        .is_some_and(|progress| {
            state.sessions.by_key.values().any(|session| {
                session.task.as_ref() == Some(task)
                    && session.started.role == RoleName::Reviewer
                    && session.since > progress.since
            })
        });
    if began && newest > 0 {
        newest
    } else {
        newest + 1
    }
}

/// Reviews a task that passed its checks: the task's reviewers, then the judge when they
/// disagree. `round` counts change rounds from 1.
pub async fn review_task(host: &Host, task: &TaskId, round: u32) -> EngineResult<Verdict> {
    review_task_among(host, task, round, &[]).await
}

/// `review_task` without reviewers of the `excluded` providers, besides the worker's.
async fn review_task_among(
    host: &Host,
    task: &TaskId,
    round: u32,
    excluded: &[String],
) -> EngineResult<Verdict> {
    if let Some(checker) = ladder_check::checker(host, task)? {
        return ladder_check::take(host, task, round, &checker, None).await;
    }
    let Some(reviewers) = reviewers_among(host, task, excluded) else {
        return Ok(Verdict::NeedsYou {
            reasons: vec![reviewers::NONE_SET.to_owned()],
        });
    };
    review_by(host, task, round, &reviewers).await
}

/// The task's reviewers by the roles the run settled on, so a rule the run eased, such as
/// review by another provider in a crew of one, is eased here too. Without the `excluded`
/// providers, besides the worker's. `None` when no reviewer is set at all.
fn reviewers_among(host: &Host, task: &TaskId, excluded: &[String]) -> Option<Vec<ModelRef>> {
    let provider = providers(host);
    let settled = crate::roles::tasks::settled(host);
    let other_provider = settled.review.other_provider;
    let crew = crate::roles::candidates(&settled, RoleName::Reviewer);
    if host.read(|state| reviewers::none_set(state, task, &crew)) {
        return None;
    }
    let mut reviewers =
        host.read(|state| reviewers::for_task(state, task, &provider, other_provider, &crew));
    reviewers.retain(|reviewer| !excluded.contains(&provider(reviewer)));
    Some(reviewers)
}

/// Why nobody may review the task: each reviewer set is its builder or, while review by
/// another provider holds, of its builder's provider. `None` when someone may, or when no
/// reviewer is set at all.
pub(crate) fn nobody_to_review(host: &Host, task: &TaskId) -> Option<String> {
    // A council's ladder always ends at a fresh session of the task's own model.
    if ladder_check::checker(host, task).ok().flatten().is_some() {
        return None;
    }
    let reviewers = reviewers_among(host, task, &[])?;
    reviewers.is_empty().then(|| nobody_left_line(host, task))
}

/// Whether the task's work is reviewed: plain Captain runs have no reviewer, and every other
/// task is reviewed by someone besides its builder.
pub(crate) fn needed(host: &Host, record: &TaskEntry) -> bool {
    !(record.spec.kind == TaskKind::Captain
        && record.spec.reviewers.is_empty()
        && !host.settings().captain.review)
}

/// Why the task has no reviewer once its builder, and under the run's review rule its
/// builder's provider, are left out, naming it.
pub(crate) fn nobody_left_line(host: &Host, task: &TaskId) -> String {
    let label = host.read(|state| crate::sessions::labels::task_label(state, task));
    let other_provider = crate::roles::tasks::settled(host).review.other_provider;
    reviewers::nobody_left(&label, other_provider)
}

/// Reviews the task's work with exactly these reviewers, even one that owns the task: the
/// model a quality debt waits for, say. `round` must be one no other review of the task used.
pub async fn review_by(
    host: &Host,
    task: &TaskId,
    round: u32,
    reviewers: &[ModelRef],
) -> EngineResult<Verdict> {
    review_in(host, task, round, reviewers, None, Reader::Reviewer).await
}

/// Reviews a race try's work in `tree` before it becomes the task's. A try is never
/// settled: it passes only when every reviewer approves, since the other tries compete.
pub async fn review_try(
    host: &Host,
    task: &TaskId,
    round: u32,
    reviewers: &[ModelRef],
    tree: &Path,
) -> EngineResult<Verdict> {
    review_in(host, task, round, reviewers, Some(tree), Reader::Reviewer).await
}

pub(crate) use ladder_check::take as checked_by;

/// Who reads a task's change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reader {
    /// The task's reviewers, by the run's review rules.
    Reviewer,
    /// A council's check, which code picked by the ladder and which reads the council's frame.
    CouncilCheck,
}

/// The task's work in `tree`, or in its own worktree when `tree` is `None`.
async fn review_in(
    host: &Host,
    task: &TaskId,
    round: u32,
    reviewers: &[ModelRef],
    tree: Option<&Path>,
    reader: Reader,
) -> EngineResult<Verdict> {
    let (record, goal, decisions, tests) = host.read(|state| {
        (
            state.tasks.get(task).cloned(),
            context::goal_text(state),
            context::decision_lines(state),
            acceptance_tests(state, task),
        )
    });
    let Some(record) = record else {
        return Err(EngineError::Refused(format!(
            "{task} is not a task of this run"
        )));
    };
    if reviewers.is_empty() {
        return Ok(Verdict::NeedsYou {
            reasons: vec![nobody_left_line(host, task)],
        });
    }
    let workdir = tree
        .map(Path::to_path_buf)
        .or_else(|| record.worktree.clone());
    let settings = host.settings();
    let blind = settings.review.blind
        || (reviewers.len() > 1 && settings.visibility.reviews_hidden_until_settled);
    let mut reviews = host.read(|state| reviews_of_round(state, &record, round, reviewers));
    // A fallback may not come from the worker's provider, as no reviewer may.
    let worker = record.worker.clone().or_else(|| record.spec.worker.clone());
    let (chunks, cut_note) = diff_cut::cut(
        &work_diff(host, workdir.as_deref(), record.base.as_deref()).await?,
        settings.review.max_diff_lines,
    );
    let mut diff = chunks.concat();
    if let Some(note) = cut_note {
        diff.push_str(&format!("\n{note}\n"));
    }
    let input = ReviewInput {
        goal,
        task: format!("{}\n\n{}", record.spec.title, record.spec.task),
        diff,
        checks: record
            .checks
            .iter()
            .map(|check| {
                let result = if check.passed { "passed" } else { "failed" };
                format!("{} {result}: {}", check.name, check.summary)
            })
            .chain(host.read(at_start::review_lines))
            .collect(),
        acceptance_tests: tests,
        decisions,
        round,
    };
    let at = host.read(|state| runner::review_at(state, task));
    let ask_of = |reviewer: &ModelRef| {
        let mut ask = Ask::new(
            at.clone(),
            RoleName::Reviewer,
            reviewer.clone(),
            REVIEW,
            input.clone(),
        );
        ask.task = Some(task.clone());
        ask.blind = blind;
        ask.slot = round;
        ask.independent_of = worker.iter().cloned().collect();
        if reader == Reader::CouncilCheck {
            ask.parts.push(ladder_check::council_frame());
            ask.without_status = true;
        }
        if is_endpoint(host, reviewer) {
            ask.access = Access::NoTools;
        } else {
            ask.access = Access::ReadOnly;
            ask.workdir = workdir.clone();
        }
        ask
    };
    // A reviewer's job, which fallbacks that take it together must fit as one alone would.
    let job = Job {
        at: at.clone(),
        role: RoleName::Reviewer,
        step: Some(Review::NAME),
        task: Some(task.clone()),
        independent_of: worker.iter().cloned().collect(),
        isolated: blind,
    };
    // Each reviewer has its own job before any is asked, so none becomes the fallback of another
    // too.
    for reviewer in reviewers {
        job_fallback::hold(host, &job, reviewer);
    }
    // Who is asked, and in whose place: each reviewer still to review, or several fallbacks
    // at once in the place of one that's out already, and those that join a fallback that
    // took a reviewer's place midway before the step was entered again.
    let mut asked: Vec<(ModelRef, ModelRef)> = Vec::new();
    for reviewer in reviewers {
        match reviews.iter().find(|(asked, _, _)| asked == reviewer) {
            None => {
                let by = together::from_the_start(host, &job, reviewer).await;
                if by.len() < 2 {
                    asked.push((reviewer.clone(), reviewer.clone()));
                } else {
                    together::say_who_reviews(host, task, round, reviewer, &by, &[])?;
                    asked.extend(by.into_iter().map(|by| (reviewer.clone(), by)));
                }
            }
            Some((_, first, _)) if first != reviewer => {
                let more = together::joining(host, &job, reviewer, first);
                together::say_who_reviews(host, task, round, reviewer, &more, &[first])?;
                asked.extend(more.into_iter().map(|by| (reviewer.clone(), by)));
            }
            Some(_) => {}
        }
    }
    let mut stalls = Vec::new();
    while !asked.is_empty() {
        let answers = join_all(asked.iter().map(|(reviewer, by)| {
            // A fallback asked in a reviewer's place reviews under its own name, so entered again
            // its review is found by that name.
            let given = (by != reviewer)
                .then(|| together::given(host, task, round, by))
                .flatten();
            let ask = ask_of(by);
            async move {
                let answer = match given {
                    Some(review) => Ok((None, review)),
                    None => host.ask::<Review>(ask).await.map(|answered| {
                        let review = answered.output.clone();
                        (Some(answered), review)
                    }),
                };
                (reviewer, by, answer)
            }
        }))
        .await;
        let mut joining = Vec::new();
        for (reviewer, by, answer) in answers {
            match answer {
                Ok((None, review)) => reviews.push((reviewer.clone(), by.clone(), review)),
                Ok((Some(answered), review)) => {
                    // Under the name of whoever reviewed, a fallback included.
                    let picked_by =
                        picked_for(host, RoleName::Reviewer, reviewer, &answered.worker, false);
                    host.record(
                        Some(task),
                        Some(&answered.session),
                        RunEventKind::Review(ReviewEvent::Reviewed {
                            reviewer: answered.worker.clone(),
                            blind,
                            round,
                            review: answered.output,
                            picked_by: Some(picked_by),
                        }),
                    )?;
                    // A fallback took the reviewer's own place midway, so the rest of its
                    // fallbacks join it.
                    if by == reviewer && answered.worker != *reviewer {
                        let first = &answered.worker;
                        let more = together::joining(host, &job, reviewer, first);
                        together::say_who_reviews(host, task, round, reviewer, &more, &[first])?;
                        joining.extend(more.into_iter().map(|by| (reviewer.clone(), by)));
                    }
                    reviews.push((reviewer.clone(), answered.worker, review));
                }
                Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
                // Nobody else may take that reviewer's place, so the review goes on without it.
                Err(EngineError::Stalled(stall)) => stalls.push(stall),
                Err(error) => {
                    return Ok(Verdict::NeedsYou {
                        reasons: vec![format!("{by}'s review failed: {error}")],
                    });
                }
            }
        }
        asked = joining;
    }
    if reviews.is_empty()
        && let Some(stall) = stalls.first()
    {
        return Err(EngineError::Stalled(stall.clone()));
    }
    for stall in stalls {
        let text = format!("{} The review goes on without that reviewer.", stall.line);
        context::warn(host, Some(task), text)?;
    }
    let outputs = together::counted_by_reviewer(host, reviews);
    if tree.is_some() {
        return Ok(strictest(&outputs));
    }
    if outputs.len() > 1 && !agree(&outputs) {
        return settle::settle(host, task).await;
    }
    Ok(combine(&outputs))
}

/// Who chose `model` for `role` in this run, by its settings.
pub(crate) fn picked(
    host: &Host,
    role: RoleName,
    model: &ModelRef,
    named_by_step: bool,
) -> PickedBy {
    reviewers::picked_by(host.settings(), role, model, named_by_step)
}

/// Who chose the model that answered a `role` call asked of `asked`: [`picked`] when the model
/// asked answered, and code when a fallback answered in its place.
pub(crate) fn picked_for(
    host: &Host,
    role: RoleName,
    asked: &ModelRef,
    answered: &ModelRef,
    named_by_step: bool,
) -> PickedBy {
    if reviewers::answered_as_asked(asked, answered) {
        picked(host, role, asked, named_by_step)
    } else {
        PickedBy::Code
    }
}

/// Reviews these reviewers already recorded for this round, so a re-entered review asks only
/// the rest: in whose place, who reviewed, and the review. A fallback's review counts for the
/// reviewer whose place it took.
fn reviews_of_round(
    state: &RunState,
    record: &TaskEntry,
    round: u32,
    reviewers: &[ModelRef],
) -> Vec<(ModelRef, ModelRef, ReviewOutput)> {
    record
        .reviews
        .iter()
        .filter_map(|event| match event {
            ReviewEvent::Reviewed {
                reviewer,
                round: r,
                review,
                ..
            } if *r == round => {
                let asked = if reviewers.contains(reviewer) {
                    Some(reviewer.clone())
                } else {
                    fallback_for(state, &record.spec.id, reviewer, reviewers)
                };
                asked.map(|asked| (asked, reviewer.clone(), review.clone()))
            }
            _ => None,
        })
        .collect()
}

/// The reviewer of `reviewers` whose place on `task` `worker` took, its newest.
fn fallback_for(
    state: &RunState,
    task: &TaskId,
    worker: &ModelRef,
    reviewers: &[ModelRef],
) -> Option<ModelRef> {
    state
        .sessions
        .in_order()
        .into_iter()
        .rev()
        .filter(|session| {
            session.task.as_ref() == Some(task)
                && session.started.role == RoleName::Reviewer
                && session.started.worker == *worker
        })
        .map(|session| session.asked_worker().clone())
        .find(|asked| asked != worker && reviewers.contains(asked))
}

pub(crate) fn agree(reviews: &[ReviewOutput]) -> bool {
    reviews
        .windows(2)
        .all(|pair| pair[0].verdict == pair[1].verdict)
}

/// The verdict of reviewers who agree.
fn combine(reviews: &[ReviewOutput]) -> Verdict {
    match reviews.first().map(|r| r.verdict) {
        Some(ReviewVerdict::Approve) | None => Verdict::Approved,
        Some(ReviewVerdict::RequestChanges) => Verdict::ChangesRequested {
            comments: reviews.iter().flat_map(comment_lines).collect(),
        },
        Some(ReviewVerdict::Reject) => Verdict::NeedsYou {
            reasons: reviews.iter().map(|r| r.summary.clone()).collect(),
        },
    }
}

/// The most severe verdict among `reviews`: one rejection sends it to you, one request for changes
/// sends the work back with every such comment.
fn strictest(reviews: &[ReviewOutput]) -> Verdict {
    let with = |verdict| reviews.iter().filter(move |r| r.verdict == verdict);
    if with(ReviewVerdict::Reject).next().is_some() {
        return Verdict::NeedsYou {
            reasons: with(ReviewVerdict::Reject)
                .map(|r| r.summary.clone())
                .collect(),
        };
    }
    if with(ReviewVerdict::RequestChanges).next().is_some() {
        return Verdict::ChangesRequested {
            comments: with(ReviewVerdict::RequestChanges)
                .flat_map(comment_lines)
                .collect(),
        };
    }
    Verdict::Approved
}

fn comment_lines(review: &ReviewOutput) -> Vec<String> {
    let mut lines = vec![review.summary.clone()];
    lines.extend(review.comments.iter().map(|c| match c.line {
        Some(line) => format!("{}:{line}: {}", c.path, c.text),
        None => format!("{}: {}", c.path, c.text),
    }));
    lines
}

/// Paths of the acceptance tests written for `task`.
fn acceptance_tests(state: &RunState, task: &TaskId) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for record in state.tasks.by_id.values() {
        let TaskKind::AcceptanceTests { for_task } = &record.spec.kind else {
            continue;
        };
        if for_task != task {
            continue;
        }
        let written = record.reviews.iter().rev().find_map(|event| match event {
            ReviewEvent::TestsWritten { files, .. } => Some(files.clone()),
            _ => None,
        });
        match written {
            Some(files) => paths.extend(files),
            None => paths.extend(record.spec.owned_paths.iter().cloned()),
        }
    }
    paths
}

/// The work in `tree` against the task's base, computed by Argonauts Code.
async fn work_diff(host: &Host, tree: Option<&Path>, base: Option<&str>) -> EngineResult<String> {
    match (tree, base) {
        (Some(tree), Some(base)) => Ok(host.git().review_diff(tree, base, "HEAD").await?),
        _ => Ok(String::new()),
    }
}

pub(crate) fn is_endpoint(host: &Host, worker: &ModelRef) -> bool {
    host.services()
        .workers
        .get(worker)
        .is_ok_and(|w| w.kind() == RunnerKind::Endpoint)
}

/// A review of the step's input, outside a task: reviewers from another provider than the
/// step's `different_provider_from`, blind when the step says so.
async fn review_input(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let settings = host.settings();
    let provider = providers(host);
    let excluded = excluded_providers(host, call);
    let count = agent_count(
        call,
        settings,
        RoleName::Reviewer,
        Some(settings.review.per_task.max(1)),
    );
    let reviewers: Vec<ModelRef> = candidates_for(call, settings, RoleName::Reviewer)
        .into_iter()
        .filter(|w| !excluded.contains(&provider(w)))
        .take(count.unwrap_or(1) as usize)
        .collect();
    if reviewers.is_empty() {
        return Ok(StepDone::skipped(
            "No reviewer from another provider is available",
        ));
    }
    let blind = call.step.blind || settings.review.blind;
    let text = context::step_input(host, call).await?;
    let (chunks, cut_note) = diff_cut::cut(&text, settings.review.max_diff_lines);
    let mut target = chunks.concat();
    if let Some(note) = cut_note {
        target.push_str(&format!("\n{note}\n"));
    }
    let input = host.read(|state| ReviewInput {
        goal: context::goal_text(state),
        task: call
            .step
            .task
            .clone()
            .unwrap_or_else(|| "Review the input below against the goal.".to_owned()),
        diff: target,
        checks: Vec::new(),
        acceptance_tests: Vec::new(),
        decisions: context::decision_lines(state),
        round: call.at.round,
    });
    // A fallback may not come from the provider of whoever made what's reviewed.
    let authors = host.read(|state| authors::excluded(state, settings, call));
    let asks = reviewers.iter().enumerate().map(|(slot, reviewer)| {
        let mut ask = Ask::new(
            Some(call.at.clone()),
            RoleName::Reviewer,
            reviewer.clone(),
            REVIEW,
            input.clone(),
        );
        ask.blind = blind;
        ask.slot = slot as u32;
        ask.independent_of = authors.clone();
        ask.access = if is_endpoint(host, reviewer) {
            Access::NoTools
        } else {
            Access::ReadOnly
        };
        async move { (reviewer, host.ask::<Review>(ask).await) }
    });
    let mut outputs = Vec::new();
    let mut stalls = Vec::new();
    for (reviewer, answer) in join_all(asks).await {
        match answer {
            Ok(answered) => {
                let named_by_step = matches!(
                    &call.step.models,
                    StepModels::Named(models) if models.contains(reviewer)
                );
                let picked_by = picked_for(
                    host,
                    RoleName::Reviewer,
                    reviewer,
                    &answered.worker,
                    named_by_step,
                );
                host.record(
                    None,
                    Some(&answered.session),
                    RunEventKind::Review(ReviewEvent::Reviewed {
                        reviewer: answered.worker.clone(),
                        blind,
                        round: call.at.round,
                        review: answered.output.clone(),
                        picked_by: Some(picked_by),
                    }),
                )?;
                outputs.push(answered.output);
            }
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            Err(EngineError::Stalled(stall)) => stalls.push(stall),
            Err(error) => {
                return Ok(StepDone {
                    end: argonauts_core::runs::events::workflow::StepEnd::Failed,
                    summary: Some(format!("{reviewer}'s review failed: {error}")),
                    flow: crate::workflow::Flow::Continue,
                });
            }
        }
    }
    if outputs.is_empty()
        && let Some(stall) = stalls.first()
    {
        return Err(EngineError::Stalled(stall.clone()));
    }
    for stall in stalls {
        let text = format!("{} The review goes on without that reviewer.", stall.line);
        context::warn(host, None, text)?;
    }
    let summary = if agree(&outputs) {
        format!(
            "{} reviewers agreed: {}",
            outputs.len(),
            combine(&outputs).line()
        )
    } else {
        "The reviewers disagree".to_owned()
    };
    Ok(StepDone::done(summary))
}

/// The providers `different_provider_from` leaves out (`workflow::authors`).
fn excluded_providers(host: &Host, call: &StepCall) -> Vec<String> {
    let workers = host.read(|state| authors::excluded(state, host.settings(), call));
    let mut excluded: Vec<String> = workers
        .iter()
        .map(|worker| crate::roles::provider(host, worker))
        .collect();
    excluded.sort();
    excluded.dedup();
    excluded
}

#[cfg(test)]
mod tests {
    use argonauts_core::workflows::steps::judgment::ReviewComment;
    use argonauts_core::workflows::steps::plan::Severity;

    use super::*;

    fn review(verdict: ReviewVerdict, summary: &str) -> ReviewOutput {
        ReviewOutput {
            verdict,
            summary: summary.to_owned(),
            comments: vec![ReviewComment {
                path: "src/lib.rs".to_owned(),
                line: Some(3),
                severity: Severity::Medium,
                text: "handle the error".to_owned(),
            }],
            tamper_suspected: false,
        }
    }

    #[test]
    fn agreeing_reviews_combine() {
        let approve = [
            review(ReviewVerdict::Approve, "fine"),
            review(ReviewVerdict::Approve, "ok"),
        ];
        assert!(agree(&approve));
        assert_eq!(combine(&approve), Verdict::Approved);

        let changes = [review(ReviewVerdict::RequestChanges, "needs work")];
        assert_eq!(
            combine(&changes),
            Verdict::ChangesRequested {
                comments: vec![
                    "needs work".to_owned(),
                    "src/lib.rs:3: handle the error".to_owned()
                ]
            }
        );

        let reject = [review(ReviewVerdict::Reject, "wrong approach")];
        assert_eq!(
            combine(&reject),
            Verdict::NeedsYou {
                reasons: vec!["wrong approach".to_owned()]
            }
        );
    }

    #[test]
    fn different_verdicts_disagree() {
        let split = [
            review(ReviewVerdict::Approve, "fine"),
            review(ReviewVerdict::RequestChanges, "no"),
        ];
        assert!(!agree(&split));
    }

    /// A task that stops after its review says why in the reviewers' own words.
    #[test]
    fn a_stop_after_review_carries_the_reviewers_findings() {
        let asked = strictest(&[review(
            ReviewVerdict::RequestChanges,
            "The diff adds compiled\n  .pyc files.",
        )]);
        assert_eq!(
            with_findings(
                "review asked for changes again after 2 rounds".into(),
                &asked
            ),
            "review asked for changes again after 2 rounds: The diff adds compiled .pyc files.; \
             src/lib.rs:3: handle the error"
        );
        let rejected = strictest(&[review(ReviewVerdict::Reject, "Wrong approach.")]);
        assert_eq!(
            with_findings("the review sent it to you".into(), &rejected),
            "the review sent it to you: Wrong approach."
        );
        assert_eq!(
            with_findings("approved".into(), &Verdict::Approved),
            "approved"
        );
    }
}
