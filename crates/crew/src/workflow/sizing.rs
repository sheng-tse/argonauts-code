//! A run keeps the workflow you chose. When the main agent writes a run's brief it may size the
//! workflow for the task, with more or fewer agents at a step or a step that doesn't
//! apply left out, but it runs another example, or one of its own, only with
//! `workflows.orchestrator_designs = "any"`. One transcript line says what it changed and why.

use std::fmt::Write as _;

use serde_json::Value;

use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::settings::workflows::OrchestratorDesign;
use argonauts_core::text::clause_case;
use argonauts_core::workflow::{
    Count, Phase, StandardPhase, Step, StepModels, Workflow, WorkflowParams, WorkflowRef,
};

use crate::engine::host::Host;

use super::{params, runner, shape, yaml};

/// The main agent sees only the chosen workflow when it may only size it, with what it may change.
/// `None` when it may pick any, so it sees every example.
pub fn part(host: &Host) -> Option<InputPart> {
    if host.settings().orchestrator_designs() != OrchestratorDesign::Size {
        return None;
    }
    let name = runner::crew_workflow(host);
    let workflow = runner::lookup(host, name.as_str()).ok()?;
    let file = yaml::print(&workflow).ok()?;
    let mut text = String::from("## The workflow's steps\n\n");
    let _ = write!(
        text,
        "The user chose the workflow that runs `{name}`, so this run keeps it. Set `based_on` to \
         `{name}`, and size it for the task: with its params, or in `workflow` with more or \
         fewer agents at a step, fewer rounds, or a step that doesn't apply left out. Keep its \
         phases, their names, and their order, and don't run another workflow, unless \
         `as_you_asked` says to run it another way.\n\n### \
         {name}\n\n{}\n\n```yaml\n{}\n```\n",
        workflow.description.trim(),
        file.trim_end()
    );
    Some(InputPart {
        kind: InputPartKind::StepInput,
        label: "the workflow's steps".to_owned(),
        approx_tokens: u32::try_from(text.len() / 4).unwrap_or(u32::MAX),
        text,
        source: None,
    })
}

/// The workflow the main agent's design runs as, and what it changed in the chosen workflow.
#[derive(Debug, Clone, PartialEq)]
pub struct Sizing {
    /// The example it runs: the chosen workflow, for a design that only sizes it.
    pub from: Option<WorkflowRef>,
    /// One phrase each, empty when it kept the workflow as it is.
    pub changes: Vec<String>,
}

/// What the main agent's brief changed in the chosen workflow. `from` is the example it started
/// from and `workflow` the one it runs, filled in, before code shapes it. When the main agent may
/// only size the workflow, `Err` says why it isn't the chosen workflow sized for the task, for the
/// main agent to fix. With `"any"`, or when your request said how to run it (`asked`), nothing is
/// refused, and running another is the change.
pub fn check(
    host: &Host,
    from: Option<&WorkflowRef>,
    workflow: &Workflow,
    asked: bool,
) -> Result<Sizing, String> {
    let chosen = runner::crew_workflow(host);
    let any = asked || host.settings().orchestrator_designs() == OrchestratorDesign::Any;
    let file = runner::lookup(host, chosen.as_str()).map_err(|err| err.to_string())?;
    // The workflow filled with the main agent's values, so a value it set shows once, as a param.
    let original = params::fill(&file, &workflow.params)
        .or_else(|_| params::fill(&file, &WorkflowParams::default()))
        .map_err(|errors| errors.join("; "))?;
    match sized(&original, workflow) {
        Ok(sized) => {
            let mut changes = params_changed(&file.params, workflow);
            changes.extend(sized);
            Ok(Sizing {
                from: Some(chosen),
                changes,
            })
        }
        Err(_) if any => Ok(Sizing {
            from: from.cloned(),
            changes: vec![match from {
                Some(from) if *from != chosen => format!("runs {from} in place of {chosen}"),
                _ => format!("runs a workflow of its own in place of {chosen}"),
            }],
        }),
        Err(why) => Err(match from {
            Some(from) if *from != chosen => format!(
                "keep `{chosen}`, the workflow's steps: size it for the task, but don't run \
                 `{from}` in its place"
            ),
            _ => format!("keep `{chosen}`, the workflow's steps, and only size it: {why}"),
        }),
    }
}

/// A change at a phase end, where the main agent may only size the workflow: what's left of the
/// running workflow, sized, never another workflow in its place. Both are as code shaped them.
pub fn keeps(host: &Host, current: &Workflow, proposed: &Workflow) -> Result<(), String> {
    sized(current, proposed).map(|_| ()).map_err(|why| {
        let workflow = runner::crew_workflow(host);
        format!("keep `{workflow}`, the workflow's steps, and only size what's left of it: {why}")
    })
}

/// The transcript's line for what the main agent changed, or `None` when it kept the workflow as it
/// is.
pub fn line(changes: &[String], why: &str) -> Option<String> {
    if changes.is_empty() {
        return None;
    }
    let why = why.trim().trim_end_matches('.');
    let because = if why.is_empty() {
        String::new()
    } else {
        format!(", since {}", clause_case(why))
    };
    Some(format!(
        "The main agent changed the workflow's steps: {}{because}.",
        changes.join("; ")
    ))
}

/// The params the main agent set to something other than the workflow's own defaults.
fn params_changed(defaults: &WorkflowParams, workflow: &Workflow) -> Vec<String> {
    workflow
        .params
        .0
        .iter()
        .filter(|(name, value)| defaults.0.get(*name) != Some(*value))
        .map(|(name, value)| format!("{name} {}", scalar(value)))
        .collect()
}

fn scalar(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The phases that are the workflow's to keep. Understand and the final Check are code's: it
/// opens a crew run with Understand as `understand.when` says, and adds the Check before Merge.
fn kept(phases: &[Phase]) -> Vec<&Phase> {
    phases
        .iter()
        .filter(|phase| {
            phase.standard != StandardPhase::Understand
                && !(phase.standard == StandardPhase::Check && phase.name == shape::CHECK_PHASE)
        })
        .collect()
}

/// The main agent's workflow against the chosen one: the same phases in the same order, and in
/// each the chosen workflow's steps, some perhaps left out, with only their agents, rounds, and
/// params changed. The chosen workflow's own params are the caller's to compare.
fn sized(chosen: &Workflow, workflow: &Workflow) -> Result<Vec<String>, String> {
    let (workflow_phases, phases) = (kept(&chosen.phases), kept(&workflow.phases));
    let names = |phases: &[&Phase]| phases.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
    if names(&workflow_phases) != names(&phases) {
        return Err(format!(
            "its phases are {}, not {}",
            names(&phases).join(", "),
            names(&workflow_phases).join(", ")
        ));
    }
    let mut changes = Vec::new();
    for (original, phase) in workflow_phases.iter().zip(&phases) {
        if original.standard != phase.standard {
            return Err(format!("phase {} has another standard phase", phase.name));
        }
        steps(&original.steps, &phase.steps, &mut changes)?;
    }
    Ok(changes)
}

/// Matches each of the main agent's steps to the chosen workflow's next step of the same name and
/// kind. The steps it passes over were left out.
fn steps(workflow: &[Step], proposed: &[Step], changes: &mut Vec<String>) -> Result<(), String> {
    let mut rest = workflow.iter();
    for step in proposed {
        let mut passed = Vec::new();
        let original = loop {
            match rest.next() {
                Some(original) if same_step(original, step) => break original,
                Some(original) => passed.push(original),
                None => {
                    return Err(format!(
                        "`{}` isn't a step of the workflow's steps at that place",
                        step_name(step)
                    ));
                }
            }
        };
        changes.extend(passed.into_iter().map(left_out));
        step_changes(original, step, changes)?;
    }
    changes.extend(rest.map(left_out));
    Ok(())
}

fn same_step(original: &Step, step: &Step) -> bool {
    original.kind == step.kind && original.name == step.name
}

fn left_out(step: &Step) -> String {
    format!("left out {}", step_name(step))
}

fn step_name(step: &Step) -> String {
    step.name.clone().unwrap_or_else(|| {
        serde_json::to_value(step.kind)
            .ok()
            .and_then(|kind| kind.as_str().map(str::to_owned))
            .unwrap_or_default()
    })
}

/// What sizing changed in one step: its agents, its rounds, its params, and its own steps.
/// Anything else makes it another step.
fn step_changes(original: &Step, step: &Step, changes: &mut Vec<String>) -> Result<(), String> {
    let name = step_name(step);
    if original.count != step.count {
        changes.push(format!(
            "{} at {name} instead of {}",
            agents(step.count.as_ref()),
            count(original.count.as_ref())
        ));
    }
    if original.models != step.models {
        changes.push(format!("{name} by {}", workers(&step.models)));
    }
    if original.max != step.max {
        let rounds = match step.max.unwrap_or_default() {
            1 => "1 round".to_owned(),
            n => format!("{n} rounds"),
        };
        changes.push(format!(
            "at most {rounds} of {name} instead of {}",
            original.max.unwrap_or_default()
        ));
    }
    if original.params != step.params {
        let defaults = WorkflowParams(original.params.0.clone());
        let mut set: Vec<String> = step
            .params
            .0
            .iter()
            .filter(|(key, value)| defaults.0.get(*key) != Some(*value))
            .map(|(key, value)| format!("{key} {}", scalar(value)))
            .collect();
        set.sort();
        changes.push(format!("{name} with {}", set.join(", ")));
    }
    steps(&original.steps, &step.steps, changes)?;
    let mut same = step.clone();
    same.count.clone_from(&original.count);
    same.models.clone_from(&original.models);
    same.max = original.max;
    same.params.clone_from(&original.params);
    same.steps.clone_from(&original.steps);
    // Its label and where its reviews show are the brief's words, not how it works.
    same.label.clone_from(&original.label);
    same.review_in.clone_from(&original.review_in);
    if same != *original {
        return Err(format!("`{name}` works differently from the workflow's"));
    }
    Ok(())
}

/// How many agents a step runs, as the line says it: "1 agent", "3 agents", or the workflow's
/// own number when it sets none.
fn agents(count: Option<&Count>) -> String {
    match count {
        Some(Count::Fixed(1)) => "1 agent".to_owned(),
        Some(Count::Fixed(n)) => format!("{n} agents"),
        Some(Count::Param(text)) => format!("{text} agents"),
        None => "the workflow's number of agents".to_owned(),
    }
}

/// The count a step had, after "instead of".
fn count(count: Option<&Count>) -> String {
    match count {
        Some(Count::Fixed(n)) => n.to_string(),
        Some(Count::Param(text)) => text.clone(),
        None => "the workflow's number".to_owned(),
    }
}

fn workers(workers: &StepModels) -> String {
    match workers {
        StepModels::Named(named) if named.is_empty() => "the router's pick".to_owned(),
        StepModels::Named(named) => named
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        StepModels::Param(text) => text.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::builtin;

    /// The crew's fixed phases, whose planning loop has drafts and critique to size.
    fn base() -> Workflow {
        builtin::load("base").unwrap_or_else(|err| panic!("{err}"))
    }

    fn find<'a>(steps: &'a mut [Step], name: &str) -> Option<&'a mut Step> {
        for step in steps {
            if step.name.as_deref() == Some(name) {
                return Some(step);
            }
            if let Some(found) = find(&mut step.steps, name) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn sizing_leaves_steps_out_and_changes_counts() {
        let workflow = base();
        assert_eq!(sized(&workflow, &workflow), Ok(Vec::new()));

        let mut smaller = workflow.clone();
        let plan = smaller
            .phases
            .iter_mut()
            .find(|phase| phase.name == "Plan")
            .expect("a Plan phase");
        let planning = find(&mut plan.steps, "planning").expect("the planning loop");
        planning
            .steps
            .retain(|step| step.name.as_deref() != Some("critique"));
        planning.max = Some(1);
        let changes = sized(&workflow, &smaller).expect("still the base phases");
        assert_eq!(
            changes,
            [
                "at most 1 round of planning instead of 3",
                "left out critique"
            ]
        );

        // One agent is one, and a count the chosen workflow leaves unset is its own default.
        let mut fewer = workflow.clone();
        let plan = fewer
            .phases
            .iter_mut()
            .find(|phase| phase.name == "Plan")
            .expect("a Plan phase");
        let drafts = find(&mut plan.steps, "drafts").expect("drafts");
        let before = drafts.count.clone();
        drafts.count = Some(Count::Fixed(1));
        drafts.label = Some("plan:{provider}".to_owned());
        let changes = sized(&workflow, &fewer).expect("still the base phases");
        let said = format!("1 agent at drafts instead of {}", count(before.as_ref()));
        assert_eq!(changes, [said]);
        assert_eq!(agents(None), "the workflow's number of agents");
        assert_eq!(agents(Some(&Count::Fixed(3))), "3 agents");
    }

    #[test]
    fn another_workflow_is_not_a_sizing() {
        let workflow = base();
        let captain = builtin::load("maker-checker").unwrap_or_else(|err| panic!("{err}"));
        let refused = sized(&workflow, &captain).expect_err("another workflow");
        assert!(refused.starts_with("its phases are"), "{refused}");

        let mut reordered = workflow.clone();
        let plan = reordered
            .phases
            .iter_mut()
            .find(|phase| phase.name == "Plan")
            .expect("a Plan phase");
        let looped = find(&mut plan.steps, "planning").expect("the planning loop");
        looped.steps.reverse();
        assert!(sized(&workflow, &reordered).is_err());

        let mut retasked = workflow.clone();
        let plan = retasked
            .phases
            .iter_mut()
            .find(|phase| phase.name == "Plan")
            .expect("a Plan phase");
        let drafts = find(&mut plan.steps, "drafts").expect("drafts");
        drafts.blind = false;
        let refused = sized(&workflow, &retasked).expect_err("a step that works differently");
        assert!(refused.contains("`drafts` works differently"), "{refused}");
    }

    /// Understand and the final Check are code's to add, so a brief that leaves them out, as
    /// the main agent is told to, still keeps the workflow.
    #[test]
    fn understand_and_the_final_check_are_codes() {
        let workflow = base();
        let mut written = workflow.clone();
        written
            .phases
            .retain(|phase| phase.standard != StandardPhase::Understand);
        assert_eq!(sized(&workflow, &written), Ok(Vec::new()));
        let mut checked = workflow.clone();
        let merge = checked.phases.len() - 1;
        checked.phases.insert(merge, shape::check_phase());
        assert_eq!(sized(&checked, &written), Ok(Vec::new()));
    }

    #[test]
    fn the_line_says_what_changed_and_why() {
        assert_eq!(line(&[], "It fits"), None);
        assert_eq!(
            line(
                &["left out critique".to_owned()],
                "One small change needs no debate."
            )
            .as_deref(),
            Some(
                "The main agent changed the workflow's steps: left out critique, since one small \
                 change needs no debate."
            )
        );
    }
}
