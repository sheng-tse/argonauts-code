//! Decision points. Every approval is opened with an evidence packet bound by its hash and the
//! options its kind offers; your policy evaluates it beside the loop, never blocking it. The first
//! decision wins: a late policy outcome is kept only as an auto preview record, and a late answer
//! of yours gets "already decided".

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard, PoisonError};

use tokio::sync::oneshot;

use argonauts_core::api::ApprovalAnswer;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::approval::{
    AnswerAction, AnswerOption, ApprovalDecided, ApprovalEvent, ApprovalKind, ApprovalOpened,
    ApprovalPacket, FreeText, PolicyCheck,
};
use argonauts_core::events::run::Party;
use argonauts_core::events::shared::BlackboardEvent;
use argonauts_core::names::{ApprovalId, ModelRef, RoleName};
use argonauts_core::policy::{
    AutoPreviewRecord, DecisionPoint, EvidenceDocument, EvidenceFacts, EvidencePacket,
    EvidenceSubject, PolicyOutcome, UserDecision, Verdict, Violation,
};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::{
    Answerer, ApprovalMode, AtHardLimit, DecideWith, Otherwise,
};
use argonauts_core::{Note, NoteLevel, TaskId};

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::state::RunState;
use crate::state::approvals::ApprovalRecord;
use crate::state::ids::IdKind;
use crate::workflow::{StepAt, change, kinds};

pub mod autopilot;
pub mod decline;
pub mod evidence;
pub mod options;

/// What a module asks for when it opens an approval.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub at: Option<StepAt>,
    pub kind: ApprovalKind,
    /// Such as "Merge this run into main?", the question's heading on screen.
    pub title: String,
    pub task: Option<TaskId>,
    pub packet: ApprovalPacket,
    /// Measured facts only. Rules read these.
    pub facts: EvidenceFacts,
    /// Reviewer verdicts, judge scores, comparer tags, one line each.
    pub agent_outputs: Vec<String>,
    /// The plan text, the combined diff, and so on. Your approval rules file, or the default
    /// rules without one, is added.
    pub documents: Vec<EvidenceDocument>,
    /// `None` takes the kind's options from [`options::standard`].
    pub options: Option<Vec<AnswerOption>>,
    pub free_text: FreeText,
    /// The judge must not be one of these.
    pub authors: Vec<ModelRef>,
}

/// Waiters for open approvals, and the decision points this process started the policy on.
/// Lives in the engine's shared state. Its lock also orders decisions, so the first one wins.
#[derive(Debug, Default)]
pub struct Desk {
    waiters: Mutex<BTreeMap<ApprovalId, Vec<oneshot::Sender<ApprovalDecided>>>>,
    points: Mutex<BTreeMap<ApprovalId, DecisionPoint>>,
    /// Why an approval goes to you even in auto, when the step that opened it knows, such as
    /// the fact the main agent said only you have.
    because: Mutex<BTreeMap<ApprovalId, String>>,
    evaluating: Mutex<BTreeSet<ApprovalId>>,
    /// Auto-approvals you already spot-checked in this process, so a second verdict on one is
    /// refused rather than counted twice.
    spot_checked: Mutex<BTreeSet<ApprovalId>>,
    /// Auto-approved merges the policy drew for a spot check, which join your queue once the
    /// run is merged: before that nothing merged, so there is no call to check.
    spot_check_once_merged: Mutex<BTreeSet<ApprovalId>>,
    /// Approvals the policy weighs, or weighed, in autopilot in this process.
    autopilot: Mutex<BTreeSet<ApprovalId>>,
    deciding: Mutex<()>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Desk {
    /// Wakes whoever waits on this approval.
    pub fn wake(&self, decided: &ApprovalDecided) {
        for waiter in lock(&self.waiters).remove(&decided.id).unwrap_or_default() {
            // A waiter that went away (its step was interrupted) re-enters and finds the
            // decision in the state instead.
            let _ = waiter.send(decided.clone());
        }
    }

    fn wait(&self, id: &ApprovalId) -> oneshot::Receiver<ApprovalDecided> {
        let (sender, receiver) = oneshot::channel();
        lock(&self.waiters)
            .entry(id.clone())
            .or_default()
            .push(sender);
        receiver
    }

    /// Drops the waiters, whose receivers then see the approval go.
    fn release(&self, id: &ApprovalId) {
        lock(&self.waiters).remove(id);
    }

    fn point(&self, id: &ApprovalId) -> Option<DecisionPoint> {
        lock(&self.points).get(id).cloned()
    }

    /// How many approvals the policy is weighing now, which hold the run's wait as a session
    /// at work does: autopilot may be about to settle one.
    pub fn weighing(&self) -> usize {
        lock(&self.evaluating).len()
    }
}

/// Opens the approval (or finds the one this step already opened), starts the policy beside
/// the loop, and waits for the first decision. A decided approval returns at once, so a
/// re-entered step picks up your answer. `Refused` when it's withdrawn while waiting.
pub async fn open_and_wait(host: &Host, request: ApprovalRequest) -> EngineResult<ApprovalDecided> {
    open(host, request)?.decided(host).await
}

/// [`open_and_wait`] for an approval the step knows auto can't settle, and `because` says why
/// on its question.
pub async fn open_and_wait_because(
    host: &Host,
    request: ApprovalRequest,
    because: Option<String>,
) -> EngineResult<ApprovalDecided> {
    open_because(host, request, because)?.decided(host).await
}

/// [`open_and_wait`] for an approval that comes to you whatever the mode, such as one about a
/// topic `approvals.ask_you_about` names: neither your rules nor the judge decide it, and
/// `because` says why on its question.
pub async fn open_and_wait_yours(
    host: &Host,
    request: ApprovalRequest,
    because: String,
) -> EngineResult<ApprovalDecided> {
    open_yours(host, request, because)?.decided(host).await
}

/// [`open_and_wait_yours`] without waiting.
pub fn open_yours(host: &Host, request: ApprovalRequest, because: String) -> EngineResult<Opening> {
    open_with(host, request, Some(because), false)
}

/// The go-ahead's topic in `approvals.ask_you_about`.
pub const GO_AHEAD_TOPIC: &str = "go-ahead";
/// A change to a running workflow's topic in `approvals.ask_you_about`.
pub const CHANGE_TOPIC: &str = "workflow change";

/// Whether `approvals.ask_you_about` names any of `topics`, matched without case: a question's
/// tag or topic, the go-ahead, a workflow change, a stuck goal, or keeping going as a goal.
pub fn asks_you_about(settings: &Settings, topics: &[&str]) -> bool {
    settings.approvals.ask_you_about.iter().any(|asked| {
        let asked = asked.trim();
        !asked.is_empty()
            && topics
                .iter()
                .any(|topic| topic.trim().eq_ignore_ascii_case(asked))
    })
}

/// [`asks_you_about`] at an approval of `kind` in this run: never in autopilot, where nothing
/// waits for you.
pub fn keeps_yours(host: &Host, kind: ApprovalKind, topics: &[&str]) -> bool {
    !autopilot::applies_now(host, kind) && asks_you_about(host.settings(), topics)
}

/// Why a question about `topic` waits for you though the main agent answers.
pub fn yours_because(topic: &str) -> String {
    format!("your /approvals settings keep {topic} yours")
}

/// An opened approval, or the decision it already has.
pub enum Opening {
    Decided(Box<ApprovalDecided>),
    Waiting {
        id: ApprovalId,
        receiver: oneshot::Receiver<ApprovalDecided>,
    },
}

impl Opening {
    pub fn id(&self) -> &ApprovalId {
        match self {
            Self::Decided(decided) => &decided.id,
            Self::Waiting { id, .. } => id,
        }
    }

    /// Waits for the first decision. `Interrupted` on a pause or stop, and `Refused` when the
    /// approval is withdrawn.
    pub async fn decided(self, host: &Host) -> EngineResult<ApprovalDecided> {
        let (id, receiver) = match self {
            Self::Decided(decided) => return Ok(*decided),
            Self::Waiting { id, receiver } => (id, receiver),
        };
        tokio::select! {
            decided = receiver => decided.map_err(|_| {
                let reason = host
                    .read(|state| {
                        state.approvals.by_id.get(&id).and_then(|record| record.withdrawn.clone())
                    })
                    .unwrap_or_else(|| "the run stopped".to_owned());
                EngineError::Refused(format!("approval {id} was withdrawn: {reason}"))
            }),
            () = host.interrupted().cancelled() => Err(EngineError::Interrupted),
        }
    }

    /// [`Self::decided`], or why it was withdrawn instead, for a caller that goes on without
    /// an answer then.
    pub async fn decided_or_withdrawn(
        self,
        host: &Host,
    ) -> EngineResult<Result<ApprovalDecided, String>> {
        let id = self.id().clone();
        match self.decided(host).await {
            Ok(decided) => Ok(Ok(decided)),
            Err(EngineError::Refused(refused)) => {
                let withdrawn = host.read(|state| {
                    state
                        .approvals
                        .by_id
                        .get(&id)
                        .and_then(|record| record.withdrawn.clone())
                });
                withdrawn.map(Err).ok_or(EngineError::Refused(refused))
            }
            Err(err) => Err(err),
        }
    }
}

/// Opens the approval without waiting: finds the one this step already opened (withdrawing
/// it when its evidence changed), or records a new one, and starts the policy on it.
pub fn open(host: &Host, request: ApprovalRequest) -> EngineResult<Opening> {
    open_because(host, request, None)
}

/// [`open`] for an approval the step knows auto can't settle, and `because` says why on its
/// question.
pub fn open_because(
    host: &Host,
    request: ApprovalRequest,
    because: Option<String>,
) -> EngineResult<Opening> {
    open_with(host, request, because, true)
}

/// Opens the approval; with `policy` off only you decide it.
fn open_with(
    host: &Host,
    request: ApprovalRequest,
    because: Option<String>,
    policy: bool,
) -> EngineResult<Opening> {
    let evidence = evidence::packet(
        host,
        request.kind,
        request.facts.clone(),
        request.agent_outputs.clone(),
        request.documents.clone(),
    )?;
    let sha256 = evidence.sha256();
    let desk = &host.shared().desk;
    // Kept yours on purpose, as a hard limit set to wait for you is.
    let kept_yours = !policy;
    let policy = policy && !only_yours(&request.packet);
    let (id, receiver, evaluate) = {
        let _deciding = lock(&desk.deciding);
        match host.read(|state| find(state, &request, &sha256)) {
            Found::Decided(decided) => return Ok(Opening::Decided(decided)),
            Found::Open { id, policy_done } => {
                let receiver = desk.wait(&id);
                let evaluate = policy && !policy_done && lock(&desk.evaluating).insert(id.clone());
                (id, receiver, evaluate)
            }
            Found::Stale(stale) => {
                withdraw_locked(host, &stale, "the evidence changed")?;
                let id = record_opened(host, &request, &evidence, &sha256, !policy)?;
                if policy {
                    lock(&desk.evaluating).insert(id.clone());
                }
                (id.clone(), desk.wait(&id), policy)
            }
            Found::None => {
                let id = record_opened(host, &request, &evidence, &sha256, !policy)?;
                if policy {
                    lock(&desk.evaluating).insert(id.clone());
                }
                (id.clone(), desk.wait(&id), policy)
            }
        }
    };
    let point = decision_point(host, &id, &request);
    lock(&desk.points).insert(id.clone(), point.clone());
    if let Some(because) = because.filter(|because| !because.trim().is_empty()) {
        lock(&desk.because).insert(id.clone(), because);
    }
    if evaluate {
        if autopilot::chosen(point.mode_override, host.settings(), request.kind) {
            lock(&desk.autopilot).insert(id.clone());
        }
        spawn_policy(host, point, evidence);
    } else if autopilot::applies_now(host, request.kind) {
        // Only you could answer it, or it already asked you before autopilot was on. One
        // autopilot itself keeps for you stays yours.
        if kept_yours {
            lock(&desk.autopilot).insert(id.clone());
        }
        autopilot::settle_open(host)?;
    }
    Ok(Opening::Waiting { id, receiver })
}

/// Waits again on an open approval that an earlier process opened, such as the question a run
/// waited on when something other than its answer relaunched it. Its decision point is rebuilt
/// from the log, and on autopilot it is settled at once. A decided one returns at once.
pub async fn wait_again(host: &Host, id: &ApprovalId) -> EngineResult<ApprovalDecided> {
    let desk = &host.shared().desk;
    let (receiver, kind) = {
        let _deciding = lock(&desk.deciding);
        let record = host
            .read(|state| state.approvals.by_id.get(id).cloned())
            .ok_or_else(|| EngineError::Refused(format!("There's no approval {id}")))?;
        if let Some(decided) = record.decided {
            return Ok(decided);
        }
        if desk.point(id).is_none() {
            let point = recorded_point(host, &record);
            lock(&desk.points).insert(id.clone(), point);
        }
        (desk.wait(id), record.opened.kind)
    };
    if autopilot::applies_now(host, kind) {
        autopilot::settle_open(host)?;
    }
    Opening::Waiting {
        id: id.clone(),
        receiver,
    }
    .decided(host)
    .await
}

/// Weighs an approval again in autopilot, which the policy weighed outside it, such as before
/// Shift+Tab turned autopilot on. Its decision point comes from the step that opened it. A
/// worker's question an earlier process opened has its point rebuilt from the log, since it
/// names no authors the judge must not be, so a relaunched run answers it at once; any other
/// kind waits for its step to open it again, which weighs it then. The caller holds the desk's
/// deciding lock.
fn ask_again(host: &Host, record: &ApprovalRecord) -> EngineResult<()> {
    let desk = &host.shared().desk;
    let id = &record.opened.id;
    let point = match desk.point(id) {
        Some(point) => point,
        None if record.opened.kind == ApprovalKind::Question => recorded_point(host, record),
        None => return Ok(()),
    };
    if !lock(&desk.evaluating).insert(id.clone()) {
        return Ok(());
    }
    let evidence = match evidence_of(host, &record.opened) {
        Ok(evidence) => evidence,
        Err(err) => {
            lock(&desk.evaluating).remove(id);
            return Err(err);
        }
    };
    let point = host.read(|state| DecisionPoint {
        mode_override: state.run.approval_mode,
        main_agent: Some(tie_breaker_of(state, host.settings())),
        ..point
    });
    lock(&desk.points).insert(id.clone(), point.clone());
    lock(&desk.autopilot).insert(id.clone());
    spawn_policy(host, point, evidence);
    Ok(())
}

enum Found {
    Decided(Box<ApprovalDecided>),
    Open {
        id: ApprovalId,
        policy_done: bool,
    },
    /// Open for the same kind and task, but over evidence that has since changed.
    Stale(ApprovalId),
    None,
}

/// At most one approval is open per kind and task. A step re-entered after a crash or a
/// wait gets back the one it opened in this round, open or decided, when the evidence is the
/// same.
fn find(state: &RunState, request: &ApprovalRequest, sha256: &str) -> Found {
    if is_workflow_change(request.kind, request.task.as_ref()) {
        return find_workflow_change(state, request, sha256);
    }
    if let Some(open) = state
        .approvals
        .open_for(request.kind, request.task.as_ref())
    {
        if open.opened.evidence_sha256 != sha256 {
            return Found::Stale(open.opened.id.clone());
        }
        return Found::Open {
            id: open.opened.id.clone(),
            policy_done: open.policy_check().is_some(),
        };
    }
    let Some(round_since) = request.at.as_ref().and_then(|at| round_since(state, at)) else {
        return Found::None;
    };
    state
        .approvals
        .by_id
        .values()
        .filter(|record| {
            record.since > round_since
                && record.withdrawn.is_none()
                && record.opened.kind == request.kind
                && record.opened.task == request.task
                && record.opened.title == request.title
                && record.opened.evidence_sha256 == sha256
        })
        .max_by_key(|record| record.since)
        .and_then(|record| record.decided.clone())
        .map_or(Found::None, |decided| Found::Decided(Box::new(decided)))
}

/// Where the step's current round began in the log, while that round is still going.
fn round_since(state: &RunState, at: &StepAt) -> Option<u64> {
    state
        .workflows
        .progress(&at.step)
        .filter(|progress| progress.round == at.round && progress.end.is_none())
        .map(|progress| progress.since)
}

/// The step's newest approval of `kind` and `title` in this round, when it's decided, with
/// the subject its evidence named. A re-entered step whose subject hasn't moved can act on the
/// answer without building its evidence again.
pub fn decided_in_round(
    host: &Host,
    at: &StepAt,
    kind: ApprovalKind,
    title: &str,
) -> EngineResult<Option<(ApprovalDecided, Option<EvidenceSubject>)>> {
    let decided = host.read(|state| {
        let since = round_since(state, at)?;
        state
            .approvals
            .by_id
            .values()
            .filter(|record| {
                record.since > since
                    && record.withdrawn.is_none()
                    && record.opened.kind == kind
                    && record.opened.task.is_none()
                    && record.opened.title == title
            })
            .max_by_key(|record| record.since)
            .and_then(|record| record.decided.clone())
    });
    let Some(decided) = decided else {
        return Ok(None);
    };
    let subject = evidence::read(host, &decided.id)?.and_then(|packet| packet.facts.subject);
    Ok(Some((decided, subject)))
}

/// A question about your own files, such as the ones in the way of landing the run: no rule or
/// judge can move them, so only you answer it and the policy never weighs in.
pub fn only_yours(packet: &ApprovalPacket) -> bool {
    matches!(packet, ApprovalPacket::Merge(merge) if merge.waits_on_you())
}

/// Why a question that follows the merge question is yours alone, for the line autopilot
/// writes when it settles it in your place.
fn why_only_yours(packet: &ApprovalPacket) -> Option<String> {
    let ApprovalPacket::Merge(merge) = packet else {
        return None;
    };
    if !merge.both_changed.is_empty() {
        Some("only you can pick between your version and the run's".to_owned())
    } else if merge.asks_which_branch {
        Some("only you can name the branch".to_owned())
    } else {
        None
    }
}

/// Why a merge waits for you whatever your rules or the judge say: a review or a ruling of a
/// task in it went to a fallback from its builder's own provider, which only autopilot does
/// when nobody else is left. `None` when every review stayed independent.
pub fn builders_provider_reviewed(host: &Host, opened: &ApprovalOpened) -> Option<String> {
    let ApprovalPacket::Merge(merge) = &opened.packet else {
        return None;
    };
    let registry = &host.services().registry;
    host.read(|state| {
        merge.rows.iter().find_map(|row| {
            let record = state.tasks.get(&row.task)?;
            let builder = registry.provider(&crate::fallback::rules::worker(record)?);
            let fallback = state.sessions.by_key.values().find(|session| {
                matches!(session.started.role, RoleName::Reviewer | RoleName::Judge)
                    && session.asked.is_some()
                    && session.task.as_ref().is_none_or(|task| *task == row.task)
                    && registry.provider(&session.started.worker) == builder
            })?;
            Some(format!(
                "{}, a fallback from its builder's own provider, reviewed {}",
                crate::roles::display_name(&fallback.started.worker),
                row.task
            ))
        })
    })
}

/// What your rules or the judge approved at approval `id` merged, so a spot check the policy
/// drew for it joins your queue now.
pub fn merged(host: &Host, id: &ApprovalId) -> EngineResult<()> {
    if lock(&host.shared().desk.spot_check_once_merged).remove(id) {
        host.record(
            None,
            None,
            RunEventKind::Approval(ApprovalEvent::SpotCheckQueued { id: id.clone() }),
        )?;
    }
    Ok(())
}

/// Landing what your rules or the judge approved at approval `id` was refused, so the
/// auto-merge it took goes back.
pub fn landing_refused(host: &Host, id: &ApprovalId) {
    let Some(record) = host.read(|state| state.approvals.by_id.get(id).cloned()) else {
        return;
    };
    let auto = record
        .decided
        .as_ref()
        .is_some_and(|decided| matches!(decided.by, Party::Rules | Party::Judge { .. }));
    if auto {
        let point = recorded_point(host, &record);
        host.services().policy.landing_refused(&point);
    }
    lock(&host.shared().desk.spot_check_once_merged).remove(id);
}

/// A change to a running workflow, rather than a worker's change request on a task.
pub fn is_workflow_change(kind: ApprovalKind, task: Option<&TaskId>) -> bool {
    kind == ApprovalKind::ChangeRequest && task.is_none()
}

/// Workflow changes are told apart by their title, which names the workflow and, for the main
/// agent's, the phase that ended. Two proposals never withdraw each other; only the main agent's
/// own proposal for the same phase end, asked again over other evidence, replaces the open one.
fn find_workflow_change(state: &RunState, request: &ApprovalRequest, sha256: &str) -> Found {
    let same = state
        .approvals
        .open()
        .filter(|record| {
            is_workflow_change(record.opened.kind, record.opened.task.as_ref())
                && record.opened.title == request.title
        })
        .max_by_key(|record| record.since);
    match same {
        Some(open) if open.opened.evidence_sha256 == sha256 => Found::Open {
            id: open.opened.id.clone(),
            policy_done: open.policy_check().is_some(),
        },
        Some(open) if request.at.is_some() => Found::Stale(open.opened.id.clone()),
        _ => Found::None,
    }
}

/// The evidence packet the approval was opened with, where `evidence::write` put it, checked
/// against the hash it was opened under.
pub fn evidence_of(host: &Host, opened: &ApprovalOpened) -> EngineResult<EvidencePacket> {
    let path = host
        .repo()
        .run_dir
        .join("evidence")
        .join(format!("{}.json", opened.id));
    let packet: EvidencePacket = serde_json::from_slice(&std::fs::read(&path)?)?;
    if packet.sha256() != opened.evidence_sha256 {
        return Err(EngineError::Refused(format!(
            "the evidence of approval {} changed since it opened",
            opened.id
        )));
    }
    Ok(packet)
}

/// `yours` opens it for you alone, as a supervised approval, whatever the mode.
fn record_opened(
    host: &Host,
    request: &ApprovalRequest,
    evidence: &EvidencePacket,
    sha256: &str,
    yours: bool,
) -> EngineResult<ApprovalId> {
    let id = ApprovalId::numbered(host.allocate(IdKind::Approval)?);
    evidence::write(host, &id, evidence)?;
    let (override_mode, last_round) = host.read(|state| {
        let last_round = request
            .at
            .as_ref()
            .is_some_and(|at| kinds::in_last_loop_round(state, at));
        (state.run.approval_mode, last_round)
    });
    let configured = configured(host.settings(), request.kind);
    // Your records may lower the mode, such as back to auto preview after a reverted auto-merge.
    let mode = if yours {
        ApprovalMode::Supervised
    } else {
        effective_mode(
            host,
            &decision_point(host, &id, request),
            override_mode,
            configured.mode,
        )
    };
    let in_autopilot = host.read(|state| autopilot::applies(state, host.settings(), request.kind));
    let mut options = request
        .options
        .clone()
        .unwrap_or_else(|| options::standard(request.kind));
    // A loop's last round has no round left to send it back to.
    if last_round {
        options.retain(|option| option.action != AnswerAction::SendBack);
    }
    let opened = ApprovalOpened {
        id: id.clone(),
        kind: request.kind,
        title: request.title.clone(),
        task: request.task.clone(),
        packet: request.packet.clone(),
        evidence_sha256: sha256.to_owned(),
        mode,
        decide_with: configured.decide_with,
        when_declined: host.settings().approvals.when_declined,
        options,
        free_text: request.free_text,
        // Only auto can settle it without you, and only for a kind your rules decide; the
        // policy's check says so once it's in. Autopilot settles what's left itself.
        needs_you: (!mode.goes_ahead() || !policy_may_settle(host.settings(), request.kind))
            && !in_autopilot,
    };
    host.record(
        request.task.as_ref(),
        None,
        RunEventKind::Approval(ApprovalEvent::Opened(Box::new(opened))),
    )?;
    Ok(id)
}

/// Shift+Tab's mode, else your settings'. Your records may lower it, such as back to auto preview
/// after a reverted auto-merge.
fn effective_mode(
    host: &Host,
    point: &DecisionPoint,
    chosen: Option<ApprovalMode>,
    configured: ApprovalMode,
) -> ApprovalMode {
    host.services()
        .policy
        .effective_mode(point, chosen.unwrap_or(configured))
}

/// The mode an approval of `kind` would open in now.
pub fn mode_now(host: &Host, kind: ApprovalKind) -> ApprovalMode {
    let (mode_override, learned_workflow) =
        host.read(|state| (state.run.approval_mode, learned_workflow_of(state)));
    let point = DecisionPoint {
        run: host.run_id().clone(),
        // No approval is open yet; the records look only at the kind and the learned workflow.
        approval: ApprovalId::numbered(0),
        kind,
        learned_workflow,
        authors: Vec::new(),
        mode_override,
        main_agent: None,
    };
    let configured = configured(host.settings(), kind).mode;
    effective_mode(host, &point, mode_override, configured)
}

/// Who answers the crew's questions at an approval of `kind` now: in autopilot the model you
/// picked, else the main agent; otherwise the main agent once you said "Yes, and let the main agent
/// answer" in this run, else `approvals.answers`, else the main agent in auto mode and you
/// otherwise.
pub fn answerer(host: &Host, kind: ApprovalKind) -> Answerer {
    let approvals = &host.settings().approvals;
    if autopilot::applies_now(host, kind) {
        return approvals.answerer(ApprovalMode::Autopilot);
    }
    if host.read(crate::understand::approval::main_agent_answers_for_you) {
        return Answerer::Main;
    }
    approvals.answerer(mode_now(host, kind))
}

fn decision_point(host: &Host, id: &ApprovalId, request: &ApprovalRequest) -> DecisionPoint {
    let (mode_override, learned_workflow, tie_breaker) = host.read(|state| {
        (
            state.run.approval_mode,
            learned_workflow_of(state),
            tie_breaker_of(state, host.settings()),
        )
    });
    DecisionPoint {
        run: host.run_id().clone(),
        approval: id.clone(),
        kind: request.kind,
        learned_workflow,
        authors: request.authors.clone(),
        mode_override,
        main_agent: Some(tie_breaker),
    }
}

/// Whose answers take the place of yours in autopilot now: the phase's tie-breaker, else the main
/// agent.
fn tie_breaker_of(state: &RunState, settings: &Settings) -> ModelRef {
    crate::tie_breaker::tie_breaker(state, settings).unwrap_or_else(|| settings.crew.main.clone())
}

/// The learned workflow the run's Captain-or-crew choice picked, which auto preview records count
/// per.
fn learned_workflow_of(state: &RunState) -> Option<String> {
    state
        .run
        .started
        .as_ref()
        .and_then(|started| started.captain_or_crew.as_ref())
        .and_then(|captain_or_crew| captain_or_crew.rule.strip_prefix("learned:"))
        .map(str::to_owned)
}

/// Runs `evaluate` beside the loop, tracked so the loop joins it before the run waits or
/// finishes. Its outcome goes through [`policy_outcome`], where the first decision wins.
fn spawn_policy(host: &Host, point: DecisionPoint, evidence: EvidencePacket) {
    let host = host.clone();
    let tasks = host.shared().tasks.clone();
    tasks.spawn(async move {
        let policy = host.services().policy.clone();
        let shared = host.shared();
        let outcome = tokio::select! {
            outcome = policy.evaluate(&point, &evidence) => outcome,
            () = shared.shutdown.cancelled() => {
                lock(&shared.desk.evaluating).remove(&point.approval);
                return;
            }
            // Still deciding after the wind-down grace: evaluated again when the run resumes.
            () = shared.halt.cancelled() => {
                lock(&shared.desk.evaluating).remove(&point.approval);
                return;
            }
        };
        lock(&host.shared().desk.evaluating).remove(&point.approval);
        if let Err(err) = policy_outcome(&host, &point.approval, outcome) {
            // The approval still waits for you, so the log says why the policy never decided.
            let note = Note {
                level: NoteLevel::Error,
                text: format!(
                    "the policy's outcome on {} wasn't recorded: {err}",
                    point.approval
                ),
            };
            if let Err(also) = host.record(None, None, RunEventKind::Note(note)) {
                tracing::warn!(approval = %point.approval, "the policy's outcome wasn't recorded: {err}; nor was the note: {also}");
            }
        }
    });
}

/// Your answer. Refused with the reason when the action wasn't offered, the evidence hash
/// is different, or it was already decided. Carry on without me is an answer too: [`decline`]
/// takes an option in your place. After it's recorded, `policy.user_decided` is called so
/// auto preview records can be scored.
pub fn answer(host: &Host, answer: ApprovalAnswer) -> Result<(), String> {
    // Carry on without me, the last row of every question that waits on you: the engine takes
    // an option in your place, so it isn't bound to the evidence.
    if answer.action == AnswerAction::CarryOn {
        return decline::left(host, &answer.id);
    }
    let desk = &host.shared().desk;
    let (decided, point, yours) = {
        let _deciding = lock(&desk.deciding);
        let record = host
            .read(|state| state.approvals.by_id.get(&answer.id).cloned())
            .ok_or_else(|| format!("no approval {}", answer.id))?;
        if record.decided.is_some() {
            return Err("already decided".to_owned());
        }
        if let Some(reason) = &record.withdrawn {
            return Err(format!("withdrawn: {reason}"));
        }
        if record.opened.evidence_sha256 != answer.evidence_sha256 {
            return Err(
                "the evidence changed since the question was shown; look at it again and answer"
                    .to_owned(),
            );
        }
        check_offered(&record.opened, &answer)?;
        let decided = ApprovalDecided {
            id: answer.id.clone(),
            action: answer.action.clone(),
            text: answer.text.clone(),
            by: Party::User,
            evidence_sha256: answer.evidence_sha256.clone(),
            auto_preview: auto_preview_of(&record),
            tasks: answer.tasks.clone(),
        };
        host.record(
            record.opened.task.as_ref(),
            None,
            RunEventKind::Approval(ApprovalEvent::Decided(decided.clone())),
        )
        .map_err(|err| err.to_string())?;
        if crate::run_review::reversals::rejects(record.opened.kind, &decided.action) {
            let what =
                crate::run_review::reversals::at_approval(record.opened.kind, &decided.action);
            crate::run_review::reversals::note(host, record.opened.task.as_ref(), what);
        }
        // Before waking the waiter, so it finds the change already recorded.
        change::on_decided(host, &record.opened, &decided);
        crate::fallback::down::on_decided(host, &record.opened, &decided);
        desk.wake(&decided);
        let yours = only_yours(&record.opened.packet);
        (decided, recorded_point(host, &record), yours)
    };
    // Only you could answer it, so there's no policy to score against your answer.
    if yours {
        return Ok(());
    }
    host.services().policy.user_decided(&UserDecision {
        point,
        edited: matches!(decided.action, AnswerAction::ApproveWithEdits),
        action: decided.action,
        auto_preview: decided.auto_preview,
    });
    Ok(())
}

/// Your verdict on an auto-approval in your spot-check queue, passed to the policy, where a
/// wrong one drops that decision point back to auto preview. Refused for an approval that never
/// joined the queue, and for one you already checked.
pub fn spot_check(host: &Host, id: &ApprovalId, correct: bool) -> Result<(), String> {
    let desk = &host.shared().desk;
    let record = host
        .read(|state| state.approvals.by_id.get(id).cloned())
        .ok_or_else(|| format!("no approval {id}"))?;
    if !record.spot_check {
        return Err(format!("{id} isn't waiting for a spot check"));
    }
    if !lock(&desk.spot_checked).insert(id.clone()) {
        return Err(format!("{id} was already spot-checked"));
    }
    let point = recorded_point(host, &record);
    host.services().policy.spot_checked(&point, correct);
    if !correct {
        let what = format!(
            "your spot check found the auto-approval at the {} approval wrong",
            serde_json::to_value(record.opened.kind)
                .ok()
                .and_then(|kind| kind.as_str().map(|kind| kind.replace('_', " ")))
                .unwrap_or_default()
        );
        crate::run_review::reversals::note(host, record.opened.task.as_ref(), what);
    }
    Ok(())
}

/// The decision point the policy was started on, or the same point rebuilt from the log when
/// another process started it.
fn recorded_point(host: &Host, record: &ApprovalRecord) -> DecisionPoint {
    let id = &record.opened.id;
    if let Some(point) = host.shared().desk.point(id) {
        return point;
    }
    let (mode_override, learned_workflow, tie_breaker) = host.read(|state| {
        (
            state.run.approval_mode,
            learned_workflow_of(state),
            tie_breaker_of(state, host.settings()),
        )
    });
    DecisionPoint {
        run: host.run_id().clone(),
        approval: id.clone(),
        kind: record.opened.kind,
        learned_workflow,
        authors: Vec::new(),
        mode_override,
        main_agent: Some(tie_breaker),
    }
}

fn check_offered(opened: &ApprovalOpened, answer: &ApprovalAnswer) -> Result<(), String> {
    if matches!(answer.action, AnswerAction::ShowDiff) {
        return Err("showing the details doesn't answer the question".to_owned());
    }
    let offered = opened.options.iter().any(|option| option.action == answer.action)
        // "Pick another agent" lists every valid agent, so any of them may come back.
        || (matches!(answer.action, AnswerAction::Candidate { .. })
            && opened
                .options
                .iter()
                .any(|option| option.action == AnswerAction::OtherCandidates));
    if !offered {
        return Err(format!(
            "that answer isn't one the question offered ({:?})",
            answer.action
        ));
    }
    let needs_text = matches!(answer.action, AnswerAction::Answer);
    if needs_text
        && answer
            .text
            .as_deref()
            .is_none_or(|text| text.trim().is_empty())
    {
        return Err("type your answer first".to_owned());
    }
    Ok(())
}

/// What auto would have decided, from the auto preview check the policy recorded before you
/// answered.
fn auto_preview_of(record: &ApprovalRecord) -> Option<AutoPreviewRecord> {
    record
        .checks
        .iter()
        .find(|check| check.auto_preview)
        .and_then(|check| {
            Some(AutoPreviewRecord {
                would: check.verdict?,
                by: check.by.unwrap_or_default(),
                reasons: check.reasons.clone(),
            })
        })
}

/// The policy's outcome, when it arrives. Kept only as an auto preview record if you already
/// decided.
pub fn policy_outcome(host: &Host, id: &ApprovalId, outcome: PolicyOutcome) -> EngineResult<()> {
    let desk = &host.shared().desk;
    let _deciding = lock(&desk.deciding);
    let record = host
        .read(|state| state.approvals.by_id.get(id).cloned())
        .ok_or_else(|| EngineError::Refused(format!("no approval {id}")))?;
    let task = record.opened.task.clone();
    let checked = |check: PolicyCheck| {
        host.record(
            task.as_ref(),
            None,
            RunEventKind::Approval(ApprovalEvent::Checked(check)),
        )
    };
    // A check already in counts as the policy's last word, unless autopilot asked it again.
    let asked_again = record.policy_check().is_some()
        && record.is_open()
        && autopilot::asked_in_autopilot(host, &record)
        && record.opened.mode != ApprovalMode::Autopilot;
    let late = !record.is_open() || (record.policy_check().is_some() && !asked_again);
    let in_autopilot = !late && autopilot::applies_now(host, record.opened.kind);
    // No rule or judge merges what the builder's own provider reviewed: that merge is yours.
    let not_independent = builders_provider_reviewed(host, &record.opened);
    let outcome = match outcome {
        PolicyOutcome::Decided { .. } if not_independent.is_some() => PolicyOutcome::AskUser {
            violations: Vec::new(),
            auto_preview: None,
        },
        outcome => outcome,
    };
    match outcome {
        PolicyOutcome::AskUser {
            violations,
            auto_preview,
        } => {
            let because = || {
                lock(&desk.because)
                    .get(id)
                    .cloned()
                    .or_else(|| why_asked(&violations, auto_preview.as_ref()))
            };
            let auto_preview_check = |auto_preview: AutoPreviewRecord| PolicyCheck {
                id: id.clone(),
                violations: Vec::new(),
                verdict: Some(auto_preview.would),
                by: Some(auto_preview.by),
                judge: None,
                reasons: auto_preview.reasons,
                auto_preview: true,
            };
            if in_autopilot && !autopilot::asked_in_autopilot(host, &record) {
                // Weighed before autopilot came on, so it's weighed again in autopilot.
                if let Some(auto_preview) = auto_preview {
                    checked(auto_preview_check(auto_preview))?;
                }
                return ask_again(host, &record);
            }
            let held = match &auto_preview {
                _ if !violations.is_empty() => {
                    autopilot::Held::HardLimit(autopilot::at_hard_limit(host, &record, &violations))
                }
                _ if not_independent.is_some()
                    || autopilot::kept_for_you(host.settings(), record.opened.kind) =>
                {
                    autopilot::Held::Kept
                }
                Some(auto_preview) if auto_preview.would == Verdict::Reject => {
                    autopilot::Held::Rejected
                }
                _ => autopilot::Held::Undecided,
            };
            let not_checked = auto_preview.as_ref().and_then(|auto_preview| {
                let rules_alone = auto_preview.by == DecideWith::Rules;
                unchecked_question(record.opened.kind, rules_alone, &auto_preview.reasons)
            });
            let why = not_independent.or(not_checked).or_else(because);
            let plan_in_auto = !late
                && autopilot::settles_a_plan_in_auto(
                    &record.opened,
                    auto_preview.is_some(),
                    !violations.is_empty(),
                );
            if in_autopilot || plan_in_auto {
                autopilot::decide(host, &record, held, why.clone(), &violations)?;
            }
            if !late {
                // In auto the question says why it still came to you, in one line, and so does
                // one autopilot left to you. One autopilot settled keeps what the policy found
                // for you to review, recorded after the decision so it never shows as waiting.
                let says_why = in_autopilot
                    || (record.opened.mode.goes_ahead()
                        && policy_may_settle(host.settings(), record.opened.kind));
                checked(PolicyCheck {
                    id: id.clone(),
                    violations,
                    verdict: None,
                    by: None,
                    judge: None,
                    reasons: says_why.then_some(why).flatten().into_iter().collect(),
                    auto_preview: false,
                })?;
            }
            if let Some(auto_preview) = auto_preview {
                checked(auto_preview_check(auto_preview))?;
            }
            Ok(())
        }
        PolicyOutcome::Decided {
            verdict,
            by,
            judge,
            reasons,
            spot_check,
        } => {
            let unchecked = in_autopilot
                && by == DecideWith::Rules
                && configured(host.settings(), record.opened.kind).decide_with != DecideWith::Rules;
            let no_model = reasons.last().cloned().filter(|_| unchecked);
            let not_checked = unchecked_question(record.opened.kind, judge.is_none(), &reasons);
            checked(PolicyCheck {
                id: id.clone(),
                violations: Vec::new(),
                verdict: Some(verdict),
                by: Some(by),
                judge: judge.clone(),
                reasons,
                auto_preview: late,
            })?;
            if late {
                return Ok(());
            }
            if let Some(why) = no_model {
                autopilot::rules_alone(host, &record.opened, &why)?;
            }
            let otherwise = configured(host.settings(), record.opened.kind).otherwise;
            let Some(action) = action_for(&record.opened.options, verdict, otherwise) else {
                // The verdict asks for you, or nothing offered fits: it waits for you, unless in
                // autopilot, or it is a plan in auto, which never stops for you.
                let plan_in_auto = autopilot::settles_a_plan_in_auto(&record.opened, true, false);
                if in_autopilot || plan_in_auto {
                    let checked = host.read(|state| state.approvals.by_id.get(id).cloned());
                    let record = checked.unwrap_or(record);
                    let held = match verdict {
                        Verdict::Reject => autopilot::Held::Rejected,
                        Verdict::Approve | Verdict::AskYou => autopilot::Held::Undecided,
                    };
                    let why = not_checked.or_else(|| why_asked_of(&record));
                    autopilot::decide(host, &record, held, why, &[])?;
                }
                return Ok(());
            };
            let decided = ApprovalDecided {
                id: id.clone(),
                action,
                text: None,
                by: match (by, judge) {
                    (DecideWith::Rules, _) | (_, None) => Party::Rules,
                    (_, Some(model)) => Party::Judge { model },
                },
                evidence_sha256: record.opened.evidence_sha256.clone(),
                auto_preview: None,
                tasks: Vec::new(),
            };
            host.record(
                task.as_ref(),
                None,
                RunEventKind::Approval(ApprovalEvent::Decided(decided.clone())),
            )?;
            // A worker's question the judge answered in autopilot says who answered and why, as
            // one autopilot answered by its own rule does.
            if in_autopilot && record.opened.kind == ApprovalKind::Question {
                let taken = record
                    .opened
                    .options
                    .iter()
                    .find(|option| option.action == decided.action)
                    .map(|option| option.label.clone());
                let judge = match &decided.by {
                    Party::Judge { model } => Some(model.clone()),
                    _ => None,
                };
                let why = host
                    .read(|state| state.approvals.by_id.get(id).cloned())
                    .and_then(|checked| why_asked_of(&checked));
                if let Some(answered) = taken.and_then(|taken| {
                    autopilot::asked_by_a_worker(
                        host,
                        &record.opened,
                        &taken,
                        why.as_deref(),
                        judge,
                    )
                }) {
                    host.record(
                        task.as_ref(),
                        None,
                        RunEventKind::Blackboard(BlackboardEvent::AnsweredForYou(answered)),
                    )?;
                }
            }
            if spot_check {
                if record.opened.kind == ApprovalKind::Merge {
                    lock(&desk.spot_check_once_merged).insert(id.clone());
                } else {
                    host.record(
                        task.as_ref(),
                        None,
                        RunEventKind::Approval(ApprovalEvent::SpotCheckQueued { id: id.clone() }),
                    )?;
                }
            }
            change::on_decided(host, &record.opened, &decided);
            desk.wake(&decided);
            Ok(())
        }
    }
}

/// Why auto left a question to you: the first hard limit that failed, else what its own check
/// turned on. A check that would approve puts what stopped it last; one that wouldn't puts what
/// held it back first.
fn why_asked(violations: &[Violation], auto_preview: Option<&AutoPreviewRecord>) -> Option<String> {
    if let Some(first) = violations.first() {
        return Some(format!("a hard limit, {}", first.detail.trim()));
    }
    let auto_preview = auto_preview?;
    let reason = match auto_preview.would {
        Verdict::Approve => auto_preview.reasons.last(),
        Verdict::Reject | Verdict::AskYou => auto_preview.reasons.first(),
    };
    reason
        .map(|reason| reason.trim().to_owned())
        .filter(|reason| !reason.is_empty())
}

/// Why no judge checked an agent's question in autopilot. Only the judge checks one, and
/// your rules never decide it, so when it couldn't, its reason comes last, after theirs.
fn unchecked_question(kind: ApprovalKind, rules_alone: bool, reasons: &[String]) -> Option<String> {
    (kind == ApprovalKind::Question && rules_alone)
        .then(|| reasons.last().cloned())
        .flatten()
}

/// Why an approval that already asked you did, from the policy's check.
fn why_asked_of(record: &ApprovalRecord) -> Option<String> {
    let check = record.policy_check()?;
    check
        .reasons
        .first()
        .cloned()
        .or_else(|| why_asked(&check.violations, None))
}

/// What your settings' verdict does on this question: approve takes its approving option; reject
/// follows `otherwise`. `None` leaves it to you.
///
/// An approved stuck question is one more try by a fallback of another provider, and an approved
/// limit question releases the reserve, which is what the policy's rules for them allow.
fn action_for(
    options: &[AnswerOption],
    verdict: Verdict,
    otherwise: Otherwise,
) -> Option<AnswerAction> {
    let wanted: &[AnswerAction] = match (verdict, otherwise) {
        (Verdict::Approve, _) => &[
            AnswerAction::Approve,
            AnswerAction::Merge,
            AnswerAction::RaiseLimit,
            AnswerAction::Fallback,
        ],
        (Verdict::Reject, Otherwise::SendBack) => {
            &[AnswerAction::SendBack, AnswerAction::RequestChanges]
        }
        (Verdict::Reject, Otherwise::Stop) => &[AnswerAction::Stop, AnswerAction::Abandon],
        (Verdict::Reject, Otherwise::OpenPullRequest) => &[AnswerAction::PullRequest],
        (Verdict::Reject, Otherwise::Ask) | (Verdict::AskYou, _) => &[],
    };
    wanted
        .iter()
        .find(|action| options.iter().any(|option| option.action == **action))
        .cloned()
}

/// Evidence changed under an open approval (main moved, a task changed): withdraw it. The
/// caller opens a new one.
pub fn withdraw(host: &Host, id: &ApprovalId, reason: &str) -> EngineResult<()> {
    let _deciding = lock(&host.shared().desk.deciding);
    withdraw_locked(host, id, reason)
}

fn withdraw_locked(host: &Host, id: &ApprovalId, reason: &str) -> EngineResult<()> {
    let task = host.read(|state| {
        state
            .approvals
            .by_id
            .get(id)
            .filter(|record| record.is_open())
            .map(|record| record.opened.task.clone())
    });
    let Some(task) = task else {
        return Err(EngineError::Refused(format!(
            "approval {id} isn't open, so it can't be withdrawn"
        )));
    };
    host.record(
        task.as_ref(),
        None,
        RunEventKind::Approval(ApprovalEvent::Withdrawn {
            id: id.clone(),
            reason: reason.to_owned(),
        }),
    )?;
    host.shared().desk.release(id);
    Ok(())
}

/// A kind's configured mode, who decides in auto, what a rejection does, and what a hard
/// limit does in autopilot. Kinds without their own settings are yours to decide, and open in
/// the mode all your approvals share, the one the bottom line shows.
pub struct Configured {
    pub mode: ApprovalMode,
    pub decide_with: DecideWith,
    pub otherwise: Otherwise,
    pub at_hard_limit: AtHardLimit,
}

pub fn configured(settings: &Settings, kind: ApprovalKind) -> Configured {
    own_settings(settings, kind).unwrap_or(Configured {
        mode: settings.approvals.mode(),
        decide_with: DecideWith::default(),
        otherwise: Otherwise::Ask,
        at_hard_limit: AtHardLimit::default(),
    })
}

/// Whether your rules or the judge may settle an approval of `kind` in auto. The others wait
/// for you in every mode but autopilot.
fn policy_may_settle(settings: &Settings, kind: ApprovalKind) -> bool {
    own_settings(settings, kind).is_some()
}

/// A kind's own settings, for the kinds that have them. The `autopilot` switch puts each one a
/// run opens in autopilot.
fn own_settings(settings: &Settings, kind: ApprovalKind) -> Option<Configured> {
    let approvals = &settings.approvals;
    macro_rules! from {
        ($point:expr) => {
            Configured {
                mode: approvals.run_mode($point.mode),
                decide_with: $point.decide_with,
                otherwise: $point.otherwise,
                at_hard_limit: $point.at_hard_limit,
            }
        };
    }
    Some(match kind {
        ApprovalKind::Understand => from!(approvals.understand),
        ApprovalKind::Plan => from!(approvals.plan),
        ApprovalKind::Merge => from!(approvals.merge),
        // Decided outside runs, so the switch and the hard-limit choice leave it be.
        ApprovalKind::WorkflowPromotion => Configured {
            mode: approvals.workflow_promotion.mode,
            decide_with: approvals.workflow_promotion.decide_with,
            otherwise: approvals.workflow_promotion.otherwise,
            at_hard_limit: AtHardLimit::default(),
        },
        ApprovalKind::Stuck => from!(approvals.stuck),
        ApprovalKind::Limit => from!(approvals.limit),
        ApprovalKind::ChangeRequest => from!(approvals.change_request),
        ApprovalKind::Fallback
        | ApprovalKind::AgentDown
        | ApprovalKind::GoalChange
        | ApprovalKind::TakeOverReturn
        | ApprovalKind::Question
        | ApprovalKind::OriginalReturned => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_approval_takes_the_questions_approving_option() {
        let merge = options::standard(ApprovalKind::Merge);
        assert_eq!(
            action_for(&merge, Verdict::Approve, Otherwise::Ask),
            Some(AnswerAction::Merge)
        );
        assert_eq!(
            action_for(&merge, Verdict::Reject, Otherwise::OpenPullRequest),
            Some(AnswerAction::PullRequest)
        );
        assert_eq!(action_for(&merge, Verdict::Reject, Otherwise::Ask), None);
        assert_eq!(action_for(&merge, Verdict::AskYou, Otherwise::Stop), None);
        let plan = options::standard(ApprovalKind::Plan);
        assert_eq!(
            action_for(&plan, Verdict::Reject, Otherwise::SendBack),
            Some(AnswerAction::SendBack)
        );
        assert_eq!(
            action_for(&plan, Verdict::Reject, Otherwise::Stop),
            Some(AnswerAction::Abandon)
        );
        let stuck = options::standard(ApprovalKind::Stuck);
        assert_eq!(
            action_for(&stuck, Verdict::Approve, Otherwise::Ask),
            Some(AnswerAction::Fallback)
        );
        let limit = options::standard(ApprovalKind::Limit);
        assert_eq!(
            action_for(&limit, Verdict::Approve, Otherwise::Ask),
            Some(AnswerAction::RaiseLimit)
        );
    }

    /// With every approval point on auto, the bottom line says auto, and an agent's question
    /// opens in auto too, though no rule may settle it: it still waits for you.
    #[test]
    fn a_kind_without_settings_of_its_own_opens_in_the_shared_mode() {
        let mut settings = Settings::default();
        let approvals = &mut settings.approvals;
        for point in [
            &mut approvals.understand,
            &mut approvals.plan,
            &mut approvals.merge,
            &mut approvals.stuck,
            &mut approvals.limit,
            &mut approvals.change_request,
        ] {
            point.mode = ApprovalMode::Auto;
        }
        assert_eq!(settings.approvals.mode(), ApprovalMode::Auto);
        for kind in [ApprovalKind::Question, ApprovalKind::Fallback] {
            assert_eq!(configured(&settings, kind).mode, ApprovalMode::Auto);
            assert!(!policy_may_settle(&settings, kind));
        }
        assert!(policy_may_settle(&settings, ApprovalKind::Merge));
        settings.approvals.autopilot = true;
        assert_eq!(
            configured(&settings, ApprovalKind::Question).mode,
            ApprovalMode::Autopilot
        );
    }
}
