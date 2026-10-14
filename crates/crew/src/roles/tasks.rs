//! Who may own and who may review the crew's tasks. The roles' lists, the request's words, and
//! "nobody reviews their own provider's work" can contradict each other, as when your settings
//! pin the reviewer, the request names another, and that one is the only reviewer left. They
//! settle by precedence: a command, then the request's words, then settings. When they still
//! can't all hold, the lowest rule gives way, and one plain line says which. A worker nobody
//! allowed may review is left out of the workers, and the planner hears why before it plans.

use std::borrow::Cow;

use argonauts_core::events::run::RunStarted;
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::settings::Settings;
use argonauts_core::steps::intake::InstructionKind;
use argonauts_core::{Note, NoteLevel};

use super::{candidates, conflict, display_name, provider};
use crate::engine::host::Host;
use crate::error::EngineResult;

/// Where a rule came from, lowest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Layer {
    #[default]
    Settings,
    /// The request's own words, as code followed them.
    Words,
    /// A `--set` on the command line.
    Command,
}

impl Layer {
    fn yours(self) -> &'static str {
        match self {
            Layer::Settings => "your settings",
            Layer::Words => "your request",
            Layer::Command => "your command",
        }
    }
}

/// A rule about who works on a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// How many reviewers each task gets.
    PerTask,
    /// Who may own tasks.
    Workers,
    /// Who may review them.
    Reviewers,
    /// Nobody reviews a task from its worker's provider.
    OtherProvider,
}

/// Among rules from the same layer, the one that gives way first: fewer reviewers before other
/// agents, and the provider rule last, since a review from another provider is the crew's point.
const GIVE_WAY: [Rule; 4] = [
    Rule::PerTask,
    Rule::Workers,
    Rule::Reviewers,
    Rule::OtherProvider,
];

/// Where each rule came from in this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Layers {
    pub per_task: Layer,
    pub workers: Layer,
    pub reviewers: Layer,
    pub other_provider: Layer,
}

impl Layers {
    /// What the run's start says: a `--set` is a command, and an instruction from the request
    /// that code followed is its words. The rest is settings.
    pub fn of(started: Option<&RunStarted>) -> Self {
        let Some(started) = started else {
            return Self::default();
        };
        let commanded = |keys: &[&str]| {
            started.overrides.iter().any(|set| {
                set.from == "flag"
                    && keys.iter().any(|key| {
                        set.key == *key
                            || set
                                .key
                                .strip_prefix(key)
                                .is_some_and(|rest| rest.starts_with('.'))
                    })
            })
        };
        let said = |kinds: &[InstructionKind]| {
            started
                .instructions
                .iter()
                .any(|instruction| instruction.followed() && kinds.contains(&instruction.kind))
        };
        let layer = |keys: &[&str], kinds: &[InstructionKind]| {
            if commanded(keys) {
                Layer::Command
            } else if said(kinds) {
                Layer::Words
            } else {
                Layer::Settings
            }
        };
        use InstructionKind as Said;
        Self {
            per_task: layer(&["review.per_task"], &[Said::Review]),
            workers: layer(
                &["roles.worker", "crew"],
                &[Said::Build, Said::Crew, Said::Exclude],
            ),
            reviewers: layer(
                &["roles.reviewer", "crew"],
                &[Said::Review, Said::Crew, Said::Exclude],
            ),
            // The request's words only ever turn this rule off.
            other_provider: layer(&["review.other_provider"], &[]),
        }
    }

    fn of_rule(&self, rule: Rule) -> Layer {
        match rule {
            Rule::PerTask => self.per_task,
            Rule::Workers => self.workers,
            Rule::Reviewers => self.reviewers,
            Rule::OtherProvider => self.other_provider,
        }
    }
}

/// Who may own and review the crew's tasks once the rules settle.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskRoles {
    /// Who may own a task, best first as the role lists them: each one enough allowed
    /// reviewers may review.
    pub workers: Vec<ModelRef>,
    pub reviewers: Vec<ModelRef>,
    pub per_task: u32,
    pub other_provider: bool,
    /// Workers the role allows but too few allowed reviewers may review, each with why.
    pub left_out: Vec<(ModelRef, String)>,
    /// The line that says which rule gave way, with why behind Ctrl+O. `None` when they all hold.
    pub relaxed: Option<String>,
}

/// The rules as they stand at one point of settling.
#[derive(Debug, Clone, PartialEq)]
struct Rules {
    workers: Vec<ModelRef>,
    reviewers: Vec<ModelRef>,
    per_task: u32,
    other_provider: bool,
}

impl Rules {
    fn of(settings: &Settings) -> Self {
        Self {
            workers: candidates(settings, RoleName::Worker),
            reviewers: candidates(settings, RoleName::Reviewer),
            per_task: settings.review.per_task,
            other_provider: settings.review.other_provider,
        }
    }

    fn reviewers_for(
        &self,
        worker: &ModelRef,
        provider: &dyn Fn(&ModelRef) -> String,
    ) -> Vec<ModelRef> {
        reviewers_for(&self.reviewers, self.other_provider, worker, provider)
    }

    fn may_own(&self, worker: &ModelRef, provider: &dyn Fn(&ModelRef) -> String) -> bool {
        self.reviewers_for(worker, provider).len() >= self.per_task as usize
    }

    fn hold(&self, provider: &dyn Fn(&ModelRef) -> String) -> bool {
        self.workers
            .iter()
            .any(|worker| self.may_own(worker, provider))
    }

    /// These rules with `rule` given way, or `None` when giving way changes nothing.
    fn eased(
        &self,
        rule: Rule,
        everyone: &[ModelRef],
        provider: &dyn Fn(&ModelRef) -> String,
    ) -> Option<Rules> {
        let widened = |pinned: &[ModelRef]| {
            let mut all = pinned.to_vec();
            all.extend(everyone.iter().filter(|w| !pinned.contains(w)).cloned());
            (all.len() > pinned.len()).then_some(all)
        };
        let mut eased = self.clone();
        match rule {
            Rule::PerTask => {
                let most = self
                    .workers
                    .iter()
                    .map(|worker| self.reviewers_for(worker, provider).len())
                    .max()
                    .unwrap_or(0) as u32;
                if most == 0 || most >= self.per_task {
                    return None;
                }
                eased.per_task = most;
            }
            Rule::Workers => eased.workers = widened(&self.workers)?,
            Rule::Reviewers => eased.reviewers = widened(&self.reviewers)?,
            Rule::OtherProvider => {
                if !self.other_provider {
                    return None;
                }
                eased.other_provider = false;
            }
        }
        Some(eased)
    }

    /// Why `worker` can't own a task under these rules, in plain words.
    fn why_not_worker(&self, worker: &ModelRef, provider: &dyn Fn(&ModelRef) -> String) -> String {
        let name = display_name(worker);
        let others: Vec<&ModelRef> = self
            .reviewers
            .iter()
            .filter(|reviewer| {
                conflict(RoleName::Reviewer, reviewer, Some(worker), provider).is_none()
            })
            .collect();
        if others.is_empty() {
            return if self.reviewers.len() == 1 {
                format!("{name} is the only reviewer you allowed, so {name} can't own a task")
            } else {
                format!("Every reviewer you allowed is {name}, so {name} can't own a task")
            };
        }
        let fit = self.reviewers_for(worker, provider).len();
        if self.other_provider && fit == 0 {
            return format!(
                "Every other reviewer you allowed is from {name}'s provider, and nobody reviews \
                 their own provider's work, so {name} can't own a task"
            );
        }
        format!(
            "{} of the reviewers you allowed may review {name}'s tasks, and each needs {}, so \
             {name} can't own a task",
            count(fit),
            self.per_task
        )
    }

    /// What giving way to `rule` means, from `from` to these rules.
    fn eased_line(&self, rule: Rule, from: &Rules) -> String {
        match rule {
            Rule::PerTask => format!(
                "each task gets {} reviewer{}, not {}",
                self.per_task,
                plural(self.per_task as usize),
                from.per_task
            ),
            Rule::Workers => format!(
                "any of the crew may own a task, not only {}",
                names(&from.workers)
            ),
            Rule::Reviewers => format!(
                "any of the crew may review, not only {}",
                names(&from.reviewers)
            ),
            Rule::OtherProvider => "a task may be reviewed by its worker's provider".to_owned(),
        }
    }
}

/// The reviewers in `pool` that may review `worker`'s tasks, in the pool's order: never the
/// worker, and from another provider when `other_provider` says so.
pub fn reviewers_for(
    pool: &[ModelRef],
    other_provider: bool,
    worker: &ModelRef,
    provider: &dyn Fn(&ModelRef) -> String,
) -> Vec<ModelRef> {
    let worker_provider = provider(worker);
    pool.iter()
        .filter(|reviewer| conflict(RoleName::Reviewer, reviewer, Some(worker), provider).is_none())
        .filter(|reviewer| !other_provider || provider(reviewer) != worker_provider)
        .cloned()
        .collect()
}

/// Settles who may own and review tasks from `settings`, each rule from the layer `layers`
/// gives it.
pub fn settle(
    settings: &Settings,
    layers: Layers,
    provider: &dyn Fn(&ModelRef) -> String,
) -> TaskRoles {
    let pinned = Rules::of(settings);
    // Who a pin that gives way opens the role to: the crew, as a role with no pin is.
    let mut everyone: Vec<ModelRef> = Vec::new();
    for model in &settings.crew.members() {
        if !everyone.contains(model) {
            everyone.push(model.clone());
        }
    }
    if everyone.is_empty() {
        everyone.push(settings.crew.main.clone());
    }
    let mut order = GIVE_WAY.to_vec();
    order.sort_by_key(|rule| layers.of_rule(*rule));

    let mut rules = pinned.clone();
    let mut gave_way: Vec<Rule> = Vec::new();
    if !rules.hold(provider) {
        // The lowest rule that alone lets the rest hold gives way; failing that, each in turn
        // until they hold.
        let alone = order.iter().find_map(|rule| {
            let eased = rules.eased(*rule, &everyone, provider)?;
            eased.hold(provider).then_some((*rule, eased))
        });
        match alone {
            Some((rule, eased)) => {
                gave_way.push(rule);
                rules = eased;
            }
            None => {
                for rule in &order {
                    if rules.hold(provider) {
                        break;
                    }
                    if let Some(eased) = rules.eased(*rule, &everyone, provider) {
                        gave_way.push(*rule);
                        rules = eased;
                    }
                }
            }
        }
    }

    let relaxed = (!gave_way.is_empty()).then(|| {
        let mut said = Vec::new();
        let mut at = pinned.clone();
        for rule in &gave_way {
            let eased = at
                .eased(*rule, &everyone, provider)
                .unwrap_or_else(|| at.clone());
            said.push(format!(
                "the one from {} gives way: {}",
                layers.of_rule(*rule).yours(),
                eased.eased_line(*rule, &at)
            ));
            at = eased;
        }
        let why: Vec<String> = pinned
            .workers
            .iter()
            .filter(|worker| !pinned.may_own(worker, provider))
            .map(|worker| format!("{}.", pinned.why_not_worker(worker, provider)))
            .collect();
        format!(
            "Your rules for who owns and reviews tasks can't all hold, so {}.\n{}",
            said.join("; and "),
            why.join("\n")
        )
        .trim_end()
        .to_owned()
    });

    let (workers, left_out): (Vec<ModelRef>, Vec<ModelRef>) = rules
        .workers
        .iter()
        .cloned()
        .partition(|worker| rules.may_own(worker, provider));
    // When no worker fits, every one stays: the plan shows who is short of reviewers.
    let (workers, left_out) = if workers.is_empty() {
        (rules.workers.clone(), Vec::new())
    } else {
        (workers, left_out)
    };
    TaskRoles {
        workers,
        left_out: left_out
            .into_iter()
            .map(|worker| {
                let why = rules.why_not_worker(&worker, provider);
                (worker, why)
            })
            .collect(),
        reviewers: rules.reviewers,
        per_task: rules.per_task,
        other_provider: rules.other_provider,
        relaxed,
    }
}

/// [`settle`] for this run.
pub fn for_run(host: &Host) -> TaskRoles {
    let layers = host.read(|state| Layers::of(state.run.started.as_deref()));
    settle(host.settings(), layers, &|worker| provider(host, worker))
}

/// The run's settings with its task roles in place, where they are different: what dividing the
/// work and the review ring go by.
pub fn settled(host: &Host) -> Cow<'_, Settings> {
    for_run(host).apply(host.settings())
}

impl TaskRoles {
    /// `settings` with these roles in place, borrowed when nothing is different.
    pub fn apply<'a>(&self, settings: &'a Settings) -> Cow<'a, Settings> {
        if self.relaxed.is_none() && self.left_out.is_empty() {
            return Cow::Borrowed(settings);
        }
        let mut settled = settings.clone();
        settled.roles.entry(RoleName::Worker).or_default().models = self.workers.clone();
        settled.roles.entry(RoleName::Reviewer).or_default().models = self.reviewers.clone();
        settled.review.per_task = self.per_task;
        settled.review.other_provider = self.other_provider;
        Cow::Owned(settled)
    }

    /// Everyone who may own or review, and the workers left out, each once.
    pub fn listed(&self) -> Vec<ModelRef> {
        let mut workers: Vec<ModelRef> = Vec::new();
        let all = self
            .workers
            .iter()
            .chain(&self.reviewers)
            .chain(self.left_out.iter().map(|(worker, _)| worker));
        for worker in all {
            if !workers.contains(worker) {
                workers.push(worker.clone());
            }
        }
        workers
    }

    /// The agent `text` names, as the crew list spells it when it's one of them.
    pub fn named(&self, text: &str) -> Option<ModelRef> {
        named_in(text, &self.listed()).or_else(|| text.trim().parse().ok())
    }

    pub fn may_own(&self, worker: &ModelRef) -> bool {
        self.workers.contains(worker)
    }

    pub fn may_review(&self, worker: &ModelRef) -> bool {
        self.reviewers.contains(worker)
    }

    /// The allowed reviewers that may review `worker`'s tasks, in the reviewers' order.
    pub fn reviewers_for(
        &self,
        worker: &ModelRef,
        provider: &dyn Fn(&ModelRef) -> String,
    ) -> Vec<ModelRef> {
        reviewers_for(&self.reviewers, self.other_provider, worker, provider)
    }

    /// Why `worker` can't own a task, in plain words with names, such as "Antigravity CLI is
    /// the only reviewer you allowed, so Antigravity CLI can't own a task".
    pub fn why_not_worker(&self, worker: &ModelRef) -> String {
        if let Some((_, why)) = self.left_out.iter().find(|(left, _)| left == worker) {
            return why.clone();
        }
        format!("only {} may own tasks", names(&self.workers))
    }

    /// Why `reviewer` can't review `worker`'s task, in plain words with names.
    pub fn why_not_reviewer(
        &self,
        reviewer: &ModelRef,
        worker: &ModelRef,
        provider: &dyn Fn(&ModelRef) -> String,
    ) -> String {
        if !self.may_review(reviewer) {
            return format!("only {} may review tasks", names(&self.reviewers));
        }
        if conflict(RoleName::Reviewer, reviewer, Some(worker), provider).is_some() {
            return "nobody reviews their own task".to_owned();
        }
        format!(
            "{} is from the provider of the task's worker, {}",
            display_name(reviewer),
            display_name(worker)
        )
    }

    /// One line per agent who may own or review, for a planner's prompt: the id to assign it
    /// by, its name and provider, and what it may do.
    pub fn lines(&self, provider: &dyn Fn(&ModelRef) -> String) -> Vec<String> {
        self.listed()
            .iter()
            .map(|worker| {
                let may = match (self.may_own(worker), self.may_review(worker)) {
                    (true, true) => "may own tasks and review them",
                    (true, false) => "may own tasks, not review them",
                    (false, true) => "may review tasks, not own them",
                    (false, false) => "may neither own nor review tasks",
                };
                let mut line = format!(
                    "`{worker}` is {}, from {}: it {may}.",
                    display_name(worker),
                    provider(worker)
                );
                if let Some((_, why)) = self.left_out.iter().find(|(left, _)| left == worker) {
                    line.push_str(&format!(" {why}."));
                }
                line
            })
            .collect()
    }

    /// The review rule in a sentence, for a planner's prompt.
    pub fn review_line(&self) -> String {
        format!(
            "Each task gets {} reviewer{}, never its worker{}.",
            self.per_task,
            plural(self.per_task as usize),
            if self.other_provider {
                ", each from another provider than its worker"
            } else {
                ""
            }
        )
    }

    /// Records the line that says which rule gave way, once per run.
    pub fn announce(&self, host: &Host) -> EngineResult<()> {
        let Some(text) = &self.relaxed else {
            return Ok(());
        };
        let first = text.lines().next().unwrap_or_default();
        if host.read(|state| state.run.said.contains_key(first)) {
            return Ok(());
        }
        host.record(
            None,
            None,
            argonauts_core::events::RunEventKind::Note(Note {
                level: NoteLevel::Warn,
                text: text.clone(),
            }),
        )?;
        Ok(())
    }
}

/// The member of `pool` that `text` names: that very reference, else the only one on the same
/// CLI, as `codex/gpt` names `codex` when a provider stands where a model goes.
pub fn named_in(text: &str, pool: &[ModelRef]) -> Option<ModelRef> {
    let named: ModelRef = text.trim().parse().ok()?;
    if pool.contains(&named) {
        return Some(named);
    }
    let mut same_cli = pool.iter().filter(|worker| worker.agent == named.agent);
    match (same_cli.next(), same_cli.next()) {
        (Some(only), None) => Some(only.clone()),
        _ => None,
    }
}

/// Names joined as a sentence says them: "Claude Code and Codex", "A, B and C".
pub fn names(workers: &[ModelRef]) -> String {
    let names: Vec<String> = workers.iter().map(display_name).collect();
    match names.as_slice() {
        [] => "nobody".to_owned(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn count(n: usize) -> String {
    match n {
        0 => "None".to_owned(),
        1 => "Only 1".to_owned(),
        n => format!("Only {n}"),
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests;
