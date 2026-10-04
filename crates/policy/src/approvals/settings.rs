//! The settings a checkpoint uses: the stricter of the run-start settings and the files as they
//! are now. A tighter change applies at the next checkpoint, and a looser one waits for the
//! next run.

use std::sync::Arc;

use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::{
    Answerer, ApprovalMode, ApprovalSettings, ApproverSettings, BudgetRules, CheckpointSettings,
    DecideWith, EscalationRules, GoalChanges, GoalRules, HardLimitSettings, MergeRules, NoRules,
    Otherwise, PlanRules, Presence, PromotionRules, SandboxChoice, SandboxSettings, UntrustedRepos,
    UserHooks,
};

use crate::SettingsReloader;

#[derive(Clone)]
pub struct SettingsSource {
    run_start: Arc<Settings>,
    reload: SettingsReloader,
}

impl SettingsSource {
    pub fn new(run_start: Arc<Settings>, reload: SettingsReloader) -> Self {
        Self { run_start, reload }
    }

    pub fn run_start(&self) -> &Arc<Settings> {
        &self.run_start
    }

    /// A reload that fails keeps the run-start values.
    pub fn current(&self) -> Arc<Settings> {
        match (self.reload)() {
            Some(now) => Arc::new(stricter(&self.run_start, &now)),
            None => self.run_start.clone(),
        }
    }
}

/// For each checkpoint: the less autonomous mode, the stricter rule numbers, and the smaller
/// caps of the two, plus the stricter hard limits and sandbox. Everything the policy doesn't
/// read keeps its run-start value.
pub fn stricter(run_start: &Settings, reloaded: &Settings) -> Settings {
    let mut settings = run_start.clone();
    settings.approvals = approvals(&run_start.approvals, &reloaded.approvals);
    settings.limits = limits(&run_start.limits, &reloaded.limits);
    settings.sandbox = sandbox(&run_start.sandbox, &reloaded.sandbox);
    settings.budget.run_usd = smaller_cap(run_start.budget.run_usd, reloaded.budget.run_usd);
    settings
}

fn approvals(a: &ApprovalSettings, b: &ApprovalSettings) -> ApprovalSettings {
    ApprovalSettings {
        answered_by: answered_by(a.answered_by, b.answered_by),
        // Topics that come back to you add up, so a reload can't drop one mid-run.
        ask_you_about: union(&a.ask_you_about, &b.ask_you_about),
        goal_changes: goal_changes(a.goal_changes, b.goal_changes),
        spot_check_sample: a.spot_check_sample.max(b.spot_check_sample),
        pause_auto_when_spot_checks_waiting: pause_limit(
            a.pause_auto_when_spot_checks_waiting,
            b.pause_auto_when_spot_checks_waiting,
        ),
        suggest_auto_after_matches: a
            .suggest_auto_after_matches
            .max(b.suggest_auto_after_matches),
        goal: checkpoint(&a.goal, &b.goal, |_, _| GoalRules {}),
        plan: checkpoint(&a.plan, &b.plan, plan_rules),
        merge: checkpoint(&a.merge, &b.merge, merge_rules),
        recipe_promotion: checkpoint(&a.recipe_promotion, &b.recipe_promotion, promotion_rules),
        escalation: checkpoint(&a.escalation, &b.escalation, |a, b| EscalationRules {
            max_extra_attempts: a.max_extra_attempts.min(b.max_extra_attempts),
        }),
        budget: checkpoint(&a.budget, &b.budget, |a, b| BudgetRules {
            use_reserve: a.use_reserve && b.use_reserve,
        }),
        change_request: checkpoint(&a.change_request, &b.change_request, |_, _| NoRules {}),
        approver: approver(&a.approver, &b.approver),
        presence: presence(a.presence, b.presence),
        refuse_ancestor_env: union(&a.refuse_ancestor_env, &b.refuse_ancestor_env),
    }
}

/// You answering is the strictest, then following the mode, then the lead in every mode.
fn answered_by(a: Option<Answerer>, b: Option<Answerer>) -> Option<Answerer> {
    match (a, b) {
        (Some(Answerer::You), _) | (_, Some(Answerer::You)) => Some(Answerer::You),
        (Some(Answerer::Lead), Some(Answerer::Lead)) => Some(Answerer::Lead),
        _ => None,
    }
}

fn checkpoint<R: Default>(
    a: &CheckpointSettings<R>,
    b: &CheckpointSettings<R>,
    rules: impl FnOnce(&R, &R) -> R,
) -> CheckpointSettings<R> {
    CheckpointSettings {
        mode: less_autonomous(a.mode, b.mode),
        decide_with: decide_with(a.decide_with, b.decide_with),
        rules: rules(&a.rules, &b.rules),
        otherwise: otherwise(a.otherwise, b.otherwise),
    }
}

pub(crate) fn less_autonomous(a: ApprovalMode, b: ApprovalMode) -> ApprovalMode {
    fn rank(mode: ApprovalMode) -> u8 {
        match mode {
            ApprovalMode::Supervised => 0,
            ApprovalMode::Shadow => 1,
            ApprovalMode::Auto => 2,
        }
    }
    if rank(a) <= rank(b) { a } else { b }
}

/// Approving under both settings means passing both conditions: `rules_or_model` with
/// `rules` is `rules`, and `rules` with `model` needs both.
fn decide_with(a: DecideWith, b: DecideWith) -> DecideWith {
    use DecideWith::*;
    match (a, b) {
        _ if a == b => a,
        (RulesOrModel, other) | (other, RulesOrModel) => other,
        _ => RulesAndModel,
    }
}

/// 0 never pauses auto, so it's the loosest limit rather than the smallest.
fn pause_limit(a: u32, b: u32) -> u32 {
    match (a, b) {
        (0, limit) | (limit, 0) => limit,
        (a, b) => a.min(b),
    }
}

/// When the two disagree, you decide.
fn otherwise(a: Otherwise, b: Otherwise) -> Otherwise {
    if a == b { a } else { Otherwise::Ask }
}

fn goal_changes(a: GoalChanges, b: GoalChanges) -> GoalChanges {
    use GoalChanges::*;
    match (a, b) {
        _ if a == b => a,
        (AllowClarifications, other) | (other, AllowClarifications) => other,
        _ => Ask,
    }
}

fn presence(a: Presence, b: Presence) -> Presence {
    fn rank(presence: Presence) -> u8 {
        match presence {
            Presence::Off => 0,
            Presence::Warn => 1,
            Presence::Required => 2,
        }
    }
    if rank(a) >= rank(b) { a } else { b }
}

fn plan_rules(a: &PlanRules, b: &PlanRules) -> PlanRules {
    PlanRules {
        max_score_spread: a.max_score_spread.min(b.max_score_spread),
        min_winner_margin: a.min_winner_margin.max(b.min_winner_margin),
    }
}

fn merge_rules(a: &MergeRules, b: &MergeRules) -> MergeRules {
    MergeRules {
        min_changed_line_coverage: a.min_changed_line_coverage.max(b.min_changed_line_coverage),
        max_combined_diff_lines: a.max_combined_diff_lines.min(b.max_combined_diff_lines),
        allow_debt: a.allow_debt && b.allow_debt,
    }
}

fn promotion_rules(a: &PromotionRules, b: &PromotionRules) -> PromotionRules {
    PromotionRules {
        change_classes: intersection(&a.change_classes, &b.change_classes),
        min_days_between: a.min_days_between.max(b.min_days_between),
    }
}

/// A new approver model waits for the next run, since the run's evidence and independence
/// checks were set up for the one it started with. Removing or replacing the model stops the
/// old one at once, and what it would decide asks you for the rest of the run.
fn approver(a: &ApproverSettings, b: &ApproverSettings) -> ApproverSettings {
    ApproverSettings {
        model: if a.model == b.model {
            a.model.clone()
        } else {
            None
        },
        model_rules: a.model_rules.clone(),
        different_family: a.different_family || b.different_family,
        second_model: a.second_model.clone().or_else(|| b.second_model.clone()),
    }
}

fn limits(a: &HardLimitSettings, b: &HardLimitSettings) -> HardLimitSettings {
    HardLimitSettings {
        protected_paths: union(&a.protected_paths, &b.protected_paths),
        dependency_allowlist: intersection(&a.dependency_allowlist, &b.dependency_allowlist),
        usd_per_day: smaller_cap(a.usd_per_day, b.usd_per_day),
        auto_merges_per_day: a.auto_merges_per_day.min(b.auto_merges_per_day),
        irreversible_allowlist: intersection(&a.irreversible_allowlist, &b.irreversible_allowlist),
        evolution_usd_per_month: a.evolution_usd_per_month.min(b.evolution_usd_per_month),
    }
}

/// The run keeps the sandbox it started with, unless the files now ask for a stricter one:
/// ours over each tool's own, and that over none. Then new sessions get it, and the
/// checkpoints still ask, since the earlier sessions ran without it.
fn sandbox(a: &SandboxSettings, b: &SandboxSettings) -> SandboxSettings {
    fn rank(mode: SandboxChoice) -> u8 {
        match mode {
            SandboxChoice::None => 0,
            SandboxChoice::Tool => 1,
            SandboxChoice::Os | SandboxChoice::Container => 2,
        }
    }
    SandboxSettings {
        mode: if rank(b.mode) > rank(a.mode) {
            b.mode
        } else {
            a.mode
        },
        network: a.network && b.network,
        agents_see_other_copies: a.agents_see_other_copies && b.agents_see_other_copies,
        agents_commit: a.agents_commit && b.agents_commit,
        fail_closed: a.fail_closed || b.fail_closed,
        extra_writable: intersection(&a.extra_writable, &b.extra_writable),
        proxy_allow: intersection(&a.proxy_allow, &b.proxy_allow),
        untrusted_repos: if a.untrusted_repos == UntrustedRepos::Refuse {
            a.untrusted_repos
        } else {
            b.untrusted_repos
        },
        container_image: a.container_image.clone(),
        user_hooks: if a.user_hooks == UserHooks::Disable {
            a.user_hooks
        } else {
            b.user_hooks
        },
    }
}

/// `None` means no cap, so any cap is smaller.
fn smaller_cap(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (cap, None) | (None, cap) => cap,
    }
}

fn union(a: &[String], b: &[String]) -> Vec<String> {
    let mut all = a.to_vec();
    all.extend(b.iter().filter(|item| !a.contains(item)).cloned());
    all
}

fn intersection(a: &[String], b: &[String]) -> Vec<String> {
    a.iter().filter(|item| b.contains(item)).cloned().collect()
}
