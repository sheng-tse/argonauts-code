//! Experiments: the tie-breaker proposes; code checks each proposal against `experiments.limit_usd`
//! and `time_box_min`. A fitting one becomes experiment tasks, one per option, in sandboxed
//! worktrees, ending with an `ExperimentReport`. Experiment worktrees are never merged.

use std::path::PathBuf;

use futures::future::join_all;

use argonauts_core::TaskId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::plan::PlanEvent;
use argonauts_core::events::session::InputPartKind;
use argonauts_core::events::task::{TaskEvent, TaskKind, TaskLimit, TaskSpec, TaskState};
use argonauts_core::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::session_runner::Access;
use argonauts_core::settings::limit::{LimitAmount, LimitUnit};
use argonauts_core::settings::protocol::ExperimentSettings;
use argonauts_core::steps::plan::{
    ExperimentProposal, ExperimentReport, ExperimentReportStep, ProposeExperiment,
    ProposeExperimentInput,
};
use argonauts_core::steps::{StepName, last_json_object, output_schema};

use crate::engine::host::{Ask, Fallbacks, Host, SessionPlan};
use crate::error::{EngineError, EngineResult};
use crate::plan::context::{crew_for, part, warn};
use crate::plan::map;
use crate::prompts::plan::{EXPERIMENT_REPORT, PROPOSE_EXPERIMENT};
use crate::state::RunState;
use crate::task::worktree;
use crate::tie_breaker;
use crate::workflow::StepCall;

/// Whether one proposal fits on its own: experiments on, at least one option, each option within
/// the time box, and the whole proposal within the run's experiment limit.
pub fn fits(proposal: &ExperimentProposal, settings: &ExperimentSettings) -> bool {
    fits_within(proposal, settings, 0.0)
}

fn fits_within(
    proposal: &ExperimentProposal,
    settings: &ExperimentSettings,
    spent_usd: f64,
) -> bool {
    let estimates_sane = proposal.est_minutes.is_finite()
        && proposal.est_cost_usd.is_finite()
        && proposal.est_minutes > 0.0
        && proposal.est_cost_usd >= 0.0;
    settings.enabled
        && !proposal.options.is_empty()
        && estimates_sane
        && proposal.est_minutes <= settings.time_box_min
        && spent_usd + proposal.est_cost_usd <= settings.limit_usd
}

/// Which proposals fit, in order, each counting against what the earlier fitting ones took
/// from the run's experiment limit.
pub fn within_limit(proposals: &[ExperimentProposal], settings: &ExperimentSettings) -> Vec<bool> {
    let mut spent = 0.0;
    proposals
        .iter()
        .map(|proposal| {
            let fits = fits_within(proposal, settings, spent);
            if fits {
                spent += proposal.est_cost_usd;
            }
            fits
        })
        .collect()
}

/// Dollar limits for the experiment tasks, one per option in task order. A proposal's estimate
/// covers all its options, so each option gets an even share, and no task gets more than the
/// earlier tasks left of the run's experiment limit.
fn task_limits_usd(proposals: &[ExperimentProposal], limit_usd: f64) -> Vec<f64> {
    let mut left = limit_usd.max(0.0);
    let mut limits = Vec::new();
    for proposal in proposals {
        let share = proposal.est_cost_usd / proposal.options.len().max(1) as f64;
        for _ in &proposal.options {
            let limit = share.min(left).max(0.0);
            left -= limit;
            limits.push(limit);
        }
    }
    limits
}

/// Proposes experiments once per run when topics are still disputed, runs every fitting one as
/// experiment tasks (one per option), and returns every experiment report the run has.
pub async fn run(host: &Host, call: &StepCall) -> EngineResult<Vec<ExperimentReport>> {
    let settings = host.settings().experiments.clone();
    let (proposed, disputed) = host.read(|state| {
        (
            state.plan.experiments.clone(),
            state
                .plan
                .disputed()
                .into_iter()
                .map(map::to_step_topic)
                .collect::<Vec<_>>(),
        )
    });
    if settings.enabled && !disputed.is_empty() {
        if proposed.is_empty() {
            propose(host, call, &settings, disputed).await?;
        }
        build(host, call, &settings).await?;
    }
    Ok(host.read(|state| {
        state
            .plan
            .experiment_reports
            .iter()
            .map(|result| result.report.clone())
            .collect()
    }))
}

/// The tie-breaker proposes; code checks each proposal against the limit and records it.
async fn propose(
    host: &Host,
    call: &StepCall,
    settings: &ExperimentSettings,
    disputed: Vec<argonauts_core::steps::plan::MapTopic>,
) -> EngineResult<()> {
    let tie_breaker = host
        .read(|state| tie_breaker::tie_breaker(state, host.settings()))
        .unwrap_or_else(|| host.settings().crew.main.clone());
    let input = ProposeExperimentInput {
        disputed,
        limit_usd: settings.limit_usd,
        time_box_min: settings.time_box_min,
    };
    let mut ask = Ask::new(
        Some(call.at.clone()),
        RoleName::TieBreaker,
        tie_breaker,
        PROPOSE_EXPERIMENT,
        input,
    );
    ask.access = Access::NoTools;
    ask.blind = true;
    let proposals = match host.ask::<ProposeExperiment>(ask).await {
        Ok(answered) => answered.output.proposals,
        Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
        Err(error) => {
            return warn(
                host,
                None,
                format!("No experiments: the proposal failed: {error}"),
            );
        }
    };
    for (proposal, fits_limit) in proposals.iter().zip(within_limit(&proposals, settings)) {
        host.record(
            None,
            None,
            RunEventKind::Plan(PlanEvent::ExperimentProposed {
                proposal: proposal.clone(),
                fits_limit,
            }),
        )?;
    }
    Ok(())
}

/// One experiment task per option of every fitting proposal, numbered in proposal order so a
/// re-entered step finds the tasks it already made. Worktrees are made one at a time, since
/// they share the repository's refs; the experiments then run side by side.
async fn build(host: &Host, call: &StepCall, settings: &ExperimentSettings) -> EngineResult<()> {
    let proposals: Vec<ExperimentProposal> = host.read(|state| {
        state
            .plan
            .experiments
            .iter()
            .filter(|record| record.fits_limit)
            .map(|record| record.proposal.clone())
            .collect()
    });
    let builders: Vec<ModelRef> = crew_for(host.settings(), RoleName::Experiment);
    if builders.is_empty() {
        return warn(
            host,
            None,
            "No experiment worker is available; the disputes stay open".to_owned(),
        );
    }
    let mut limits = task_limits_usd(&proposals, settings.limit_usd).into_iter();
    let mut ready = Vec::new();
    let mut n = 0u32;
    for proposal in &proposals {
        for (index, option) in proposal.options.iter().enumerate() {
            n += 1;
            let task = TaskId::new(format!("experiment-{n:03}"))
                .map_err(|error| EngineError::Refused(error.to_string()))?;
            let worker = builders[index % builders.len()].clone();
            // One option's experiment carries that option's share of the estimate, which is
            // also its task's limit.
            let experiment = ExperimentProposal {
                options: vec![option.clone()],
                est_cost_usd: limits.next().unwrap_or(0.0),
                ..proposal.clone()
            };
            match prepare(host, settings, &task, &worker, &experiment).await {
                Ok(Some(workdir)) => ready.push((task, worker, experiment, workdir)),
                Ok(None) => {}
                Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
                Err(error) => warn(
                    host,
                    Some(&task),
                    format!("{task} couldn't start; its dispute stays open: {error}"),
                )?,
            }
        }
    }
    let runs = ready
        .into_iter()
        .map(|(task, worker, proposal, workdir)| async move {
            let outcome = experiment(host, call, &task, worker, &proposal, workdir).await;
            (task, outcome)
        });
    for (task, outcome) in join_all(runs).await {
        match outcome {
            Ok(()) => {}
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            Err(error) => warn(
                host,
                Some(&task),
                format!("{task} failed; its dispute stays open: {error}"),
            )?,
        }
    }
    Ok(())
}

/// Creates the experiment task and its worktree, or finds them. `None` when it already reported.
async fn prepare(
    host: &Host,
    settings: &ExperimentSettings,
    task: &TaskId,
    worker: &ModelRef,
    experiment: &ExperimentProposal,
) -> EngineResult<Option<PathBuf>> {
    let (created, finished) = host.read(|state| {
        (
            state.tasks.get(task).is_some(),
            state
                .plan
                .experiment_reports
                .iter()
                .any(|r| &r.task == task),
        )
    });
    if finished {
        return Ok(None);
    }
    let Some(option) = experiment.options.first() else {
        return Ok(None);
    };
    if !created {
        let spec = TaskSpec {
            id: task.clone(),
            title: format!("Experiment: {}: {}", experiment.topic, option.label),
            task: format!(
                "{}\n\nBuild: {}\nMeasure: {}\nDecides by: {}",
                experiment.question, option.build, experiment.measure, experiment.decides_by
            ),
            kind: TaskKind::Experiment {
                topic: experiment.topic.clone(),
                option: option.label.clone(),
            },
            owned_paths: Vec::new(),
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: Vec::new(),
            worker: Some(worker.clone()),
            reviewers: Vec::new(),
            limit: TaskLimit {
                limit: Some(LimitAmount {
                    unit: LimitUnit::Usd,
                    amount: experiment.est_cost_usd,
                }),
                max_minutes: Some(settings.time_box_min),
                ..TaskLimit::default()
            },
            predictions: None,
            item: Some(argonauts_core::labels::slug(&option.label, 3))
                .filter(|item| !item.is_empty()),
        };
        host.record(
            Some(task),
            None,
            RunEventKind::Task(TaskEvent::Created(Box::new(spec))),
        )?;
        host.record(
            Some(task),
            None,
            RunEventKind::Task(TaskEvent::State {
                from: None,
                to: TaskState::Running,
                reason: "experiment started".to_owned(),
            }),
        )?;
    }
    Ok(Some(worktree::ensure(host, task).await?.path))
}

/// Runs one option's experiment session, keeps its report, and closes the task unmerged.
async fn experiment(
    host: &Host,
    call: &StepCall,
    task: &TaskId,
    worker: ModelRef,
    experiment: &ExperimentProposal,
    workdir: PathBuf,
) -> EngineResult<()> {
    let plan = SessionPlan {
        at: Some(call.at.clone()),
        role: RoleName::Experiment,
        worker,
        task: Some(task.clone()),
        access: Access::Experiment,
        step: Some(StepName::ExperimentReport),
        workdir,
        parts: vec![
            part(
                InputPartKind::Prompt,
                "experiment",
                EXPERIMENT_REPORT.to_owned(),
            ),
            part(
                InputPartKind::StepInput,
                "experiment",
                serde_json::to_string_pretty(experiment)?,
            ),
        ],
        json_schema: Some(output_schema::<ExperimentReportStep>()),
        resume: None,
        slot: 0,
        label: None,
        reviews: false,
        fresh: false,
    };
    // A CLI error has the experiment go again, or to another agent; when nobody can run it, the
    // dispute stays open, as without a report.
    let (session, end) = match host.run_session(plan, Fallbacks::Allowed).await {
        Ok(ran) => (ran.key, Some(ran.end)),
        Err(EngineError::Stalled(stall)) => {
            warn(host, Some(task), stall.line.clone())?;
            let session = host.read(|state| newest_experiment_session(state, task));
            let Some(session) = session else {
                return Ok(());
            };
            (session, None)
        }
        Err(err) => return Err(err),
    };
    worktree::commit_after_end(host, task, &session).await?;
    let report = end.and_then(|end| {
        end.structured
            .clone()
            .or_else(|| end.result_text.as_deref().and_then(last_json_object))
            .and_then(|value| serde_json::from_value::<ExperimentReport>(value).ok())
    });
    match report {
        Some(report) => {
            host.record(
                Some(task),
                Some(&session),
                RunEventKind::Plan(PlanEvent::ExperimentFinished {
                    task: task.clone(),
                    report,
                }),
            )?;
        }
        None => warn(
            host,
            Some(task),
            format!("{task} gave no experiment report; the dispute stays open"),
        )?,
    }
    // Experiment worktrees are never merged; only the report is kept.
    host.record(
        Some(task),
        Some(&session),
        RunEventKind::Task(TaskEvent::State {
            from: Some(TaskState::Running),
            to: TaskState::Cancelled,
            reason: "experiment done; experiments are never merged into the run".to_owned(),
        }),
    )?;
    Ok(())
}

/// The experiment task's newest session.
fn newest_experiment_session(state: &RunState, task: &TaskId) -> Option<SessionKey> {
    state
        .sessions
        .by_key
        .values()
        .filter(|session| session.task.as_ref() == Some(task))
        .max_by_key(|session| session.since)
        .map(|session| session.key.clone())
}

#[cfg(test)]
mod tests {
    use argonauts_core::steps::plan::ExperimentOption;

    use super::*;

    fn proposal(minutes: f64, cost: f64) -> ExperimentProposal {
        ExperimentProposal {
            topic: "counters".to_owned(),
            question: "Redis or memory?".to_owned(),
            options: vec![
                ExperimentOption {
                    label: "Redis".to_owned(),
                    build: "a counter in Redis".to_owned(),
                },
                ExperimentOption {
                    label: "memory".to_owned(),
                    build: "a counter in a map".to_owned(),
                },
            ],
            measure: "p50 latency".to_owned(),
            decides_by: "lower p50 wins".to_owned(),
            est_minutes: minutes,
            est_cost_usd: cost,
        }
    }

    #[test]
    fn a_proposal_fits_the_time_box_and_limit() {
        let settings = ExperimentSettings::default();
        assert!(fits(&proposal(10.0, 1.0), &settings));
        assert!(fits(&proposal(15.0, 2.0), &settings));
        assert!(!fits(&proposal(16.0, 1.0), &settings));
        assert!(!fits(&proposal(10.0, 2.5), &settings));
    }

    #[test]
    fn nonsense_estimates_or_no_options_never_fit() {
        let settings = ExperimentSettings::default();
        assert!(!fits(&proposal(f64::NAN, 1.0), &settings));
        assert!(!fits(&proposal(10.0, -1.0), &settings));
        assert!(!fits(&proposal(0.0, 1.0), &settings));
        let mut empty = proposal(10.0, 1.0);
        empty.options.clear();
        assert!(!fits(&empty, &settings));
    }

    #[test]
    fn experiments_off_means_nothing_fits() {
        let settings = ExperimentSettings {
            enabled: false,
            ..ExperimentSettings::default()
        };
        assert!(!fits(&proposal(1.0, 0.1), &settings));
    }

    #[test]
    fn experiment_task_limits_add_up_to_no_more_than_the_limit() {
        let settings = ExperimentSettings {
            limit_usd: 2.5,
            ..ExperimentSettings::default()
        };
        let mut three = proposal(10.0, 2.0);
        three.options.push(ExperimentOption {
            label: "sqlite".to_owned(),
            build: "a counter in sqlite".to_owned(),
        });
        let proposals = [three, proposal(10.0, 1.5), proposal(5.0, 0.5)];
        let fitting: Vec<ExperimentProposal> = proposals
            .iter()
            .zip(within_limit(&proposals, &settings))
            .filter(|(_, fits)| *fits)
            .map(|(proposal, _)| proposal.clone())
            .collect();
        let limits = task_limits_usd(&fitting, settings.limit_usd);
        // Three options sharing $2, then two sharing $0.50.
        let expected = [2.0 / 3.0, 2.0 / 3.0, 2.0 / 3.0, 0.25, 0.25];
        assert_eq!(limits.len(), expected.len());
        for (limit, want) in limits.iter().zip(expected) {
            assert!((limit - want).abs() < 1e-9, "{limits:?}");
        }
        assert!(limits.iter().sum::<f64>() <= settings.limit_usd + 1e-9);

        // A smaller limit than the proposals were checked against still caps the tasks.
        let limits = task_limits_usd(&fitting, 1.0);
        assert!(limits.iter().sum::<f64>() <= 1.0 + 1e-9, "{limits:?}");
        assert_eq!(limits.last(), Some(&0.0));
    }

    #[test]
    fn the_limit_is_shared_by_every_experiment_in_the_run() {
        let settings = ExperimentSettings::default();
        let proposals = [proposal(10.0, 1.5), proposal(10.0, 1.0), proposal(5.0, 0.5)];
        assert_eq!(within_limit(&proposals, &settings), vec![true, false, true]);
    }
}
