//! Checks a workflow before it runs, whether a built-in, your file, or one the lead composed.
//! An invalid one goes back to its author with every error at once. A `workflow` step's
//! target is checked too, in place, so a broken or too deep nested workflow is found before
//! anything runs.

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::events::checkpoint::CheckpointKind;
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::settings::Settings;
use argonauts_core::workflow::{Count, Step, StepKind, StepWorkers, Workflow, WorkflowParams};

use crate::card;
use crate::plan::context::crew_for;
use crate::plan::judge;
use crate::workflow::{conditions, kinds, params};

/// What the rules are checked against.
#[derive(Clone, Copy)]
pub struct Context<'a> {
    pub settings: &'a Settings,
    /// Workers this run may use: the crew and every role's workers.
    pub allowed: &'a [WorkerRef],
    /// Each allowed worker's model family.
    pub families: &'a BTreeMap<WorkerRef, String>,
    /// The workflow a `workflow` step names, found as the runner finds it.
    pub lookup: &'a dyn Fn(&str) -> Result<Workflow, String>,
    /// The depth the workflow would run at: 0 for the run's own.
    pub depth: u32,
    /// What's left of the run budget, when there is one.
    pub budget_left_usd: Option<f64>,
}

/// Each rule a workflow can break, so tests and the lead's retry can name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rule {
    /// Every `build` is followed by a gate and a review by another family.
    BuildNeedsGateAndReview,
    ReviewSameFamily,
    LoopWithoutMax,
    OverBudget,
    WorkerNotAllowed,
    MissingCheckpoint,
    TooDeep,
    UnknownCondition,
    UnknownInput,
    /// A `workflow` step names no workflow that can be found, or one that runs itself.
    UnknownWorkflow,
    /// A field set on a step kind that doesn't take it, or where its executor would ignore it.
    FieldNotForKind,
    /// A value the step's executor can't use, such as `count: 0` or an unknown judging method.
    InvalidValue,
    MissingParam,
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowError {
    pub rule: Rule,
    /// Such as "phase 2 (Build), step 1".
    pub at: String,
    pub message: String,
}

/// The inputs every workflow may name besides its own earlier steps.
pub const RESERVED_INPUTS: &[&str] = &["goal", "plan", "diff", "spikes"];

/// Nesting never goes deeper than this, whatever the settings say.
pub const MAX_DEPTH: u32 = 2;

/// No loop runs more rounds than this, whatever its `max` says.
pub const MAX_LOOP_ROUNDS: u32 = 10;

/// What a card build costs at the least when `budget.per_card_usd` isn't set.
const MIN_CARD_USD: f64 = 0.05;

/// A template is checked as it would run, with its placeholders filled from its own params.
pub fn validate(workflow: &Workflow, context: &Context<'_>) -> Result<(), Vec<WorkflowError>> {
    let mut check = Check {
        context,
        errors: Vec::new(),
        flat: Vec::new(),
        depth: context.depth,
        scope: 0,
        scopes: 1,
        chain: vec![workflow.name.clone()],
    };
    let filled = params::fill(workflow, &WorkflowParams::default());
    let workflow = match &filled {
        Ok(filled) => filled,
        Err(errors) => {
            // Placeholders with no value at all are reported where they are, step by step.
            for error in errors
                .iter()
                .filter(|error| !error.starts_with("no value for"))
            {
                check.error(Rule::InvalidValue, "the workflow's params", error.clone());
            }
            workflow
        }
    };
    let max_depth = check.max_depth();
    if context.depth > max_depth {
        check.error(
            Rule::TooDeep,
            "the workflow",
            format!(
                "it would run at depth {}, and nesting stops at {max_depth}",
                context.depth
            ),
        );
    }
    check.workflow(workflow, "", 1);
    check.order();
    check.budget();
    if check.errors.is_empty() {
        Ok(())
    } else {
        Err(check.errors)
    }
}

impl std::fmt::Display for WorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.at, self.message)
    }
}

/// One step in the order it runs, nested workflows' steps in place of their `workflow` step.
struct Seen {
    at: String,
    kind: StepKind,
    name: Option<String>,
    input: Option<String>,
    checkpoint: Option<CheckpointKind>,
    /// The step may not run: it or a step around it has `when`, or it's inside `for_each`.
    conditional: bool,
    when: Option<String>,
    workers: Vec<WorkerRef>,
    /// How many times it may run: the product of the enclosing loops' maximums.
    runs: u32,
    /// Cards each run builds, at least: a solo `build`'s attempts, else 1.
    cards: u32,
    /// Which workflow it belongs to, since step names are only known inside their own.
    scope: usize,
}

struct Check<'a, 'c> {
    context: &'a Context<'c>,
    errors: Vec<WorkflowError>,
    flat: Vec<Seen>,
    /// The depth of the workflow being checked now.
    depth: u32,
    scope: usize,
    scopes: usize,
    /// The nested workflows being checked, outermost first, to catch one that runs itself.
    chain: Vec<String>,
}

impl Check<'_, '_> {
    fn error(&mut self, rule: Rule, at: &str, message: String) {
        self.errors.push(WorkflowError {
            rule,
            at: at.to_owned(),
            message,
        });
    }

    fn max_depth(&self) -> u32 {
        self.context.settings.workflows.max_depth.min(MAX_DEPTH)
    }

    fn family(&self, worker: &WorkerRef) -> String {
        self.context
            .families
            .get(worker)
            .cloned()
            .unwrap_or_else(|| worker.to_string())
    }

    /// Every phase and step of `workflow`, with `prefix` before each place.
    fn workflow(&mut self, workflow: &Workflow, prefix: &str, runs: u32) {
        if workflow.phases.is_empty() {
            let at = if prefix.is_empty() {
                "the workflow".to_owned()
            } else {
                prefix.trim_end_matches(": ").to_owned()
            };
            self.error(Rule::Empty, &at, "it has no phases".to_owned());
        }
        for (index, phase) in workflow.phases.iter().enumerate() {
            let at = format!("{prefix}phase {} ({})", index + 1, phase.name);
            if phase.steps.is_empty() {
                self.error(Rule::Empty, &at, "the phase has no steps".to_owned());
            }
            for (position, step) in phase.steps.iter().enumerate() {
                self.step(step, format!("{at}, step {}", position + 1), runs, false);
            }
        }
    }

    fn step(&mut self, step: &Step, at: String, runs: u32, conditional: bool) {
        self.fields(step, &at);
        self.placeholders(step, &at);
        for condition in [&step.when, &step.until].into_iter().flatten() {
            if !conditions::KNOWN.contains(&condition.as_str()) {
                self.error(
                    Rule::UnknownCondition,
                    &at,
                    format!(
                        "`{condition}` isn't a known condition; use one of {}",
                        conditions::KNOWN.join(", ")
                    ),
                );
            }
        }
        if let StepWorkers::Named(workers) = &step.workers {
            for worker in workers {
                if !self.context.allowed.contains(worker) {
                    self.error(
                        Rule::WorkerNotAllowed,
                        &at,
                        format!("{worker} isn't in the crew or any role's workers"),
                    );
                }
            }
        }
        let doing_owner_work =
            step.role == Some(RoleName::Owner) || step.roles.contains(&RoleName::Owner);
        if doing_owner_work && step.kind != StepKind::Build {
            self.error(
                Rule::BuildNeedsGateAndReview,
                &at,
                "only a `build` step gives owners work, since only its cards run the gates and a review by another family".to_owned(),
            );
        }
        match step.kind {
            StepKind::LoopUntil => {
                match step.max {
                    None | Some(0) => self.error(
                        Rule::LoopWithoutMax,
                        &at,
                        "a loop needs `max`, a hard limit of at least 1 round".to_owned(),
                    ),
                    Some(max) if max > MAX_LOOP_ROUNDS => self.error(
                        Rule::LoopWithoutMax,
                        &at,
                        format!("`max: {max}` is over the limit of {MAX_LOOP_ROUNDS} rounds"),
                    ),
                    Some(_) => {}
                }
                if step.until.is_none() {
                    self.error(
                        Rule::UnknownCondition,
                        &at,
                        "a loop needs `until`, the condition that ends it".to_owned(),
                    );
                }
            }
            StepKind::ForEach if step.input.is_none() => self.error(
                Rule::UnknownInput,
                &at,
                "`for_each` needs `input`, the step whose items it goes over".to_owned(),
            ),
            StepKind::FanOut if step.role.is_none() && step.roles.is_empty() => self.error(
                Rule::MissingParam,
                &at,
                "`fan_out` needs `role` or `roles`".to_owned(),
            ),
            StepKind::Checkpoint => self.checkpoint(step, &at),
            StepKind::Build => self.build(step, &at),
            StepKind::Judge => self.judge(step, &at),
            _ => {}
        }
        if matches!(step.kind, StepKind::LoopUntil | StepKind::ForEach) && step.steps.is_empty() {
            self.error(Rule::Empty, &at, "it has no steps to repeat".to_owned());
        }
        let conditional = conditional || step.when.is_some();
        self.flat.push(Seen {
            at: at.clone(),
            kind: step.kind,
            name: step.name.clone(),
            input: step.input.clone(),
            checkpoint: step.checkpoint,
            conditional,
            when: step.when.clone(),
            workers: match &step.workers {
                StepWorkers::Named(workers) => workers.clone(),
                StepWorkers::Param(_) => Vec::new(),
            },
            runs,
            cards: match step.count {
                Some(Count::Fixed(count)) if step.kind == StepKind::Build => count.max(1),
                _ => 1,
            },
            scope: self.scope,
        });
        self.review_family(step, &at);
        if step.kind == StepKind::Workflow {
            self.nested(step, &at, runs, conditional);
        }
        let inner_runs = match step.kind {
            StepKind::LoopUntil => {
                runs.saturating_mul(step.max.unwrap_or(1).clamp(1, MAX_LOOP_ROUNDS))
            }
            _ => runs,
        };
        let inner_conditional = conditional || step.kind == StepKind::ForEach;
        for (position, inner) in step.steps.iter().enumerate() {
            self.step(
                inner,
                format!("{at}.{}", position + 1),
                inner_runs,
                inner_conditional,
            );
        }
    }

    /// A `checkpoint` step opens the goal or the plan checkpoint. Only the solo goal
    /// checkpoint may wait for `restate_first`; `order` finds whether a build has the
    /// unconditional ones it needs.
    fn checkpoint(&mut self, step: &Step, at: &str) {
        match step.checkpoint {
            None => self.error(
                Rule::MissingCheckpoint,
                at,
                "a `checkpoint` step names its kind, `goal` or `plan`".to_owned(),
            ),
            Some(CheckpointKind::Goal | CheckpointKind::Plan) => {}
            Some(CheckpointKind::Merge) => self.error(
                Rule::FieldNotForKind,
                at,
                "the merge checkpoint comes with the `merge` step".to_owned(),
            ),
            Some(other) => self.error(
                Rule::FieldNotForKind,
                at,
                format!("a `{other:?}` checkpoint opens by itself when its event happens, never as a step"),
            ),
        }
        if step.when.as_deref() == Some("restate_first")
            && step.checkpoint != Some(CheckpointKind::Goal)
        {
            self.error(
                Rule::MissingCheckpoint,
                at,
                "only the goal checkpoint may wait for `restate_first`".to_owned(),
            );
        }
    }

    /// Checks the workflow a `workflow` step names, with its params filled, one level down.
    /// Its steps join the flat list in place, so the rules about order see them run.
    fn nested(&mut self, step: &Step, at: &str, runs: u32, conditional: bool) {
        let Some(name) = step.workflow.as_ref().map(|name| name.as_str().to_owned()) else {
            self.error(
                Rule::MissingParam,
                at,
                "a `workflow` step names the workflow it runs".to_owned(),
            );
            return;
        };
        let max_depth = self.max_depth();
        if self.depth + 1 > max_depth {
            self.error(
                Rule::TooDeep,
                at,
                format!(
                    "it would start a workflow at depth {}, and nesting stops at {max_depth}",
                    self.depth + 1
                ),
            );
            return;
        }
        if self.chain.contains(&name) {
            self.error(
                Rule::UnknownWorkflow,
                at,
                format!("`{name}` would run itself"),
            );
            return;
        }
        let workflow = match (self.context.lookup)(&name) {
            Ok(workflow) => workflow,
            Err(err) => {
                self.error(Rule::UnknownWorkflow, at, err);
                return;
            }
        };
        let workflow = match params::fill(&workflow, &step.params) {
            Ok(filled) => filled,
            Err(errors) => {
                for error in errors {
                    self.error(Rule::MissingParam, at, format!("{name}: {error}"));
                }
                return;
            }
        };
        let (scope, depth) = (self.scope, self.depth);
        self.scope = self.scopes;
        self.scopes += 1;
        self.depth += 1;
        self.chain.push(name.clone());
        let first = self.flat.len();
        self.workflow(&workflow, &format!("{at}, in {name}: "), runs);
        if conditional {
            for seen in &mut self.flat[first..] {
                seen.conditional = true;
            }
        }
        self.chain.pop();
        self.depth = depth;
        self.scope = scope;
    }

    /// Each kind takes only its own fields, so a misplaced one never silently does nothing.
    fn fields(&mut self, step: &Step, at: &str) {
        let allowed = allowed_fields(step.kind);
        let set = [
            ("role", step.role.is_some()),
            ("roles", !step.roles.is_empty()),
            ("workers", !step.workers.is_empty()),
            ("count", step.count.is_some()),
            ("blind", step.blind),
            (
                "different_family_from",
                step.different_family_from.is_some(),
            ),
            ("task", step.task.is_some()),
            ("input", step.input.is_some()),
            ("steps", !step.steps.is_empty()),
            ("until", step.until.is_some()),
            ("max", step.max.is_some()),
            ("checkpoint", step.checkpoint.is_some()),
            ("workflow", step.workflow.is_some()),
            ("params", !step.params.is_empty()),
        ];
        for (field, _) in set
            .iter()
            .filter(|(field, set)| *set && !allowed.contains(field))
        {
            self.error(
                Rule::FieldNotForKind,
                at,
                format!("`{}` doesn't take `{field}`", kind_name(step.kind)),
            );
        }
        // Only reviewers pick their workers by family; judges and critics pick their own.
        if step.kind == StepKind::FanOut && step.different_family_from.is_some() {
            let roles = step.role.iter().chain(&step.roles);
            for role in roles.filter(|role| kinds::kind_for_role(**role) != Some(StepKind::Review))
            {
                self.error(
                    Rule::FieldNotForKind,
                    at,
                    format!(
                        "`different_family_from` picks reviewers, and the {} role doesn't review",
                        role.as_str()
                    ),
                );
            }
        }
    }

    /// What `build` does comes from its `input`: `goal` builds the solo card, `count` attempts
    /// of it, reviewed by another family with `different_family_from: author`; `plan` builds
    /// the plan's cards, whose owners and reviewers the plan checkpoint settles.
    fn build(&mut self, step: &Step, at: &str) {
        if let Some(role) = step.role.filter(|role| *role != RoleName::Owner) {
            self.error(
                Rule::FieldNotForKind,
                at,
                format!(
                    "`build` gives owners work, so it takes `role: owner`, not `{}`",
                    role.as_str()
                ),
            );
        }
        let solo = match step.input.as_deref() {
            Some(card::GOAL) => true,
            Some("plan" | card::SPIKES) => false,
            other => {
                let said = other.map_or_else(
                    || "no `input`".to_owned(),
                    |input| format!("`input: {input}`"),
                );
                self.error(
                    Rule::UnknownInput,
                    at,
                    format!(
                        "`build` has {said}; it builds `goal` (the solo card), `plan` (the plan's cards), or `spikes`"
                    ),
                );
                return;
            }
        };
        if !solo {
            for (field, set) in [
                ("count", step.count.is_some()),
                (
                    "different_family_from",
                    step.different_family_from.is_some(),
                ),
            ] {
                if set {
                    self.error(
                        Rule::FieldNotForKind,
                        at,
                        format!(
                            "only `build` with `input: goal` takes `{field}`; the plan checkpoint settles who builds and reviews the plan's cards"
                        ),
                    );
                }
            }
            return;
        }
        if step.count == Some(Count::Fixed(0)) {
            self.error(
                Rule::InvalidValue,
                at,
                "`count` is the number of attempts, at least 1".to_owned(),
            );
        }
        if let Some(from) = step
            .different_family_from
            .as_deref()
            .filter(|from| *from != card::AUTHOR && !from.contains("{{"))
        {
            self.error(
                Rule::InvalidValue,
                at,
                format!(
                    "a `build` is reviewed by another family than its `author`, not than `{from}`"
                ),
            );
        }
    }

    /// A planning `judge` may name its method in `params`, such as `method: vote`; the
    /// adjudicator rules on reviews and takes none.
    fn judge(&mut self, step: &Step, at: &str) {
        if step.params.is_empty() {
            return;
        }
        if step.role == Some(RoleName::Adjudicator) {
            self.error(
                Rule::FieldNotForKind,
                at,
                "the adjudicator rules on disagreeing reviews and takes no `params`".to_owned(),
            );
            return;
        }
        for name in step.params.0.keys().filter(|name| *name != judge::METHOD) {
            self.error(
                Rule::FieldNotForKind,
                at,
                format!(
                    "`judge` takes only `{}` in its params, not `{name}`",
                    judge::METHOD
                ),
            );
        }
        if let Err(message) = judge::method(step) {
            self.error(Rule::InvalidValue, at, message);
        }
    }

    /// Placeholders are filled before validation, so any left had no value.
    fn placeholders(&mut self, step: &Step, at: &str) {
        let mut left = Vec::new();
        if let Some(Count::Param(text)) = &step.count {
            left.push(text.clone());
        }
        if let StepWorkers::Param(text) = &step.workers {
            left.push(text.clone());
        }
        for text in [
            &step.name,
            &step.different_family_from,
            &step.when,
            &step.task,
            &step.input,
            &step.until,
        ]
        .into_iter()
        .flatten()
        {
            if text.contains("{{") {
                left.push(text.clone());
            }
        }
        for value in step.params.0.values() {
            if value.as_str().is_some_and(|text| text.contains("{{")) {
                left.push(value.to_string());
            }
        }
        for text in left {
            self.error(Rule::MissingParam, at, format!("`{text}` has no value"));
        }
    }

    /// A reviewer never reviews its own work, and with `different_family_from` never work
    /// from its own model family. Only named workers are known here; the runner leaves out
    /// the rest by family when it picks reviewers.
    fn review_family(&mut self, step: &Step, at: &str) {
        if !matches!(step.kind, StepKind::Review | StepKind::FanOut) {
            return;
        }
        let from = step.different_family_from.as_deref();
        let target = match from {
            Some("author") | None => step.input.as_deref(),
            Some(name) => {
                if !self.named_here(name) {
                    self.error(
                        Rule::ReviewSameFamily,
                        at,
                        format!(
                            "`different_family_from: {name}` names neither `author` nor an earlier step"
                        ),
                    );
                    return;
                }
                Some(name)
            }
        };
        let Some(target) = target else {
            return;
        };
        let authors: Vec<WorkerRef> = self
            .flat
            .iter()
            .filter(|seen| seen.scope == self.scope && seen.name.as_deref() == Some(target))
            .flat_map(|seen| seen.workers.clone())
            .collect();
        if authors.is_empty() {
            return;
        }
        let named = match &step.workers {
            StepWorkers::Named(workers) if !workers.is_empty() => Some(workers.clone()),
            _ => None,
        };
        if from.is_none() {
            for reviewer in named.iter().flatten().filter(|r| authors.contains(r)) {
                self.error(
                    Rule::ReviewSameFamily,
                    at,
                    format!("{reviewer} would review its own work from `{target}`"),
                );
            }
            return;
        }
        let author_families: BTreeSet<String> =
            authors.iter().map(|worker| self.family(worker)).collect();
        match named {
            Some(reviewers) => {
                for reviewer in &reviewers {
                    let family = self.family(reviewer);
                    if author_families.contains(&family) {
                        self.error(
                            Rule::ReviewSameFamily,
                            at,
                            format!(
                                "{reviewer} is of the {family} family, like the authors of `{target}`"
                            ),
                        );
                    }
                }
            }
            None => {
                let pool = crew_for(self.context.settings, RoleName::Reviewer);
                if !pool
                    .iter()
                    .any(|reviewer| !author_families.contains(&self.family(reviewer)))
                {
                    self.error(
                        Rule::ReviewSameFamily,
                        at,
                        format!(
                            "no reviewer comes from another family than the authors of `{target}`"
                        ),
                    );
                }
            }
        }
    }

    /// A step called `name` came earlier in the workflow being checked now.
    fn named_here(&self, name: &str) -> bool {
        self.flat
            .iter()
            .any(|seen| seen.scope == self.scope && seen.name.as_deref() == Some(name))
    }

    /// The rules about order: inputs come from earlier steps of the same workflow, and
    /// building needs the merge after it and, from a plan, the goal and plan checkpoints
    /// before it, neither of them conditional.
    fn order(&mut self) {
        let mut errors = Vec::new();
        let crew_build = self
            .flat
            .iter()
            .any(|seen| seen.kind == StepKind::Build && !solo_build(seen));
        for (index, seen) in self.flat.iter().enumerate() {
            let before = &self.flat[..index];
            let after = &self.flat[index + 1..];
            if let Some(input) = &seen.input {
                let known = RESERVED_INPUTS.contains(&input.as_str())
                    || before.iter().any(|earlier| {
                        earlier.scope == seen.scope
                            && earlier.name.as_deref() == Some(input.as_str())
                    });
                if !known {
                    errors.push((
                        Rule::UnknownInput,
                        seen.at.clone(),
                        format!(
                            "`input: {input}` names no earlier step, nor {}",
                            RESERVED_INPUTS.join(", ")
                        ),
                    ));
                }
            }
            if crew_build
                && seen.kind == StepKind::Checkpoint
                && seen.when.as_deref() == Some("restate_first")
            {
                errors.push((
                    Rule::MissingCheckpoint,
                    seen.at.clone(),
                    "only a solo workflow's goal checkpoint may wait for `restate_first`; a crew always asks you"
                        .to_owned(),
                ));
            }
            if seen.kind != StepKind::Build {
                continue;
            }
            if !after.iter().any(|later| later.kind == StepKind::Merge) {
                errors.push((
                    Rule::MissingCheckpoint,
                    seen.at.clone(),
                    "a `build` needs a `merge` after it, where the merge checkpoint asks"
                        .to_owned(),
                ));
            }
            if solo_build(seen) {
                continue;
            }
            for (needed, name) in [
                (CheckpointKind::Goal, "goal"),
                (CheckpointKind::Plan, "plan"),
            ] {
                let present = before.iter().any(|earlier| {
                    earlier.kind == StepKind::Checkpoint
                        && earlier.checkpoint == Some(needed)
                        && !earlier.conditional
                });
                if !present {
                    errors.push((
                        Rule::MissingCheckpoint,
                        seen.at.clone(),
                        format!(
                            "building from a plan needs the {name} checkpoint before it, with no `when` and outside `for_each`"
                        ),
                    ));
                }
            }
        }
        for (rule, at, message) in errors {
            self.error(rule, &at, message);
        }
    }

    /// A floor on what the workflow costs, nested workflows included: each card build at
    /// `budget.per_card_usd`, times the rounds its loops may run.
    fn budget(&mut self) {
        let Some(left) = self.context.budget_left_usd else {
            return;
        };
        let per_card = self
            .context
            .settings
            .budget
            .per_card_usd
            .filter(|usd| *usd > 0.0)
            .unwrap_or(MIN_CARD_USD);
        let estimate: f64 = self
            .flat
            .iter()
            .filter(|seen| seen.kind == StepKind::Build)
            .map(|seen| per_card * f64::from(seen.runs) * f64::from(seen.cards))
            .sum();
        let spends = self.flat.iter().any(|seen| takes_agents(seen.kind));
        if estimate > left || (left <= 0.0 && spends) {
            self.error(
                Rule::OverBudget,
                "the workflow",
                format!("it needs at least ${estimate:.2}, and ${left:.2} of the budget is left"),
            );
        }
    }
}

/// A solo build makes one card from the goal, so it needs no plan and no plan checkpoint.
fn solo_build(seen: &Seen) -> bool {
    seen.input.as_deref() == Some(card::GOAL)
}

fn takes_agents(kind: StepKind) -> bool {
    !matches!(
        kind,
        StepKind::Gate
            | StepKind::LoopUntil
            | StepKind::ForEach
            | StepKind::Checkpoint
            | StepKind::Workflow
    )
}

fn allowed_fields(kind: StepKind) -> &'static [&'static str] {
    match kind {
        StepKind::Restate => &["role", "workers", "count", "task", "input"],
        StepKind::Differ => &["role", "workers", "count", "input"],
        StepKind::Draft => &["role", "workers", "count", "blind", "task", "input"],
        StepKind::Map => &["role", "workers", "input"],
        StepKind::FanOut => &[
            "role",
            "roles",
            "workers",
            "count",
            "blind",
            "different_family_from",
            "task",
            "input",
        ],
        StepKind::ForEach => &["input", "steps"],
        StepKind::Review => &[
            "role",
            "workers",
            "count",
            "blind",
            "different_family_from",
            "task",
            "input",
        ],
        StepKind::Judge => &[
            "role", "workers", "count", "blind", "task", "input", "params",
        ],
        StepKind::Refute => &["role", "workers", "count", "task", "input"],
        StepKind::Synthesize => &["role", "workers", "task", "input"],
        // The solo card's owner is the default agent, the plan's owners come from the plan
        // checkpoint, and each card's task is its spec, so `workers` and `task` would do nothing.
        StepKind::Build => &["role", "count", "different_family_from", "input"],
        StepKind::Gate => &["input"],
        StepKind::LoopUntil => &["steps", "until", "max"],
        StepKind::Checkpoint => &["checkpoint", "input"],
        StepKind::Merge => &[],
        StepKind::Workflow => &["workflow", "params"],
    }
}

/// The kind as a workflow file spells it.
pub fn kind_name(kind: StepKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{kind:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::{builtin, yaml};

    fn rules(steps: &str) -> Vec<Rule> {
        let text = format!(
            "name: checked\nparams:\n  tries: 2\nphases:\n  - name: Build\n    standard: do\n    steps:\n{steps}      - kind: merge\n"
        );
        let workflow = yaml::parse(&text).unwrap_or_else(|err| panic!("{err}\n{text}"));
        let settings = Settings::default();
        let families = BTreeMap::new();
        let lookup = |name: &str| builtin::load(name);
        let context = Context {
            settings: &settings,
            allowed: &[],
            families: &families,
            lookup: &lookup,
            depth: 0,
            budget_left_usd: None,
        };
        match validate(&workflow, &context) {
            Ok(()) => Vec::new(),
            Err(errors) => errors.into_iter().map(|error| error.rule).collect(),
        }
    }

    #[test]
    fn build_refuses_what_its_executor_would_ignore() {
        let cases = [
            ("        workers: [codex]\n", Rule::FieldNotForKind),
            ("        task: do it twice\n", Rule::FieldNotForKind),
            ("        role: reviewer\n", Rule::FieldNotForKind),
            ("        count: 0\n", Rule::InvalidValue),
            ("        different_family_from: judge\n", Rule::InvalidValue),
        ];
        for (field, rule) in cases {
            let step = format!("      - kind: build\n        input: goal\n{field}");
            assert!(rules(&step).contains(&rule), "{field}: {:?}", rules(&step));
        }
        let unknown = rules("      - kind: build\n        role: owner\n");
        assert!(unknown.contains(&Rule::UnknownInput), "{unknown:?}");
        // The plan's cards get their owners and reviewers at the plan checkpoint.
        for field in ["count: 2", "different_family_from: author"] {
            let step = format!(
                "      - kind: checkpoint\n        checkpoint: goal\n      - kind: checkpoint\n        checkpoint: plan\n      - kind: build\n        input: plan\n        {field}\n"
            );
            assert_eq!(rules(&step), [Rule::FieldNotForKind], "{field}");
        }
    }

    #[test]
    fn solo_builds_take_attempts_and_a_review() {
        let step = "      - kind: build\n        role: owner\n        input: goal\n        count: \"{{tries}}\"\n        different_family_from: author\n";
        assert_eq!(rules(step), []);
    }

    #[test]
    fn judge_names_a_known_method_only() {
        let judge = |params: &str| {
            rules(&format!(
                "      - kind: judge\n        role: judge\n        params:\n{params}"
            ))
        };
        assert_eq!(judge("          method: vote\n"), []);
        assert_eq!(judge("          method: loudest\n"), [Rule::InvalidValue]);
        assert_eq!(judge("          rounds: 2\n"), [Rule::FieldNotForKind]);
    }

    #[test]
    fn builtins_validate_with_their_own_params() {
        let settings = Settings::default();
        let families = BTreeMap::new();
        let lookup = |name: &str| builtin::load(name);
        let context = Context {
            settings: &settings,
            allowed: &[],
            families: &families,
            lookup: &lookup,
            depth: 0,
            budget_left_usd: None,
        };
        for (name, _) in builtin::BUILTIN {
            let workflow = builtin::load(name).unwrap_or_else(|err| panic!("{err}"));
            assert_eq!(validate(&workflow, &context), Ok(()), "{name}");
        }
    }
}
