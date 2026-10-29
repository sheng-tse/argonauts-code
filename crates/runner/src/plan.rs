//! What a file of steps would run, without running it: each step, its role, who would take it
//! and how, and about how many agents that makes.

use argonauts_core::names::ModelRef;
use argonauts_core::settings::crew::CrewRole;

use crate::file::{Count, ForEach, Step, WorkflowFile};

/// The tasks an estimate assumes when the plan isn't written yet.
const ASSUMED_TASKS: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DryRun {
    pub rows: Vec<Row>,
    pub estimate: Estimate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// From 1, in the file's order. `None` for a step you require, which the file doesn't list.
    pub number: Option<u32>,
    pub step: String,
    pub role: CrewRole,
    /// In order of preference. The first that's available is asked first.
    pub who: Vec<ModelRef>,
    pub how: String,
}

/// About how many agents the steps start: `low` when every review accepts at once, `high` with
/// one more round of every loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Estimate {
    pub low: u32,
    pub high: u32,
    /// The step that lists the tasks and how many the numbers assume, when steps run per task.
    pub assumes: Option<(String, u32)>,
}

impl Estimate {
    /// `about 11 to 20 agents if Spec lists 3 tasks`.
    pub fn words(&self) -> String {
        let count = if self.low == self.high {
            format!("about {} agents", self.low)
        } else {
            format!("about {} to {} agents", self.low, self.high)
        };
        match &self.assumes {
            Some((planner, tasks)) => format!("{count} if {planner} lists {tasks} tasks"),
            None => count,
        }
    }
}

/// `models` says which models each role has, in order.
pub fn dry_run(file: &WorkflowFile, models: &dyn Fn(CrewRole) -> Vec<ModelRef>) -> DryRun {
    let mut number = 0;
    let rows = file
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            if !step.required {
                number += 1;
            }
            Row {
                number: (!step.required).then_some(number),
                step: step.name.clone(),
                role: step.role,
                who: who(step, &models(step.role)),
                how: how(file, index),
            }
        })
        .collect();
    DryRun {
        rows,
        estimate: estimate(file, models),
    }
}

fn who(step: &Step, models: &[ModelRef]) -> Vec<ModelRef> {
    if let Some(model) = &step.model {
        return vec![model.clone()];
    }
    let taken = match step.count {
        Some(Count::All) => models.len(),
        Some(Count::Fixed(count)) => count as usize,
        None => 1,
    };
    models.iter().take(taken.max(1)).cloned().collect()
}

/// One line on how a step runs, such as `2 per task · sends back to Revise`.
pub fn how(file: &WorkflowFile, index: usize) -> String {
    let step = &file.steps[index];
    let per_task = file.per_task(index);
    let mut notes = Vec::new();
    if step.independent {
        notes.push("without seeing the others".to_owned());
    }
    if step.not_the_author {
        notes.push("by one who didn't build it".to_owned());
    }
    if let Some(until) = &step.until {
        notes.push(format!("until {until}"));
    }
    if let Some(target) = &step.send_back_to {
        notes.push(format!("sends back to {target}"));
    }
    if let Some(next) = &step.then {
        notes.push(again(file, next));
    }
    if step.model.is_some() {
        notes.push("the model this step names".to_owned());
    }
    if step.own_workflow {
        notes.push("each may run a workflow of its own".to_owned());
    }
    if step.required {
        notes.insert(0, "a step you require".to_owned());
    }
    if let Some(rounds) = step.max_rounds {
        notes.push(format!("up to {rounds} rounds"));
    }
    let said = step.for_each == Some(ForEach::Task) || step.count.is_some();
    if said || notes.is_empty() {
        notes.insert(0, agents(step, per_task));
    }
    notes.join(" · ")
}

fn agents(step: &Step, per_task: bool) -> String {
    match (step.count, per_task) {
        (None | Some(Count::Fixed(1)), true) => "one per task".to_owned(),
        (None | Some(Count::Fixed(1)), false) => "1 agent".to_owned(),
        (Some(Count::Fixed(count)), true) => format!("{count} per task"),
        (Some(Count::Fixed(count)), false) => format!("{count} agents"),
        (Some(Count::All), true) => format!("all the {} per task", step.role.plural()),
        (Some(Count::All), false) => format!("all the {}", step.role.plural()),
    }
}

/// `then Review again, until both accept`.
fn again(file: &WorkflowFile, next: &str) -> String {
    let accepts = file
        .index_of(next)
        .map(|index| &file.steps[index])
        .filter(|step| step.send_back_to.is_some())
        .map(|step| match step.count {
            None | Some(Count::Fixed(1)) => ", until it accepts",
            Some(Count::Fixed(2)) => ", until both accept",
            _ => ", until all accept",
        })
        .unwrap_or_default();
    format!("then {next} again{accepts}")
}

fn estimate(file: &WorkflowFile, models: &dyn Fn(CrewRole) -> Vec<ModelRef>) -> Estimate {
    let per_task = |index: usize| file.per_task(index);
    let once = |index: usize| {
        let step = &file.steps[index];
        let agents = match step.count {
            Some(Count::All) => u32::try_from(models(step.role).len()).unwrap_or(1).max(1),
            Some(Count::Fixed(count)) => count,
            None => 1,
        };
        agents * if per_task(index) { ASSUMED_TASKS } else { 1 }
    };
    let low: u32 = (0..file.steps.len())
        .filter(|index| !file.only_when_sent_back(*index))
        .map(once)
        .sum();
    // One more round of a loop is the step it sends back to and the judging step again.
    let again: u32 = file
        .steps
        .iter()
        .enumerate()
        .filter_map(|(index, step)| {
            let target = file.index_of(step.send_back_to.as_ref()?)?;
            Some(once(index) + once(target))
        })
        .sum();
    let assumes = (0..file.steps.len()).any(per_task).then(|| {
        let planner = file
            .planner()
            .map(|index| file.steps[index].name.clone())
            .unwrap_or_else(|| "the request".to_owned());
        (planner, ASSUMED_TASKS)
    });
    Estimate {
        low,
        high: low + again,
        assumes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn models(role: CrewRole) -> Vec<ModelRef> {
        let names: &[&str] = match role {
            CrewRole::Main | CrewRole::Orchestrator => &["claude-code/opus-5-5"],
            CrewRole::Worker => &["codex/gpt-5.6", "claude-code/sonnet-5-5"],
            CrewRole::Reviewer => &["antigravity-cli/gemini-3.6-pro", "codex/gpt-5.6"],
            CrewRole::Judge => &["antigravity-cli/gemini-3.6-flash"],
        };
        names
            .iter()
            .map(|name| name.parse().expect("a model"))
            .collect()
    }

    #[test]
    fn the_dry_run_of_spec_first_reads_as_the_steps_do() {
        let file = WorkflowFile::parse(include_str!("../tests/fixtures/spec-first.toml"))
            .expect("reads")
            .expect("has steps");
        let dry = dry_run(&file, &models);
        let shown: Vec<(Option<u32>, &str, String, &str)> = dry
            .rows
            .iter()
            .map(|row| {
                let who: Vec<String> = row.who.iter().map(ToString::to_string).collect();
                (
                    row.number,
                    row.step.as_str(),
                    who.join(", "),
                    row.how.as_str(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                (
                    Some(1),
                    "Spec",
                    "claude-code/opus-5-5".to_owned(),
                    "1 agent"
                ),
                (
                    Some(2),
                    "Build",
                    "codex/gpt-5.6".to_owned(),
                    "one per task · until its acceptance tests pass"
                ),
                (
                    Some(3),
                    "Review",
                    "antigravity-cli/gemini-3.6-pro, codex/gpt-5.6".to_owned(),
                    "2 per task · sends back to Revise"
                ),
                (
                    Some(4),
                    "Revise",
                    "codex/gpt-5.6".to_owned(),
                    "then Review again, until both accept"
                ),
                (
                    Some(5),
                    "Sign off",
                    "codex/gpt-5.6".to_owned(),
                    "the model this step names"
                ),
            ]
        );
        assert_eq!(
            dry.estimate.words(),
            "about 11 to 20 agents if Spec lists 3 tasks"
        );
    }

    #[test]
    fn a_council_counts_every_worker() {
        let file = WorkflowFile::parse(crate::COUNCIL_STEPS)
            .expect("reads")
            .expect("has steps");
        let dry = dry_run(&file, &models);
        assert_eq!(
            dry.rows[0].how,
            "all the workers · without seeing the others"
        );
        assert_eq!(dry.rows[0].who.len(), 2);
        assert_eq!(
            dry.rows[4].how,
            "one per task · by one who didn't build it · sends back to Build"
        );
        // Propose 2, Challenge 2, Decide 1, Build 3, Cross-review 3, Sign off 1.
        assert_eq!(dry.estimate.low, 12);
        assert_eq!(dry.estimate.high, 18);
    }
}
