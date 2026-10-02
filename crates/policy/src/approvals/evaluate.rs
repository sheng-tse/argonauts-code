//! The decision for one checkpoint. It runs beside the engine's loop and the first decision
//! wins, so nothing here may depend on this outcome being the one that counts.

use std::sync::Arc;

use argonauts_core::events::checkpoint::CheckpointKind;
use argonauts_core::policy::{
    DecisionPoint, EvidencePacket, PolicyOutcome, ShadowRecord, Verdict, Violation,
};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::{
    ApprovalMode, CheckpointSettings, DecideWith, GoalChanges, Otherwise, Presence,
};
use tokio::runtime::Handle;

use super::decide::{Decision, decide};
use super::settings::less_autonomous;
use crate::parts::{Autonomy, PolicyParts, Refusal};
use crate::rules;
use crate::rules::merge::debt_blocks;

const SHUTTING_DOWN: &str = "the runtime is shutting down";

pub async fn evaluate(
    parts: &PolicyParts,
    point: &DecisionPoint,
    evidence: &EvidencePacket,
) -> PolicyOutcome {
    let settings = parts.source.current();
    let violations = parts.limits.check_point(point, evidence);
    if !violations.is_empty() {
        return ask_with(violations);
    }
    let Some(route) = route(point.kind, &settings) else {
        return ask();
    };

    let run_start = route_mode(point.kind, parts.source.run_start());
    let requested = requested_mode(point, route.mode, run_start);
    let effective = {
        let point = point.clone();
        off_the_engine(parts, ApprovalMode::Supervised, move |autonomy| {
            autonomy.effective_mode(&point, requested)
        })
        .await
    };
    // The records may only lower a mode, and a mistake there must not raise one.
    let mut mode = less_autonomous(requested, effective);
    if needs_presence(point.kind, &settings) {
        mode = ApprovalMode::Supervised;
    }

    match mode {
        ApprovalMode::Supervised => {
            // Broken records and `auto-off` lower every mode, so say which, or auto quietly stops.
            let lowered =
                requested != ApprovalMode::Supervised && effective == ApprovalMode::Supervised;
            let why = if lowered {
                auto_paused(parts, &settings).await
            } else {
                None
            };
            match why {
                Some(why) => ask_about(route.decide_with, why),
                None => ask(),
            }
        }
        ApprovalMode::Shadow => {
            let decision = decide_point(parts, point, evidence, &settings, route.decide_with).await;
            shadow(parts, point, evidence, &settings, &decision, None).await
        }
        ApprovalMode::Auto => {
            let decision = decide_point(parts, point, evidence, &settings, route.decide_with).await;
            auto(parts, point, evidence, &settings, route.on_reject, decision).await
        }
    }
}

/// Runs the autonomy records' file work on the blocking pool, since a lock held elsewhere or a
/// slow disk must not stall the engine. `fail_closed` stands in if the runtime shuts down first.
async fn off_the_engine<T, F>(parts: &PolicyParts, fail_closed: T, work: F) -> T
where
    T: Send + 'static,
    F: FnOnce(&dyn Autonomy) -> T + Send + 'static,
{
    let autonomy = Arc::clone(&parts.autonomy);
    if Handle::try_current().is_err() {
        // Polled outside a Tokio runtime, where there's no pool to move to.
        return work(&*autonomy);
    }
    match tokio::task::spawn_blocking(move || work(&*autonomy)).await {
        Ok(value) => value,
        Err(err) => match err.try_into_panic() {
            Ok(panic) => std::panic::resume_unwind(panic),
            Err(_) => fail_closed,
        },
    }
}

/// Why auto can't approve right now, if it can't. A runtime shutting down counts as a reason.
async fn auto_paused(parts: &PolicyParts, settings: &Arc<Settings>) -> Option<String> {
    let settings = Arc::clone(settings);
    let stopped = Some(SHUTTING_DOWN.to_owned());
    off_the_engine(parts, stopped, move |autonomy| {
        autonomy.auto_paused(&settings)
    })
    .await
}

/// How one kind of checkpoint is decided.
struct Route {
    mode: ApprovalMode,
    decide_with: DecideWith,
    on_reject: OnReject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnReject {
    Ask,
    /// Return the rejection; the engine applies `otherwise`.
    Decide,
}

/// `None` for kinds that always ask you.
fn route(kind: CheckpointKind, settings: &Settings) -> Option<Route> {
    let approvals = &settings.approvals;
    match kind {
        CheckpointKind::Goal => Some(configured(kind, &approvals.goal)),
        CheckpointKind::Plan => Some(configured(kind, &approvals.plan)),
        CheckpointKind::Merge => Some(configured(kind, &approvals.merge)),
        CheckpointKind::Escalation => Some(configured(kind, &approvals.escalation)),
        CheckpointKind::Budget => Some(configured(kind, &approvals.budget)),
        CheckpointKind::RecipePromotion => Some(configured(kind, &approvals.recipe_promotion)),
        CheckpointKind::ChangeRequest => Some(configured(kind, &approvals.change_request)),
        // `deny` refuses the change itself, so a rejection is decided here, not by `otherwise`.
        CheckpointKind::GoalChange => match approvals.goal_changes {
            GoalChanges::Ask => None,
            GoalChanges::Deny | GoalChanges::AllowClarifications => Some(Route {
                mode: ApprovalMode::Auto,
                decide_with: DecideWith::Rules,
                on_reject: OnReject::Decide,
            }),
        },
        CheckpointKind::Handoff
        | CheckpointKind::VendorDown
        | CheckpointKind::TakeOverReturn
        | CheckpointKind::Question
        | CheckpointKind::OriginalReturned => None,
    }
}

/// Only a rejected plan or merge is decided by `otherwise`. Sending back a goal, an extra
/// attempt, a reserve, or a promotion, or stopping the run over one, would cancel work that's
/// yours to call, so those ask.
fn configured<R: Default>(kind: CheckpointKind, table: &CheckpointSettings<R>) -> Route {
    let on_reject = match (kind, table.otherwise) {
        (_, Otherwise::Ask) => OnReject::Ask,
        (CheckpointKind::Plan, Otherwise::SendBack | Otherwise::Stop) => OnReject::Decide,
        (CheckpointKind::Merge, _) => OnReject::Decide,
        _ => OnReject::Ask,
    };
    Route {
        mode: table.mode,
        decide_with: table.decide_with,
        on_reject,
    }
}

fn route_mode(kind: CheckpointKind, settings: &Settings) -> Option<ApprovalMode> {
    route(kind, settings).map(|route| route.mode)
}

/// Shift+Tab's mode replaces the file's, except that a mode the file tightened mid-run still
/// applies.
fn requested_mode(
    point: &DecisionPoint,
    now: ApprovalMode,
    run_start: Option<ApprovalMode>,
) -> ApprovalMode {
    let Some(chosen) = point.mode_override else {
        return now;
    };
    // The stricter-of merge only ever lowers a mode, so any change from run start tightened it.
    if run_start.is_some_and(|start| start != now) {
        less_autonomous(chosen, now)
    } else {
        chosen
    }
}

/// With `presence = required`, plan and merge need a passkey no one can give yet, so they
/// never decide on their own.
fn needs_presence(kind: CheckpointKind, settings: &Settings) -> bool {
    settings.approvals.presence == Presence::Required
        && matches!(kind, CheckpointKind::Plan | CheckpointKind::Merge)
}

/// The goal, budget, escalation, change request, and promotion rules hold conditions no
/// approver model may waive, such as an open question at the goal, `use_reserve = false`, or a
/// change that can't be undone. So those rules must approve before `decide_with` is asked at
/// all.
fn binding_rules(kind: CheckpointKind) -> bool {
    matches!(
        kind,
        CheckpointKind::Goal
            | CheckpointKind::Budget
            | CheckpointKind::Escalation
            | CheckpointKind::ChangeRequest
            | CheckpointKind::RecipePromotion
    )
}

async fn decide_point(
    parts: &PolicyParts,
    point: &DecisionPoint,
    evidence: &EvidencePacket,
    settings: &Settings,
    decide_with: DecideWith,
) -> Decision {
    // Merging debt anyway is yours unless `allow_debt` is set, so no approver model and no
    // `rules_or_model` may take that call.
    if let Some(why) = debt_blocks_merge(point, evidence, settings) {
        return Decision {
            verdict: Verdict::Escalate,
            by: decide_with,
            approver: None,
            reasons: vec![why],
        };
    }
    let now = parts.clock.now();
    if binding_rules(point.kind) {
        let bound = rules::check(point, evidence, settings, now);
        if bound.verdict != Verdict::Approve {
            return Decision {
                verdict: Verdict::Escalate,
                by: DecideWith::Rules,
                approver: None,
                reasons: bound.reasons,
            };
        }
    }
    decide(
        &*parts.approver,
        point,
        evidence,
        settings,
        decide_with,
        now,
    )
    .await
}

fn debt_blocks_merge(
    point: &DecisionPoint,
    evidence: &EvidencePacket,
    settings: &Settings,
) -> Option<String> {
    if point.kind != CheckpointKind::Merge {
        return None;
    }
    debt_blocks(
        evidence.facts.unreviewed_debt.as_deref(),
        &settings.approvals.merge.rules,
    )
}

/// Notes what auto would decide, so your answer scores it. With `asked_because`, auto would
/// have approved but can't right now, and the reason goes last, so you see why you're asked.
async fn shadow(
    parts: &PolicyParts,
    point: &DecisionPoint,
    evidence: &EvidencePacket,
    settings: &Arc<Settings>,
    decision: &Decision,
    asked_because: Option<String>,
) -> PolicyOutcome {
    let record = record(decision, asked_because);
    let noted = (
        point.clone(),
        record.clone(),
        evidence.clone(),
        Arc::clone(settings),
    );
    off_the_engine(parts, (), move |autonomy| {
        let (point, record, evidence, settings) = noted;
        autonomy.note_shadow(&point, &record, &evidence, &settings);
    })
    .await;
    ask_with_record(Vec::new(), record)
}

async fn auto(
    parts: &PolicyParts,
    point: &DecisionPoint,
    evidence: &EvidencePacket,
    settings: &Arc<Settings>,
    on_reject: OnReject,
    decision: Decision,
) -> PolicyOutcome {
    match decision.verdict {
        Verdict::Approve => {
            // Checked again in `reserve`, under the records lock, since a pause can land while
            // the approver model is deciding.
            if let Some(why) = auto_paused(parts, settings).await {
                return shadow(parts, point, evidence, settings, &decision, Some(why)).await;
            }
            let reserved = {
                let taken = (point.clone(), evidence.clone(), Arc::clone(settings));
                let stopped = Refusal::Unavailable(SHUTTING_DOWN.to_owned());
                off_the_engine(parts, Err(stopped), move |autonomy| {
                    let (point, evidence, settings) = taken;
                    autonomy.reserve(&point, &evidence, &settings)
                })
                .await
            };
            match reserved {
                Ok(reservation) => decided(decision, reservation.spot_check),
                // Your answer came first, so this only scores against it.
                Err(Refusal::AlreadyDecided) => {
                    shadow(parts, point, evidence, settings, &decision, None).await
                }
                Err(Refusal::Capped(violations)) => {
                    ask_with_record(violations, record(&decision, None))
                }
                Err(Refusal::Paused(why) | Refusal::Unavailable(why)) => {
                    shadow(parts, point, evidence, settings, &decision, Some(why)).await
                }
            }
        }
        Verdict::Reject if on_reject == OnReject::Decide => decided(decision, false),
        Verdict::Reject | Verdict::Escalate => ask_with_record(Vec::new(), record(&decision, None)),
    }
}

fn decided(decision: Decision, spot_check: bool) -> PolicyOutcome {
    PolicyOutcome::Decided {
        verdict: decision.verdict,
        by: decision.by,
        approver: decision.approver,
        reasons: decision.reasons,
        spot_check,
    }
}

/// What auto made of the checkpoint, and `why` it asks anyway. The approver is named in the
/// reasons, since the record has no field for it.
fn record(decision: &Decision, why: Option<String>) -> ShadowRecord {
    let mut reasons = decision.reasons.clone();
    if let Some(approver) = &decision.approver {
        reasons.push(format!("asked the approver model {approver}"));
    }
    reasons.extend(why);
    ShadowRecord {
        would: decision.verdict,
        by: decision.by,
        reasons,
    }
}

/// Auto didn't get to decide, and `why` says what stopped it.
fn ask_about(by: DecideWith, why: String) -> PolicyOutcome {
    let record = ShadowRecord {
        would: Verdict::Escalate,
        by,
        reasons: vec![why],
    };
    ask_with_record(Vec::new(), record)
}

fn ask() -> PolicyOutcome {
    ask_with(Vec::new())
}

fn ask_with(violations: Vec<Violation>) -> PolicyOutcome {
    PolicyOutcome::AskUser {
        violations,
        shadow: None,
    }
}

fn ask_with_record(violations: Vec<Violation>, record: ShadowRecord) -> PolicyOutcome {
    PolicyOutcome::AskUser {
        violations,
        shadow: Some(record),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, mpsc};
    use std::time::Duration;

    use argonauts_core::RunId;
    use argonauts_core::events::card::CardSpec;
    use argonauts_core::future::BoxFuture;
    use argonauts_core::names::CheckpointId;
    use argonauts_core::policy::{
        ApprovalsView, EvidenceFacts, HardLimits, ShadowRecord, UserDecision,
    };
    use argonauts_core::worker::SessionSpec;
    use argonauts_testkit::FakeClock;

    use super::*;
    use crate::approvals::settings::SettingsSource;
    use crate::parts::{Approve, Autonomy, ModelVerdict, Reservation};

    struct NoLimits;

    impl HardLimits for NoLimits {
        fn check_point(&self, _: &DecisionPoint, _: &EvidencePacket) -> Vec<Violation> {
            Vec::new()
        }

        fn check_card_diff(&self, _: &CardSpec, _: &str) -> Vec<Violation> {
            Vec::new()
        }

        fn check_session(&self, _: &SessionSpec) -> Vec<Violation> {
            Vec::new()
        }
    }

    struct Approving;

    impl Approve for Approving {
        fn verdict<'a>(
            &'a self,
            _: &'a DecisionPoint,
            _: &'a EvidencePacket,
            _: &'a Settings,
        ) -> BoxFuture<'a, ModelVerdict> {
            Box::pin(async {
                ModelVerdict {
                    verdict: Verdict::Approve,
                    approver: None,
                    reasons: Vec::new(),
                }
            })
        }
    }

    /// Its records wait on a lock that only a task on the engine's thread gives up.
    struct LockedElsewhere {
        released: Mutex<mpsc::Receiver<()>>,
        waited_it_out: AtomicBool,
    }

    impl Autonomy for LockedElsewhere {
        fn effective_mode(&self, _: &DecisionPoint, configured: ApprovalMode) -> ApprovalMode {
            let released = self.released.lock().expect("receiver");
            if released.recv_timeout(Duration::from_secs(2)).is_ok() {
                self.waited_it_out.store(true, Ordering::SeqCst);
            }
            configured
        }

        fn auto_paused(&self, _: &Settings) -> Option<String> {
            None
        }

        fn reserve(
            &self,
            _: &DecisionPoint,
            _: &EvidencePacket,
            _: &Settings,
        ) -> Result<Reservation, Refusal> {
            Ok(Reservation { spot_check: false })
        }

        fn note_shadow(
            &self,
            _: &DecisionPoint,
            _: &ShadowRecord,
            _: &EvidencePacket,
            _: &Settings,
        ) {
        }

        fn user_decided(&self, _: &UserDecision, _: &Settings) {}

        fn spot_checked(&self, _: &DecisionPoint, _: bool, _: &Settings) {}

        fn auto_off(&self) {}

        fn view(&self, _: &Settings) -> ApprovalsView {
            ApprovalsView::default()
        }
    }

    #[tokio::test]
    async fn waiting_records_leave_the_engine_thread_free() {
        let (release, released) = mpsc::channel();
        let autonomy = Arc::new(LockedElsewhere {
            released: Mutex::new(released),
            waited_it_out: AtomicBool::new(false),
        });
        let parts = PolicyParts {
            source: SettingsSource::new(Arc::new(Settings::default()), Arc::new(|| None)),
            limits: Arc::new(NoLimits),
            approver: Arc::new(Approving),
            autonomy: autonomy.clone(),
            clock: Arc::new(FakeClock::default()),
        };
        let point = DecisionPoint {
            run: RunId::new("run-01").expect("run id"),
            checkpoint: CheckpointId::numbered(1),
            kind: CheckpointKind::Goal,
            recipe: None,
            authors: Vec::new(),
            mode_override: None,
        };
        let evidence = EvidencePacket {
            kind: CheckpointKind::Goal,
            goal: None,
            facts: EvidenceFacts::default(),
            agent_outputs: Vec::new(),
            documents: Vec::new(),
        };

        // This test runtime has one thread, so the release runs only if evaluate yields it.
        let engine = tokio::spawn(async move { release.send(()) });
        evaluate(&parts, &point, &evidence).await;
        assert!(autonomy.waited_it_out.load(Ordering::SeqCst));
        engine.await.expect("engine task").expect("release");
    }
}
