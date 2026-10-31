//! A fresh judge rules when two or more reviewers disagree.

use argonauts_core::TaskId;
use argonauts_core::agents::session_runner::Access;
use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::task::ReviewEvent;
use argonauts_core::workflows::steps::StepName;
use argonauts_core::workflows::steps::judgment::{
    ReviewOutput, ReviewVerdict, Settle, SettleInput, SettleOutput,
};

use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::{Ask, Host};
use crate::plan::context::{self, crew_for, providers};
use crate::prompts::review::SETTLE;
use crate::review::{Verdict, agree, diff_cut, picked, picked_for, task_of};
use crate::state::RunState;
use crate::workflow::{StepCall, StepDone};

pub async fn settle(host: &Host, task: &TaskId) -> EngineResult<Verdict> {
    let provider = providers(host);
    let (record, goal) =
        host.read(|state| (state.tasks.get(task).cloned(), context::goal_text(state)));
    let Some(record) = record else {
        return Err(EngineError::Refused(format!(
            "{task} is not a task of this run"
        )));
    };
    let mut round = 0;
    let mut reviews: Vec<(ModelRef, ReviewOutput)> = Vec::new();
    let mut ruled: Option<SettleOutput> = None;
    for event in &record.reviews {
        match event {
            ReviewEvent::Reviewed {
                reviewer,
                round: r,
                review,
                ..
            } => {
                if *r != round {
                    round = *r;
                    reviews.clear();
                    ruled = None;
                }
                reviews.push((reviewer.clone(), review.clone()));
            }
            ReviewEvent::Settled { ruling, .. } => ruled = Some(ruling.clone()),
            ReviewEvent::TestsWritten { .. } => {}
        }
    }
    if let Some(ruling) = ruled {
        return Ok(verdict(&ruling));
    }
    let worker = record.worker.clone().or_else(|| record.spec.worker.clone());
    let reviewers: Vec<ModelRef> = reviews.iter().map(|(r, _)| r.clone()).collect();
    let Some(judge) = pick(host, worker.as_ref(), &reviewers, &provider) else {
        return Ok(Verdict::NeedsYou {
            reasons: vec!["No judge outside the worker and reviewers".to_owned()],
        });
    };
    let diff = match (&record.worktree, &record.base) {
        (Some(worktree), Some(base)) => host.git().review_diff(worktree, base, "HEAD").await?,
        _ => String::new(),
    };
    let (chunks, note) = diff_cut::cut(&diff, host.settings().review.max_diff_lines);
    let input = SettleInput {
        goal,
        task: format!("{}\n\n{}", record.spec.title, record.spec.task),
        diff: chunks.concat() + &note.map(|n| format!("\n{n}\n")).unwrap_or_default(),
        reviews: reviews.into_iter().map(|(_, review)| review).collect(),
    };
    let mut ask = Ask::new(None, RoleName::Judge, judge.clone(), SETTLE, input);
    ask.task = Some(task.clone());
    ask.access = Access::NoTools;
    ask.blind = true;
    ask.slot = round;
    // A fallback rules only from outside the worker's and the reviewers' providers.
    ask.independent_of = worker.iter().cloned().chain(reviewers).collect();
    match host.ask::<Settle>(ask).await {
        Ok(answered) => {
            let verdict = verdict(&answered.output);
            // A judge you named counts as a reviewer you chose does.
            let picked_by = picked_for(host, RoleName::Judge, &judge, &answered.worker, false);
            host.record(
                Some(task),
                Some(&answered.session),
                RunEventKind::Review(ReviewEvent::Settled {
                    judge: answered.worker,
                    ruling: answered.output,
                    picked_by: Some(picked_by),
                }),
            )?;
            Ok(verdict)
        }
        Err(EngineError::Interrupted) => Err(EngineError::Interrupted),
        // Nobody may rule in its place, so the run waits until someone can.
        Err(EngineError::Stalled(stall)) => Err(EngineError::Stalled(stall)),
        Err(error) => Ok(Verdict::NeedsYou {
            reasons: vec![format!("The judge failed: {error}")],
        }),
    }
}

/// A fresh judge: never the worker or a reviewer, from another provider than the worker when
/// there is one.
fn pick(
    host: &Host,
    worker: Option<&ModelRef>,
    reviewers: &[ModelRef],
    provider: &dyn Fn(&ModelRef) -> String,
) -> Option<ModelRef> {
    let worker_provider = worker.map(provider);
    let free: Vec<ModelRef> = crew_for(host.settings(), RoleName::Judge)
        .into_iter()
        .filter(|w| Some(w) != worker && !reviewers.contains(w))
        .collect();
    free.iter()
        .find(|w| Some(provider(w)) != worker_provider)
        .or(free.first())
        .cloned()
}

fn verdict(ruling: &SettleOutput) -> Verdict {
    match ruling.verdict {
        ReviewVerdict::Approve => Verdict::Approved,
        ReviewVerdict::RequestChanges => Verdict::ChangesRequested {
            comments: ruling
                .required_changes
                .iter()
                .chain(&ruling.reasons)
                .cloned()
                .collect(),
        },
        ReviewVerdict::Reject => Verdict::NeedsYou {
            reasons: ruling.reasons.clone(),
        },
    }
}

/// `judge` outside a plan phase: rules on the newest disagreeing reviews, of the task the
/// step works on or of the workflow's last `review` step.
pub async fn step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    if let Some(task) = task_of(host, call) {
        let disagree = host.read(|state| {
            let reviews = latest_task_reviews(state, &task);
            reviews.len() > 1 && !agree(&reviews)
        });
        if !disagree {
            return Ok(StepDone::skipped("The reviewers agree"));
        }
        let verdict = settle(host, &task).await?;
        return Ok(StepDone::done(verdict.line()));
    }
    let (reviewers, reviews) = host.read(latest_step_reviews);
    if reviews.len() < 2 || agree(&reviews) {
        return Ok(StepDone::skipped("The reviewers agree"));
    }
    let provider = providers(host);
    let Some(judge) = pick(host, None, &reviewers, &provider) else {
        return Ok(StepDone::skipped("No judge outside the reviewers"));
    };
    let text = context::step_input(host, call).await?;
    let (chunks, note) = diff_cut::cut(&text, host.settings().review.max_diff_lines);
    let input = SettleInput {
        goal: host.read(context::goal_text),
        task: call
            .step
            .task
            .clone()
            .unwrap_or_else(|| "Settle the reviews below.".to_owned()),
        diff: chunks.concat() + &note.map(|n| format!("\n{n}\n")).unwrap_or_default(),
        reviews,
    };
    let mut ask = Ask::new(
        Some(call.at.clone()),
        RoleName::Judge,
        judge.clone(),
        SETTLE,
        input,
    );
    ask.access = Access::NoTools;
    ask.blind = true;
    let answered = host.ask::<Settle>(ask).await?;
    let line = verdict(&answered.output).line();
    let picked_by = picked(host, RoleName::Judge, &judge, false);
    host.record(
        None,
        Some(&answered.session),
        RunEventKind::Review(ReviewEvent::Settled {
            judge,
            ruling: answered.output,
            picked_by: Some(picked_by),
        }),
    )?;
    Ok(StepDone::done(line))
}

fn latest_task_reviews(state: &RunState, task: &TaskId) -> Vec<ReviewOutput> {
    let Some(record) = state.tasks.get(task) else {
        return Vec::new();
    };
    let round = record
        .reviews
        .iter()
        .filter_map(|event| match event {
            ReviewEvent::Reviewed { round, .. } => Some(*round),
            _ => None,
        })
        .max();
    record
        .reviews
        .iter()
        .filter_map(|event| match event {
            ReviewEvent::Reviewed {
                round: r, review, ..
            } if Some(*r) == round => Some(review.clone()),
            _ => None,
        })
        .collect()
}

/// The reviews of the newest workflow `review` step outside any task, from its reviewers'
/// sessions.
fn latest_step_reviews(state: &RunState) -> (Vec<ModelRef>, Vec<ReviewOutput>) {
    let sessions: Vec<_> = state
        .sessions
        .by_key
        .values()
        .filter(|s| {
            s.task.is_none()
                && s.started.role == RoleName::Reviewer
                && s.started.step == Some(StepName::Review)
        })
        .collect();
    let Some(newest) = sessions.iter().max_by_key(|s| s.since) else {
        return (Vec::new(), Vec::new());
    };
    let step = newest.started.workflow_step.clone();
    sessions
        .iter()
        .filter(|s| s.started.workflow_step == step)
        .filter_map(|s| {
            let value = s.end.as_ref()?.structured.clone()?;
            let review = serde_json::from_value::<ReviewOutput>(value).ok()?;
            Some((s.started.worker.clone(), review))
        })
        .unzip()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rulings_map_to_verdicts() {
        let ruling = |verdict| SettleOutput {
            verdict,
            reasons: vec!["tests are thin".to_owned()],
            required_changes: vec!["add a test for 429".to_owned()],
        };
        assert_eq!(verdict(&ruling(ReviewVerdict::Approve)), Verdict::Approved);
        assert_eq!(
            verdict(&ruling(ReviewVerdict::RequestChanges)),
            Verdict::ChangesRequested {
                comments: vec!["add a test for 429".to_owned(), "tests are thin".to_owned()]
            }
        );
        assert_eq!(
            verdict(&ruling(ReviewVerdict::Reject)),
            Verdict::NeedsYou {
                reasons: vec!["tests are thin".to_owned()]
            }
        );
    }
}
