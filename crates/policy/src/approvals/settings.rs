//! The settings an approval uses: the stricter of the run-start settings and the files as they
//! are now. A tighter change applies at the next approval, and a looser one waits for the
//! next run.

use std::sync::Arc;

use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::{
    Answerer, ApprovalMode, ApprovalPointSettings, ApprovalRules, ApprovalSettings, AtHardLimit,
    DecideWith, Decides, GoalChanges, HardLimitSettings, IsolatedLeak, JudgeSettings, LimitRules,
    MergeRules, Otherwise, PlanRules, Presence, PromotionRules, SandboxChoice, SandboxSettings,
    StuckRules, UntrustedRepos, UserHooks,
};
use argonauts_core::settings::asking::WhenDeclined;
use argonauts_core::settings::limit::SpendLimits;

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

/// For each approval: the less autonomous mode, the stricter rule numbers, and the smaller
/// caps of the two, plus the stricter hard limits and sandbox. Everything the policy doesn't
/// read keeps its run-start value.
pub fn stricter(run_start: &Settings, reloaded: &Settings) -> Settings {
    let mut settings = run_start.clone();
    settings.approvals = approvals(&run_start.approvals, &reloaded.approvals);
    settings.limits = limits(&run_start.limits, &reloaded.limits);
    settings.sandbox = sandbox(&run_start.sandbox, &reloaded.sandbox);
    settings
}

fn approvals(a: &ApprovalSettings, b: &ApprovalSettings) -> ApprovalSettings {
    ApprovalSettings {
        // Autopilot everywhere only while both say so.
        autopilot: a.autopilot && b.autopilot,
        // Read by the app at your terminal as it starts, never by a run.
        start_mode: a.start_mode,
        answers: answered_by(a.answers.as_ref(), b.answers.as_ref()),
        decides: decides(&a.decides, &b.decides),
        // Topics that come back to you add up, so a reload can't drop one mid-run.
        ask_you_about: union(&a.ask_you_about, &b.ask_you_about),
        // Ending the part decides less in your place than carrying on does.
        when_declined: if a.when_declined == WhenDeclined::Stop {
            a.when_declined
        } else {
            b.when_declined
        },
        ask_limit: a.ask_limit.min(b.ask_limit),
        goal_changes: goal_changes(a.goal_changes, b.goal_changes),
        spot_check_sample: a.spot_check_sample.max(b.spot_check_sample),
        pause_auto_when_spot_checks_waiting: pause_limit(
            a.pause_auto_when_spot_checks_waiting,
            b.pause_auto_when_spot_checks_waiting,
        ),
        suggest_auto_after_matches: a
            .suggest_auto_after_matches
            .max(b.suggest_auto_after_matches),
        rules: rules(&a.rules, &b.rules),
        understand: approval(&a.understand, &b.understand),
        plan: approval(&a.plan, &b.plan),
        merge: approval(&a.merge, &b.merge),
        workflow_promotion: approval(
            &a.workflow_promotion.as_approval(),
            &b.workflow_promotion.as_approval(),
        )
        .into(),
        stuck: approval(&a.stuck, &b.stuck),
        limit: approval(&a.limit, &b.limit),
        change_request: approval(&a.change_request, &b.change_request),
        judge: judge(&a.judge, &b.judge),
        presence: presence(a.presence, b.presence),
        refuse_ancestor_env: union(&a.refuse_ancestor_env, &b.refuse_ancestor_env),
        // A command runs without asking only while both allow it.
        allow_commands: a
            .allow_commands
            .iter()
            .filter(|pattern| b.allow_commands.contains(pattern))
            .cloned()
            .collect(),
    }
}

/// You answering is the strictest, then following the mode, then the main agent, or a model you
/// picked for autopilot, which both must name.
fn answered_by(a: Option<&Answerer>, b: Option<&Answerer>) -> Option<Answerer> {
    match (a, b) {
        (Some(Answerer::You), _) | (_, Some(Answerer::You)) => Some(Answerer::You),
        (Some(a), Some(b)) if a == b => Some(a.clone()),
        _ => None,
    }
}

fn approval(a: &ApprovalPointSettings, b: &ApprovalPointSettings) -> ApprovalPointSettings {
    ApprovalPointSettings {
        mode: less_autonomous(a.mode, b.mode),
        decide_with: decide_with(a.decide_with, b.decide_with),
        otherwise: otherwise(a.otherwise, b.otherwise),
        at_hard_limit: at_hard_limit(a.at_hard_limit, b.at_hard_limit),
    }
}

pub(crate) fn less_autonomous(a: ApprovalMode, b: ApprovalMode) -> ApprovalMode {
    if a.autonomy() <= b.autonomy() { a } else { b }
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

/// When the two disagree: wait when either says so, else stop, which ends that part safely,
/// so a run never waits for you at a hard limit that neither file left to you.
fn at_hard_limit(a: AtHardLimit, b: AtHardLimit) -> AtHardLimit {
    match (a, b) {
        _ if a == b => a,
        (AtHardLimit::Wait, _) | (_, AtHardLimit::Wait) => AtHardLimit::Wait,
        _ => AtHardLimit::Stop,
    }
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

/// The stricter number of the two, rule by rule.
fn rules(a: &ApprovalRules, b: &ApprovalRules) -> ApprovalRules {
    ApprovalRules {
        plan: plan_rules(&a.plan, &b.plan),
        merge: merge_rules(&a.merge, &b.merge),
        workflow_promotion: promotion_rules(&a.workflow_promotion, &b.workflow_promotion),
        stuck: StuckRules {
            max_extra_tries: u32::min(a.stuck.max_extra_tries, b.stuck.max_extra_tries),
        },
        limit: LimitRules {
            use_reserve: a.limit.use_reserve && b.limit.use_reserve,
        },
    }
}

fn plan_rules(a: &PlanRules, b: &PlanRules) -> PlanRules {
    PlanRules {
        max_score_spread: a.max_score_spread.min(b.max_score_spread),
        min_winner_margin: a.min_winner_margin.max(b.min_winner_margin),
    }
}

fn merge_rules(a: &MergeRules, b: &MergeRules) -> MergeRules {
    MergeRules {
        min_test_coverage_of_changed_lines: a
            .min_test_coverage_of_changed_lines
            .max(b.min_test_coverage_of_changed_lines),
        max_changed_lines: a.max_changed_lines.min(b.max_changed_lines),
        allow_debt: a.allow_debt && b.allow_debt,
    }
}

fn promotion_rules(a: &PromotionRules, b: &PromotionRules) -> PromotionRules {
    PromotionRules {
        change_classes: intersection(&a.change_classes, &b.change_classes),
        min_days_between: a.min_days_between.max(b.min_days_between),
    }
}

/// A model you name to judge waits for the next run, since the run's evidence and independence
/// checks were set up for the one it started with. Removing or replacing it stops the old one
/// at once, and the judge the policy picks settles the rest of the run.
fn decides(a: &Decides, b: &Decides) -> Decides {
    if a == b { a.clone() } else { Decides::Judge }
}

fn judge(a: &JudgeSettings, b: &JudgeSettings) -> JudgeSettings {
    JudgeSettings {
        model_rules: a.model_rules.clone(),
        different_provider: a.different_provider || b.different_provider,
        second_model: a.second_model.clone().or_else(|| b.second_model.clone()),
    }
}

fn limits(a: &HardLimitSettings, b: &HardLimitSettings) -> HardLimitSettings {
    HardLimitSettings {
        protected_paths: union(&a.protected_paths, &b.protected_paths),
        dependency_allowlist: intersection(&a.dependency_allowlist, &b.dependency_allowlist),
        spend_per_day: smaller_cap(a.spend_per_day, b.spend_per_day),
        auto_merges_per_day: smaller_cap(a.auto_merges_per_day, b.auto_merges_per_day),
        irreversible_allowlist: intersection(&a.irreversible_allowlist, &b.irreversible_allowlist),
        evolution_usd_per_month: smaller_cap(a.evolution_usd_per_month, b.evolution_usd_per_month),
        // Of the run and task limits, only the run's dollars are a cap the policy reads.
        spend: SpendLimits {
            spend_per_run: smaller_cap(a.spend.spend_per_run, b.spend.spend_per_run),
            ..a.spend.clone()
        },
    }
}

/// The run keeps the sandbox it started with, unless the files now ask for a stricter one:
/// ours over each tool's own, and that over none. Then new sessions get it, and the
/// approvals still ask, since the earlier sessions ran without it.
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
        agents_see_other_worktrees: a.agents_see_other_worktrees && b.agents_see_other_worktrees,
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
        isolated_leak: if a.isolated_leak == IsolatedLeak::Stop {
            a.isolated_leak
        } else {
            b.isolated_leak
        },
    }
}

/// `None` means no cap, so any cap is smaller.
fn smaller_cap<T: PartialOrd>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if b < a { b } else { a }),
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
