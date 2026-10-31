//! What code makes sure of in a crew run's workflow, whoever wrote it:
//! it opens with Understand when `understand.when = "crew"`, nothing drafts or builds before an
//! Understand approval (the one question before writing), an approval holds back the phase it comes
//! before and is never a phase of its own, a Check phase judges the done-when list before Merge
//! when `workflows.final_check` is on, and a goal run is Understand, its tries, then Merge.
//! Nothing here nests a workflow or adds an Understand or a Plan phase.
//!
//! Shaping twice changes nothing, so a phase end's design compares like for like.

use argonauts_core::runs::events::approval::ApprovalKind;
use argonauts_core::runs::events::run::RunMode;
use argonauts_core::settings::protocol::UnderstandWhen;
use argonauts_core::settings::workflows::RunStyle;
use argonauts_core::workflows::workflow::{Phase, StandardPhase, Step, StepKind, Workflow};

use crate::engine::host::Host;
use crate::task;
use crate::workflow::{builtin, naming, runner};

/// The Check phase's name. A goal's tries end with their own check instead.
pub const CHECK_PHASE: &str = "Check";

/// What shapes a run's workflow, from the settings and the run's style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rules {
    pub understand: UnderstandWhen,
    pub final_check: bool,
    pub style: RunStyle,
}

impl Rules {
    pub fn of(host: &Host) -> Self {
        let settings = host.settings();
        Self {
            understand: settings.understand.when,
            // A goal's check is what decides it, whatever `workflows.final_check` says.
            final_check: settings.workflows.final_check
                || host.read(|state| crate::goal::pursue::pursuing(state, settings)),
            style: run_style(host),
        }
    }
}

/// The style the run started with, or the one it would start with now.
pub fn run_style(host: &Host) -> RunStyle {
    host.read(|state| state.run.named.as_ref().map(|named| named.style))
        .unwrap_or_else(|| naming::style(host))
}

/// The main agent's workflow as a crew run follows it. A Captain run's is left as it is: Captain
/// skips Understand, and its checks are its check.
pub fn for_run(host: &Host, workflow: Workflow, first_try: Option<Phase>) -> Workflow {
    if host.read(runner::run_mode) == Some(RunMode::Captain) {
        return workflow;
    }
    shape(workflow, Rules::of(host), first_try)
}

/// A template or your own file, run as written, or the main agent's change at a phase end: its
/// phases stay as written, so Understand is only there if it says so. It still gets the final
/// check, and a goal run its tries.
pub fn as_written(host: &Host, workflow: Workflow) -> Workflow {
    if host.read(runner::run_mode) == Some(RunMode::Captain) {
        return workflow;
    }
    let rules = Rules::of(host);
    let rules = Rules {
        // Off still holds: its Understand skips, so the go-ahead must come after it.
        understand: match rules.understand {
            UnderstandWhen::Off => UnderstandWhen::Off,
            UnderstandWhen::Crew | UnderstandWhen::Main => UnderstandWhen::Main,
        },
        ..rules
    };
    shape(workflow, rules, None)
}

pub fn shape(mut workflow: Workflow, mut rules: Rules, first_try: Option<Phase>) -> Workflow {
    // A council's seats take your request as it is, and its plan is the first thing you approve.
    if workflow.talks() {
        rules.understand = UnderstandWhen::Off;
    }
    match rules.understand {
        UnderstandWhen::Crew => crew_understands(&mut workflow),
        UnderstandWhen::Main => one_reading_goes(&mut workflow),
        // The phase stays, and its steps skip as the run reaches them.
        UnderstandWhen::Off => {}
    }
    match rules.style {
        RunStyle::Goal => {
            let mut workflow = as_goal(workflow, first_try);
            goal_before_work(&mut workflow, rules.understand);
            workflow
        }
        RunStyle::Workflow => {
            goal_before_work(&mut workflow, rules.understand);
            check_builds(&mut workflow);
            final_check(&mut workflow, rules.final_check);
            workflow
        }
    }
}

/// Whether an Understand approval will run: an unconditional one, outside Understand when
/// `understand.when` is off, since Understand's steps skip then.
pub fn understand_approval_runs(workflow: &Workflow, understand: UnderstandWhen) -> bool {
    workflow.phases.iter().any(|phase| {
        !(understand == UnderstandWhen::Off && phase.standard == StandardPhase::Understand)
            && unconditional_approval(&phase.steps, ApprovalKind::Understand)
    })
}

/// Whether a plan approval will run. When nothing read the request, its question is the
/// go-ahead, so the Understand approval before the drafting needn't ask.
pub fn plan_approval_runs(workflow: &Workflow) -> bool {
    workflow
        .phases
        .iter()
        .any(|phase| unconditional_approval(&phase.steps, ApprovalKind::Plan))
}

/// Nothing drafts, builds, or experiments before the Understand approval, which holds the go-ahead.
/// When none will run, as when the main agent leaves Understand out or `understand.when` is off,
/// one checks the first phase that works from the goal. With nothing read it takes your prompt as
/// the goal, or the main agent's own questions, then asks "Run this workflow?" or "Pursue this
/// goal?".
fn goal_before_work(workflow: &mut Workflow, understand: UnderstandWhen) {
    if understand_approval_runs(workflow, understand) || workflow.talks() {
        return;
    }
    let first = workflow
        .phases
        .iter()
        .enumerate()
        .filter(|(_, phase)| phase.standard != StandardPhase::Understand)
        .find_map(|(index, phase)| {
            phase
                .steps
                .iter()
                .position(works_from_goal)
                .map(|at| (index, at))
        });
    if let Some((phase, at)) = first {
        workflow.phases[phase]
            .steps
            .insert(at, approval(ApprovalKind::Understand));
    }
}

/// A step that works from the goal, at any depth: drafting or mapping a plan, or building or
/// spiking. A nested workflow's own steps answer for themselves: a build there still waits
/// for an approved goal.
fn works_from_goal(step: &Step) -> bool {
    matches!(step.kind, StepKind::Draft | StepKind::Map | StepKind::Build)
        || step.steps.iter().any(works_from_goal)
}

/// A crew run whose Understand is the main agent's to add: an opening phase that only aligns the
/// goal some other way, such as a Captain example's one reading, goes, so the Understand approval
/// before building takes your prompt, or the main agent's own questions.
fn one_reading_goes(workflow: &mut Workflow) {
    let captain_reading = workflow.phases.first().is_some_and(|phase| {
        phase.standard == StandardPhase::Understand
            && !crew_reads(phase)
            && phase.steps.iter().all(goal_only)
    });
    if captain_reading {
        workflow.phases.remove(0);
    }
}

/// Whether the first phase is the crew's Understand: readings, comparisons, and the question.
pub fn opens_with_understand(workflow: &Workflow) -> bool {
    workflow.phases.first().is_some_and(crew_reads)
}

fn crew_reads(phase: &Phase) -> bool {
    phase.standard == StandardPhase::Understand
        && has_kind(&phase.steps, StepKind::Read)
        && has_kind(&phase.steps, StepKind::Compare)
        && unconditional_approval(&phase.steps, ApprovalKind::Understand)
}

/// The run opens with the crew's Understand, and reading the request belongs to it alone: an
/// opening phase that only aligns the goal some other way, such as a Captain example's single
/// reading, gives way to it, and later phases lose their own readings.
fn crew_understands(workflow: &mut Workflow) {
    let Some(understand) = understand_phase() else {
        return;
    };
    let first = workflow.phases.first();
    if first.is_some_and(crew_reads) {
        // Kept as the workflow wrote it.
    } else if first.is_some_and(|phase| {
        phase.standard == StandardPhase::Understand && phase.steps.iter().all(goal_only)
    }) {
        workflow.phases[0] = understand;
    } else {
        workflow.phases.insert(0, understand);
    }
    // A later phase that only aligned the goal goes; one with no steps of its own stays, since
    // it shows an earlier build's reviews.
    let mut index = 0;
    workflow.phases.retain(|phase| {
        index += 1;
        index == 1 || phase.steps.is_empty() || !phase.steps.iter().all(goal_only)
    });
    for phase in &mut workflow.phases[1..] {
        phase.steps.retain(|step| !goal_only(step));
    }
}

/// A step that only aligns the goal: a reading, a comparison, the Understand approval, or a loop
/// of those.
fn goal_only(step: &Step) -> bool {
    match step.kind {
        StepKind::Read | StepKind::Compare => true,
        StepKind::Approval => step.approval == Some(ApprovalKind::Understand),
        StepKind::LoopUntil => !step.steps.is_empty() && step.steps.iter().all(goal_only),
        _ => false,
    }
}

/// An approval of `kind` that runs whatever the conditions say: neither it nor a step around
/// it has a `when`.
fn unconditional_approval(steps: &[Step], kind: ApprovalKind) -> bool {
    steps.iter().filter(|step| step.when.is_none()).any(|step| {
        (step.kind == StepKind::Approval && step.approval == Some(kind))
            || unconditional_approval(&step.steps, kind)
    })
}

/// Understand as the examples write it: readings from several providers, comparisons, and the
/// one question, in a loop that a send back runs again.
fn understand_phase() -> Option<Phase> {
    let example = builtin::load("orchestrator").ok()?;
    example
        .phases
        .into_iter()
        .next()
        .filter(|phase| phase.standard == StandardPhase::Understand)
}

/// A goal run: Understand, the tries so far (the first one only, at the start), then Merge.
/// Whatever planning and building the workflow had becomes the main agent's first try, so the
/// goal is never both.
fn as_goal(workflow: Workflow, first_try: Option<Phase>) -> Workflow {
    let Workflow {
        name,
        description,
        based_on,
        params,
        phases,
    } = workflow;
    let mut understand = Vec::new();
    let mut tries = Vec::new();
    let mut merge = None;
    for phase in phases {
        if is_try(&phase) {
            tries.push(phase);
        } else if has_top(&phase.steps, StepKind::Merge) {
            merge = Some(only_merge(phase));
        } else if tries.is_empty() && phase.standard == StandardPhase::Understand {
            understand.push(phase);
        }
    }
    if tries.is_empty() {
        tries.push(first_try.map_or_else(|| try_phase(1, None), with_check));
    }
    let mut phases = understand;
    phases.extend(tries);
    phases.push(merge.unwrap_or_else(merge_phase));
    Workflow {
        name,
        description,
        based_on,
        params,
        phases,
    }
}

/// A try is a phase that ends with its own check.
pub fn is_try(phase: &Phase) -> bool {
    phase
        .steps
        .last()
        .is_some_and(|step| step.kind == StepKind::GoalCheck)
        && !has_top(&phase.steps, StepKind::Merge)
}

/// The try the main agent didn't plan: the main agent works toward every unmet criterion in
/// its own worktree, its checks run, another provider reviews it, and the check judges it.
pub fn try_phase(n: u32, name: Option<&str>) -> Phase {
    let mut build = bare(StepKind::Build);
    build.input = Some(task::GOAL.to_owned());
    build.label = Some("build:{item}".to_owned());
    build.different_provider_from = Some(task::AUTHOR.to_owned());
    Phase {
        name: name.map_or_else(|| format!("Try {n}"), str::to_owned),
        detail: "one worker works toward what's unmet, then a review and the check from another \
                 provider"
            .to_owned(),
        standard: StandardPhase::Do,
        steps: vec![build, check_step("check:{try}")],
    }
}

/// A try the main agent planned, ending with its check whether or not the main agent wrote one, and
/// never merging: Merge waits at the end of the goal.
pub fn with_check(mut phase: Phase) -> Phase {
    phase
        .steps
        .retain(|step| !matches!(step.kind, StepKind::Merge | StepKind::GoalCheck));
    phase.steps.push(check_step("check:{try}"));
    phase
}

fn only_merge(mut phase: Phase) -> Phase {
    phase.steps.retain(|step| step.kind == StepKind::Merge);
    phase
}

fn merge_phase() -> Phase {
    let mut merge = bare(StepKind::Merge);
    merge.label = Some("merge:{branch}".to_owned());
    Phase {
        name: "Merge".to_owned(),
        detail: "the merge into your branch".to_owned(),
        standard: StandardPhase::Decide,
        steps: vec![merge],
    }
}

fn check_step(label: &str) -> Step {
    let mut check = bare(StepKind::GoalCheck);
    check.label = Some(label.to_owned());
    check
}

/// Building from a plan waits for the plan. Where the workflow has no plan approval, one
/// holds back the phase it comes before: the end of the last drafting phase, or the start of the
/// first building one, after its Understand approval.
fn check_builds(workflow: &mut Workflow) {
    let Some(first_build) = workflow
        .phases
        .iter()
        .position(|phase| phase.steps.iter().any(any_build))
    else {
        return;
    };
    let from_plan = workflow
        .phases
        .iter()
        .any(|phase| phase.steps.iter().any(crew_build));
    if !from_plan || has_approval(&workflow.phases, ApprovalKind::Plan) {
        return;
    }
    let check = approval(ApprovalKind::Plan);
    let last_draft = workflow.phases[..=first_build]
        .iter()
        .rposition(|phase| has_kind(&phase.steps, StepKind::Draft));
    match last_draft {
        Some(index) if index < first_build => workflow.phases[index].steps.push(check),
        _ => {
            let at = workflow.phases[first_build]
                .steps
                .iter()
                .position(any_build)
                .unwrap_or(0);
            workflow.phases[first_build].steps.insert(at, check);
        }
    }
}

fn any_build(step: &Step) -> bool {
    step.kind == StepKind::Build && step.input.as_deref() != Some(task::EXPERIMENTS)
}

fn crew_build(step: &Step) -> bool {
    step.kind == StepKind::Build
        && !matches!(step.input.as_deref(), Some(task::GOAL | task::EXPERIMENTS))
}

/// The done-when list's check, as its own phase right before the one that merges. Any check the
/// workflow put elsewhere moves there, and with `workflows.final_check` off it goes.
fn final_check(workflow: &mut Workflow, on: bool) {
    // A phase that held only checks goes with them; one with no steps of its own stays, since
    // it shows an earlier build's reviews.
    workflow.phases.retain(|phase| {
        phase.steps.is_empty()
            || phase
                .steps
                .iter()
                .any(|step| step.kind != StepKind::GoalCheck)
    });
    for phase in &mut workflow.phases {
        phase.steps.retain(|step| step.kind != StepKind::GoalCheck);
    }
    if !on {
        return;
    }
    let Some(merge) = workflow
        .phases
        .iter()
        .position(|phase| has_top(&phase.steps, StepKind::Merge))
    else {
        return;
    };
    workflow.phases.insert(merge, check_phase());
}

/// The phase that checks a workflow's done-when list before Merge.
pub fn check_phase() -> Phase {
    Phase {
        name: CHECK_PHASE.to_owned(),
        detail: "a model from another provider checks the done-when list".to_owned(),
        standard: StandardPhase::Check,
        steps: vec![check_step("check:final")],
    }
}

fn has_top(steps: &[Step], kind: StepKind) -> bool {
    steps.iter().any(|step| step.kind == kind)
}

/// Whether any step, at any depth, is of `kind`.
pub fn has_kind(steps: &[Step], kind: StepKind) -> bool {
    steps
        .iter()
        .any(|step| step.kind == kind || has_kind(&step.steps, kind))
}

fn has_approval(phases: &[Phase], kind: ApprovalKind) -> bool {
    fn any(steps: &[Step], kind: ApprovalKind) -> bool {
        steps.iter().any(|step| {
            (step.kind == StepKind::Approval && step.approval == Some(kind))
                || any(&step.steps, kind)
        })
    }
    phases.iter().any(|phase| any(&phase.steps, kind))
}

fn approval(kind: ApprovalKind) -> Step {
    let mut step = bare(StepKind::Approval);
    step.approval = Some(kind);
    step
}

/// A step of `kind` with nothing else set.
pub fn bare(kind: StepKind) -> Step {
    Step {
        kind,
        name: None,
        label: None,
        review_in: None,
        role: None,
        roles: Vec::new(),
        models: Default::default(),
        count: None,
        blind: false,
        different_provider_from: None,
        when: None,
        task: None,
        input: None,
        steps: Vec::new(),
        until: None,
        max: None,
        approval: None,
        workflow: None,
        params: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::yaml;

    fn builtin(name: &str) -> Workflow {
        builtin::load(name).unwrap_or_else(|err| panic!("{err}"))
    }

    fn names(workflow: &Workflow) -> Vec<&str> {
        workflow
            .phases
            .iter()
            .map(|phase| phase.name.as_str())
            .collect()
    }

    fn rules(style: RunStyle) -> Rules {
        Rules {
            understand: UnderstandWhen::Crew,
            final_check: true,
            style,
        }
    }

    const FIX_THEN_REVIEW: &str = "name: fix\ndescription: fix it\nphases:\n  - name: Fix\n    \
        detail: a worker per task\n    standard: do\n    steps:\n      - { kind: build, input: \
        plan, label: \"fix:{item}\" }\n  - name: Merge\n    detail: the merge\n    standard: \
        decide\n    steps:\n      - { kind: goal_check, label: \"check:final\" }\n      - { kind: \
        merge, label: \"merge:{branch}\" }\n";

    #[test]
    fn a_workflow_opens_with_understand_and_checks_before_merge() {
        let shaped = shape(
            yaml::parse(FIX_THEN_REVIEW).unwrap(),
            rules(RunStyle::Workflow),
            None,
        );
        assert_eq!(names(&shaped), ["Understand", "Fix", "Check", "Merge"]);
        // The approvals hold back the phase they come before, never a phase of their own.
        let fix = &shaped.phases[1];
        assert_eq!(fix.steps[0].approval, Some(ApprovalKind::Plan));
        assert_eq!(fix.steps[1].kind, StepKind::Build);
        assert_eq!(shaped.phases[2].steps[0].kind, StepKind::GoalCheck);
        assert_eq!(
            shaped.phases[3]
                .steps
                .iter()
                .map(|step| step.kind)
                .collect::<Vec<_>>(),
            [StepKind::Merge]
        );
        assert_eq!(
            shape(shaped.clone(), rules(RunStyle::Workflow), None),
            shaped
        );
    }

    #[test]
    fn the_examples_keep_their_phases_and_gain_only_the_check() {
        for name in ["council", "orchestrator"] {
            let example = builtin(name);
            let shaped = shape(example.clone(), rules(RunStyle::Workflow), None);
            let mut expected = names(&example);
            expected.insert(expected.len() - 1, CHECK_PHASE);
            assert_eq!(names(&shaped), expected, "{name}");
            let off = Rules {
                final_check: false,
                ..rules(RunStyle::Workflow)
            };
            assert_eq!(shape(example.clone(), off, None), example, "{name}");
        }
    }

    /// A council talks from your request: nothing reads it first, and no goal question comes
    /// before the plan.
    #[test]
    fn a_council_opens_with_its_talk() {
        let shaped = shape(builtin("council"), rules(RunStyle::Workflow), None);
        assert_eq!(names(&shaped), ["Talk", "Build", CHECK_PHASE, "Merge"]);
        assert!(!understand_approval_runs(&shaped, UnderstandWhen::Crew));
        assert!(plan_approval_runs(&shaped));
    }

    /// A phase with no steps of its own shows an earlier build's reviews, so it stays.
    #[test]
    fn a_phase_that_shows_reviews_stays() {
        let text = FIX_THEN_REVIEW.replace(
            "    steps:\n      - { kind: goal_check, label: \"check:final\" }\n      - { kind: \
             merge",
            "    steps:\n      - { kind: merge",
        );
        let mut fix = yaml::parse(&text).unwrap();
        fix.phases[0].steps[0].review_in = Some("Review".to_owned());
        fix.phases.insert(
            1,
            Phase {
                name: "Review".to_owned(),
                detail: "another provider reviews each fix".to_owned(),
                standard: StandardPhase::Check,
                steps: Vec::new(),
            },
        );
        let shaped = shape(fix, rules(RunStyle::Workflow), None);
        assert_eq!(
            names(&shaped),
            ["Understand", "Fix", "Review", "Check", "Merge"]
        );
    }

    #[test]
    fn understand_follows_its_setting() {
        let fix = yaml::parse(FIX_THEN_REVIEW).unwrap();
        for (when, opens) in [
            (UnderstandWhen::Crew, true),
            (UnderstandWhen::Main, false),
            (UnderstandWhen::Off, false),
        ] {
            let shaped = shape(
                fix.clone(),
                Rules {
                    understand: when,
                    ..rules(RunStyle::Workflow)
                },
                None,
            );
            assert_eq!(opens_with_understand(&shaped), opens, "{when:?}");
            // Without Understand the Understand approval still comes before building, and takes
            // your prompt as the goal.
            let fix = shaped
                .phases
                .iter()
                .find(|phase| phase.name == "Fix")
                .unwrap();
            assert!(has_approval(std::slice::from_ref(fix), ApprovalKind::Understand) || opens);
        }
    }

    /// No phase for the goal itself, no phase holding only an approval, no nested goal.
    #[test]
    fn no_goal_phase_no_lone_approval_and_no_nested_goal() {
        for name in ["council", "orchestrator", "draft-and-vote"] {
            for style in [RunStyle::Workflow, RunStyle::Goal] {
                let shaped = shape(builtin(name), rules(style), None);
                for phase in &shaped.phases {
                    assert_ne!(phase.name, "Goal", "{name} {style:?}");
                    assert!(
                        phase
                            .steps
                            .iter()
                            .any(|step| step.kind != StepKind::Approval),
                        "{name} {style:?}: {} holds only approvals",
                        phase.name
                    );
                    assert!(!has_kind(&phase.steps, StepKind::Workflow));
                }
            }
        }
        let review = "name: review\ndescription: a review\nphases:\n  - name: Review\n    \
            detail: two reviewers\n    standard: check\n    steps:\n      - { kind: review, \
            input: diff }\n";
        let shaped = shape(
            yaml::parse(review).unwrap(),
            rules(RunStyle::Workflow),
            None,
        );
        assert_eq!(names(&shaped), ["Understand", "Review"]);
    }

    #[test]
    fn a_goal_is_understand_its_tries_then_merge() {
        let shaped = shape(builtin("orchestrator"), rules(RunStyle::Goal), None);
        assert_eq!(names(&shaped), ["Understand", "Try 1", "Merge"]);
        let first = &shaped.phases[1];
        assert!(is_try(first));
        assert_eq!(first.steps[0].input.as_deref(), Some(task::GOAL));
        assert_eq!(
            first.steps.last().unwrap().label.as_deref(),
            Some("check:{try}")
        );
        assert_eq!(shape(shaped.clone(), rules(RunStyle::Goal), None), shaped);

        // The main agent's first try keeps its name and gains its check.
        let planned = Phase {
            name: "Both Fixes".to_owned(),
            detail: "a worker per function".to_owned(),
            standard: StandardPhase::Do,
            steps: try_phase(1, None).steps[..1].to_vec(),
        };
        let shaped = shape(
            builtin("orchestrator"),
            rules(RunStyle::Goal),
            Some(planned),
        );
        assert_eq!(names(&shaped), ["Understand", "Both Fixes", "Merge"]);
        assert!(is_try(&shaped.phases[1]));
    }
}
