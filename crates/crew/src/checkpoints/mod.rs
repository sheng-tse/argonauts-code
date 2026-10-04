//! Decision points. Every checkpoint is opened with an evidence packet bound by its hash and
//! the options its kind offers; your policy evaluates it beside the loop, never blocking it.
//! The first decision wins: a late policy outcome is kept only as a shadow record, and a late
//! answer of yours gets "already decided".

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard, PoisonError};

use tokio::sync::oneshot;

use argonauts_core::api::CheckpointAnswer;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::checkpoint::{
    AnswerAction, AnswerOption, CheckpointDecided, CheckpointEvent, CheckpointKind,
    CheckpointOpened, CheckpointPacket, FreeText, PolicyCheck,
};
use argonauts_core::events::run::Party;
use argonauts_core::names::{CheckpointId, WorkerRef};
use argonauts_core::policy::{
    DecisionPoint, EvidenceDocument, EvidenceFacts, EvidencePacket, EvidenceSubject, PolicyOutcome,
    ShadowRecord, UserDecision, Verdict, Violation,
};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::{Answerer, ApprovalMode, DecideWith, Otherwise};
use argonauts_core::{CardId, Note, NoteLevel};

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::state::RunState;
use crate::state::checkpoints::CheckpointRecord;
use crate::state::ids::IdKind;
use crate::workflow::{StepAt, change, kinds};

pub mod evidence;
pub mod options;

/// What a module asks for when it opens a checkpoint.
#[derive(Debug, Clone)]
pub struct CheckpointRequest {
    pub at: Option<StepAt>,
    pub kind: CheckpointKind,
    /// Such as "Merge this run into main?", the card's heading on screen.
    pub title: String,
    pub card: Option<CardId>,
    pub packet: CheckpointPacket,
    /// Measured facts only. Rules read these.
    pub facts: EvidenceFacts,
    /// Reviewer verdicts, judge scores, differ tags, one line each.
    pub agent_outputs: Vec<String>,
    /// The plan text, the combined diff, and so on. Your approval rules file, or the default
    /// rules without one, is added.
    pub documents: Vec<EvidenceDocument>,
    /// `None` takes the kind's options from [`options::standard`].
    pub options: Option<Vec<AnswerOption>>,
    pub free_text: FreeText,
    /// The approver must not be one of these.
    pub authors: Vec<WorkerRef>,
}

/// Waiters for open checkpoints, and the decision points this process started the policy on.
/// Lives in the engine's shared state. Its lock also orders decisions, so the first one wins.
#[derive(Debug, Default)]
pub struct Desk {
    waiters: Mutex<BTreeMap<CheckpointId, Vec<oneshot::Sender<CheckpointDecided>>>>,
    points: Mutex<BTreeMap<CheckpointId, DecisionPoint>>,
    /// Why a checkpoint goes to you even in auto, when the step that opened it knows, such as
    /// the fact the lead said only you have.
    because: Mutex<BTreeMap<CheckpointId, String>>,
    evaluating: Mutex<BTreeSet<CheckpointId>>,
    /// Auto-approvals you already spot-checked in this process, so a second verdict on one is
    /// refused rather than counted twice.
    spot_checked: Mutex<BTreeSet<CheckpointId>>,
    deciding: Mutex<()>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Desk {
    /// Wakes whoever waits on this checkpoint.
    pub fn wake(&self, decided: &CheckpointDecided) {
        for waiter in lock(&self.waiters).remove(&decided.id).unwrap_or_default() {
            // A waiter that went away (its step was interrupted) re-enters and finds the
            // decision in the state instead.
            let _ = waiter.send(decided.clone());
        }
    }

    fn wait(&self, id: &CheckpointId) -> oneshot::Receiver<CheckpointDecided> {
        let (sender, receiver) = oneshot::channel();
        lock(&self.waiters)
            .entry(id.clone())
            .or_default()
            .push(sender);
        receiver
    }

    /// Drops the waiters, whose receivers then see the checkpoint go.
    fn release(&self, id: &CheckpointId) {
        lock(&self.waiters).remove(id);
    }

    fn point(&self, id: &CheckpointId) -> Option<DecisionPoint> {
        lock(&self.points).get(id).cloned()
    }
}

/// Opens the checkpoint (or finds the one this step already opened), starts the policy beside
/// the loop, and waits for the first decision. A decided checkpoint returns at once, so a
/// re-entered step picks up your answer. `Refused` when it's withdrawn while waiting.
pub async fn open_and_wait(
    host: &Host,
    request: CheckpointRequest,
) -> EngineResult<CheckpointDecided> {
    open(host, request)?.decided(host).await
}

/// [`open_and_wait`] for a checkpoint the step knows auto can't settle, and `because` says why
/// on its card.
pub async fn open_and_wait_because(
    host: &Host,
    request: CheckpointRequest,
    because: Option<String>,
) -> EngineResult<CheckpointDecided> {
    open_because(host, request, because)?.decided(host).await
}

/// [`open_and_wait`] for a checkpoint that comes to you whatever the mode, such as one about a
/// topic `approvals.ask_you_about` names: neither your rules nor the approver decide it, and
/// `because` says why on its card.
pub async fn open_and_wait_yours(
    host: &Host,
    request: CheckpointRequest,
    because: String,
) -> EngineResult<CheckpointDecided> {
    open_yours(host, request, because)?.decided(host).await
}

/// [`open_and_wait_yours`] without waiting.
pub fn open_yours(
    host: &Host,
    request: CheckpointRequest,
    because: String,
) -> EngineResult<Opening> {
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

/// Why a card about `topic` waits for you though the lead answers.
pub fn yours_because(topic: &str) -> String {
    format!("approvals.ask_you_about keeps {topic} yours")
}

/// An opened checkpoint, or the decision it already has.
pub enum Opening {
    Decided(Box<CheckpointDecided>),
    Waiting {
        id: CheckpointId,
        receiver: oneshot::Receiver<CheckpointDecided>,
    },
}

impl Opening {
    pub fn id(&self) -> &CheckpointId {
        match self {
            Self::Decided(decided) => &decided.id,
            Self::Waiting { id, .. } => id,
        }
    }

    /// Waits for the first decision. `Interrupted` on a pause or stop, and `Refused` when the
    /// checkpoint is withdrawn.
    pub async fn decided(self, host: &Host) -> EngineResult<CheckpointDecided> {
        let (id, receiver) = match self {
            Self::Decided(decided) => return Ok(*decided),
            Self::Waiting { id, receiver } => (id, receiver),
        };
        tokio::select! {
            decided = receiver => decided.map_err(|_| {
                let reason = host
                    .read(|state| {
                        state.checkpoints.by_id.get(&id).and_then(|record| record.withdrawn.clone())
                    })
                    .unwrap_or_else(|| "the run stopped".to_owned());
                EngineError::Refused(format!("checkpoint {id} was withdrawn: {reason}"))
            }),
            () = host.interrupted().cancelled() => Err(EngineError::Interrupted),
        }
    }

    /// [`Self::decided`], or why it was withdrawn instead, for a caller that goes on without
    /// an answer then.
    pub async fn decided_or_withdrawn(
        self,
        host: &Host,
    ) -> EngineResult<Result<CheckpointDecided, String>> {
        let id = self.id().clone();
        match self.decided(host).await {
            Ok(decided) => Ok(Ok(decided)),
            Err(EngineError::Refused(refused)) => {
                let withdrawn = host.read(|state| {
                    state
                        .checkpoints
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

/// Opens the checkpoint without waiting: finds the one this step already opened (withdrawing
/// it when its evidence changed), or records a new one, and starts the policy on it.
pub fn open(host: &Host, request: CheckpointRequest) -> EngineResult<Opening> {
    open_because(host, request, None)
}

fn open_because(
    host: &Host,
    request: CheckpointRequest,
    because: Option<String>,
) -> EngineResult<Opening> {
    open_with(host, request, because, true)
}

/// Opens the checkpoint; with `policy` off only you decide it.
fn open_with(
    host: &Host,
    request: CheckpointRequest,
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
        spawn_policy(host, point, evidence);
    }
    Ok(Opening::Waiting { id, receiver })
}

enum Found {
    Decided(Box<CheckpointDecided>),
    Open {
        id: CheckpointId,
        policy_done: bool,
    },
    /// Open for the same kind and card, but over evidence that has since changed.
    Stale(CheckpointId),
    None,
}

/// At most one checkpoint is open per kind and card. A step re-entered after a crash or a
/// park gets back the one it opened in this round, open or decided, when the evidence is the
/// same.
fn find(state: &RunState, request: &CheckpointRequest, sha256: &str) -> Found {
    if is_workflow_change(request.kind, request.card.as_ref()) {
        return find_workflow_change(state, request, sha256);
    }
    if let Some(open) = state
        .checkpoints
        .open_for(request.kind, request.card.as_ref())
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
        .checkpoints
        .by_id
        .values()
        .filter(|record| {
            record.since > round_since
                && record.withdrawn.is_none()
                && record.opened.kind == request.kind
                && record.opened.card == request.card
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

/// The step's newest checkpoint of `kind` and `title` in this round, when it's decided, with
/// the subject its evidence named. A re-entered step whose subject hasn't moved can act on the
/// answer without building its evidence again.
pub fn decided_in_round(
    host: &Host,
    at: &StepAt,
    kind: CheckpointKind,
    title: &str,
) -> EngineResult<Option<(CheckpointDecided, Option<EvidenceSubject>)>> {
    let decided = host.read(|state| {
        let since = round_since(state, at)?;
        state
            .checkpoints
            .by_id
            .values()
            .filter(|record| {
                record.since > since
                    && record.withdrawn.is_none()
                    && record.opened.kind == kind
                    && record.opened.card.is_none()
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

/// A card about your own files, such as the ones in the way of landing the run: no rule or
/// approver can move them, so only you answer it and the policy never weighs in.
pub fn only_yours(packet: &CheckpointPacket) -> bool {
    matches!(packet, CheckpointPacket::Merge(merge) if merge.waits_on_you())
}

/// Landing what your rules or the approver approved at checkpoint `id` was refused, so the
/// auto-merge it took goes back.
pub fn landing_refused(host: &Host, id: &CheckpointId) {
    let Some(record) = host.read(|state| state.checkpoints.by_id.get(id).cloned()) else {
        return;
    };
    let auto = record
        .decided
        .as_ref()
        .is_some_and(|decided| matches!(decided.by, Party::Rules | Party::Approver { .. }));
    if auto {
        let point = recorded_point(host, &record);
        host.services().policy.landing_refused(&point);
    }
}

/// A change to a running workflow, rather than an owner's change request on a card.
pub fn is_workflow_change(kind: CheckpointKind, card: Option<&CardId>) -> bool {
    kind == CheckpointKind::ChangeRequest && card.is_none()
}

/// Workflow changes are told apart by their title, which names the workflow and, for the
/// lead's, the phase that ended. Two proposals never withdraw each other; only the lead's own
/// proposal for the same phase end, asked again over other evidence, replaces the open one.
fn find_workflow_change(state: &RunState, request: &CheckpointRequest, sha256: &str) -> Found {
    let same = state
        .checkpoints
        .open()
        .filter(|record| {
            is_workflow_change(record.opened.kind, record.opened.card.as_ref())
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

/// The evidence packet the checkpoint was opened with, where `evidence::write` put it, checked
/// against the hash it was opened under.
pub fn evidence_of(host: &Host, opened: &CheckpointOpened) -> EngineResult<EvidencePacket> {
    let path = host
        .repo()
        .run_dir
        .join("evidence")
        .join(format!("{}.json", opened.id));
    let packet: EvidencePacket = serde_json::from_slice(&std::fs::read(&path)?)?;
    if packet.sha256() != opened.evidence_sha256 {
        return Err(EngineError::Refused(format!(
            "the evidence of checkpoint {} changed since it opened",
            opened.id
        )));
    }
    Ok(packet)
}

/// `yours` opens it for you alone, as a supervised checkpoint, whatever the mode.
fn record_opened(
    host: &Host,
    request: &CheckpointRequest,
    evidence: &EvidencePacket,
    sha256: &str,
    yours: bool,
) -> EngineResult<CheckpointId> {
    let id = CheckpointId::numbered(host.allocate(IdKind::Checkpoint)?);
    evidence::write(host, &id, evidence)?;
    let (override_mode, last_round) = host.read(|state| {
        let last_round = request
            .at
            .as_ref()
            .is_some_and(|at| kinds::in_last_loop_round(state, at));
        (state.run.approval_mode, last_round)
    });
    let configured = configured(host.settings(), request.kind);
    // Your records may lower the mode, such as back to shadow after a reverted auto-merge.
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
    let mut options = request
        .options
        .clone()
        .unwrap_or_else(|| options::standard(request.kind));
    // A loop's last round has no round left to send it back to.
    if last_round {
        options.retain(|option| option.action != AnswerAction::SendBack);
    }
    let opened = CheckpointOpened {
        id: id.clone(),
        kind: request.kind,
        title: request.title.clone(),
        card: request.card.clone(),
        packet: request.packet.clone(),
        evidence_sha256: sha256.to_owned(),
        mode,
        decide_with: configured.decide_with,
        options,
        free_text: request.free_text,
        // Only auto can settle it without you; the policy's check says so once it's in.
        needs_you: mode != ApprovalMode::Auto,
    };
    host.record(
        request.card.as_ref(),
        None,
        RunEventKind::Checkpoint(CheckpointEvent::Opened(Box::new(opened))),
    )?;
    Ok(id)
}

/// Shift+Tab's mode, else your settings'. Your records may lower it, such as back to shadow
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

/// The mode a checkpoint of `kind` would open in now.
pub fn mode_now(host: &Host, kind: CheckpointKind) -> ApprovalMode {
    let (mode_override, recipe) = host.read(|state| (state.run.approval_mode, recipe_of(state)));
    let point = DecisionPoint {
        run: host.run_id().clone(),
        // No checkpoint is open yet; the records look only at the kind and the recipe.
        checkpoint: CheckpointId::numbered(0),
        kind,
        recipe,
        authors: Vec::new(),
        mode_override,
    };
    let configured = configured(host.settings(), kind).mode;
    effective_mode(host, &point, mode_override, configured)
}

/// Who answers the crew's questions at a checkpoint of `kind` now: the lead once you said
/// "Yes, and let the lead answer" in this run, else `approvals.answered_by`, else the lead in
/// auto mode and you otherwise.
pub fn answerer(host: &Host, kind: CheckpointKind) -> Answerer {
    if host.read(crate::goal::checkpoint::lead_answers_for_you) {
        return Answerer::Lead;
    }
    host.settings().approvals.answerer(mode_now(host, kind))
}

fn decision_point(host: &Host, id: &CheckpointId, request: &CheckpointRequest) -> DecisionPoint {
    let (mode_override, recipe) = host.read(|state| (state.run.approval_mode, recipe_of(state)));
    DecisionPoint {
        run: host.run_id().clone(),
        checkpoint: id.clone(),
        kind: request.kind,
        recipe,
        authors: request.authors.clone(),
        mode_override,
    }
}

/// The recipe the run's triage picked, which shadow records count per.
fn recipe_of(state: &RunState) -> Option<String> {
    state
        .run
        .started
        .as_ref()
        .and_then(|started| started.triage.as_ref())
        .and_then(|triage| triage.rule.strip_prefix("recipe:"))
        .map(str::to_owned)
}

/// Runs `evaluate` beside the loop, tracked so the loop joins it before the run parks or
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
                lock(&shared.desk.evaluating).remove(&point.checkpoint);
                return;
            }
            // Still deciding after the wind-down grace: evaluated again when the run resumes.
            () = shared.halt.cancelled() => {
                lock(&shared.desk.evaluating).remove(&point.checkpoint);
                return;
            }
        };
        lock(&host.shared().desk.evaluating).remove(&point.checkpoint);
        if let Err(err) = policy_outcome(&host, &point.checkpoint, outcome) {
            // The checkpoint still waits for you, so the log says why the policy never decided.
            let note = Note {
                level: NoteLevel::Error,
                text: format!(
                    "the policy's outcome on {} wasn't recorded: {err}",
                    point.checkpoint
                ),
            };
            if let Err(also) = host.record(None, None, RunEventKind::Note(note)) {
                tracing::warn!(checkpoint = %point.checkpoint, "the policy's outcome wasn't recorded: {err}; nor was the note: {also}");
            }
        }
    });
}

/// Your answer. Refused with the reason when the action wasn't offered, the evidence hash
/// differs, or it was already decided. After it's recorded, `policy.user_decided` is called so
/// shadow records can be scored.
pub fn answer(host: &Host, answer: CheckpointAnswer) -> Result<(), String> {
    let desk = &host.shared().desk;
    let (decided, point, yours) = {
        let _deciding = lock(&desk.deciding);
        let record = host
            .read(|state| state.checkpoints.by_id.get(&answer.id).cloned())
            .ok_or_else(|| format!("no checkpoint {}", answer.id))?;
        if record.decided.is_some() {
            return Err("already decided".to_owned());
        }
        if let Some(reason) = &record.withdrawn {
            return Err(format!("withdrawn: {reason}"));
        }
        if record.opened.evidence_sha256 != answer.evidence_sha256 {
            return Err(
                "the evidence changed since the card was shown; look at it again and answer"
                    .to_owned(),
            );
        }
        check_offered(&record.opened, &answer)?;
        let decided = CheckpointDecided {
            id: answer.id.clone(),
            action: answer.action.clone(),
            text: answer.text.clone(),
            by: Party::User,
            evidence_sha256: answer.evidence_sha256.clone(),
            shadow: shadow_of(&record),
            cards: answer.cards.clone(),
        };
        host.record(
            record.opened.card.as_ref(),
            None,
            RunEventKind::Checkpoint(CheckpointEvent::Decided(decided.clone())),
        )
        .map_err(|err| err.to_string())?;
        if crate::retro::reversals::rejects(record.opened.kind, &decided.action) {
            let what = crate::retro::reversals::at_checkpoint(record.opened.kind, &decided.action);
            crate::retro::reversals::note(host, record.opened.card.as_ref(), what);
        }
        // Before waking the waiter, so it finds the change already recorded.
        change::on_decided(host, &record.opened, &decided);
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
        shadow: decided.shadow,
    });
    Ok(())
}

/// Your verdict on an auto-approval in your spot-check queue, passed to the policy, where a
/// wrong one drops that decision point back to shadow. Refused for a checkpoint that never
/// joined the queue, and for one you already checked.
pub fn spot_check(host: &Host, id: &CheckpointId, correct: bool) -> Result<(), String> {
    let desk = &host.shared().desk;
    let record = host
        .read(|state| state.checkpoints.by_id.get(id).cloned())
        .ok_or_else(|| format!("no checkpoint {id}"))?;
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
            "your spot check found the auto-approval at the {} checkpoint wrong",
            serde_json::to_value(record.opened.kind)
                .ok()
                .and_then(|kind| kind.as_str().map(|kind| kind.replace('_', " ")))
                .unwrap_or_default()
        );
        crate::retro::reversals::note(host, record.opened.card.as_ref(), what);
    }
    Ok(())
}

/// The decision point the policy was started on, or the same point rebuilt from the log when
/// another process started it.
fn recorded_point(host: &Host, record: &CheckpointRecord) -> DecisionPoint {
    let id = &record.opened.id;
    if let Some(point) = host.shared().desk.point(id) {
        return point;
    }
    let (mode_override, recipe) = host.read(|state| (state.run.approval_mode, recipe_of(state)));
    DecisionPoint {
        run: host.run_id().clone(),
        checkpoint: id.clone(),
        kind: record.opened.kind,
        recipe,
        authors: Vec::new(),
        mode_override,
    }
}

fn check_offered(opened: &CheckpointOpened, answer: &CheckpointAnswer) -> Result<(), String> {
    if matches!(answer.action, AnswerAction::ShowDiff) {
        return Err("showing the details doesn't answer the card".to_owned());
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
            "that answer isn't one the card offered ({:?})",
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

/// What auto would have decided, from the shadow check the policy recorded before you answered.
fn shadow_of(record: &CheckpointRecord) -> Option<ShadowRecord> {
    record
        .checks
        .iter()
        .find(|check| check.shadow)
        .and_then(|check| {
            Some(ShadowRecord {
                would: check.verdict?,
                by: check.by.unwrap_or_default(),
                reasons: check.reasons.clone(),
            })
        })
}

/// The policy's outcome, when it arrives. Kept only as a shadow record if you already decided.
pub fn policy_outcome(host: &Host, id: &CheckpointId, outcome: PolicyOutcome) -> EngineResult<()> {
    let desk = &host.shared().desk;
    let _deciding = lock(&desk.deciding);
    let record = host
        .read(|state| state.checkpoints.by_id.get(id).cloned())
        .ok_or_else(|| EngineError::Refused(format!("no checkpoint {id}")))?;
    let card = record.opened.card.clone();
    let checked = |check: PolicyCheck| {
        host.record(
            card.as_ref(),
            None,
            RunEventKind::Checkpoint(CheckpointEvent::Checked(check)),
        )
    };
    let late = !record.is_open() || record.policy_check().is_some();
    match outcome {
        PolicyOutcome::AskUser { violations, shadow } => {
            if !late {
                // In auto the card says why it still came to you, in one line.
                let why = (record.opened.mode == ApprovalMode::Auto)
                    .then(|| {
                        lock(&desk.because)
                            .get(id)
                            .cloned()
                            .or_else(|| why_asked(&violations, shadow.as_ref()))
                    })
                    .flatten();
                checked(PolicyCheck {
                    id: id.clone(),
                    violations,
                    verdict: None,
                    by: None,
                    approver: None,
                    reasons: why.into_iter().collect(),
                    shadow: false,
                })?;
            }
            if let Some(shadow) = shadow {
                checked(PolicyCheck {
                    id: id.clone(),
                    violations: Vec::new(),
                    verdict: Some(shadow.would),
                    by: Some(shadow.by),
                    approver: None,
                    reasons: shadow.reasons,
                    shadow: true,
                })?;
            }
            Ok(())
        }
        PolicyOutcome::Decided {
            verdict,
            by,
            approver,
            reasons,
            spot_check,
        } => {
            checked(PolicyCheck {
                id: id.clone(),
                violations: Vec::new(),
                verdict: Some(verdict),
                by: Some(by),
                approver: approver.clone(),
                reasons,
                shadow: late,
            })?;
            if late {
                return Ok(());
            }
            let otherwise = configured(host.settings(), record.opened.kind).otherwise;
            let Some(action) = action_for(&record.opened.options, verdict, otherwise) else {
                // Escalated, or nothing offered fits: it waits for you.
                return Ok(());
            };
            let decided = CheckpointDecided {
                id: id.clone(),
                action,
                text: None,
                by: match (by, approver) {
                    (DecideWith::Rules, _) | (_, None) => Party::Rules,
                    (_, Some(model)) => Party::Approver { model },
                },
                evidence_sha256: record.opened.evidence_sha256.clone(),
                shadow: None,
                cards: Vec::new(),
            };
            host.record(
                card.as_ref(),
                None,
                RunEventKind::Checkpoint(CheckpointEvent::Decided(decided.clone())),
            )?;
            if spot_check {
                host.record(
                    card.as_ref(),
                    None,
                    RunEventKind::Checkpoint(CheckpointEvent::SpotCheckQueued { id: id.clone() }),
                )?;
            }
            change::on_decided(host, &record.opened, &decided);
            desk.wake(&decided);
            Ok(())
        }
    }
}

/// Why auto left a card to you: the first hard limit that failed, else what its own check
/// turned on. A check that would approve puts what stopped it last; one that wouldn't puts what
/// held it back first.
fn why_asked(violations: &[Violation], shadow: Option<&ShadowRecord>) -> Option<String> {
    if let Some(first) = violations.first() {
        return Some(format!("a hard limit, {}", first.detail.trim()));
    }
    let shadow = shadow?;
    let reason = match shadow.would {
        Verdict::Approve => shadow.reasons.last(),
        Verdict::Reject | Verdict::Escalate => shadow.reasons.first(),
    };
    reason
        .map(|reason| reason.trim().to_owned())
        .filter(|reason| !reason.is_empty())
}

/// What your settings' verdict does on this card: approve takes its approving option; reject
/// follows `otherwise`. `None` leaves it to you.
///
/// An approved escalation is one more attempt by a fallback of another family, and an approved
/// budget card releases the reserve, which is what the policy's rules for them allow.
fn action_for(
    options: &[AnswerOption],
    verdict: Verdict,
    otherwise: Otherwise,
) -> Option<AnswerAction> {
    let wanted: &[AnswerAction] = match (verdict, otherwise) {
        (Verdict::Approve, _) => &[
            AnswerAction::Approve,
            AnswerAction::Merge,
            AnswerAction::MoreBudget,
            AnswerAction::HandOff,
        ],
        (Verdict::Reject, Otherwise::SendBack) => {
            &[AnswerAction::SendBack, AnswerAction::RequestChanges]
        }
        (Verdict::Reject, Otherwise::Stop) => &[AnswerAction::Stop, AnswerAction::Abandon],
        (Verdict::Reject, Otherwise::OpenPullRequest) => &[AnswerAction::PullRequest],
        (Verdict::Reject, Otherwise::Ask) | (Verdict::Escalate, _) => &[],
    };
    wanted
        .iter()
        .find(|action| options.iter().any(|option| option.action == **action))
        .cloned()
}

/// Evidence changed under an open checkpoint (main moved, a card changed): withdraw it. The
/// caller opens a new one.
pub fn withdraw(host: &Host, id: &CheckpointId, reason: &str) -> EngineResult<()> {
    let _deciding = lock(&host.shared().desk.deciding);
    withdraw_locked(host, id, reason)
}

fn withdraw_locked(host: &Host, id: &CheckpointId, reason: &str) -> EngineResult<()> {
    let card = host.read(|state| {
        state
            .checkpoints
            .by_id
            .get(id)
            .filter(|record| record.is_open())
            .map(|record| record.opened.card.clone())
    });
    let Some(card) = card else {
        return Err(EngineError::Refused(format!(
            "checkpoint {id} isn't open, so it can't be withdrawn"
        )));
    };
    host.record(
        card.as_ref(),
        None,
        RunEventKind::Checkpoint(CheckpointEvent::Withdrawn {
            id: id.clone(),
            reason: reason.to_owned(),
        }),
    )?;
    host.shared().desk.release(id);
    Ok(())
}

/// A kind's configured mode, who decides in auto, and what a rejection does. Kinds without
/// their own settings are yours to decide.
pub struct Configured {
    pub mode: ApprovalMode,
    pub decide_with: DecideWith,
    pub otherwise: Otherwise,
}

pub fn configured(settings: &Settings, kind: CheckpointKind) -> Configured {
    let approvals = &settings.approvals;
    macro_rules! from {
        ($point:expr) => {
            Configured {
                mode: $point.mode,
                decide_with: $point.decide_with,
                otherwise: $point.otherwise,
            }
        };
    }
    match kind {
        CheckpointKind::Goal => from!(approvals.goal),
        CheckpointKind::Plan => from!(approvals.plan),
        CheckpointKind::Merge => from!(approvals.merge),
        CheckpointKind::RecipePromotion => from!(approvals.recipe_promotion),
        CheckpointKind::Escalation => from!(approvals.escalation),
        CheckpointKind::Budget => from!(approvals.budget),
        CheckpointKind::ChangeRequest => from!(approvals.change_request),
        CheckpointKind::Handoff
        | CheckpointKind::VendorDown
        | CheckpointKind::GoalChange
        | CheckpointKind::TakeOverReturn
        | CheckpointKind::Question
        | CheckpointKind::OriginalReturned => Configured {
            mode: ApprovalMode::Supervised,
            decide_with: DecideWith::default(),
            otherwise: Otherwise::Ask,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_approval_takes_the_cards_approving_option() {
        let merge = options::standard(CheckpointKind::Merge);
        assert_eq!(
            action_for(&merge, Verdict::Approve, Otherwise::Ask),
            Some(AnswerAction::Merge)
        );
        assert_eq!(
            action_for(&merge, Verdict::Reject, Otherwise::OpenPullRequest),
            Some(AnswerAction::PullRequest)
        );
        assert_eq!(action_for(&merge, Verdict::Reject, Otherwise::Ask), None);
        assert_eq!(action_for(&merge, Verdict::Escalate, Otherwise::Stop), None);
        let plan = options::standard(CheckpointKind::Plan);
        assert_eq!(
            action_for(&plan, Verdict::Reject, Otherwise::SendBack),
            Some(AnswerAction::SendBack)
        );
        assert_eq!(
            action_for(&plan, Verdict::Reject, Otherwise::Stop),
            Some(AnswerAction::Abandon)
        );
        let escalation = options::standard(CheckpointKind::Escalation);
        assert_eq!(
            action_for(&escalation, Verdict::Approve, Otherwise::Ask),
            Some(AnswerAction::HandOff)
        );
        let budget = options::standard(CheckpointKind::Budget);
        assert_eq!(
            action_for(&budget, Verdict::Approve, Otherwise::Ask),
            Some(AnswerAction::MoreBudget)
        );
    }
}
