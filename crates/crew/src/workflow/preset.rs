//! A run keeps the preset you chose. When the lead writes a run's brief it may size the
//! preset's workflow for the task, with more or fewer agents at a step or a step that doesn't
//! apply left out, but it runs another example, or one of its own, only with
//! `workflows.lead_designs = "any"`. One transcript line says what it changed and why.

use std::fmt::Write as _;

use serde_json::Value;

use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::settings::workflows::LeadDesign;
use argonauts_core::text::clause_case;
use argonauts_core::workflow::{
    Count, Phase, StandardPhase, Step, StepWorkers, Workflow, WorkflowParams, WorkflowRef,
};

use crate::engine::host::Host;

use super::{params, runner, shape, yaml};

/// The lead sees only the preset's workflow when it may only size it, with what it may change.
/// `None` when it may pick any, so it sees every example.
pub fn part(host: &Host) -> Option<InputPart> {
    if host.settings().lead_designs() != LeadDesign::Size {
        return None;
    }
    let name = runner::crew_workflow(host);
    let workflow = runner::lookup(host, name.as_str()).ok()?;
    let file = yaml::print(&workflow).ok()?;
    let mut text = String::from("## The preset's workflow\n\n");
    let _ = write!(
        text,
        "The user chose the preset that runs `{name}`, so this run keeps it. Set `based_on` to \
         `{name}`, and size it for the task: with its params, or in `workflow` with more or \
         fewer agents at a step, fewer rounds, or a step that doesn't apply left out. Keep its \
         phases, their titles, and their order, and don't run another workflow, unless \
         `as_you_asked` says to run it another way.\n\n### \
         {name}\n\n{}\n\n```yaml\n{}\n```\n",
        workflow.description.trim(),
        file.trim_end()
    );
    Some(InputPart {
        kind: InputPartKind::StepInput,
        label: "the preset's workflow".to_owned(),
        approx_tokens: u32::try_from(text.len() / 4).unwrap_or(u32::MAX),
        text,
        source: None,
    })
}

/// The workflow the lead's design runs as, and what it changed in the preset's.
#[derive(Debug, Clone, PartialEq)]
pub struct Sizing {
    /// The example it runs, the preset's for a workflow that only sizes it.
    pub from: Option<WorkflowRef>,
    /// One phrase each, empty when it kept the preset as it is.
    pub changes: Vec<String>,
}

/// What the lead's brief changed in the preset's workflow. `from` is the example it started
/// from and `workflow` the one it runs, filled in, before code shapes it. When the lead may only
/// size the preset, `Err` says why it isn't the preset's workflow sized for the task, for the
/// lead to fix. With `"any"`, or when your request said how to run it (`asked`),
/// nothing is refused, and running another is the change.
pub fn check(
    host: &Host,
    from: Option<&WorkflowRef>,
    workflow: &Workflow,
    asked: bool,
) -> Result<Sizing, String> {
    let preset = runner::crew_workflow(host);
    let any = asked || host.settings().lead_designs() == LeadDesign::Any;
    let file = runner::lookup(host, preset.as_str()).map_err(|err| err.to_string())?;
    // The preset filled with the lead's values, so a value it set shows once, as a param.
    let original = params::fill(&file, &workflow.params)
        .or_else(|_| params::fill(&file, &WorkflowParams::default()))
        .map_err(|errors| errors.join("; "))?;
    match sized(&original, workflow) {
        Ok(sized) => {
            let mut changes = params_changed(&file.params, workflow);
            changes.extend(sized);
            Ok(Sizing {
                from: Some(preset),
                changes,
            })
        }
        Err(_) if any => Ok(Sizing {
            from: from.cloned(),
            changes: vec![match from {
                Some(from) if *from != preset => format!("runs {from} in place of {preset}"),
                _ => format!("runs a workflow of its own in place of {preset}"),
            }],
        }),
        Err(why) => Err(match from {
            Some(from) if *from != preset => format!(
                "keep `{preset}`, the preset's workflow: size it for the task, but don't run \
                 `{from}` in its place"
            ),
            _ => format!("keep `{preset}`, the preset's workflow, and only size it: {why}"),
        }),
    }
}

/// A change at a phase end, where the lead may only size the preset: what's left of the
/// running workflow, sized, never another workflow in its place. Both are as code shaped them.
pub fn keeps(host: &Host, current: &Workflow, proposed: &Workflow) -> Result<(), String> {
    sized(current, proposed).map(|_| ()).map_err(|why| {
        let preset = runner::crew_workflow(host);
        format!("keep `{preset}`, the preset's workflow, and only size what's left of it: {why}")
    })
}

/// The transcript's line for what the lead changed, or `None` when it kept the preset as it is.
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
        "The lead changed the preset's workflow: {}{because}.",
        changes.join("; ")
    ))
}

/// The params the lead set to something other than the preset's own defaults.
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

/// The phases that are the preset's to keep. Understand and the final Check are code's: it
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

/// The lead's workflow against the preset's: the same phases in the same order, and in each
/// the preset's steps, some perhaps left out, with only their agents, rounds, and params
/// changed. The workflow's own params are the caller's to compare.
fn sized(preset: &Workflow, workflow: &Workflow) -> Result<Vec<String>, String> {
    let (preset_phases, phases) = (kept(&preset.phases), kept(&workflow.phases));
    let names = |phases: &[&Phase]| phases.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
    if names(&preset_phases) != names(&phases) {
        return Err(format!(
            "its phases are {}, not {}",
            names(&phases).join(", "),
            names(&preset_phases).join(", ")
        ));
    }
    let mut changes = Vec::new();
    for (original, phase) in preset_phases.iter().zip(&phases) {
        if original.standard != phase.standard {
            return Err(format!("phase {} has another standard phase", phase.name));
        }
        steps(&original.steps, &phase.steps, &mut changes)?;
    }
    Ok(changes)
}

/// Matches each of the lead's steps to the next preset step of the same name and kind. The
/// preset steps it passes over were left out.
fn steps(preset: &[Step], proposed: &[Step], changes: &mut Vec<String>) -> Result<(), String> {
    let mut rest = preset.iter();
    for step in proposed {
        let mut passed = Vec::new();
        let original = loop {
            match rest.next() {
                Some(original) if same_step(original, step) => break original,
                Some(original) => passed.push(original),
                None => {
                    return Err(format!(
                        "`{}` isn't a step of the preset's workflow at that place",
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
    if original.workers != step.workers {
        changes.push(format!("{name} by {}", workers(&step.workers)));
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
    same.workers.clone_from(&original.workers);
    same.max = original.max;
    same.params.clone_from(&original.params);
    same.steps.clone_from(&original.steps);
    // Its label and where its reviews show are the brief's words, not how it works.
    same.label.clone_from(&original.label);
    same.review_in.clone_from(&original.review_in);
    if same != *original {
        return Err(format!("`{name}` works differently from the preset's"));
    }
    Ok(())
}

/// How many agents a step runs, as the line says it: "1 agent", "3 agents", or the preset's
/// own number when it sets none.
fn agents(count: Option<&Count>) -> String {
    match count {
        Some(Count::Fixed(1)) => "1 agent".to_owned(),
        Some(Count::Fixed(n)) => format!("{n} agents"),
        Some(Count::Param(text)) => format!("{text} agents"),
        None => "the preset's number of agents".to_owned(),
    }
}

/// The count a step had, after "instead of".
fn count(count: Option<&Count>) -> String {
    match count {
        Some(Count::Fixed(n)) => n.to_string(),
        Some(Count::Param(text)) => text.clone(),
        None => "the preset's number".to_owned(),
    }
}

fn workers(workers: &StepWorkers) -> String {
    match workers {
        StepWorkers::Named(named) if named.is_empty() => "the router's pick".to_owned(),
        StepWorkers::Named(named) => named
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        StepWorkers::Param(text) => text.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::builtin;

    fn peer_crew() -> Workflow {
        builtin::load("peer-crew").unwrap_or_else(|err| panic!("{err}"))
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
        let preset = peer_crew();
        assert_eq!(sized(&preset, &preset), Ok(Vec::new()));

        let mut smaller = preset.clone();
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
        let changes = sized(&preset, &smaller).expect("still peer-crew");
        assert_eq!(
            changes,
            [
                "at most 1 round of planning instead of 3",
                "left out critique"
            ]
        );

        // One agent is one, and a count the preset leaves unset is the preset's.
        let mut fewer = preset.clone();
        let plan = fewer
            .phases
            .iter_mut()
            .find(|phase| phase.name == "Plan")
            .expect("a Plan phase");
        let drafts = find(&mut plan.steps, "drafts").expect("drafts");
        let before = drafts.count.clone();
        drafts.count = Some(Count::Fixed(1));
        drafts.label = Some("plan:{family}".to_owned());
        let changes = sized(&preset, &fewer).expect("still peer-crew");
        let said = format!("1 agent at drafts instead of {}", count(before.as_ref()));
        assert_eq!(changes, [said]);
        assert_eq!(agents(None), "the preset's number of agents");
        assert_eq!(agents(Some(&Count::Fixed(3))), "3 agents");
    }

    #[test]
    fn another_workflow_is_not_a_sizing() {
        let preset = peer_crew();
        let solo = builtin::load("solo-reviewed").unwrap_or_else(|err| panic!("{err}"));
        let refused = sized(&preset, &solo).expect_err("another workflow");
        assert!(refused.starts_with("its phases are"), "{refused}");

        let mut reordered = preset.clone();
        let plan = reordered
            .phases
            .iter_mut()
            .find(|phase| phase.name == "Plan")
            .expect("a Plan phase");
        let looped = find(&mut plan.steps, "planning").expect("the planning loop");
        looped.steps.reverse();
        assert!(sized(&preset, &reordered).is_err());

        let mut retasked = preset.clone();
        let plan = retasked
            .phases
            .iter_mut()
            .find(|phase| phase.name == "Plan")
            .expect("a Plan phase");
        let drafts = find(&mut plan.steps, "drafts").expect("drafts");
        drafts.blind = false;
        let refused = sized(&preset, &retasked).expect_err("a step that works differently");
        assert!(refused.contains("`drafts` works differently"), "{refused}");
    }

    /// Understand and the final Check are code's to add, so a brief that leaves them out, as
    /// the lead is told to, still keeps the preset.
    #[test]
    fn understand_and_the_final_check_are_codes() {
        let preset = peer_crew();
        let mut written = preset.clone();
        written
            .phases
            .retain(|phase| phase.standard != StandardPhase::Understand);
        assert_eq!(sized(&preset, &written), Ok(Vec::new()));
        let mut checked = preset.clone();
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
                "The lead changed the preset's workflow: left out critique, since one small \
                 change needs no debate."
            )
        );
    }
}
