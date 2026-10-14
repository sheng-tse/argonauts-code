//! Tests for deciding approvals, over fake hard limits, a fake judge, and fake autonomy
//! records, so they don't wait for the real parts.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use argonauts_core::RunId;
use argonauts_core::events::approval::{AnswerAction, ApprovalKind};
use argonauts_core::events::task::TaskSpec;
use argonauts_core::future::BoxFuture;
use argonauts_core::names::{ApprovalId, ModelRef};
use argonauts_core::policy::{
    ApprovalPolicy, ApprovalsView, AutoPreviewRecord, DecisionPoint, EvidenceFacts, EvidencePacket,
    HardLimit, HardLimits, PolicyOutcome, UserDecision, Verdict, Violation,
};
use argonauts_core::session_runner::SessionSpec;
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::{
    Answerer, ApprovalMode, ApprovalPointSettings, DecideWith, Decides, GoalChanges, Otherwise,
    Presence, SandboxChoice,
};
use argonauts_testkit::FakeClock;
use tokio::sync::Notify;

use super::ApprovalsPolicy;
use super::decide::decide;
use super::settings::{SettingsSource, stricter};
use crate::SettingsReloader;
use crate::parts::{Approve, Autonomy, ModelVerdict, PolicyParts, Refusal, Reservation};

struct FakeLimits {
    violations: Vec<Violation>,
}

impl HardLimits for FakeLimits {
    fn check_point(&self, _: &DecisionPoint, _: &EvidencePacket) -> Vec<Violation> {
        self.violations.clone()
    }

    fn check_task_diff(&self, _: &TaskSpec, _: &str) -> Vec<Violation> {
        Vec::new()
    }

    fn check_session(&self, _: &SessionSpec) -> Vec<Violation> {
        Vec::new()
    }
}

struct FakeJudge {
    answer: Mutex<ModelVerdict>,
    calls: AtomicUsize,
    /// When set, the verdict waits for it, like a model still thinking.
    check: Option<Arc<Notify>>,
}

impl FakeJudge {
    fn answering(verdict: Verdict) -> Self {
        Self {
            answer: Mutex::new(model_says(verdict)),
            calls: AtomicUsize::new(0),
            check: None,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Approve for FakeJudge {
    fn verdict<'a>(
        &'a self,
        _: &'a DecisionPoint,
        _: &'a EvidencePacket,
        _: &'a Settings,
    ) -> BoxFuture<'a, ModelVerdict> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(check) = &self.check {
                check.notified().await;
            }
            self.answer.lock().unwrap().clone()
        })
    }
}

fn judge_model() -> ModelRef {
    ModelRef::new("codex", Some("gpt-5.6-luna".to_owned()))
}

fn model_says(verdict: Verdict) -> ModelVerdict {
    ModelVerdict {
        verdict,
        judge: Some(judge_model()),
        reasons: vec![format!("the model says {verdict:?}")],
    }
}

#[derive(Default)]
struct FakeAutonomy {
    /// Replaces what `effective_mode` returns, even when that would raise the mode.
    effective: Mutex<Option<ApprovalMode>>,
    paused: Mutex<Option<String>>,
    refusal: Mutex<Option<Refusal>>,
    spot_check: bool,
    answered: Mutex<Vec<ApprovalId>>,
    auto_previews: Mutex<Vec<AutoPreviewRecord>>,
    log: Mutex<Vec<&'static str>>,
}

impl FakeAutonomy {
    fn log(&self) -> Vec<&'static str> {
        self.log.lock().unwrap().clone()
    }

    fn auto_previews(&self) -> Vec<AutoPreviewRecord> {
        self.auto_previews.lock().unwrap().clone()
    }
}

impl Autonomy for FakeAutonomy {
    fn effective_mode(&self, _: &DecisionPoint, configured: ApprovalMode) -> ApprovalMode {
        self.effective.lock().unwrap().unwrap_or(configured)
    }

    fn auto_paused(&self, _: &Settings) -> Option<String> {
        self.paused.lock().unwrap().clone()
    }

    fn reserve(
        &self,
        point: &DecisionPoint,
        _: &EvidencePacket,
        _: &Settings,
    ) -> Result<Reservation, Refusal> {
        self.log.lock().unwrap().push("reserve");
        if self.answered.lock().unwrap().contains(&point.approval) {
            return Err(Refusal::AlreadyDecided);
        }
        match self.refusal.lock().unwrap().clone() {
            Some(refusal) => Err(refusal),
            None => Ok(Reservation {
                spot_check: self.spot_check,
            }),
        }
    }

    fn note_auto_preview(
        &self,
        _: &DecisionPoint,
        auto_preview: &AutoPreviewRecord,
        _: &EvidencePacket,
        _: &Settings,
    ) {
        self.log.lock().unwrap().push("note_auto_preview");
        self.auto_previews
            .lock()
            .unwrap()
            .push(auto_preview.clone());
    }

    fn user_decided(&self, decision: &UserDecision, _: &Settings) {
        self.log.lock().unwrap().push("user_decided");
        self.answered
            .lock()
            .unwrap()
            .push(decision.point.approval.clone());
    }

    fn spot_checked(&self, _: &DecisionPoint, _: bool, _: &Settings) {}

    fn auto_off(&self) {}

    fn view(&self, _: &Settings) -> ApprovalsView {
        ApprovalsView::default()
    }
}

struct TestDriver {
    policy: ApprovalsPolicy,
    judge: Arc<FakeJudge>,
    autonomy: Arc<FakeAutonomy>,
}

struct Fakes {
    violations: Vec<Violation>,
    judge: FakeJudge,
    autonomy: FakeAutonomy,
    reload: SettingsReloader,
}

impl Default for Fakes {
    fn default() -> Self {
        Self {
            violations: Vec::new(),
            judge: FakeJudge::answering(Verdict::Approve),
            autonomy: FakeAutonomy::default(),
            reload: Arc::new(|| None),
        }
    }
}

fn driver(settings: Settings) -> TestDriver {
    driver_with(settings, Fakes::default())
}

fn driver_with(settings: Settings, fakes: Fakes) -> TestDriver {
    let judge = Arc::new(fakes.judge);
    let autonomy = Arc::new(fakes.autonomy);
    let policy = ApprovalsPolicy::from_parts(PolicyParts {
        source: SettingsSource::new(Arc::new(settings), fakes.reload),
        limits: Arc::new(FakeLimits {
            violations: fakes.violations,
        }),
        judge: judge.clone(),
        autonomy: autonomy.clone(),
        clock: Arc::new(FakeClock::default()),
    });
    TestDriver {
        policy,
        judge,
        autonomy,
    }
}

fn point(kind: ApprovalKind) -> DecisionPoint {
    DecisionPoint {
        run: RunId::new("run-01").unwrap(),
        approval: ApprovalId::numbered(1),
        kind,
        learned_workflow: None,
        authors: Vec::new(),
        mode_override: None,
        main_agent: None,
    }
}

fn evidence(kind: ApprovalKind, facts: EvidenceFacts) -> EvidencePacket {
    EvidencePacket {
        kind,
        goal: None,
        facts,
        agent_outputs: Vec::new(),
        documents: Vec::new(),
    }
}

/// Merge facts on which the default merge rules give `verdict`.
fn merge_facts(verdict: Verdict) -> EvidenceFacts {
    let passing = EvidenceFacts {
        checks_passed: Some(true),
        reviews_approved: Some(true),
        reviewed_by_other_provider: Some(true),
        acceptance_tests_valid: Some(true),
        changed_line_coverage: Some(0.9),
        tamper_flags: Some(0),
        combined_diff_lines: Some(120),
        suite_on_latest_main: Some(true),
        unreviewed_debt: Some(Vec::new()),
        ..EvidenceFacts::default()
    };
    match verdict {
        Verdict::Approve => passing,
        Verdict::Reject => EvidenceFacts {
            checks_passed: Some(false),
            ..passing
        },
        Verdict::AskYou => EvidenceFacts {
            checks_passed: None,
            ..passing
        },
    }
}

fn merge(verdict: Verdict) -> EvidencePacket {
    evidence(ApprovalKind::Merge, merge_facts(verdict))
}

fn merge_settings(mode: ApprovalMode, decide_with: DecideWith, otherwise: Otherwise) -> Settings {
    let mut settings = Settings::default();
    settings.approvals.merge.mode = mode;
    settings.approvals.merge.decide_with = decide_with;
    settings.approvals.merge.otherwise = otherwise;
    settings
}

fn auto_merge() -> Settings {
    merge_settings(ApprovalMode::Auto, DecideWith::Rules, Otherwise::Ask)
}

fn asks() -> PolicyOutcome {
    PolicyOutcome::AskUser {
        violations: Vec::new(),
        auto_preview: None,
    }
}

fn violation() -> Violation {
    Violation {
        limit: HardLimit::ProtectedPath,
        detail: "a task touched a protected path".to_owned(),
        paths: vec![".github/workflows/ci.yml".to_owned()],
    }
}

#[derive(Debug, Clone, Copy)]
enum Expect {
    Ask,
    /// Asks you, with what auto would decide.
    Would(Verdict),
    Decided(Verdict),
}

fn assert_outcome(outcome: &PolicyOutcome, expect: Expect, case: &str) {
    match (expect, outcome) {
        (Expect::Ask, outcome) => assert_eq!(outcome, &asks(), "{case}"),
        (
            Expect::Would(would),
            PolicyOutcome::AskUser {
                violations,
                auto_preview: Some(record),
            },
        ) => {
            assert!(violations.is_empty(), "{case}");
            assert_eq!(record.would, would, "{case}");
        }
        (Expect::Decided(want), PolicyOutcome::Decided { verdict, .. }) => {
            assert_eq!(*verdict, want, "{case}");
        }
        (expect, outcome) => panic!("{case}: expected {expect:?}, got {outcome:?}"),
    }
}

fn last_reason(outcome: &PolicyOutcome) -> Option<String> {
    match outcome {
        PolicyOutcome::AskUser {
            auto_preview: Some(record),
            ..
        } => record.reasons.last().cloned(),
        _ => None,
    }
}

#[tokio::test]
async fn policy_modes_table() {
    use ApprovalMode::{Auto, AutoPreview, Supervised};
    use Verdict::{Approve, AskYou, Reject};
    let cases = [
        (Supervised, Approve, Otherwise::Ask, Expect::Ask),
        (Supervised, Reject, Otherwise::Stop, Expect::Ask),
        (Supervised, AskYou, Otherwise::Ask, Expect::Ask),
        (AutoPreview, Approve, Otherwise::Ask, Expect::Would(Approve)),
        (AutoPreview, Reject, Otherwise::Stop, Expect::Would(Reject)),
        (AutoPreview, AskYou, Otherwise::Ask, Expect::Would(AskYou)),
        (Auto, Approve, Otherwise::Ask, Expect::Decided(Approve)),
        (Auto, Approve, Otherwise::Stop, Expect::Decided(Approve)),
        (Auto, Reject, Otherwise::Ask, Expect::Would(Reject)),
        (Auto, Reject, Otherwise::SendBack, Expect::Decided(Reject)),
        (Auto, Reject, Otherwise::Stop, Expect::Decided(Reject)),
        (
            Auto,
            Reject,
            Otherwise::OpenPullRequest,
            Expect::Decided(Reject),
        ),
        (Auto, AskYou, Otherwise::Ask, Expect::Would(AskYou)),
        (Auto, AskYou, Otherwise::Stop, Expect::Would(AskYou)),
    ];
    for (mode, rules, otherwise, expect) in cases {
        let case = format!("{mode:?} with rules {rules:?} and otherwise {otherwise:?}");
        let h = driver(merge_settings(mode, DecideWith::Rules, otherwise));
        let outcome = h
            .policy
            .evaluate(&point(ApprovalKind::Merge), &merge(rules))
            .await;
        assert_outcome(&outcome, expect, &case);
        assert_eq!(h.judge.calls(), 0, "{case}");
        let auto_previews = usize::from(mode == AutoPreview);
        assert_eq!(h.autonomy.auto_previews().len(), auto_previews, "{case}");
        if let PolicyOutcome::Decided { by, judge, .. } = &outcome {
            assert_eq!((*by, judge), (DecideWith::Rules, &None), "{case}");
        }
    }

    // Opening a pull request fits only a merge, so a rejected plan asks.
    let mut settings = Settings::default();
    settings.approvals.plan.mode = Auto;
    settings.approvals.plan.otherwise = Otherwise::OpenPullRequest;
    let flagged = EvidenceFacts {
        flagged_plans: Some(1),
        ..EvidenceFacts::default()
    };
    let outcome = driver(settings)
        .policy
        .evaluate(
            &point(ApprovalKind::Plan),
            &evidence(ApprovalKind::Plan, flagged),
        )
        .await;
    assert_outcome(&outcome, Expect::Would(Verdict::Reject), "plan");
}

#[tokio::test]
async fn auto_approval_reports_the_spot_check_and_asks_when_it_cannot_reserve() {
    let fakes = Fakes {
        autonomy: FakeAutonomy {
            spot_check: true,
            ..FakeAutonomy::default()
        },
        ..Fakes::default()
    };
    let h = driver_with(auto_merge(), fakes);
    let outcome = h
        .policy
        .evaluate(&point(ApprovalKind::Merge), &merge(Verdict::Approve))
        .await;
    assert!(matches!(
        outcome,
        PolicyOutcome::Decided {
            verdict: Verdict::Approve,
            spot_check: true,
            ..
        }
    ));

    let capped = vec![Violation {
        limit: HardLimit::AutoMergeCap,
        detail: "3 auto-merges today".to_owned(),
        paths: Vec::new(),
    }];
    // Each ask carries what auto decided and why it asks anyway.
    let h = driver(auto_merge());
    *h.autonomy.refusal.lock().unwrap() = Some(Refusal::Capped(capped.clone()));
    let outcome = h
        .policy
        .evaluate(&point(ApprovalKind::Merge), &merge(Verdict::Approve))
        .await;
    let PolicyOutcome::AskUser {
        violations,
        auto_preview: Some(record),
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(violations, capped);
    assert_eq!(record.would, Verdict::Approve);
    assert!(!record.reasons.is_empty());

    // Auto would approve but can't right now: the verdict is an auto preview record that says why.
    let refusals = [
        (Refusal::Unavailable("locked".to_owned()), "locked"),
        (Refusal::Paused("demoted".to_owned()), "demoted"),
    ];
    for (refusal, reason) in refusals {
        let h = driver(auto_merge());
        *h.autonomy.refusal.lock().unwrap() = Some(refusal);
        let outcome = h
            .policy
            .evaluate(&point(ApprovalKind::Merge), &merge(Verdict::Approve))
            .await;
        let PolicyOutcome::AskUser {
            violations,
            auto_preview: Some(auto_preview),
        } = outcome
        else {
            panic!("asks with an auto preview record: {outcome:?}");
        };
        assert!(violations.is_empty());
        assert_eq!(auto_preview.would, Verdict::Approve);
        assert_eq!(
            auto_preview.reasons.last().map(String::as_str),
            Some(reason)
        );
        assert_eq!(h.autonomy.log(), ["reserve", "note_auto_preview"]);
    }

    let h = driver(auto_merge());
    *h.autonomy.paused.lock().unwrap() = Some("5 spot checks waiting".to_owned());
    let outcome = h
        .policy
        .evaluate(&point(ApprovalKind::Merge), &merge(Verdict::Approve))
        .await;
    assert_outcome(&outcome, Expect::Would(Verdict::Approve), "paused");
    assert_eq!(
        last_reason(&outcome),
        Some("5 spot checks waiting".to_owned())
    );
    assert_eq!(
        h.autonomy.log(),
        ["note_auto_preview"],
        "a paused auto reserves nothing"
    );
}

/// Nobody answers spot checks in autopilot, so neither waiting ones nor a kind demoted to
/// auto preview stop an approval there; `auto-off` still does, and a plan or merge you keep for
/// yourself with `presence = required` says so.
#[tokio::test]
async fn autopilot_goes_on_where_auto_would_pause_or_be_demoted() {
    let mut autopilot = point(ApprovalKind::Merge);
    autopilot.mode_override = Some(ApprovalMode::Autopilot);

    let h = driver(auto_merge());
    *h.autonomy.paused.lock().unwrap() = Some("5 spot checks waiting".to_owned());
    let outcome = h
        .policy
        .evaluate(&autopilot, &merge(Verdict::Approve))
        .await;
    assert_outcome(&outcome, Expect::Decided(Verdict::Approve), "paused");
    assert_eq!(h.autonomy.log(), ["reserve"]);

    let h = driver(auto_merge());
    *h.autonomy.effective.lock().unwrap() = Some(ApprovalMode::AutoPreview);
    let outcome = h
        .policy
        .evaluate(&autopilot, &merge(Verdict::Approve))
        .await;
    assert_outcome(&outcome, Expect::Decided(Verdict::Approve), "demoted");

    let h = driver(auto_merge());
    *h.autonomy.effective.lock().unwrap() = Some(ApprovalMode::Supervised);
    *h.autonomy.paused.lock().unwrap() = Some("auto is off".to_owned());
    let outcome = h
        .policy
        .evaluate(&autopilot, &merge(Verdict::Approve))
        .await;
    assert_outcome(&outcome, Expect::Would(Verdict::AskYou), "auto-off");

    let mut settings = auto_merge();
    settings.approvals.presence = Presence::Required;
    let outcome = driver(settings)
        .policy
        .evaluate(&autopilot, &merge(Verdict::Approve))
        .await;
    assert_outcome(&outcome, Expect::Would(Verdict::AskYou), "presence");
    assert_eq!(
        last_reason(&outcome),
        Some("your /approvals settings keep it for you at the terminal".to_owned())
    );
}

#[tokio::test]
async fn the_mode_comes_from_the_override_and_is_only_ever_lowered() {
    let supervised = merge_settings(ApprovalMode::Supervised, DecideWith::Rules, Otherwise::Ask);
    let mut shift_tab = point(ApprovalKind::Merge);
    shift_tab.mode_override = Some(ApprovalMode::Auto);
    let outcome = driver(supervised.clone())
        .policy
        .evaluate(&shift_tab, &merge(Verdict::Approve))
        .await;
    assert_outcome(&outcome, Expect::Decided(Verdict::Approve), "override");

    // Autonomy records that lost auto for this kind lower it to auto preview.
    let h = driver(auto_merge());
    *h.autonomy.effective.lock().unwrap() = Some(ApprovalMode::AutoPreview);
    let outcome = h
        .policy
        .evaluate(&point(ApprovalKind::Merge), &merge(Verdict::Approve))
        .await;
    assert_outcome(&outcome, Expect::Would(Verdict::Approve), "lowered");

    // Records that try to raise the mode don't.
    let h = driver(supervised);
    *h.autonomy.effective.lock().unwrap() = Some(ApprovalMode::Auto);
    let outcome = h
        .policy
        .evaluate(&point(ApprovalKind::Merge), &merge(Verdict::Approve))
        .await;
    assert_eq!(outcome, asks());
}

#[tokio::test]
async fn kinds_without_a_table_and_required_presence_always_ask() {
    for kind in [
        ApprovalKind::Fallback,
        ApprovalKind::AgentDown,
        ApprovalKind::TakeOverReturn,
        ApprovalKind::Question,
        ApprovalKind::OriginalReturned,
    ] {
        let mut auto = point(kind);
        auto.mode_override = Some(ApprovalMode::Auto);
        let h = driver(Settings::default());
        let outcome = h
            .policy
            .evaluate(&auto, &evidence(kind, EvidenceFacts::default()))
            .await;
        assert_eq!(outcome, asks(), "{kind:?}");
        assert_eq!(h.judge.calls(), 0, "{kind:?}");
    }

    let mut settings = auto_merge();
    settings.approvals.presence = Presence::Required;
    let outcome = driver(settings)
        .policy
        .evaluate(&point(ApprovalKind::Merge), &merge(Verdict::Approve))
        .await;
    assert_eq!(outcome, asks());
}

#[tokio::test]
async fn goal_changes_follow_the_setting() {
    let goal_change = point(ApprovalKind::GoalChange);
    let packet = evidence(ApprovalKind::GoalChange, EvidenceFacts::default());
    let with = |goal_changes| {
        let mut settings = Settings::default();
        settings.approvals.goal_changes = goal_changes;
        driver(settings)
    };

    let outcome = with(GoalChanges::Ask)
        .policy
        .evaluate(&goal_change, &packet)
        .await;
    assert_eq!(outcome, asks());

    let outcome = with(GoalChanges::Deny)
        .policy
        .evaluate(&goal_change, &packet)
        .await;
    assert!(matches!(
        outcome,
        PolicyOutcome::Decided {
            verdict: Verdict::Reject,
            by: DecideWith::Rules,
            ..
        }
    ));

    let outcome = with(GoalChanges::AllowClarifications)
        .policy
        .evaluate(&goal_change, &packet)
        .await;
    assert_outcome(&outcome, Expect::Would(Verdict::AskYou), "clarification");

    let mut supervised = goal_change.clone();
    supervised.mode_override = Some(ApprovalMode::Supervised);
    let outcome = with(GoalChanges::Deny)
        .policy
        .evaluate(&supervised, &packet)
        .await;
    assert_eq!(outcome, asks());
}

#[tokio::test]
async fn policy_decide_with_combinations() {
    use DecideWith::{Model, Rules, RulesAndModel, RulesOrModel};
    use Verdict::{Approve as A, AskYou as E, Reject as R};
    // (decide_with, rules, model, expected, model asked)
    let cases = [
        (Rules, A, R, A, false),
        (Rules, R, A, R, false),
        (Rules, E, A, E, false),
        (Model, R, A, A, true),
        (Model, A, R, R, true),
        (Model, A, E, E, true),
        (RulesAndModel, A, A, A, true),
        (RulesAndModel, A, R, R, true),
        (RulesAndModel, A, E, E, true),
        (RulesAndModel, R, A, R, false),
        (RulesAndModel, R, E, R, false),
        (RulesAndModel, E, A, E, true),
        (RulesAndModel, E, R, R, true),
        (RulesAndModel, E, E, E, true),
        (RulesOrModel, A, R, A, false),
        (RulesOrModel, A, E, A, false),
        (RulesOrModel, R, A, A, true),
        (RulesOrModel, R, R, R, true),
        (RulesOrModel, R, E, R, true),
        (RulesOrModel, E, A, A, true),
        (RulesOrModel, E, R, R, true),
        (RulesOrModel, E, E, E, true),
    ];
    let settings = Settings::default();
    for (decide_with, rules, model, expected, model_asked) in cases {
        let case = format!("{decide_with:?} with rules {rules:?} and model {model:?}");
        let judge = FakeJudge::answering(model);
        let decision = decide(
            &judge,
            &point(ApprovalKind::Merge),
            &merge(rules),
            &settings,
            decide_with,
            time::OffsetDateTime::now_utc(),
        )
        .await;
        assert_eq!(decision.verdict, expected, "{case}");
        assert_eq!(decision.by, decide_with, "{case}");
        assert_eq!(judge.calls(), usize::from(model_asked), "{case}");
        let judge_named = decision.judge == Some(judge_model());
        assert_eq!(judge_named, model_asked, "{case}");
        let model_reason = decision.reasons.iter().any(|r| r.starts_with("the model"));
        assert_eq!(model_reason, model_asked, "{case}");
    }

    let settings = merge_settings(ApprovalMode::Auto, RulesAndModel, Otherwise::Ask);
    let h = driver(settings);
    let outcome = h
        .policy
        .evaluate(&point(ApprovalKind::Merge), &merge(A))
        .await;
    match outcome {
        PolicyOutcome::Decided {
            verdict, by, judge, ..
        } => assert_eq!(
            (verdict, by, judge),
            (A, RulesAndModel, Some(judge_model()))
        ),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn hard_limits_checked_first() {
    for mode in [
        ApprovalMode::Supervised,
        ApprovalMode::AutoPreview,
        ApprovalMode::Auto,
    ] {
        let fakes = Fakes {
            violations: vec![violation()],
            ..Fakes::default()
        };
        let settings = merge_settings(mode, DecideWith::Model, Otherwise::Stop);
        let h = driver_with(settings, fakes);
        let outcome = h
            .policy
            .evaluate(&point(ApprovalKind::Merge), &merge(Verdict::Approve))
            .await;
        let want = PolicyOutcome::AskUser {
            violations: vec![violation()],
            auto_preview: None,
        };
        assert_eq!(outcome, want, "{mode:?}");
        assert_eq!(h.judge.calls(), 0, "{mode:?}");
        assert!(h.autonomy.log().is_empty(), "{mode:?}");
    }

    // Even a goal change that `deny` would refuse asks when a limit failed.
    let mut settings = Settings::default();
    settings.approvals.goal_changes = GoalChanges::Deny;
    let fakes = Fakes {
        violations: vec![violation()],
        ..Fakes::default()
    };
    let packet = evidence(ApprovalKind::GoalChange, EvidenceFacts::default());
    let outcome = driver_with(settings, fakes)
        .policy
        .evaluate(&point(ApprovalKind::GoalChange), &packet)
        .await;
    assert!(matches!(outcome, PolicyOutcome::AskUser { violations, .. } if violations.len() == 1));
}

#[tokio::test]
async fn policy_auto_goal_approves_when_no_questions() {
    let mut settings = Settings::default();
    settings.approvals.understand = ApprovalPointSettings::auto(DecideWith::Rules);
    let goal = |open_questions| {
        evidence(
            ApprovalKind::Understand,
            EvidenceFacts {
                open_questions,
                ..EvidenceFacts::default()
            },
        )
    };
    let h = driver(settings);
    let outcome = h
        .policy
        .evaluate(&point(ApprovalKind::Understand), &goal(Some(0)))
        .await;
    match outcome {
        PolicyOutcome::Decided {
            verdict,
            by,
            judge,
            spot_check,
            ..
        } => assert_eq!(
            (verdict, by, judge, spot_check),
            (Verdict::Approve, DecideWith::Rules, None, false)
        ),
        other => panic!("{other:?}"),
    }

    for open in [Some(1), Some(4), None] {
        let outcome = h
            .policy
            .evaluate(&point(ApprovalKind::Understand), &goal(open))
            .await;
        assert_outcome(
            &outcome,
            Expect::Would(Verdict::AskYou),
            &format!("{open:?}"),
        );
    }
    assert_eq!(h.judge.calls(), 0);
}

#[tokio::test]
async fn policy_auto_preview_records_would_decide() {
    let settings = merge_settings(ApprovalMode::AutoPreview, DecideWith::Rules, Otherwise::Ask);
    let h = driver(settings);
    let outcome = h
        .policy
        .evaluate(&point(ApprovalKind::Merge), &merge(Verdict::Approve))
        .await;
    let PolicyOutcome::AskUser {
        violations,
        auto_preview: Some(record),
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    assert!(violations.is_empty());
    assert_eq!(
        (record.would, record.by),
        (Verdict::Approve, DecideWith::Rules)
    );
    assert!(!record.reasons.is_empty());
    assert_eq!(h.autonomy.auto_previews(), [record]);
    assert!(
        !h.autonomy.log().contains(&"reserve"),
        "auto preview reserves nothing"
    );

    let settings = merge_settings(ApprovalMode::AutoPreview, DecideWith::Model, Otherwise::Ask);
    let fakes = Fakes {
        judge: FakeJudge::answering(Verdict::Reject),
        ..Fakes::default()
    };
    let h = driver_with(settings, fakes);
    let outcome = h
        .policy
        .evaluate(&point(ApprovalKind::Merge), &merge(Verdict::Approve))
        .await;
    let want = AutoPreviewRecord {
        would: Verdict::Reject,
        by: DecideWith::Model,
        reasons: vec![
            "the model says Reject".to_owned(),
            "asked GPT 5.6 Luna on Codex to approve it".to_owned(),
        ],
    };
    assert_eq!(
        outcome,
        PolicyOutcome::AskUser {
            violations: Vec::new(),
            auto_preview: Some(want.clone()),
        }
    );
    assert_eq!(h.autonomy.auto_previews(), [want]);
    assert_eq!(h.judge.calls(), 1);
}

/// A reloader over a settings file the test can change between approvals.
fn reloadable() -> (Arc<Mutex<Option<Settings>>>, SettingsReloader) {
    let file = Arc::new(Mutex::new(None));
    let read = file.clone();
    let reload: SettingsReloader = Arc::new(move || read.lock().unwrap().clone());
    (file, reload)
}

#[tokio::test]
async fn policy_stricter_of_run_start_and_reloaded_settings() {
    let (file, reload) = reloadable();
    let fakes = Fakes {
        reload,
        ..Fakes::default()
    };
    let h = driver_with(auto_merge(), fakes);
    let merge_point = point(ApprovalKind::Merge);
    let run_start = h.policy.parts().source.run_start().clone();

    // A reload that fails keeps the run-start values.
    assert_eq!(*h.policy.parts().source.current(), *run_start);
    let outcome = h
        .policy
        .evaluate(&merge_point, &merge(Verdict::Approve))
        .await;
    assert_outcome(&outcome, Expect::Decided(Verdict::Approve), "run start");

    // A tighter mode applies at the next approval.
    let tighter = merge_settings(ApprovalMode::Supervised, DecideWith::Rules, Otherwise::Ask);
    *file.lock().unwrap() = Some(tighter);
    let outcome = h
        .policy
        .evaluate(&merge_point, &merge(Verdict::Approve))
        .await;
    assert_eq!(outcome, asks());

    // So does a stricter number: 90% coverage no longer clears the bar.
    let mut stricter_number = auto_merge();
    stricter_number
        .approvals
        .rules
        .merge
        .min_test_coverage_of_changed_lines = 0.95;
    *file.lock().unwrap() = Some(stricter_number);
    let outcome = h
        .policy
        .evaluate(&merge_point, &merge(Verdict::Approve))
        .await;
    assert_outcome(&outcome, Expect::Would(Verdict::Reject), "stricter number");

    // Back to the run-start file, auto decides again.
    *file.lock().unwrap() = Some(auto_merge());
    let outcome = h
        .policy
        .evaluate(&merge_point, &merge(Verdict::Approve))
        .await;
    assert_outcome(&outcome, Expect::Decided(Verdict::Approve), "back");

    // A looser file waits for the next run.
    let (file, reload) = reloadable();
    let fakes = Fakes {
        reload,
        ..Fakes::default()
    };
    let mut supervised =
        merge_settings(ApprovalMode::Supervised, DecideWith::Rules, Otherwise::Ask);
    supervised.limits.auto_merges_per_day = Some(3);
    let h = driver_with(supervised, fakes);
    let mut looser = auto_merge();
    looser
        .approvals
        .rules
        .merge
        .min_test_coverage_of_changed_lines = 0.1;
    looser.limits.auto_merges_per_day = Some(50);
    *file.lock().unwrap() = Some(looser);
    let now = h.policy.parts().source.current();
    assert_eq!(now.approvals.merge.mode, ApprovalMode::Supervised);
    assert_eq!(
        now.approvals.rules.merge.min_test_coverage_of_changed_lines,
        0.8
    );
    assert_eq!(now.limits.auto_merges_per_day, Some(3));
    let outcome = h
        .policy
        .evaluate(&merge_point, &merge(Verdict::Approve))
        .await;
    assert_eq!(outcome, asks());

    // A pause limit of 0 never pauses: set mid-run it waits for the next run, and a limit set
    // on a run that started at 0 applies at once.
    let limit = |run: u32, now: u32| {
        let (file, reload) = reloadable();
        let mut start = auto_merge();
        start.approvals.pause_auto_when_spot_checks_waiting = run;
        let source = SettingsSource::new(Arc::new(start), reload);
        let mut reloaded = auto_merge();
        reloaded.approvals.pause_auto_when_spot_checks_waiting = now;
        *file.lock().unwrap() = Some(reloaded);
        source
            .current()
            .approvals
            .pause_auto_when_spot_checks_waiting
    };
    assert_eq!(limit(2, 0), 2);
    assert_eq!(limit(0, 2), 2);
}

#[tokio::test]
async fn a_mode_tightened_mid_run_caps_the_override() {
    let (file, reload) = reloadable();
    let fakes = Fakes {
        reload,
        ..Fakes::default()
    };
    let h = driver_with(auto_merge(), fakes);
    let mut shift_tab = point(ApprovalKind::Merge);
    shift_tab.mode_override = Some(ApprovalMode::Auto);

    *file.lock().unwrap() = Some(auto_merge());
    let outcome = h
        .policy
        .evaluate(&shift_tab, &merge(Verdict::Approve))
        .await;
    assert_outcome(&outcome, Expect::Decided(Verdict::Approve), "unchanged");

    let auto_preview = merge_settings(ApprovalMode::AutoPreview, DecideWith::Rules, Otherwise::Ask);
    *file.lock().unwrap() = Some(auto_preview);
    let outcome = h
        .policy
        .evaluate(&shift_tab, &merge(Verdict::Approve))
        .await;
    assert_outcome(&outcome, Expect::Would(Verdict::Approve), "auto_preview");
    assert_eq!(h.autonomy.auto_previews().len(), 1);

    let supervised = merge_settings(ApprovalMode::Supervised, DecideWith::Rules, Otherwise::Ask);
    *file.lock().unwrap() = Some(supervised.clone());
    let outcome = h
        .policy
        .evaluate(&shift_tab, &merge(Verdict::Approve))
        .await;
    assert_eq!(outcome, asks());

    // An unchanged file still lets Shift+Tab raise the mode.
    let (file, reload) = reloadable();
    let fakes = Fakes {
        reload,
        ..Fakes::default()
    };
    let h = driver_with(supervised.clone(), fakes);
    *file.lock().unwrap() = Some(supervised);
    let outcome = h
        .policy
        .evaluate(&shift_tab, &merge(Verdict::Approve))
        .await;
    assert_outcome(&outcome, Expect::Decided(Verdict::Approve), "raised");
}

/// Auto for `kind`, decided with `decide_with`, and a rejection would stop the run.
fn auto_kind(kind: ApprovalKind, decide_with: DecideWith) -> Settings {
    fn set(table: &mut ApprovalPointSettings, decide_with: DecideWith) {
        table.mode = ApprovalMode::Auto;
        table.decide_with = decide_with;
        table.otherwise = Otherwise::Stop;
    }
    let mut settings = Settings::default();
    let approvals = &mut settings.approvals;
    match kind {
        ApprovalKind::Understand => set(&mut approvals.understand, decide_with),
        ApprovalKind::Limit => set(&mut approvals.limit, decide_with),
        ApprovalKind::Stuck => set(&mut approvals.stuck, decide_with),
        ApprovalKind::ChangeRequest => set(&mut approvals.change_request, decide_with),
        ApprovalKind::WorkflowPromotion => {
            let mut table = approvals.workflow_promotion.as_approval();
            set(&mut table, decide_with);
            approvals.workflow_promotion = table.into();
        }
        other => panic!("no table for {other:?}"),
    }
    settings
}

#[tokio::test]
async fn no_judge_model_waives_what_is_always_yours() {
    use ApprovalKind::{ChangeRequest, Limit, Stuck, Understand, WorkflowPromotion};
    let open = |open_questions| EvidenceFacts {
        open_questions,
        ..EvidenceFacts::default()
    };
    let none = EvidenceFacts::default;
    // A name, the kind, a change to its settings, and the facts.
    type Case = (&'static str, ApprovalKind, fn(&mut Settings), EvidenceFacts);
    let cases: [Case; 10] = [
        (
            "understand with 2 open questions",
            Understand,
            |_| {},
            open(Some(2)),
        ),
        ("understand with no count", Understand, |_| {}, open(None)),
        (
            "limit without use_reserve",
            Limit,
            |s| s.approvals.rules.limit.use_reserve = false,
            none(),
        ),
        (
            "limit with use_reserve",
            Limit,
            |s| s.approvals.rules.limit.use_reserve = true,
            none(),
        ),
        (
            "a stuck task with no extra try",
            Stuck,
            |s| s.approvals.rules.stuck.max_extra_tries = 0,
            none(),
        ),
        (
            "a stuck task with extra tries",
            Stuck,
            |s| s.approvals.rules.stuck.max_extra_tries = 3,
            none(),
        ),
        (
            "promotion with no change class",
            WorkflowPromotion,
            |s| s.approvals.rules.workflow_promotion.change_classes.clear(),
            none(),
        ),
        (
            "promotion with change classes",
            WorkflowPromotion,
            |_| {},
            none(),
        ),
        ("change request with no fact", ChangeRequest, |_| {}, none()),
        (
            "irreversible change request",
            ChangeRequest,
            |_| {},
            EvidenceFacts {
                irreversible_actions: Some(vec!["deploy".to_owned()]),
                ..EvidenceFacts::default()
            },
        ),
    ];
    for (name, kind, tweak, facts) in cases {
        for decide_with in [DecideWith::Model, DecideWith::RulesOrModel] {
            for model in [Verdict::Approve, Verdict::Reject] {
                let case = format!("{name}, {decide_with:?}, model says {model:?}");
                let mut settings = auto_kind(kind, decide_with);
                tweak(&mut settings);
                let fakes = Fakes {
                    judge: FakeJudge::answering(model),
                    ..Fakes::default()
                };
                let h = driver_with(settings, fakes);
                let outcome = h
                    .policy
                    .evaluate(&point(kind), &evidence(kind, facts.clone()))
                    .await;
                assert_outcome(&outcome, Expect::Would(Verdict::AskYou), &case);
                assert!(last_reason(&outcome).is_some(), "{case}");
                assert_eq!(h.judge.calls(), 0, "{case}");
            }
        }
    }

    // With no open question the model decides the goal, but its rejection asks, since stopping
    // the run over a goal is yours to call.
    let no_questions = || evidence(Understand, open(Some(0)));
    let cases = [
        (
            DecideWith::Model,
            Verdict::Approve,
            Expect::Decided(Verdict::Approve),
            1,
        ),
        (
            DecideWith::Model,
            Verdict::Reject,
            Expect::Would(Verdict::Reject),
            1,
        ),
        (
            DecideWith::RulesOrModel,
            Verdict::Reject,
            Expect::Decided(Verdict::Approve),
            0,
        ),
        (
            DecideWith::RulesAndModel,
            Verdict::Reject,
            Expect::Would(Verdict::Reject),
            1,
        ),
    ];
    for (decide_with, model, expect, calls) in cases {
        let case = format!("understand, no open question, {decide_with:?}, model says {model:?}");
        let fakes = Fakes {
            judge: FakeJudge::answering(model),
            ..Fakes::default()
        };
        let h = driver_with(auto_kind(Understand, decide_with), fakes);
        let outcome = h.policy.evaluate(&point(Understand), &no_questions()).await;
        assert_outcome(&outcome, expect, &case);
        assert_eq!(h.judge.calls(), calls, "{case}");
    }
}

#[tokio::test]
async fn auto_that_does_not_decide_says_why() {
    let settings = merge_settings(ApprovalMode::Auto, DecideWith::Model, Otherwise::Stop);
    let fakes = Fakes {
        judge: FakeJudge::answering(Verdict::AskYou),
        ..Fakes::default()
    };
    let outcome = driver_with(settings, fakes)
        .policy
        .evaluate(&point(ApprovalKind::Merge), &merge(Verdict::Approve))
        .await;
    let want = AutoPreviewRecord {
        would: Verdict::AskYou,
        by: DecideWith::Model,
        reasons: vec![
            "the model says AskYou".to_owned(),
            "asked GPT 5.6 Luna on Codex to approve it".to_owned(),
        ],
    };
    assert_eq!(
        outcome,
        PolicyOutcome::AskUser {
            violations: Vec::new(),
            auto_preview: Some(want),
        }
    );

    // Records that lowered auto to supervised, such as broken ones or `auto-off`, say so.
    let why = "the approval records couldn't take a spot check: disk full";
    let h = driver(auto_merge());
    *h.autonomy.effective.lock().unwrap() = Some(ApprovalMode::Supervised);
    *h.autonomy.paused.lock().unwrap() = Some(why.to_owned());
    let outcome = h
        .policy
        .evaluate(&point(ApprovalKind::Merge), &merge(Verdict::Approve))
        .await;
    assert_outcome(&outcome, Expect::Would(Verdict::AskYou), "lowered");
    assert_eq!(last_reason(&outcome), Some(why.to_owned()));
    assert_eq!(h.judge.calls(), 0);

    // An approval you set to supervised just asks.
    let supervised = merge_settings(ApprovalMode::Supervised, DecideWith::Rules, Otherwise::Ask);
    let h = driver(supervised);
    *h.autonomy.paused.lock().unwrap() = Some(why.to_owned());
    let outcome = h
        .policy
        .evaluate(&point(ApprovalKind::Merge), &merge(Verdict::Approve))
        .await;
    assert_eq!(outcome, asks());
}

/// A plan nobody judged, as a workflow the main agent picked runs: within the limit, nothing
/// flagged.
fn unjudged_plan(protected: &[&str]) -> EvidencePacket {
    evidence(
        ApprovalKind::Plan,
        EvidenceFacts {
            estimate_usd: Some(1.0),
            protected_paths: protected.iter().map(|path| (*path).to_owned()).collect(),
            ..EvidenceFacts::default()
        },
    )
}

/// The review's probe: in auto the main agent answers the crew's questions, yet your plan rules
/// still hold. A plan with no judging facts asks, by your rules alone too, and so does a close
/// call or a partly priced spend under a dollar limit. An uncontested plan's facts, which a
/// workflow the main agent picks without drafting carries, go through.
#[tokio::test]
async fn the_plan_rules_hold_when_the_main_agent_answers() {
    for decide_with in [DecideWith::RulesAndModel, DecideWith::Rules] {
        let mut settings = Settings::default();
        settings.approvals.plan.decide_with = decide_with;
        let h = driver(settings);
        let outcome = h
            .policy
            .evaluate(&point(ApprovalKind::Plan), &unjudged_plan(&[]))
            .await;
        assert_outcome(
            &outcome,
            Expect::Would(Verdict::AskYou),
            &format!("unjudged, {decide_with:?}"),
        );
        if decide_with == DecideWith::Rules {
            assert_eq!(h.judge.calls(), 0, "your rules alone never ask the judge");
        }
    }

    let uncontested = EvidenceFacts {
        open_questions: Some(0),
        max_score_spread: Some(0.0),
        winner_margin: Some(10.0),
        judges_other_provider: Some(true),
        disputes_settled: Some(true),
        flagged_plans: Some(0),
        estimate_usd: Some(1.0),
        ..EvidenceFacts::default()
    };
    let h = driver(Settings::default());
    let outcome = h
        .policy
        .evaluate(
            &point(ApprovalKind::Plan),
            &evidence(ApprovalKind::Plan, uncontested.clone()),
        )
        .await;
    assert!(
        matches!(
            &outcome,
            PolicyOutcome::Decided { verdict: Verdict::Approve, judge: Some(model), .. }
                if *model == judge_model()
        ),
        "{outcome:?}"
    );

    let close_call = EvidenceFacts {
        winner_margin: Some(0.1),
        ..uncontested.clone()
    };
    let outcome = driver(Settings::default())
        .policy
        .evaluate(
            &point(ApprovalKind::Plan),
            &evidence(ApprovalKind::Plan, close_call),
        )
        .await;
    assert!(
        matches!(outcome, PolicyOutcome::AskUser { .. }),
        "a near tie asks: {outcome:?}"
    );

    let mut dollars = Settings::default();
    dollars.limits.spend.spend_per_run = Some(20.0);
    let mut partly_priced = uncontested;
    partly_priced.spent.cost_partial = true;
    let outcome = driver(dollars)
        .policy
        .evaluate(
            &point(ApprovalKind::Plan),
            &evidence(ApprovalKind::Plan, partly_priced),
        )
        .await;
    assert_outcome(
        &outcome,
        Expect::Would(Verdict::AskYou),
        "a partly priced spend under a dollar limit",
    );
}

#[tokio::test]
async fn when_rules_approve_and_the_model_does_not_its_reasons_come_first() {
    let mut settings = Settings::default();
    settings.approvals.answers = Some(Answerer::You);
    let fakes = Fakes {
        judge: FakeJudge::answering(Verdict::AskYou),
        ..Fakes::default()
    };
    let facts = EvidenceFacts {
        open_questions: Some(0),
        ..EvidenceFacts::default()
    };
    let outcome = driver_with(settings, fakes)
        .policy
        .evaluate(
            &point(ApprovalKind::Understand),
            &evidence(ApprovalKind::Understand, facts),
        )
        .await;
    let PolicyOutcome::AskUser {
        auto_preview: Some(auto_preview),
        ..
    } = &outcome
    else {
        panic!("the model needs you: {outcome:?}");
    };
    assert_eq!(auto_preview.reasons[0], "the model says AskYou");
}

#[test]
fn who_answers_takes_the_stricter_side() {
    let pick = |a: Option<Answerer>, b: Option<Answerer>| {
        let mut x = Settings::default();
        let mut y = Settings::default();
        x.approvals.answers = a;
        y.approvals.answers = b;
        stricter(&x, &y).approvals.answers
    };
    use Answerer::{Main, You};
    assert_eq!(pick(Some(Main), Some(You)), Some(You));
    assert_eq!(pick(None, Some(You)), Some(You));
    assert_eq!(pick(Some(Main), None), None);
    assert_eq!(pick(Some(Main), Some(Main)), Some(Main));
    assert_eq!(pick(None, None), None);
    // A model you picked holds while both name it; two different ones follow the mode.
    let glm = Answerer::Model("openrouter/z-ai/glm-5".parse().expect("a model"));
    let gpt = Answerer::Model("codex/gpt-x".parse().expect("a model"));
    assert_eq!(
        pick(Some(glm.clone()), Some(glm.clone())),
        Some(glm.clone())
    );
    assert_eq!(pick(Some(glm.clone()), Some(gpt)), None);
    assert_eq!(pick(Some(glm.clone()), Some(Main)), None);
    assert_eq!(pick(Some(glm), Some(You)), Some(You));
}

/// Autopilot everywhere holds only while both say so, and two different hard-limit choices
/// stop safely, or wait when either side waits.
#[test]
fn the_autopilot_switch_and_hard_limits_take_the_stricter_side() {
    use argonauts_core::settings::approvals::AtHardLimit;
    let mut a = Settings::default();
    let mut b = Settings::default();
    a.approvals.autopilot = true;
    assert!(!stricter(&a, &b).approvals.autopilot);
    b.approvals.autopilot = true;
    assert!(stricter(&a, &b).approvals.autopilot);

    a.approvals.merge.at_hard_limit = AtHardLimit::OpenPullRequest;
    b.approvals.merge.at_hard_limit = AtHardLimit::OpenPullRequest;
    assert_eq!(
        stricter(&a, &b).approvals.merge.at_hard_limit,
        AtHardLimit::OpenPullRequest
    );
    // Changed mid-run, the run stops safely at a hard limit rather than wait for you, unless
    // either side leaves it to you.
    b.approvals.merge.at_hard_limit = AtHardLimit::SendBack;
    assert_eq!(
        stricter(&a, &b).approvals.merge.at_hard_limit,
        AtHardLimit::Stop
    );
    a.approvals.merge.at_hard_limit = AtHardLimit::Stop;
    assert_eq!(
        stricter(&a, &b).approvals.merge.at_hard_limit,
        AtHardLimit::Stop
    );
    b.approvals.merge.at_hard_limit = AtHardLimit::Wait;
    assert_eq!(
        stricter(&a, &b).approvals.merge.at_hard_limit,
        AtHardLimit::Wait
    );
}

#[test]
fn stricter_takes_the_tighter_side_of_every_key() {
    use DecideWith::{Model, Rules, RulesAndModel, RulesOrModel};
    let pick = |a: DecideWith, b: DecideWith| {
        let mut x = Settings::default();
        let mut y = Settings::default();
        x.approvals.plan.decide_with = a;
        y.approvals.plan.decide_with = b;
        stricter(&x, &y).approvals.plan.decide_with
    };
    assert_eq!(pick(Rules, Model), RulesAndModel);
    assert_eq!(pick(Model, Rules), RulesAndModel);
    assert_eq!(pick(RulesOrModel, Rules), Rules);
    assert_eq!(pick(Model, RulesOrModel), Model);
    assert_eq!(pick(RulesAndModel, RulesOrModel), RulesAndModel);
    assert_eq!(pick(RulesOrModel, RulesOrModel), RulesOrModel);

    let mut a = Settings::default();
    let mut b = Settings::default();
    a.approvals.understand.mode = ApprovalMode::Auto;
    b.approvals.understand.mode = ApprovalMode::AutoPreview;
    a.approvals.merge.otherwise = Otherwise::Stop;
    b.approvals.merge.otherwise = Otherwise::SendBack;
    a.approvals.plan.otherwise = Otherwise::Stop;
    b.approvals.plan.otherwise = Otherwise::Stop;
    a.approvals.goal_changes = GoalChanges::AllowClarifications;
    b.approvals.goal_changes = GoalChanges::Deny;
    a.approvals.presence = Presence::Off;
    a.approvals.spot_check_sample = 0.05;
    b.approvals.pause_auto_when_spot_checks_waiting = 2;
    b.approvals.suggest_auto_after_matches = 40;
    a.approvals.rules.plan.max_score_spread = 0.5;
    b.approvals.rules.plan.min_winner_margin = 3.0;
    b.approvals.rules.merge.max_changed_lines = 200;
    a.approvals.rules.merge.allow_debt = true;
    b.approvals.rules.limit.use_reserve = false;
    b.approvals.rules.stuck.max_extra_tries = 0;
    b.approvals.rules.workflow_promotion.change_classes = vec!["checklist_item".to_owned()];
    b.approvals.rules.workflow_promotion.min_days_between = 30;
    b.approvals
        .refuse_ancestor_env
        .push("OTHER_AGENT".to_owned());
    a.approvals.decides = judge_model().into();
    b.approvals.decides = ModelRef::new("claude", None).into();
    a.approvals.judge.different_provider = false;
    b.approvals.judge.second_model = Some(ModelRef::new("agy", None));
    a.limits.protected_paths.push("migrations/**".to_owned());
    a.limits.dependency_allowlist = vec!["serde".to_owned(), "rand".to_owned()];
    b.limits.dependency_allowlist = vec!["serde".to_owned()];
    a.limits.spend_per_day = None;
    b.limits.spend_per_day = Some(10.0);
    b.limits.auto_merges_per_day = Some(1);
    a.limits.irreversible_allowlist = vec!["publish".to_owned()];
    a.sandbox.mode = SandboxChoice::Tool;
    b.sandbox.mode = SandboxChoice::Os;
    a.sandbox.network = true;
    b.sandbox.network = false;
    b.sandbox.agents_commit = false;
    a.sandbox.agents_see_other_worktrees = true;
    b.limits.spend.spend_per_run = None;
    a.limits.spend.spend_per_run = Some(5.0);

    let s = stricter(&a, &b);
    let ap = &s.approvals;
    assert_eq!(ap.understand.mode, ApprovalMode::AutoPreview);
    assert_eq!(ap.merge.otherwise, Otherwise::Ask);
    assert_eq!(ap.plan.otherwise, Otherwise::Stop);
    assert_eq!(ap.goal_changes, GoalChanges::Deny);
    assert_eq!(ap.presence, Presence::Warn);
    assert_eq!(ap.spot_check_sample, 0.1);
    assert_eq!(ap.pause_auto_when_spot_checks_waiting, 2);
    assert_eq!(ap.suggest_auto_after_matches, 40);
    assert_eq!(ap.rules.plan.max_score_spread, 0.5);
    assert_eq!(ap.rules.plan.min_winner_margin, 3.0);
    assert_eq!(ap.rules.merge.max_changed_lines, 200);
    assert_eq!(ap.rules.merge.min_test_coverage_of_changed_lines, 0.8);
    assert!(!ap.rules.merge.allow_debt);
    assert!(!ap.rules.limit.use_reserve);
    assert_eq!(ap.rules.stuck.max_extra_tries, 0);
    assert_eq!(
        ap.rules.workflow_promotion.change_classes,
        ["checklist_item"]
    );
    assert_eq!(ap.rules.workflow_promotion.min_days_between, 30);
    assert!(ap.refuse_ancestor_env.contains(&"OTHER_AGENT".to_owned()));
    assert!(
        ap.refuse_ancestor_env
            .contains(&"OPENCLAW_SHELL".to_owned())
    );
    assert_eq!(ap.decides, Decides::Judge);
    assert!(ap.judge.different_provider);
    assert_eq!(ap.judge.second_model, Some(ModelRef::new("agy", None)));
    assert!(
        s.limits
            .protected_paths
            .contains(&"migrations/**".to_owned())
    );
    assert!(s.limits.protected_paths.contains(&".github/**".to_owned()));
    assert_eq!(s.limits.dependency_allowlist, ["serde"]);
    assert_eq!(s.limits.spend_per_day, Some(10.0));
    assert_eq!(s.limits.auto_merges_per_day, Some(1));
    assert!(s.limits.irreversible_allowlist.is_empty());
    assert_eq!(s.sandbox.mode, SandboxChoice::Os);
    assert!(!s.sandbox.network);
    assert!(!s.sandbox.agents_commit);
    assert!(!s.sandbox.agents_see_other_worktrees);
    assert_eq!(s.limits.spend.spend_per_run, Some(5.0));

    let mode = |mode| {
        let mut settings = Settings::default();
        settings.sandbox.mode = mode;
        settings
    };
    let (none, tool, os) = (
        mode(SandboxChoice::None),
        mode(SandboxChoice::Tool),
        mode(SandboxChoice::Os),
    );
    assert_eq!(stricter(&tool, &tool).sandbox.mode, SandboxChoice::Tool);
    assert_eq!(stricter(&os, &tool).sandbox.mode, SandboxChoice::Os);
    assert_eq!(stricter(&tool, &none).sandbox.mode, SandboxChoice::Tool);
    assert_eq!(stricter(&none, &tool).sandbox.mode, SandboxChoice::Tool);
    let mut ask = Settings::default();
    ask.approvals.goal_changes = GoalChanges::Ask;
    let mut deny = Settings::default();
    deny.approvals.goal_changes = GoalChanges::Deny;
    assert_eq!(
        stricter(&deny, &ask).approvals.goal_changes,
        GoalChanges::Ask
    );

    // 0 never pauses, so it's the loosest limit.
    let pause = |run: u32, now: u32| {
        let mut x = Settings::default();
        let mut y = Settings::default();
        x.approvals.pause_auto_when_spot_checks_waiting = run;
        y.approvals.pause_auto_when_spot_checks_waiting = now;
        stricter(&x, &y)
            .approvals
            .pause_auto_when_spot_checks_waiting
    };
    assert_eq!(pause(5, 0), 5);
    assert_eq!(pause(0, 5), 5);
    assert_eq!(pause(0, 0), 0);
    assert_eq!(pause(2, 5), 2);

    // Removing or replacing the judge stops it at once.
    let model = |run: Decides, now: Decides| {
        let mut x = Settings::default();
        let mut y = Settings::default();
        x.approvals.decides = run;
        y.approvals.decides = now;
        stricter(&x, &y).approvals.decides
    };
    let codex = Decides::Model(judge_model());
    assert_eq!(model(codex.clone(), codex.clone()), codex);
    assert_eq!(model(codex.clone(), Decides::Judge), Decides::Judge);
    assert_eq!(model(Decides::Judge, codex), Decides::Judge);
}

/// Approvable merge evidence, but for the tasks whose quality debt no strong enough model
/// reviewed. `None` means nobody measured it.
fn indebted(debt: Option<&[&str]>) -> EvidencePacket {
    let facts = EvidenceFacts {
        unreviewed_debt: debt.map(|tasks| tasks.iter().map(|task| (*task).to_owned()).collect()),
        ..merge_facts(Verdict::Approve)
    };
    evidence(ApprovalKind::Merge, facts)
}

#[tokio::test]
async fn allow_debt_lets_auto_merge_debt() {
    let merge_point = point(ApprovalKind::Merge);
    let debt = indebted(Some(&["task-03"]));

    // Off, debt asks you whoever decides, and no judge is started for it.
    for decide_with in [
        DecideWith::Rules,
        DecideWith::Model,
        DecideWith::RulesAndModel,
        DecideWith::RulesOrModel,
    ] {
        for otherwise in [Otherwise::Ask, Otherwise::SendBack, Otherwise::Stop] {
            let h = driver(merge_settings(ApprovalMode::Auto, decide_with, otherwise));
            let case = format!("{decide_with:?} {otherwise:?}");
            let outcome = h.policy.evaluate(&merge_point, &debt).await;
            assert_outcome(&outcome, Expect::Would(Verdict::AskYou), &case);
            assert!(last_reason(&outcome).is_some_and(|why| why.contains("task-03")));
            let outcome = h.policy.evaluate(&merge_point, &indebted(None)).await;
            let unmeasured = format!("{case}, debt not measured");
            assert_outcome(&outcome, Expect::Would(Verdict::AskYou), &unmeasured);
            assert_eq!(h.judge.calls(), 0, "{case}");
        }
    }

    // Auto preview records that auto would have asked.
    let h = driver(merge_settings(
        ApprovalMode::AutoPreview,
        DecideWith::Model,
        Otherwise::Ask,
    ));
    let outcome = h.policy.evaluate(&merge_point, &debt).await;
    assert_outcome(&outcome, Expect::Would(Verdict::AskYou), "auto_preview");
    assert!(h.autonomy.auto_previews()[0].reasons[0].contains("task-03"));

    // On, auto merges it.
    let (file, reload) = reloadable();
    let fakes = Fakes {
        reload,
        ..Fakes::default()
    };
    let mut lenient = auto_merge();
    lenient.approvals.rules.merge.allow_debt = true;
    let h = driver_with(lenient.clone(), fakes);
    let outcome = h.policy.evaluate(&merge_point, &debt).await;
    assert_outcome(&outcome, Expect::Decided(Verdict::Approve), "allowed");
    let outcome = h.policy.evaluate(&merge_point, &indebted(None)).await;
    assert_outcome(
        &outcome,
        Expect::Decided(Verdict::Approve),
        "allowed, not measured",
    );

    // Turning it off mid-run applies at the next approval.
    *file.lock().unwrap() = Some(auto_merge());
    let outcome = h.policy.evaluate(&merge_point, &debt).await;
    assert_outcome(&outcome, Expect::Would(Verdict::AskYou), "turned off");
    let outcome = h.policy.evaluate(&merge_point, &indebted(Some(&[]))).await;
    assert_outcome(&outcome, Expect::Decided(Verdict::Approve), "no debt");

    // Turning it on mid-run waits for the next run.
    let fakes = Fakes {
        reload: Arc::new(move || Some(lenient.clone())),
        ..Fakes::default()
    };
    let h = driver_with(auto_merge(), fakes);
    let outcome = h.policy.evaluate(&merge_point, &debt).await;
    assert_outcome(
        &outcome,
        Expect::Would(Verdict::AskYou),
        "turned on mid-run",
    );
}

fn user_approved(point: DecisionPoint) -> UserDecision {
    UserDecision {
        point,
        action: AnswerAction::Merge,
        edited: false,
        auto_preview: None,
    }
}

#[tokio::test]
async fn user_decided_before_evaluate_finishes() {
    let check = Arc::new(Notify::new());
    let fakes = Fakes {
        judge: FakeJudge {
            check: Some(check.clone()),
            ..FakeJudge::answering(Verdict::Approve)
        },
        ..Fakes::default()
    };
    let settings = merge_settings(ApprovalMode::Auto, DecideWith::Model, Otherwise::Ask);
    let h = driver_with(settings, fakes);
    let merge_point = point(ApprovalKind::Merge);
    let packet = merge(Verdict::Approve);

    // Your answer lands while the judge is still thinking.
    let (outcome, ()) = tokio::join!(h.policy.evaluate(&merge_point, &packet), async {
        h.policy.user_decided(&user_approved(merge_point.clone()));
        check.notify_one();
    });

    let want = AutoPreviewRecord {
        would: Verdict::Approve,
        by: DecideWith::Model,
        reasons: vec![
            "the model says Approve".to_owned(),
            "asked GPT 5.6 Luna on Codex to approve it".to_owned(),
        ],
    };
    assert_eq!(
        outcome,
        PolicyOutcome::AskUser {
            violations: Vec::new(),
            auto_preview: Some(want.clone()),
        }
    );
    assert_eq!(
        h.autonomy.log(),
        ["user_decided", "reserve", "note_auto_preview"]
    );
    assert_eq!(h.autonomy.auto_previews(), [want]);
    assert_eq!(h.judge.calls(), 1);
}

#[tokio::test]
async fn evaluate_finishes_before_user_decided() {
    let h = driver(auto_merge());
    let merge_point = point(ApprovalKind::Merge);
    let outcome = h
        .policy
        .evaluate(&merge_point, &merge(Verdict::Approve))
        .await;
    assert_outcome(&outcome, Expect::Decided(Verdict::Approve), "first");

    // Your late answer still reaches the records, which give back what auto reserved.
    h.policy.user_decided(&user_approved(merge_point.clone()));
    assert_eq!(h.autonomy.log(), ["reserve", "user_decided"]);
    assert!(h.autonomy.auto_previews().is_empty());

    // A second evaluation of the same approval now only records an auto preview.
    let again = h
        .policy
        .evaluate(&merge_point, &merge(Verdict::Approve))
        .await;
    assert_outcome(&again, Expect::Would(Verdict::Approve), "again");
}
