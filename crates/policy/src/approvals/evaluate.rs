//! The decision for one approval. It runs beside the engine's loop and the first decision
//! wins, so nothing here may depend on this outcome being the one that counts.

use std::sync::Arc;

use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::approval::ApprovalKind;
use argonauts_core::policy::{
    AutoPreviewRecord, DecisionPoint, EvidencePacket, PolicyOutcome, Verdict, Violation,
};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::{
    ApprovalMode, ApprovalPointSettings, DecideWith, GoalChanges, Otherwise, Presence,
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
    let Some(route) = route(point.kind, &settings).or_else(|| autopilot_route(point, &settings))
    else {
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
    // The records may only lower a mode, and a mistake there must not raise one. Autopilot
    // keeps going where they'd lower auto to auto preview, since nobody answers spot checks there;
    // `auto-off` still stops it.
    let mut mode = match (requested, effective) {
        (ApprovalMode::Autopilot, ApprovalMode::AutoPreview) => ApprovalMode::Autopilot,
        _ => less_autonomous(requested, effective),
    };
    if needs_presence(point.kind, &settings) {
        if mode == ApprovalMode::Autopilot {
            return ask_about(route.decide_with, PRESENCE.to_owned());
        }
        mode = ApprovalMode::Supervised;
    }
    // What decides below, and the records, read the mode this point is decided in.
    let point = &DecisionPoint {
        mode_override: Some(mode),
        ..point.clone()
    };

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
        ApprovalMode::AutoPreview => {
            let decision = decide_point(parts, point, evidence, &settings, route.decide_with).await;
            auto_preview(parts, point, evidence, &settings, &decision, None).await
        }
        // Autopilot decides as auto does. What it can't approve comes back as a question,
        // which the engine ends safely instead of asking, since only it knows the options.
        ApprovalMode::Auto | ApprovalMode::Autopilot => {
            let decision = decide_point(parts, point, evidence, &settings, route.decide_with).await;
            auto(parts, point, evidence, &settings, route.on_reject, decision).await
        }
    }
}

/// Why autopilot leaves a plan or a merge to you with `presence = required`.
const PRESENCE: &str = "your /approvals settings keep it for you at the terminal";

/// Whether this point is decided in autopilot, as `evaluate` set it for what decides below.
pub(crate) fn in_autopilot(point: &DecisionPoint) -> bool {
    point.mode_override == Some(ApprovalMode::Autopilot)
}

/// In autopilot an agent's question meant for you comes here with the main agent's answer, for
/// the judge to check before it's sent. Every other kind without a table asks.
fn autopilot_route(point: &DecisionPoint, settings: &Settings) -> Option<Route> {
    let chosen = point
        .mode_override
        .unwrap_or_else(|| settings.approvals.mode());
    (point.kind == ApprovalKind::Question && chosen == ApprovalMode::Autopilot).then_some(Route {
        mode: ApprovalMode::Autopilot,
        decide_with: DecideWith::Model,
        on_reject: OnReject::Ask,
    })
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

/// How one kind of approval is decided.
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

/// `None` for kinds that always ask you. The `autopilot` switch puts each approval a run
/// opens in autopilot; a learned workflow promotion is decided outside runs, so it keeps its own
/// mode.
fn route(kind: ApprovalKind, settings: &Settings) -> Option<Route> {
    let approvals = &settings.approvals;
    let route = own_route(kind, settings)?;
    Some(match kind {
        ApprovalKind::WorkflowPromotion | ApprovalKind::GoalChange => route,
        _ => Route {
            mode: approvals.run_mode(route.mode),
            ..route
        },
    })
}

fn own_route(kind: ApprovalKind, settings: &Settings) -> Option<Route> {
    let approvals = &settings.approvals;
    match kind {
        ApprovalKind::Understand => Some(configured(kind, &approvals.understand)),
        ApprovalKind::Plan => Some(configured(kind, &approvals.plan)),
        ApprovalKind::Merge => Some(configured(kind, &approvals.merge)),
        ApprovalKind::Stuck => Some(configured(kind, &approvals.stuck)),
        ApprovalKind::Limit => Some(configured(kind, &approvals.limit)),
        ApprovalKind::WorkflowPromotion => Some(configured(
            kind,
            &approvals.workflow_promotion.as_approval(),
        )),
        ApprovalKind::ChangeRequest => Some(configured(kind, &approvals.change_request)),
        // `deny` refuses the change itself, so a rejection is decided here, not by `otherwise`.
        ApprovalKind::GoalChange => match approvals.goal_changes {
            GoalChanges::Ask => None,
            GoalChanges::Deny | GoalChanges::AllowClarifications => Some(Route {
                mode: ApprovalMode::Auto,
                decide_with: DecideWith::Rules,
                on_reject: OnReject::Decide,
            }),
        },
        ApprovalKind::Fallback
        | ApprovalKind::AgentDown
        | ApprovalKind::TakeOverReturn
        | ApprovalKind::Question
        | ApprovalKind::OriginalReturned => None,
    }
}

/// Only a rejected plan or merge is decided by `otherwise`. Sending back a goal, an extra
/// try, a reserve, or a promotion, or stopping the run over one, would cancel work that's
/// yours to call, so those ask.
fn configured(kind: ApprovalKind, table: &ApprovalPointSettings) -> Route {
    let on_reject = match (kind, table.otherwise) {
        (_, Otherwise::Ask) => OnReject::Ask,
        (ApprovalKind::Plan, Otherwise::SendBack | Otherwise::Stop) => OnReject::Decide,
        (ApprovalKind::Merge, _) => OnReject::Decide,
        _ => OnReject::Ask,
    };
    Route {
        mode: table.mode,
        decide_with: table.decide_with,
        on_reject,
    }
}

fn route_mode(kind: ApprovalKind, settings: &Settings) -> Option<ApprovalMode> {
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
fn needs_presence(kind: ApprovalKind, settings: &Settings) -> bool {
    settings.approvals.presence == Presence::Required
        && matches!(kind, ApprovalKind::Plan | ApprovalKind::Merge)
}

/// The understand, limit, stuck, change request, and promotion rules hold conditions no
/// judge may waive, such as an open question about the goal, `use_reserve = false`, or a
/// change that can't be undone. So those rules must approve before `decide_with` is asked at
/// all.
fn binding_rules(kind: ApprovalKind) -> bool {
    matches!(
        kind,
        ApprovalKind::Understand
            | ApprovalKind::Limit
            | ApprovalKind::Stuck
            | ApprovalKind::ChangeRequest
            | ApprovalKind::WorkflowPromotion
    )
}

async fn decide_point(
    parts: &PolicyParts,
    point: &DecisionPoint,
    evidence: &EvidencePacket,
    settings: &Settings,
    decide_with: DecideWith,
) -> Decision {
    // Merging debt anyway is yours unless `allow_debt` is set, so no judge and no
    // `rules_or_model` may take that call.
    if let Some(why) = debt_blocks_merge(point, evidence, settings) {
        return Decision {
            verdict: Verdict::AskYou,
            by: decide_with,
            judge: None,
            reasons: vec![why],
        };
    }
    let now = parts.clock.now();
    if binding_rules(point.kind) {
        let bound = rules::check(point, evidence, settings, now);
        if bound.verdict != Verdict::Approve {
            return Decision {
                verdict: Verdict::AskYou,
                by: DecideWith::Rules,
                judge: None,
                reasons: bound.reasons,
            };
        }
    }
    decide(&*parts.judge, point, evidence, settings, decide_with, now).await
}

fn debt_blocks_merge(
    point: &DecisionPoint,
    evidence: &EvidencePacket,
    settings: &Settings,
) -> Option<String> {
    if point.kind != ApprovalKind::Merge {
        return None;
    }
    debt_blocks(
        evidence.facts.unreviewed_debt.as_deref(),
        &settings.approvals.rules.merge,
    )
}

/// Notes what auto would decide, so your answer scores it. With `asked_because`, auto would
/// have approved but can't right now, and the reason goes last, so you see why you're asked.
async fn auto_preview(
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
        autonomy.note_auto_preview(&point, &record, &evidence, &settings);
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
            // the judge is deciding. Autopilot leaves it all to `reserve`, which still
            // stops it for `auto-off` and broken records.
            if !in_autopilot(point)
                && let Some(why) = auto_paused(parts, settings).await
            {
                return auto_preview(parts, point, evidence, settings, &decision, Some(why)).await;
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
                    auto_preview(parts, point, evidence, settings, &decision, None).await
                }
                Err(Refusal::Capped(violations)) => {
                    ask_with_record(violations, record(&decision, None))
                }
                Err(Refusal::Paused(why) | Refusal::Unavailable(why)) => {
                    auto_preview(parts, point, evidence, settings, &decision, Some(why)).await
                }
            }
        }
        Verdict::Reject if on_reject == OnReject::Decide => decided(decision, false),
        Verdict::Reject | Verdict::AskYou => ask_with_record(Vec::new(), record(&decision, None)),
    }
}

fn decided(decision: Decision, spot_check: bool) -> PolicyOutcome {
    PolicyOutcome::Decided {
        verdict: decision.verdict,
        by: decision.by,
        judge: decision.judge,
        reasons: decision.reasons,
        spot_check,
    }
}

/// What auto made of the approval, and `why` it asks anyway. The judge is named in the
/// reasons, since the record has no field for it.
fn record(decision: &Decision, why: Option<String>) -> AutoPreviewRecord {
    let mut reasons = decision.reasons.clone();
    if let Some(judge) = &decision.judge {
        let judge = DisplayNames::default().worker(judge);
        reasons.push(format!("asked {judge} to approve it"));
    }
    reasons.extend(why);
    AutoPreviewRecord {
        would: decision.verdict,
        by: decision.by,
        reasons,
    }
}

/// Auto didn't get to decide, and `why` says what stopped it.
fn ask_about(by: DecideWith, why: String) -> PolicyOutcome {
    let record = AutoPreviewRecord {
        would: Verdict::AskYou,
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
        auto_preview: None,
    }
}

fn ask_with_record(violations: Vec<Violation>, record: AutoPreviewRecord) -> PolicyOutcome {
    PolicyOutcome::AskUser {
        violations,
        auto_preview: Some(record),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, mpsc};
    use std::time::Duration;

    use argonauts_core::RunId;
    use argonauts_core::events::task::TaskSpec;
    use argonauts_core::future::BoxFuture;
    use argonauts_core::names::ApprovalId;
    use argonauts_core::policy::{
        ApprovalsView, AutoPreviewRecord, EvidenceFacts, HardLimits, UserDecision,
    };
    use argonauts_core::session_runner::SessionSpec;
    use argonauts_testkit::FakeClock;

    use super::*;
    use crate::approvals::settings::SettingsSource;
    use crate::parts::{Approve, Autonomy, ModelVerdict, Reservation};

    struct NoLimits;

    impl HardLimits for NoLimits {
        fn check_point(&self, _: &DecisionPoint, _: &EvidencePacket) -> Vec<Violation> {
            Vec::new()
        }

        fn check_task_diff(&self, _: &TaskSpec, _: &str) -> Vec<Violation> {
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
                    judge: None,
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

        fn note_auto_preview(
            &self,
            _: &DecisionPoint,
            _: &AutoPreviewRecord,
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
            judge: Arc::new(Approving),
            autonomy: autonomy.clone(),
            clock: Arc::new(FakeClock::default()),
        };
        let point = DecisionPoint {
            run: RunId::new("run-01").expect("run id"),
            approval: ApprovalId::numbered(1),
            kind: ApprovalKind::Understand,
            learned_workflow: None,
            authors: Vec::new(),
            mode_override: None,
            main_agent: None,
        };
        let evidence = EvidencePacket {
            kind: ApprovalKind::Understand,
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
