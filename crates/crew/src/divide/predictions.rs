//! Predictions (`divide.predictions`): each worker runs `Predict` for its task. Each becomes a
//! `prediction` line in the outcome log before any task starts, and the run review scores it. A
//! prediction never changes a task's path and never skips a check.

use argonauts_core::events::task::TaskSpec;
use argonauts_core::names::RoleName;
use argonauts_core::session_runner::Access;
use argonauts_core::settings::Settings;
use argonauts_core::settings::limit::LimitUnit;
use argonauts_core::steps::task::{Predict, PredictInput, Predictions};

use crate::divide::{assign, repo_state_dir};
use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::prompts;
use crate::run_review::outcomes::{self, LogLine, Prediction, Predictor};
use crate::workflow::StepAt;

/// The task's worker, asked without tools.
pub async fn predict(
    host: &Host,
    at: Option<StepAt>,
    task: &TaskSpec,
) -> EngineResult<Predictions> {
    let worker = task
        .worker
        .clone()
        .ok_or_else(|| EngineError::Refused(format!("{} has no worker to predict it", task.id)))?;
    let input = PredictInput {
        task: format!("{}\n\n{}", task.title, task.task),
        time_cap_min: time_cap_min(task, host.settings()),
    };
    let mut ask = Ask::new(
        at,
        RoleName::Worker,
        worker,
        prompts::divide::PREDICT,
        input,
    );
    ask.task = Some(task.id.clone());
    ask.item.clone_from(&task.item);
    ask.access = Access::NoTools;
    Ok(clamped(host.ask::<Predict>(ask).await?.output))
}

/// Appends a `prediction` line for each predicted task not logged yet, so a re-divided plan
/// logs nothing twice.
pub fn log(host: &Host, tasks: &[TaskSpec], run_task_type: Option<&str>) -> EngineResult<()> {
    let state_dir = repo_state_dir(host);
    let logged: Vec<_> = outcomes::run_lines(&state_dir, host.run_id())?
        .into_iter()
        .filter_map(|line| match line {
            LogLine::Prediction(prediction) => Some(prediction.task),
            _ => None,
        })
        .collect();
    let lines: Vec<LogLine> = tasks
        .iter()
        .filter(|task| !logged.contains(&task.id))
        .filter_map(|task| {
            let predicted = task.predictions?;
            Some(LogLine::Prediction(Prediction {
                run: host.run_id().clone(),
                task: task.id.clone(),
                worker: task.worker.clone()?,
                role: Predictor::Worker,
                task_type: assign::task_type(&task.kind, run_task_type),
                at: host.now(),
                p_pass_checks_first_try: Some(predicted.p_pass_checks_first_try),
                p_within_time_cap: Some(predicted.p_within_time_cap),
                p_at_most_two_tries: Some(predicted.p_at_most_two_tries),
                p_passes_merge: None,
                expected_minutes: None,
            }))
        })
        .collect();
    outcomes::append_lines(&state_dir, host.run_id(), &lines)
}

/// What a prediction is made against when nothing limits a task's time, so predictions stay
/// comparable from run to run. It stops nothing.
const TIME_YARDSTICK_MIN: f64 = 30.0;

/// The task's own time limit, else a per-task limit in minutes, else the wall-clock watchdog,
/// else the yardstick.
pub fn time_cap_min(task: &TaskSpec, settings: &Settings) -> f64 {
    let in_minutes = task
        .limit
        .limit
        .filter(|limit| limit.unit == LimitUnit::Minutes)
        .map(|limit| limit.amount);
    task.limit
        .max_minutes
        .or(in_minutes)
        .or(settings.limits.spend.per_task_minutes)
        .or(settings.watchdogs.wall_clock_min)
        .unwrap_or(TIME_YARDSTICK_MIN)
}

/// Probabilities kept between 0 and 1; a missing one says nothing either way.
pub fn clamped(predictions: Predictions) -> Predictions {
    let clamp = |p: f64| {
        if p.is_finite() {
            p.clamp(0.0, 1.0)
        } else {
            0.5
        }
    };
    Predictions {
        p_pass_checks_first_try: clamp(predictions.p_pass_checks_first_try),
        p_within_time_cap: clamp(predictions.p_within_time_cap),
        p_at_most_two_tries: clamp(predictions.p_at_most_two_tries),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::TaskId;
    use argonauts_core::events::task::{TaskKind, TaskLimit};
    use argonauts_core::settings::limit::LimitAmount;

    use super::*;

    #[test]
    fn probabilities_are_clamped() {
        let wild = Predictions {
            p_pass_checks_first_try: 1.4,
            p_within_time_cap: -0.2,
            p_at_most_two_tries: f64::NAN,
        };
        let fixed = clamped(wild);
        assert_eq!(fixed.p_pass_checks_first_try, 1.0);
        assert_eq!(fixed.p_within_time_cap, 0.0);
        assert_eq!(fixed.p_at_most_two_tries, 0.5);
    }

    #[test]
    fn the_time_cap_comes_from_the_task_first() {
        let mut settings = Settings::default();
        let mut task = TaskSpec {
            id: TaskId::numbered(1),
            title: "t".into(),
            task: "t".into(),
            kind: TaskKind::Build,
            owned_paths: Vec::new(),
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: Vec::new(),
            worker: None,
            reviewers: Vec::new(),
            limit: TaskLimit::default(),
            predictions: None,
            item: None,
            way: None,
        };
        assert_eq!(time_cap_min(&task, &settings), TIME_YARDSTICK_MIN);
        settings.watchdogs.wall_clock_min = Some(45.0);
        assert_eq!(time_cap_min(&task, &settings), 45.0);
        task.limit.limit = Some(LimitAmount {
            unit: LimitUnit::Minutes,
            amount: 12.0,
        });
        assert_eq!(time_cap_min(&task, &settings), 12.0);
        task.limit.max_minutes = Some(9.0);
        assert_eq!(time_cap_min(&task, &settings), 9.0);
    }
}
