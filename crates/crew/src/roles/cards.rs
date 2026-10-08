//! Who may own and who may review the crew's cards. The roles' lists, the request's words, and
//! "nobody reviews their own company's work" can contradict each other, as when your settings
//! pin the reviewer, the request names another, and that one is the only reviewer left. They
//! settle by precedence: a command, then the request's words, then settings. When they still
//! can't all hold, the lowest rule gives way, and one plain line says which. An owner nobody
//! allowed may review is left out of the owners, and the planner hears why before it plans.

use std::borrow::Cow;

use argonauts_core::events::run::RunStarted;
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::settings::Settings;
use argonauts_core::steps::intake::InstructionKind;
use argonauts_core::{Note, NoteLevel};

use super::{candidates, conflict, display_name, family};
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

/// A rule about who works on a card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// How many reviewers each card gets.
    PerCard,
    /// Who may own cards.
    Owners,
    /// Who may review them.
    Reviewers,
    /// Nobody reviews a card from its owner's company.
    OtherProvider,
}

/// Among rules from the same layer, the one that gives way first: fewer reviewers before other
/// agents, and the company rule last, since a review from another company is the crew's point.
const GIVE_WAY: [Rule; 4] = [
    Rule::PerCard,
    Rule::Owners,
    Rule::Reviewers,
    Rule::OtherProvider,
];

/// Where each rule came from in this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Layers {
    pub per_task: Layer,
    pub owners: Layer,
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
            owners: layer(
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
            Rule::PerCard => self.per_task,
            Rule::Owners => self.owners,
            Rule::Reviewers => self.reviewers,
            Rule::OtherProvider => self.other_provider,
        }
    }
}

/// Who may own and review the crew's cards once the rules settle.
#[derive(Debug, Clone, PartialEq)]
pub struct CardRoles {
    /// Who may own a card, best first as the role lists them: each one enough allowed
    /// reviewers may review.
    pub owners: Vec<WorkerRef>,
    pub reviewers: Vec<WorkerRef>,
    pub per_task: u32,
    pub other_provider: bool,
    /// Owners the role allows but too few allowed reviewers may review, each with why.
    pub left_out: Vec<(WorkerRef, String)>,
    /// The line that says which rule gave way, with why behind Ctrl+O. `None` when they all hold.
    pub relaxed: Option<String>,
}

/// The rules as they stand at one point of settling.
#[derive(Debug, Clone, PartialEq)]
struct Rules {
    owners: Vec<WorkerRef>,
    reviewers: Vec<WorkerRef>,
    per_task: u32,
    other_provider: bool,
}

impl Rules {
    fn of(settings: &Settings) -> Self {
        Self {
            owners: candidates(settings, RoleName::Worker),
            reviewers: candidates(settings, RoleName::Reviewer),
            per_task: settings.review.per_task,
            other_provider: settings.review.other_provider,
        }
    }

    fn reviewers_for(
        &self,
        owner: &WorkerRef,
        family: &dyn Fn(&WorkerRef) -> String,
    ) -> Vec<WorkerRef> {
        reviewers_for(&self.reviewers, self.other_provider, owner, family)
    }

    fn may_own(&self, owner: &WorkerRef, family: &dyn Fn(&WorkerRef) -> String) -> bool {
        self.reviewers_for(owner, family).len() >= self.per_task as usize
    }

    fn hold(&self, family: &dyn Fn(&WorkerRef) -> String) -> bool {
        self.owners.iter().any(|owner| self.may_own(owner, family))
    }

    /// These rules with `rule` given way, or `None` when giving way changes nothing.
    fn eased(
        &self,
        rule: Rule,
        everyone: &[WorkerRef],
        family: &dyn Fn(&WorkerRef) -> String,
    ) -> Option<Rules> {
        let widened = |pinned: &[WorkerRef]| {
            let mut all = pinned.to_vec();
            all.extend(everyone.iter().filter(|w| !pinned.contains(w)).cloned());
            (all.len() > pinned.len()).then_some(all)
        };
        let mut eased = self.clone();
        match rule {
            Rule::PerCard => {
                let most = self
                    .owners
                    .iter()
                    .map(|owner| self.reviewers_for(owner, family).len())
                    .max()
                    .unwrap_or(0) as u32;
                if most == 0 || most >= self.per_task {
                    return None;
                }
                eased.per_task = most;
            }
            Rule::Owners => eased.owners = widened(&self.owners)?,
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

    /// Why `owner` can't own a card under these rules, in plain words.
    fn why_not_owner(&self, owner: &WorkerRef, family: &dyn Fn(&WorkerRef) -> String) -> String {
        let name = display_name(owner);
        let others: Vec<&WorkerRef> = self
            .reviewers
            .iter()
            .filter(|reviewer| {
                conflict(RoleName::Reviewer, reviewer, Some(owner), family).is_none()
            })
            .collect();
        if others.is_empty() {
            return if self.reviewers.len() == 1 {
                format!("{name} is the only reviewer you allowed, so {name} can't own a task")
            } else {
                format!("Every reviewer you allowed is {name}, so {name} can't own a task")
            };
        }
        let fit = self.reviewers_for(owner, family).len();
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
            Rule::PerCard => format!(
                "each task gets {} reviewer{}, not {}",
                self.per_task,
                plural(self.per_task as usize),
                from.per_task
            ),
            Rule::Owners => format!(
                "any of the crew may own a task, not only {}",
                names(&from.owners)
            ),
            Rule::Reviewers => format!(
                "any of the crew may review, not only {}",
                names(&from.reviewers)
            ),
            Rule::OtherProvider => "a task may be reviewed by its worker's provider".to_owned(),
        }
    }
}

/// The reviewers in `pool` that may review `owner`'s cards, in the pool's order: never the
/// owner, and from another company when `other_provider` says so.
pub fn reviewers_for(
    pool: &[WorkerRef],
    other_provider: bool,
    owner: &WorkerRef,
    family: &dyn Fn(&WorkerRef) -> String,
) -> Vec<WorkerRef> {
    let owner_family = family(owner);
    pool.iter()
        .filter(|reviewer| conflict(RoleName::Reviewer, reviewer, Some(owner), family).is_none())
        .filter(|reviewer| !other_provider || family(reviewer) != owner_family)
        .cloned()
        .collect()
}

/// Settles who may own and review cards from `settings`, each rule from the layer `layers`
/// gives it.
pub fn settle(
    settings: &Settings,
    layers: Layers,
    family: &dyn Fn(&WorkerRef) -> String,
) -> CardRoles {
    let pinned = Rules::of(settings);
    // Who a pin that gives way opens the role to: the crew, as a role with no pin is.
    let mut everyone: Vec<WorkerRef> = Vec::new();
    for worker in &settings.crew.members() {
        if !everyone.contains(worker) {
            everyone.push(worker.clone());
        }
    }
    if everyone.is_empty() {
        everyone.push(settings.main_agent.clone());
    }
    let mut order = GIVE_WAY.to_vec();
    order.sort_by_key(|rule| layers.of_rule(*rule));

    let mut rules = pinned.clone();
    let mut gave_way: Vec<Rule> = Vec::new();
    if !rules.hold(family) {
        // The lowest rule that alone lets the rest hold gives way; failing that, each in turn
        // until they hold.
        let alone = order.iter().find_map(|rule| {
            let eased = rules.eased(*rule, &everyone, family)?;
            eased.hold(family).then_some((*rule, eased))
        });
        match alone {
            Some((rule, eased)) => {
                gave_way.push(rule);
                rules = eased;
            }
            None => {
                for rule in &order {
                    if rules.hold(family) {
                        break;
                    }
                    if let Some(eased) = rules.eased(*rule, &everyone, family) {
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
                .eased(*rule, &everyone, family)
                .unwrap_or_else(|| at.clone());
            said.push(format!(
                "the one from {} gives way: {}",
                layers.of_rule(*rule).yours(),
                eased.eased_line(*rule, &at)
            ));
            at = eased;
        }
        let why: Vec<String> = pinned
            .owners
            .iter()
            .filter(|owner| !pinned.may_own(owner, family))
            .map(|owner| format!("{}.", pinned.why_not_owner(owner, family)))
            .collect();
        format!(
            "Your rules for who owns and reviews tasks can't all hold, so {}.\n{}",
            said.join("; and "),
            why.join("\n")
        )
        .trim_end()
        .to_owned()
    });

    let (owners, left_out): (Vec<WorkerRef>, Vec<WorkerRef>) = rules
        .owners
        .iter()
        .cloned()
        .partition(|owner| rules.may_own(owner, family));
    // When no owner fits, every one stays: the plan shows who is short of reviewers.
    let (owners, left_out) = if owners.is_empty() {
        (rules.owners.clone(), Vec::new())
    } else {
        (owners, left_out)
    };
    CardRoles {
        owners,
        left_out: left_out
            .into_iter()
            .map(|owner| {
                let why = rules.why_not_owner(&owner, family);
                (owner, why)
            })
            .collect(),
        reviewers: rules.reviewers,
        per_task: rules.per_task,
        other_provider: rules.other_provider,
        relaxed,
    }
}

/// [`settle`] for this run.
pub fn for_run(host: &Host) -> CardRoles {
    let layers = host.read(|state| Layers::of(state.run.started.as_deref()));
    settle(host.settings(), layers, &|worker| family(host, worker))
}

/// The run's settings with its card roles in place, where they differ: what dividing the work
/// and the review ring go by.
pub fn settled(host: &Host) -> Cow<'_, Settings> {
    for_run(host).apply(host.settings())
}

impl CardRoles {
    /// `settings` with these roles in place, borrowed when nothing differs.
    pub fn apply<'a>(&self, settings: &'a Settings) -> Cow<'a, Settings> {
        if self.relaxed.is_none() && self.left_out.is_empty() {
            return Cow::Borrowed(settings);
        }
        let mut settled = settings.clone();
        settled.roles.entry(RoleName::Worker).or_default().workers = self.owners.clone();
        settled.roles.entry(RoleName::Reviewer).or_default().workers = self.reviewers.clone();
        settled.review.per_task = self.per_task;
        settled.review.other_provider = self.other_provider;
        Cow::Owned(settled)
    }

    /// Everyone who may own or review, and the owners left out, each once.
    pub fn listed(&self) -> Vec<WorkerRef> {
        let mut workers: Vec<WorkerRef> = Vec::new();
        let all = self
            .owners
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
    pub fn named(&self, text: &str) -> Option<WorkerRef> {
        named_in(text, &self.listed()).or_else(|| text.trim().parse().ok())
    }

    pub fn may_own(&self, worker: &WorkerRef) -> bool {
        self.owners.contains(worker)
    }

    pub fn may_review(&self, worker: &WorkerRef) -> bool {
        self.reviewers.contains(worker)
    }

    /// The allowed reviewers that may review `owner`'s cards, in the reviewers' order.
    pub fn reviewers_for(
        &self,
        owner: &WorkerRef,
        family: &dyn Fn(&WorkerRef) -> String,
    ) -> Vec<WorkerRef> {
        reviewers_for(&self.reviewers, self.other_provider, owner, family)
    }

    /// Why `worker` can't own a card, in plain words with names, such as "Antigravity CLI is
    /// the only reviewer you allowed, so Antigravity CLI can't own a card".
    pub fn why_not_owner(&self, worker: &WorkerRef) -> String {
        if let Some((_, why)) = self.left_out.iter().find(|(left, _)| left == worker) {
            return why.clone();
        }
        format!("only {} may own tasks", names(&self.owners))
    }

    /// Why `reviewer` can't review `owner`'s card, in plain words with names.
    pub fn why_not_reviewer(
        &self,
        reviewer: &WorkerRef,
        owner: &WorkerRef,
        family: &dyn Fn(&WorkerRef) -> String,
    ) -> String {
        if !self.may_review(reviewer) {
            return format!("only {} may review tasks", names(&self.reviewers));
        }
        if conflict(RoleName::Reviewer, reviewer, Some(owner), family).is_some() {
            return "nobody reviews their own task".to_owned();
        }
        format!(
            "{} is from the provider of the task's worker, {}",
            display_name(reviewer),
            display_name(owner)
        )
    }

    /// One line per agent who may own or review, for a planner's prompt: the id to assign it
    /// by, its name and family, and what it may do.
    pub fn lines(&self, family: &dyn Fn(&WorkerRef) -> String) -> Vec<String> {
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
                    family(worker)
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
/// CLI, as `codex/gpt` names `codex` when a family stands where a model goes.
pub fn named_in(text: &str, pool: &[WorkerRef]) -> Option<WorkerRef> {
    let named: WorkerRef = text.trim().parse().ok()?;
    if pool.contains(&named) {
        return Some(named);
    }
    let mut same_cli = pool.iter().filter(|worker| worker.worker == named.worker);
    match (same_cli.next(), same_cli.next()) {
        (Some(only), None) => Some(only.clone()),
        _ => None,
    }
}

/// Names joined as a sentence says them: "Claude Code and Codex", "A, B and C".
pub fn names(workers: &[WorkerRef]) -> String {
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
