//! Checks a workflow before it runs, whether a built-in, your file, or one the main agent composed.
//! An invalid one goes back to its author with every error at once. A `workflow` step's
//! target is checked too, in place, so a broken or too deep nested workflow is found before
//! anything runs.

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::events::approval::ApprovalKind;
use argonauts_core::events::run::CheckedInstruction;
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::settings::Settings;
use argonauts_core::steps::intake::InstructionKind;
use argonauts_core::workflow::{
    Count, StandardPhase, Step, StepKind, StepModels, Workflow, WorkflowParams,
};

use argonauts_core::labels;

use crate::plan::context::crew_for;
use crate::plan::judge;
use crate::task;
use crate::workflow::{conditions, kinds, params};

/// What the rules are checked against.
#[derive(Clone, Copy)]
pub struct Context<'a> {
    pub settings: &'a Settings,
    /// Workers this run may use: the crew and every role's workers.
    pub allowed: &'a [ModelRef],
    /// Each allowed worker's model provider.
    pub providers: &'a BTreeMap<ModelRef, String>,
    /// The workflow a `workflow` step names, found as the runner finds it.
    pub lookup: &'a dyn Fn(&str) -> Result<Workflow, String>,
    /// The depth the workflow would run at: 0 for the run's own.
    pub depth: u32,
    /// What's left of the run limit, when there is one.
    pub limit_left_usd: Option<f64>,
    /// The roles your request gave to agents, each with its agents. A step of one of these
    /// roles may name only them.
    pub as_you_asked: &'a [(RoleName, Vec<ModelRef>)],
}

/// The roles a followed instruction gave to agents, with the agents the request layer set
/// for each.
pub fn roles_as_you_asked(
    instructions: &[CheckedInstruction],
    settings: &Settings,
) -> Vec<(RoleName, Vec<ModelRef>)> {
    let mut roles: Vec<(RoleName, Vec<ModelRef>)> = Vec::new();
    for instruction in instructions.iter().filter(|i| i.followed()) {
        let role = match instruction.kind {
            InstructionKind::Build => RoleName::Worker,
            InstructionKind::Review => RoleName::Reviewer,
            InstructionKind::Judge => RoleName::Judge,
            InstructionKind::Plan => RoleName::Draft,
            InstructionKind::Orchestrate => RoleName::Orchestrator,
            _ => continue,
        };
        let workers = settings
            .roles
            .get(&role)
            .map(|settings| settings.models.clone())
            .unwrap_or_default();
        if !workers.is_empty() && !roles.iter().any(|(known, _)| *known == role) {
            roles.push((role, workers));
        }
    }
    roles
}

/// Each rule a workflow can break, so tests and the main agent's retry can name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rule {
    /// Every `build` is followed by a check and a review by another provider.
    BuildNeedsCheckAndReview,
    ReviewSameProvider,
    LoopWithoutMax,
    OverLimit,
    ModelNotAllowed,
    /// A step gives a role your request gave to some agents to another agent.
    NotAsYouAsked,
    MissingApproval,
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
    /// A label that isn't one lowercase token of at most 20 cells, or one two steps share.
    Label,
    /// A `review_in` that names no later phase.
    ReviewIn,
    /// Two phases share a name, so a `review_in` or a row couldn't tell them apart.
    PhaseName,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowError {
    pub rule: Rule,
    /// Such as "phase 2 (Build), step 1".
    pub at: String,
    pub message: String,
}

/// The inputs every workflow may name besides its own earlier steps.
pub const RESERVED_INPUTS: &[&str] = &["goal", "plan", "diff", "experiments"];

/// Nesting never goes deeper than this, whatever the settings say.
pub const MAX_DEPTH: u32 = 2;

/// No loop runs more rounds than this, whatever its `max` says.
pub const MAX_LOOP_ROUNDS: u32 = 10;

/// What a task build costs at the least when `limits.per_task_usd` isn't set.
const MIN_TASK_USD: f64 = 0.05;

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
        standard: StandardPhase::Understand,
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
    check.limit();
    if check.errors.is_empty() {
        Ok(())
    } else {
        Err(check.errors)
    }
}

impl std::fmt::Display for WorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // "the workflow needs at least $3.00", where "the workflow: it needs …" would stack
        // colons once a notice quotes it.
        match self.message.strip_prefix("it ") {
            Some(rest) if self.at == "the workflow" => write!(f, "the workflow {rest}"),
            _ => write!(f, "{}: {}", self.at, self.message),
        }
    }
}

/// One step in the order it runs, nested workflows' steps in place of their `workflow` step.
struct Seen {
    at: String,
    kind: StepKind,
    name: Option<String>,
    input: Option<String>,
    approval: Option<ApprovalKind>,
    /// The step may not run: it or a step around it has `when`, or it's inside `for_each`.
    conditional: bool,
    when: Option<String>,
    workers: Vec<ModelRef>,
    /// How many times it may run: the product of the enclosing loops' maximums.
    runs: u32,
    /// Tasks each run builds, at least: a Captain `build`'s tries, else 1.
    tasks: u32,
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
    /// The standard of the phase whose steps are being checked now.
    standard: StandardPhase,
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

    fn provider(&self, worker: &ModelRef) -> String {
        self.context
            .providers
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
        let mut names = BTreeSet::new();
        for (index, phase) in workflow.phases.iter().enumerate() {
            if !names.insert(phase.name.trim().to_lowercase()) {
                let at = format!("{prefix}phase {} ({})", index + 1, phase.name);
                self.error(
                    Rule::PhaseName,
                    &at,
                    format!("another phase is also named `{}`", phase.name.trim()),
                );
            }
        }
        // A phase may leave its steps to an earlier build's reviews, which show there.
        let mut reviewed_in = BTreeSet::new();
        for (index, phase) in workflow.phases.iter().enumerate() {
            let at = format!("{prefix}phase {} ({})", index + 1, phase.name);
            for (position, target) in review_targets(&phase.steps) {
                let at = format!("{at}, step {}", position + 1);
                match workflow
                    .phases
                    .iter()
                    .position(|later| later.name == target)
                {
                    Some(later) if later > index => {
                        reviewed_in.insert(later);
                    }
                    Some(_) => self.error(
                        Rule::ReviewIn,
                        &at,
                        format!(
                            "`review_in: {target}` names an earlier phase; it must be a later one"
                        ),
                    ),
                    None => self.error(
                        Rule::ReviewIn,
                        &at,
                        format!("`review_in: {target}` names no phase of this workflow"),
                    ),
                }
            }
        }
        let mut labels = BTreeMap::new();
        let outer = self.standard;
        for (index, phase) in workflow.phases.iter().enumerate() {
            self.standard = phase.standard;
            let at = format!("{prefix}phase {} ({})", index + 1, phase.name);
            if phase.steps.is_empty() && !reviewed_in.contains(&index) {
                self.error(Rule::Empty, &at, "the phase has no steps".to_owned());
            }
            for (position, step) in phase.steps.iter().enumerate() {
                let at = format!("{at}, step {}", position + 1);
                self.labels(step, &at, &mut labels);
                self.step(step, at, runs, false);
            }
        }
        self.standard = outer;
    }

    /// Each step's label pattern, and no fixed label twice: two agents never share a name.
    fn labels(&mut self, step: &Step, at: &str, seen: &mut BTreeMap<String, String>) {
        if let Some(label) = &step.label {
            if let Some(problem) = labels::pattern_problem(label) {
                self.error(Rule::Label, at, problem);
            } else if !label.contains('{') {
                match seen.get(label) {
                    Some(first) => self.error(
                        Rule::Label,
                        at,
                        format!("label `{label}` is also {first}'s; each label names one agent"),
                    ),
                    None => {
                        seen.insert(label.clone(), at.to_owned());
                    }
                }
            }
        }
        for (position, inner) in step.steps.iter().enumerate() {
            self.labels(inner, &format!("{at}.{}", position + 1), seen);
        }
    }

    /// A step of a role your request gave to some agents names only them.
    fn as_you_asked(&mut self, step: &Step, at: &str) {
        let StepModels::Named(workers) = &step.models else {
            return;
        };
        let default_role = match step.kind {
            StepKind::Review => Some(RoleName::Reviewer),
            StepKind::Build => Some(RoleName::Worker),
            StepKind::Draft => Some(RoleName::Draft),
            _ => None,
        };
        let roles: Vec<RoleName> = match (step.role, step.roles.is_empty()) {
            (Some(role), _) => vec![role],
            (None, false) => step.roles.clone(),
            (None, true) => default_role.into_iter().collect(),
        };
        for role in roles {
            let Some((_, asked)) = self.context.as_you_asked.iter().find(|(r, _)| *r == role)
            else {
                continue;
            };
            for worker in workers.iter().filter(|worker| !asked.contains(worker)) {
                let names: Vec<String> = asked.iter().map(ToString::to_string).collect();
                self.error(
                    Rule::NotAsYouAsked,
                    at,
                    format!(
                        "the request gives the {} role to {}, so {worker} can't take it",
                        role.as_str(),
                        names.join(" and ")
                    ),
                );
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
        if let StepModels::Named(workers) = &step.models {
            for worker in workers {
                if !self.context.allowed.contains(worker) {
                    self.error(
                        Rule::ModelNotAllowed,
                        &at,
                        format!("{worker} isn't in the crew or any role's models"),
                    );
                }
            }
        }
        self.as_you_asked(step, &at);
        let doing_worker_work =
            step.role == Some(RoleName::Worker) || step.roles.contains(&RoleName::Worker);
        if doing_worker_work && step.kind != StepKind::Build {
            self.error(
                Rule::BuildNeedsCheckAndReview,
                &at,
                "only a `build` step gives workers work, since only its tasks run the checks and a review by another provider".to_owned(),
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
            StepKind::Approval => self.approval(step, &at),
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
            approval: step.approval,
            conditional,
            when: step.when.clone(),
            workers: match &step.models {
                StepModels::Named(workers) => workers.clone(),
                StepModels::Param(_) => Vec::new(),
            },
            runs,
            tasks: match step.count {
                Some(Count::Fixed(count)) if step.kind == StepKind::Build => count.max(1),
                _ => 1,
            },
            scope: self.scope,
        });
        self.review_provider(step, &at);
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

    /// An `approval` step opens the Understand or the plan approval. Only the Captain Understand
    /// approval may wait for `understand_first`; `order` finds whether a build has the
    /// unconditional ones it needs.
    fn approval(&mut self, step: &Step, at: &str) {
        match step.approval {
            None => self.error(
                Rule::MissingApproval,
                at,
                "an `approval` step names its kind, `understand` or `plan`".to_owned(),
            ),
            Some(ApprovalKind::Understand | ApprovalKind::Plan) => {}
            Some(ApprovalKind::Merge) => self.error(
                Rule::FieldNotForKind,
                at,
                "the merge approval comes with the `merge` step".to_owned(),
            ),
            Some(other) => self.error(
                Rule::FieldNotForKind,
                at,
                format!(
                    "a `{other:?}` approval opens by itself when its event happens, never as a step"
                ),
            ),
        }
        if step.when.as_deref() == Some("understand_first")
            && step.approval != Some(ApprovalKind::Understand)
        {
            self.error(
                Rule::MissingApproval,
                at,
                "only the Understand approval may wait for `understand_first`".to_owned(),
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
            ("models", !step.models.is_empty()),
            ("count", step.count.is_some()),
            ("blind", step.blind),
            (
                "different_provider_from",
                step.different_provider_from.is_some(),
            ),
            ("task", step.task.is_some()),
            ("input", step.input.is_some()),
            ("steps", !step.steps.is_empty()),
            ("until", step.until.is_some()),
            ("max", step.max.is_some()),
            ("approval", step.approval.is_some()),
            ("workflow", step.workflow.is_some()),
            ("params", !step.params.is_empty()),
            ("review_in", step.review_in.is_some()),
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
        // Only reviewers pick their workers by provider; judges and critics pick their own.
        if step.kind == StepKind::FanOut && step.different_provider_from.is_some() {
            let roles = step.role.iter().chain(&step.roles);
            for role in roles.filter(|role| kinds::kind_for_role(**role) != Some(StepKind::Review))
            {
                self.error(
                    Rule::FieldNotForKind,
                    at,
                    format!(
                        "`different_provider_from` picks reviewers, and the {} role doesn't review",
                        role.as_str()
                    ),
                );
            }
        }
    }

    /// What `build` does comes from its `input`: `goal` builds the Captain task, `count` tries
    /// of it, reviewed by another provider with `different_provider_from: author`; `plan` builds
    /// the plan's tasks, whose workers and reviewers the plan approval settles.
    fn build(&mut self, step: &Step, at: &str) {
        if let Some(role) = step.role.filter(|role| *role != RoleName::Worker) {
            self.error(
                Rule::FieldNotForKind,
                at,
                format!(
                    "`build` gives workers work, so it takes `role: worker`, not `{}`",
                    role.as_str()
                ),
            );
        }
        let captain = match step.input.as_deref() {
            Some(task::GOAL) => true,
            Some("plan" | task::EXPERIMENTS) => false,
            other => {
                let said = other.map_or_else(
                    || "no `input`".to_owned(),
                    |input| format!("`input: {input}`"),
                );
                self.error(
                    Rule::UnknownInput,
                    at,
                    format!(
                        "`build` has {said}; it builds `goal` (the Captain task), `plan` (the plan's tasks), or `experiments`"
                    ),
                );
                return;
            }
        };
        if !captain {
            for (field, set) in [
                ("count", step.count.is_some()),
                (
                    "different_provider_from",
                    step.different_provider_from.is_some(),
                ),
            ] {
                if set {
                    self.error(
                        Rule::FieldNotForKind,
                        at,
                        format!(
                            "only `build` with `input: goal` takes `{field}`; the plan approval settles who builds and reviews the plan's tasks"
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
                "`count` is the number of tries, at least 1".to_owned(),
            );
        }
        if let Some(from) = step
            .different_provider_from
            .as_deref()
            .filter(|from| *from != task::AUTHOR && !from.contains("{{"))
        {
            self.error(
                Rule::InvalidValue,
                at,
                format!(
                    "a `build` is reviewed by another provider than its `author`, not than `{from}`"
                ),
            );
        }
    }

    /// A planning `judge` may name its method in `params`, such as `method: vote`; outside a
    /// plan phase the judge settles reviewers who disagree and takes none.
    fn judge(&mut self, step: &Step, at: &str) {
        if step.params.is_empty() {
            return;
        }
        if self.standard != StandardPhase::Plan {
            self.error(
                Rule::FieldNotForKind,
                at,
                "outside a plan phase the judge settles reviewers who disagree and takes no \
                 `params`"
                    .to_owned(),
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
        if let StepModels::Param(text) = &step.models {
            left.push(text.clone());
        }
        for text in [
            &step.name,
            &step.different_provider_from,
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

    /// A reviewer never reviews its own work, and with `different_provider_from` never work
    /// from its own model provider. Only named workers are known here; the runner leaves out
    /// the rest by provider when it picks reviewers.
    fn review_provider(&mut self, step: &Step, at: &str) {
        if !matches!(step.kind, StepKind::Review | StepKind::FanOut) {
            return;
        }
        let from = step.different_provider_from.as_deref();
        let target = match from {
            Some("author") | None => step.input.as_deref(),
            Some(name) => {
                if !self.named_here(name) {
                    self.error(
                        Rule::ReviewSameProvider,
                        at,
                        format!(
                            "`different_provider_from: {name}` names neither `author` nor an earlier step"
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
        let authors: Vec<ModelRef> = self
            .flat
            .iter()
            .filter(|seen| seen.scope == self.scope && seen.name.as_deref() == Some(target))
            .flat_map(|seen| seen.workers.clone())
            .collect();
        if authors.is_empty() {
            return;
        }
        let named = match &step.models {
            StepModels::Named(workers) if !workers.is_empty() => Some(workers.clone()),
            _ => None,
        };
        if from.is_none() {
            for reviewer in named.iter().flatten().filter(|r| authors.contains(r)) {
                self.error(
                    Rule::ReviewSameProvider,
                    at,
                    format!("{reviewer} would review its own work from `{target}`"),
                );
            }
            return;
        }
        let author_providers: BTreeSet<String> =
            authors.iter().map(|worker| self.provider(worker)).collect();
        match named {
            Some(reviewers) => {
                for reviewer in &reviewers {
                    let provider = self.provider(reviewer);
                    if author_providers.contains(&provider) {
                        self.error(
                            Rule::ReviewSameProvider,
                            at,
                            format!(
                                "{reviewer} is of the {provider} provider, like the authors of `{target}`"
                            ),
                        );
                    }
                }
            }
            None => {
                let pool = crew_for(self.context.settings, RoleName::Reviewer);
                if !pool
                    .iter()
                    .any(|reviewer| !author_providers.contains(&self.provider(reviewer)))
                {
                    self.error(
                        Rule::ReviewSameProvider,
                        at,
                        format!(
                            "no reviewer comes from another provider than the authors of `{target}`"
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
    /// building needs the merge after it and, from a plan, the Understand and plan approvals
    /// before it, neither of them conditional.
    fn order(&mut self) {
        let mut errors = Vec::new();
        let crew_build = self
            .flat
            .iter()
            .any(|seen| seen.kind == StepKind::Build && !captain_build(seen));
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
                && seen.kind == StepKind::Approval
                && seen.when.as_deref() == Some("understand_first")
            {
                errors.push((
                    Rule::MissingApproval,
                    seen.at.clone(),
                    "only a Captain workflow's Understand approval may wait for `understand_first`; a crew always asks you"
                        .to_owned(),
                ));
            }
            if seen.kind != StepKind::Build {
                continue;
            }
            if !after.iter().any(|later| later.kind == StepKind::Merge) {
                errors.push((
                    Rule::MissingApproval,
                    seen.at.clone(),
                    "a `build` needs a `merge` after it, where the merge approval asks".to_owned(),
                ));
            }
            if captain_build(seen) {
                continue;
            }
            for (needed, name) in [
                (ApprovalKind::Understand, "Understand"),
                (ApprovalKind::Plan, "plan"),
            ] {
                let present = before.iter().any(|earlier| {
                    earlier.kind == StepKind::Approval
                        && earlier.approval == Some(needed)
                        && !earlier.conditional
                });
                if !present {
                    errors.push((
                        Rule::MissingApproval,
                        seen.at.clone(),
                        format!(
                            "building from a plan needs the {name} approval before it, with no `when` and outside `for_each`"
                        ),
                    ));
                }
            }
        }
        for (rule, at, message) in errors {
            self.error(rule, &at, message);
        }
    }

    /// A floor on what the workflow costs, nested workflows included: each task build at
    /// `limits.per_task_usd`, times the rounds its loops may run.
    fn limit(&mut self) {
        let Some(left) = self.context.limit_left_usd else {
            return;
        };
        let per_task = self
            .context
            .settings
            .limits
            .spend
            .per_task_usd
            .filter(|usd| *usd > 0.0)
            .unwrap_or(MIN_TASK_USD);
        let estimate: f64 = self
            .flat
            .iter()
            .filter(|seen| seen.kind == StepKind::Build)
            .map(|seen| per_task * f64::from(seen.runs) * f64::from(seen.tasks))
            .sum();
        let spends = self.flat.iter().any(|seen| takes_agents(seen.kind));
        if estimate > left || (left <= 0.0 && spends) {
            self.error(
                Rule::OverLimit,
                "the workflow",
                format!("it needs at least ${estimate:.2}, and ${left:.2} of the limit is left"),
            );
        }
    }
}

/// A Captain build makes one task from the goal, so it needs no plan and no plan approval.
fn captain_build(seen: &Seen) -> bool {
    seen.input.as_deref() == Some(task::GOAL)
}

fn takes_agents(kind: StepKind) -> bool {
    !matches!(
        kind,
        StepKind::Checks
            | StepKind::LoopUntil
            | StepKind::ForEach
            | StepKind::Approval
            | StepKind::Workflow
    )
}

fn allowed_fields(kind: StepKind) -> &'static [&'static str] {
    match kind {
        StepKind::Read => &["role", "models", "count", "task", "input"],
        StepKind::Compare => &["role", "models", "count", "input"],
        StepKind::Draft => &["role", "models", "count", "blind", "task", "input"],
        StepKind::Map => &["role", "models", "input"],
        StepKind::FanOut => &[
            "role",
            "roles",
            "models",
            "count",
            "blind",
            "different_provider_from",
            "task",
            "input",
        ],
        StepKind::ForEach => &["input", "steps"],
        StepKind::Review => &[
            "role",
            "models",
            "count",
            "blind",
            "different_provider_from",
            "task",
            "input",
        ],
        StepKind::Judge => &[
            "role", "models", "count", "blind", "task", "input", "params",
        ],
        StepKind::Critique => &["role", "models", "count", "task", "input"],
        StepKind::Synthesize => &["role", "models", "task", "input"],
        // The Captain task's worker is the main agent, the plan's workers come from the plan
        // approval, and each task's brief is its spec, so `models` and `task` would do nothing.
        StepKind::Build => &[
            "role",
            "count",
            "different_provider_from",
            "input",
            "review_in",
        ],
        StepKind::Checks => &["input"],
        StepKind::LoopUntil => &["steps", "until", "max"],
        StepKind::Approval => &["approval", "input"],
        StepKind::Merge => &[],
        StepKind::GoalCheck => &[],
        StepKind::Workflow => &["workflow", "params"],
        StepKind::Script => &[],
    }
}

/// Every `review_in` among these steps and their inner ones, with the top-level step's place.
fn review_targets(steps: &[Step]) -> Vec<(usize, String)> {
    fn walk(step: &Step, position: usize, out: &mut Vec<(usize, String)>) {
        if let Some(target) = &step.review_in {
            out.push((position, target.clone()));
        }
        for inner in &step.steps {
            walk(inner, position, out);
        }
    }
    let mut out = Vec::new();
    for (position, step) in steps.iter().enumerate() {
        walk(step, position, &mut out);
    }
    out
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

    #[test]
    fn a_workflow_wide_error_reads_as_one_phrase() {
        let error = |at: &str, message: &str| WorkflowError {
            rule: Rule::OverLimit,
            at: at.to_owned(),
            message: message.to_owned(),
        };
        let limit = error(
            "the workflow",
            "it needs at least $3.00, and $0.61 of the limit is left",
        );
        assert_eq!(
            limit.to_string(),
            "the workflow needs at least $3.00, and $0.61 of the limit is left"
        );
        let step = error("phase Build, step 1", "it has no worker");
        assert_eq!(step.to_string(), "phase Build, step 1: it has no worker");
    }

    fn rules(steps: &str) -> Vec<Rule> {
        rules_in("do", steps)
    }

    /// The rules `steps` break in a phase of this `standard`.
    fn rules_in(standard: &str, steps: &str) -> Vec<Rule> {
        let text = format!(
            "name: checked\nparams:\n  tries: 2\nphases:\n  - name: Build\n    standard: {standard}\n    steps:\n{steps}      - kind: merge\n"
        );
        let workflow = yaml::parse(&text).unwrap_or_else(|err| panic!("{err}\n{text}"));
        let settings = Settings::default();
        let providers = BTreeMap::new();
        let lookup = |name: &str| builtin::load(name);
        let context = Context {
            settings: &settings,
            allowed: &[],
            providers: &providers,
            lookup: &lookup,
            depth: 0,
            limit_left_usd: None,
            as_you_asked: &[],
        };
        match validate(&workflow, &context) {
            Ok(()) => Vec::new(),
            Err(errors) => errors.into_iter().map(|error| error.rule).collect(),
        }
    }

    /// A role your request gave to some agents goes to them, so a workflow naming another
    /// agent for it goes back to the main agent with the error.
    #[test]
    fn a_step_must_give_an_instructed_role_to_its_agents() {
        use argonauts_core::events::run::InstructionOutcome;

        let mut settings = Settings::default();
        let agy = ModelRef::new("antigravity-cli", None);
        settings.roles.entry(RoleName::Reviewer).or_default().models = vec![agy.clone()];
        let review = CheckedInstruction {
            says: "gemini reviews".to_owned(),
            kind: InstructionKind::Review,
            outcome: InstructionOutcome::Followed {
                said: "Antigravity CLI reviews".to_owned(),
                note: None,
            },
        };
        let mut refused = review.clone();
        refused.outcome = InstructionOutcome::NotFollowed {
            why: "no".to_owned(),
        };
        assert_eq!(
            roles_as_you_asked(std::slice::from_ref(&review), &settings),
            [(RoleName::Reviewer, vec![agy.clone()])]
        );
        assert!(roles_as_you_asked(&[refused], &settings).is_empty());

        let as_you_asked = roles_as_you_asked(&[review], &settings);
        let check = |workers: &str| {
            let text = format!(
                "name: checked\nphases:\n  - name: Look\n    standard: do\n    steps:\n      \
                 - kind: review\n        input: goal\n        models: [{workers}]\n"
            );
            let workflow = yaml::parse(&text).unwrap_or_else(|err| panic!("{err}\n{text}"));
            let providers = BTreeMap::new();
            let lookup = |name: &str| builtin::load(name);
            let allowed = [ModelRef::new("codex", None), agy.clone()];
            let context = Context {
                settings: &settings,
                allowed: &allowed,
                providers: &providers,
                lookup: &lookup,
                depth: 1,
                limit_left_usd: None,
                as_you_asked: &as_you_asked,
            };
            match validate(&workflow, &context) {
                Ok(()) => Vec::new(),
                Err(errors) => errors
                    .into_iter()
                    .filter(|error| error.rule == Rule::NotAsYouAsked)
                    .map(|error| error.message)
                    .collect(),
            }
        };
        assert_eq!(check("antigravity-cli"), Vec::<String>::new());
        assert_eq!(
            check("codex"),
            ["the request gives the reviewer role to antigravity-cli, so codex can't take it"]
        );
    }

    #[test]
    fn build_refuses_what_its_executor_would_ignore() {
        let cases = [
            ("        models: [codex]\n", Rule::FieldNotForKind),
            ("        task: do it twice\n", Rule::FieldNotForKind),
            ("        role: reviewer\n", Rule::FieldNotForKind),
            ("        count: 0\n", Rule::InvalidValue),
            (
                "        different_provider_from: judge\n",
                Rule::InvalidValue,
            ),
        ];
        for (field, rule) in cases {
            let step = format!("      - kind: build\n        input: goal\n{field}");
            assert!(rules(&step).contains(&rule), "{field}: {:?}", rules(&step));
        }
        let unknown = rules("      - kind: build\n        role: worker\n");
        assert!(unknown.contains(&Rule::UnknownInput), "{unknown:?}");
        // The plan's tasks get their workers and reviewers at the plan approval.
        for field in ["count: 2", "different_provider_from: author"] {
            let step = format!(
                "      - kind: approval\n        approval: understand\n      - kind: approval\n        approval: plan\n      - kind: build\n        input: plan\n        {field}\n"
            );
            assert_eq!(rules(&step), [Rule::FieldNotForKind], "{field}");
        }
    }

    /// The whole workflow's rules, for checks that span its phases.
    fn workflow_rules(text: &str) -> Vec<Rule> {
        let workflow = yaml::parse(text).unwrap_or_else(|err| panic!("{err}\n{text}"));
        let settings = Settings::default();
        let providers = BTreeMap::new();
        let lookup = |name: &str| builtin::load(name);
        let context = Context {
            settings: &settings,
            allowed: &[],
            providers: &providers,
            lookup: &lookup,
            depth: 0,
            limit_left_usd: None,
            as_you_asked: &[],
        };
        match validate(&workflow, &context) {
            Ok(()) => Vec::new(),
            Err(errors) => errors.into_iter().map(|error| error.rule).collect(),
        }
    }

    const FIX_THEN_REVIEW: &str = "name: fix\nphases:\n  - name: Understand\n    standard: understand\n    steps:\n      - { kind: approval, approval: understand }\n  - name: Fix\n    standard: do\n    steps:\n      - { kind: approval, approval: plan }\n      - { kind: build, input: plan, label: \"fix:{item}\", review_in: REVIEW_IN }\n  - name: Review\n    standard: check\n    steps: []\n  - name: Merge\n    standard: decide\n    steps:\n      - { kind: merge, label: \"merge:{branch}\" }\n";

    #[test]
    fn a_phase_may_hold_only_an_earlier_builds_reviews() {
        let later = FIX_THEN_REVIEW.replace("REVIEW_IN", "Review");
        assert_eq!(workflow_rules(&later), []);
        let earlier = FIX_THEN_REVIEW.replace("REVIEW_IN", "Understand");
        let refused = workflow_rules(&earlier);
        assert!(refused.contains(&Rule::ReviewIn), "{refused:?}");
        // Nothing feeds Review now, so its emptiness counts too.
        assert!(refused.contains(&Rule::Empty), "{refused:?}");
        let unknown = FIX_THEN_REVIEW.replace("REVIEW_IN", "Nowhere");
        assert!(workflow_rules(&unknown).contains(&Rule::ReviewIn));
        // Only a build points its reviews somewhere.
        assert!(
            rules("      - kind: checks\n        review_in: Build\n")
                .contains(&Rule::FieldNotForKind)
        );
        // Two phases by one name can't be told apart, whatever their case.
        let twice = FIX_THEN_REVIEW
            .replace("REVIEW_IN", "Review")
            .replace("name: Understand", "name: review");
        assert!(workflow_rules(&twice).contains(&Rule::PhaseName));
    }

    #[test]
    fn labels_are_one_token_and_never_shared() {
        assert_eq!(
            rules("      - kind: checks\n        label: \"checks:{item}\"\n"),
            []
        );
        for bad in [
            "Tests:x",
            "tests x",
            "checks:{thing}",
            "a-label-much-too-long-to-fit",
        ] {
            let step = format!("      - kind: checks\n        label: \"{bad}\"\n");
            assert_eq!(rules(&step), [Rule::Label], "{bad}");
        }
        let twice = "      - kind: checks\n        label: \"look:once\"\n      - kind: checks\n        label: \"look:once\"\n";
        assert_eq!(rules(twice), [Rule::Label]);
    }

    #[test]
    fn the_check_step_takes_only_its_label() {
        assert_eq!(
            rules("      - kind: goal_check\n        label: \"check:final\"\n"),
            []
        );
        assert_eq!(
            rules("      - kind: goal_check\n        role: reviewer\n"),
            [Rule::FieldNotForKind]
        );
    }

    #[test]
    fn captain_builds_take_tries_and_a_review() {
        let step = "      - kind: build\n        role: worker\n        input: goal\n        count: \"{{tries}}\"\n        different_provider_from: author\n";
        assert_eq!(rules(step), []);
    }

    #[test]
    fn judge_names_a_known_method_only() {
        let judge = |standard: &str, params: &str| {
            rules_in(
                standard,
                &format!("      - kind: judge\n        role: judge\n        params:\n{params}"),
            )
        };
        assert_eq!(judge("plan", "          method: vote\n"), []);
        assert_eq!(
            judge("plan", "          method: loudest\n"),
            [Rule::InvalidValue]
        );
        assert_eq!(
            judge("plan", "          rounds: 2\n"),
            [Rule::FieldNotForKind]
        );
        // Outside a plan phase the judge settles reviews, which takes no method.
        assert_eq!(
            judge("check", "          method: vote\n"),
            [Rule::FieldNotForKind]
        );
    }

    #[test]
    fn builtins_validate_with_their_own_params() {
        let settings = Settings::default();
        let providers = BTreeMap::new();
        let lookup = |name: &str| builtin::load(name);
        let context = Context {
            settings: &settings,
            allowed: &[],
            providers: &providers,
            lookup: &lookup,
            depth: 0,
            limit_left_usd: None,
            as_you_asked: &[],
        };
        for name in builtin::names() {
            let workflow = builtin::load(name).unwrap_or_else(|err| panic!("{err}"));
            assert_eq!(validate(&workflow, &context), Ok(()), "{name}");
        }
    }
}
