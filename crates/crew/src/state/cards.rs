//! Every card: its spec, state, attempts, worktree, commits, reports, gates, reviews, handoffs,
//! checkpoint tags, and quality debt, plus the pipeline's own bookkeeping in `progress`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use argonauts_core::events::RunEvent;
use argonauts_core::events::card::{
    AttemptCause, CardEvent, CardSpec, CardState, GateOutcome, ReviewEvent,
};
use argonauts_core::events::run::Party;
use argonauts_core::events::vendor::HandoffEvent;
use argonauts_core::handoff::QualityDebt;
use argonauts_core::names::{VendorId, WorkerRef};
use argonauts_core::steps::card::CardReport;
use argonauts_core::{CardId, CommitInfo, DiffStat};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Cards {
    pub by_id: BTreeMap<CardId, CardRecord>,
    /// What the pipeline keeps per card beyond the record. A card without an entry has none.
    #[serde(default)]
    pub progress: BTreeMap<CardId, CardProgress>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardRecord {
    pub spec: CardSpec,
    pub state: CardState,
    /// Attempts started, from 1.
    pub attempts: u32,
    pub owner: Option<WorkerRef>,
    pub reassigned: u32,
    pub worktree: Option<PathBuf>,
    pub branch: Option<String>,
    pub base: Option<String>,
    pub commits: Vec<CommitInfo>,
    pub reports: Vec<CardReport>,
    pub gates: Vec<GateOutcome>,
    pub reviews: Vec<ReviewEvent>,
    /// The `n` of the newest `handoff-<n>` tag.
    pub handoffs: u32,
    pub debt: Option<QualityDebt>,
}

/// The pipeline's bookkeeping for one card, folded from the same events as its record.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CardProgress {
    pub last_cause: Option<AttemptCause>,
    /// `Card::Attempt` events so far; each one starts exactly one owner session.
    pub attempt_events: u32,
    /// The seq of the newest `Card::Attempt`, to tell whether its session has started.
    pub attempt_seq: Option<u64>,
    /// Deferred because the run's budget ran low, not by you, so more budget brings it back.
    pub deferred_for_budget: bool,
    /// Gate retries by the current owner, which the ladder allows once. A pause, a resume
    /// after a stop, and a context rotation keep the count.
    pub gate_retries: u32,
    /// Fresh sessions on another agent after a failure, which the ladder allows once. Handoffs
    /// for budgets or usage limits, hand-backs, races, and your reassignments don't count.
    #[serde(default)]
    pub ladder_retries: u32,
    /// Times the current owner got the card back from integration.
    #[serde(default)]
    pub integration_returns: u32,
    /// Attempts granted after the card escalated, each one a move from `Escalated` back to
    /// `Ready`. The escalation rules cap these at `max_extra_attempts`.
    #[serde(default)]
    pub extra_attempts: u32,
    /// Review rounds that asked for changes.
    pub change_rounds: u32,
    /// The newest saved diff against the base.
    pub diff: Option<(DiffStat, PathBuf)>,
    /// The newest handoff packet written for this card.
    pub packet: Option<PathBuf>,
    /// Who had the card before its newest reassignment. That vendor's handoff rules say what
    /// the new owner is told.
    #[serde(default)]
    pub handed_from: Option<WorkerRef>,
    /// Set while the card waits for its vendor to come back.
    pub waiting_for: Option<VendorId>,
    /// Times it waited for its vendor, which makes each next wait with no reset to wait for
    /// longer.
    #[serde(default)]
    pub vendor_waits: u32,
    /// What your take-over changed and its commit, once you're done with it, until the card
    /// is taken over again.
    pub take_over_end: Option<(DiffStat, Option<String>)>,
    /// Events folded for this card, so a waiting card is looked at again whenever it changes.
    pub changes: u64,
}

/// Progress of a card nothing has happened to yet.
static NO_PROGRESS: CardProgress = CardProgress {
    last_cause: None,
    attempt_events: 0,
    attempt_seq: None,
    deferred_for_budget: false,
    gate_retries: 0,
    ladder_retries: 0,
    integration_returns: 0,
    extra_attempts: 0,
    change_rounds: 0,
    diff: None,
    packet: None,
    handed_from: None,
    waiting_for: None,
    vendor_waits: 0,
    take_over_end: None,
    changes: 0,
};

impl CardRecord {
    pub fn new(spec: CardSpec) -> Self {
        Self {
            owner: spec.owner.clone(),
            spec,
            state: CardState::Waiting,
            attempts: 0,
            reassigned: 0,
            worktree: None,
            branch: None,
            base: None,
            commits: Vec::new(),
            reports: Vec::new(),
            gates: Vec::new(),
            reviews: Vec::new(),
            handoffs: 0,
            debt: None,
        }
    }

    /// Whether this commit is already recorded.
    pub fn has_commit(&self, sha: &str) -> bool {
        self.commits
            .iter()
            .any(|commit| commit.sha.as_deref() == Some(sha))
    }

    pub fn newest_report(&self) -> Option<&CardReport> {
        self.reports.last()
    }
}

impl Cards {
    pub fn apply(&mut self, event: &CardEvent, envelope: &RunEvent) {
        if let CardEvent::Created(spec) = event {
            let id = spec.id.clone();
            self.by_id
                .entry(id.clone())
                .and_modify(|record| record.spec = (**spec).clone())
                .or_insert_with(|| CardRecord::new((**spec).clone()));
            self.progress.entry(id).or_default().changes += 1;
            return;
        }
        let Some(card) = envelope.card_id.as_ref() else {
            return;
        };
        let Some(record) = self.by_id.get_mut(card) else {
            return;
        };
        let progress = self.progress.entry(card.clone()).or_default();
        progress.changes += 1;
        match event {
            CardEvent::Created(_) => {}
            CardEvent::State { from, to, .. } => {
                record.state = *to;
                progress.deferred_for_budget =
                    *to == CardState::Deferred && *from == Some(CardState::Ready);
                if *from == Some(CardState::Integrating) && *to == CardState::Running {
                    progress.integration_returns += 1;
                }
                if *from == Some(CardState::Escalated) && *to == CardState::Ready {
                    progress.extra_attempts += 1;
                }
            }
            CardEvent::Attempt {
                attempt,
                worker,
                cause,
            } => {
                record.attempts = record.attempts.max(*attempt);
                record.owner = Some(worker.clone());
                progress.last_cause = Some(*cause);
                progress.waiting_for = None;
                match cause {
                    AttemptCause::GateRetry => progress.gate_retries += 1,
                    AttemptCause::ReviewChanges => {
                        progress.change_rounds += 1;
                        progress.gate_retries = 0;
                    }
                    AttemptCause::IntegrationFailed
                    | AttemptCause::Rotation
                    | AttemptCause::Resumed => {}
                    // A new try at the task's goal gets its own check retry.
                    AttemptCause::First
                    | AttemptCause::GoalRetry
                    | AttemptCause::AfterFailure
                    | AttemptCause::Handoff
                    | AttemptCause::Reassigned => progress.gate_retries = 0,
                }
                progress.attempt_events += 1;
                progress.attempt_seq = Some(envelope.seq);
            }
            CardEvent::Worktree { path, branch, base } => {
                record.worktree = Some(path.clone());
                record.branch = Some(branch.clone());
                record.base = Some(base.clone());
            }
            CardEvent::Committed(commit) => {
                let known = commit
                    .sha
                    .as_deref()
                    .is_some_and(|sha| record.has_commit(sha));
                if !known {
                    record.commits.push(commit.clone());
                }
            }
            CardEvent::Reported(report) => record.reports.push((**report).clone()),
            CardEvent::Diff { stat, path } => progress.diff = Some((*stat, path.clone())),
            CardEvent::TakenOver { .. } => {
                progress.take_over_end = None;
                progress.integration_returns = 0;
            }
            CardEvent::TakeOverEnded { changed, commit } => {
                progress.take_over_end = Some((*changed, commit.clone()));
            }
            CardEvent::Reassigned { from, to, by, .. } => {
                // Only the conductor or your rules move a failed card on their own, and that
                // is the ladder's retry.
                if record.state == CardState::Failed
                    && matches!(by, Party::Conductor | Party::Rules)
                {
                    progress.ladder_retries += 1;
                }
                progress.handed_from = from
                    .clone()
                    .or_else(|| record.owner.clone())
                    .or_else(|| record.spec.owner.clone());
                record.owner = Some(to.clone());
                record.reassigned += 1;
                progress.gate_retries = 0;
                progress.integration_returns = 0;
            }
            CardEvent::CheckpointTagged { n, .. } => record.handoffs = record.handoffs.max(*n),
            CardEvent::QualityDebt(debt) => record.debt = Some(debt.clone()),
            CardEvent::DebtCleared { .. } => record.debt = None,
            CardEvent::Reverted { to, .. } => {
                // Work after checkpoint `to` is gone from the branch, and so is its debt.
                if record.debt.as_ref().is_some_and(|debt| debt.since >= *to) {
                    record.debt = None;
                }
            }
        }
    }

    pub fn apply_gate(&mut self, event: &GateOutcome, envelope: &RunEvent) {
        if let Some(record) = self.record_mut(envelope) {
            record.gates.push(event.clone());
            self.touch(envelope);
        }
    }

    pub fn apply_review(&mut self, event: &ReviewEvent, envelope: &RunEvent) {
        if let Some(record) = self.record_mut(envelope) {
            record.reviews.push(event.clone());
            self.touch(envelope);
        }
    }

    pub fn apply_handoff(&mut self, event: &HandoffEvent, _envelope: &RunEvent) {
        let card = match event {
            HandoffEvent::Needed(plan) => &plan.card,
            HandoffEvent::Packet { card, .. }
            | HandoffEvent::HandedOff { card, .. }
            | HandoffEvent::Waiting { card, .. } => card,
        };
        let Some(record) = self.by_id.get_mut(card) else {
            return;
        };
        let progress = self.progress.entry(card.clone()).or_default();
        progress.changes += 1;
        match event {
            HandoffEvent::Needed(_) => {}
            HandoffEvent::Packet { path, .. } => progress.packet = Some(path.clone()),
            HandoffEvent::HandedOff { to, .. } => {
                record.owner = Some(to.clone());
                progress.waiting_for = None;
            }
            HandoffEvent::Waiting { vendor, .. } => {
                progress.waiting_for = Some(vendor.clone());
                progress.vendor_waits += 1;
            }
        }
    }

    pub fn get(&self, card: &CardId) -> Option<&CardRecord> {
        self.by_id.get(card)
    }

    /// The pipeline's bookkeeping for `card`, empty when nothing has happened to it yet.
    pub fn progress(&self, card: &CardId) -> &CardProgress {
        self.progress.get(card).unwrap_or(&NO_PROGRESS)
    }

    /// Cards in `state`, in id order.
    pub fn in_state(&self, state: CardState) -> Vec<CardId> {
        self.by_id
            .values()
            .filter(|record| record.state == state)
            .map(|record| record.spec.id.clone())
            .collect()
    }

    fn record_mut(&mut self, envelope: &RunEvent) -> Option<&mut CardRecord> {
        let card = envelope.card_id.as_ref()?;
        self.by_id.get_mut(card)
    }

    fn touch(&mut self, envelope: &RunEvent) {
        if let Some(card) = envelope.card_id.as_ref() {
            self.progress.entry(card.clone()).or_default().changes += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::RunEventKind;
    use argonauts_core::events::card::GateKind;
    use argonauts_core::settings::handoff::HandoffMode;

    use super::*;
    use crate::testutil::{card_spec, run_event};

    fn worker(name: &str) -> WorkerRef {
        name.parse().unwrap()
    }

    fn card(kind: CardEvent) -> RunEventKind {
        RunEventKind::Card(kind)
    }

    fn fold(events: Vec<RunEvent>) -> Cards {
        let mut cards = Cards::default();
        for event in &events {
            match &event.kind {
                RunEventKind::Card(e) => cards.apply(e, event),
                RunEventKind::Gate(e) => cards.apply_gate(e, event),
                RunEventKind::Review(e) => cards.apply_review(e, event),
                RunEventKind::Handoff(e) => cards.apply_handoff(e, event),
                _ => {}
            }
        }
        cards
    }

    fn id(card: &str) -> CardId {
        CardId::new(card).unwrap()
    }

    #[test]
    fn created_cards_wait_with_their_planned_owner() {
        let spec = card_spec("task-001", &[], Some("claude-code/opus"));
        let cards = fold(vec![run_event(
            1,
            Some("task-001"),
            None,
            card(CardEvent::Created(Box::new(spec.clone()))),
        )]);
        let record = cards.get(&id("task-001")).unwrap();
        assert_eq!(record.state, CardState::Waiting);
        assert_eq!(record.owner, Some(worker("claude-code/opus")));
        assert_eq!(record.spec, spec);
        assert_eq!(cards.in_state(CardState::Waiting), [id("task-001")]);
    }

    #[test]
    fn states_attempts_worktrees_and_commits_fold() {
        let c = Some("task-001");
        let commit = CommitInfo {
            branch: "argonauts/run-01/task-001".into(),
            sha: Some("abc".into()),
            message: "task-001 s-001".into(),
            stat: DiffStat::default(),
            by: Some(worker("codex")),
        };
        let cards = fold(vec![
            run_event(
                1,
                c,
                None,
                card(CardEvent::Created(Box::new(card_spec(
                    "task-001",
                    &[],
                    None,
                )))),
            ),
            run_event(
                2,
                c,
                None,
                card(CardEvent::State {
                    from: None,
                    to: CardState::Ready,
                    reason: "ready".into(),
                }),
            ),
            run_event(
                3,
                c,
                None,
                card(CardEvent::Worktree {
                    path: "/w/task-001".into(),
                    branch: "argonauts/run-01/task-001".into(),
                    base: "base".into(),
                }),
            ),
            run_event(
                4,
                c,
                Some("s-001"),
                card(CardEvent::Attempt {
                    attempt: 1,
                    worker: worker("codex"),
                    cause: AttemptCause::First,
                }),
            ),
            run_event(
                5,
                c,
                Some("s-001"),
                card(CardEvent::Committed(commit.clone())),
            ),
            run_event(
                6,
                c,
                Some("s-001"),
                card(CardEvent::Committed(commit.clone())),
            ),
            run_event(
                7,
                c,
                None,
                card(CardEvent::Diff {
                    stat: DiffStat {
                        files_changed: 1,
                        insertions: 2,
                        deletions: 0,
                    },
                    path: "/d/1.diff".into(),
                }),
            ),
            run_event(
                8,
                c,
                Some("s-001"),
                card(CardEvent::Attempt {
                    attempt: 1,
                    worker: worker("codex"),
                    cause: AttemptCause::GateRetry,
                }),
            ),
            run_event(
                9,
                c,
                Some("s-001"),
                card(CardEvent::Attempt {
                    attempt: 1,
                    worker: worker("codex"),
                    cause: AttemptCause::ReviewChanges,
                }),
            ),
        ]);
        let record = cards.get(&id("task-001")).unwrap();
        let progress = cards.progress(&id("task-001"));
        assert_eq!(record.state, CardState::Ready);
        assert_eq!(record.attempts, 1);
        assert_eq!(record.owner, Some(worker("codex")));
        assert_eq!(progress.attempt_events, 3);
        assert_eq!(progress.attempt_seq, Some(9));
        assert_eq!(record.worktree, Some(PathBuf::from("/w/task-001")));
        assert_eq!(record.base.as_deref(), Some("base"));
        assert_eq!(record.commits, [commit]);
        assert_eq!(progress.diff.as_ref().unwrap().0.insertions, 2);
        assert_eq!(progress.change_rounds, 1);
        assert_eq!(
            progress.gate_retries, 0,
            "a review round starts the checks ladder over"
        );
        assert_eq!(progress.last_cause, Some(AttemptCause::ReviewChanges));
    }

    #[test]
    fn only_a_ready_card_deferred_waits_for_budget() {
        let c = Some("task-001");
        let state = |seq, from, to| {
            run_event(
                seq,
                c,
                None,
                card(CardEvent::State {
                    from: Some(from),
                    to,
                    reason: String::new(),
                }),
            )
        };
        let created = run_event(
            1,
            c,
            None,
            card(CardEvent::Created(Box::new(card_spec(
                "task-001",
                &[],
                None,
            )))),
        );
        let cards = fold(vec![
            created.clone(),
            state(2, CardState::Waiting, CardState::Ready),
            state(3, CardState::Ready, CardState::Deferred),
        ]);
        assert!(cards.progress(&id("task-001")).deferred_for_budget);
        let cards = fold(vec![
            created,
            state(2, CardState::Running, CardState::Escalated),
            state(3, CardState::Escalated, CardState::Deferred),
        ]);
        assert!(!cards.progress(&id("task-001")).deferred_for_budget);
    }

    #[test]
    fn each_move_out_of_escalation_back_in_line_is_an_extra_attempt() {
        let c = Some("task-001");
        let state = |seq, from, to| {
            run_event(
                seq,
                c,
                None,
                card(CardEvent::State {
                    from: Some(from),
                    to,
                    reason: String::new(),
                }),
            )
        };
        let created = run_event(
            1,
            c,
            None,
            card(CardEvent::Created(Box::new(card_spec(
                "task-001",
                &[],
                None,
            )))),
        );
        let cards = fold(vec![
            created,
            state(2, CardState::Running, CardState::Escalated),
            state(3, CardState::Escalated, CardState::Ready),
            state(4, CardState::Ready, CardState::Running),
            state(5, CardState::Running, CardState::NeedsDecision),
            state(6, CardState::NeedsDecision, CardState::Ready),
            state(7, CardState::Running, CardState::Escalated),
            state(8, CardState::Escalated, CardState::Ready),
        ]);
        assert_eq!(cards.progress(&id("task-001")).extra_attempts, 2);
    }

    #[test]
    fn gate_retries_reset_on_a_fresh_session() {
        let c = Some("task-001");
        let attempt = |seq, n, cause| {
            run_event(
                seq,
                c,
                None,
                card(CardEvent::Attempt {
                    attempt: n,
                    worker: worker("codex"),
                    cause,
                }),
            )
        };
        let cards = fold(vec![
            run_event(
                1,
                c,
                None,
                card(CardEvent::Created(Box::new(card_spec(
                    "task-001",
                    &[],
                    None,
                )))),
            ),
            attempt(2, 1, AttemptCause::First),
            attempt(3, 1, AttemptCause::GateRetry),
            attempt(4, 1, AttemptCause::Resumed),
            attempt(5, 1, AttemptCause::Rotation),
        ]);
        assert_eq!(
            cards.progress(&id("task-001")).gate_retries,
            1,
            "a resume or a rotation of the same worker keeps the checks retry used"
        );
        let cards = fold(vec![
            run_event(
                1,
                c,
                None,
                card(CardEvent::Created(Box::new(card_spec(
                    "task-001",
                    &[],
                    None,
                )))),
            ),
            attempt(2, 1, AttemptCause::First),
            attempt(3, 1, AttemptCause::GateRetry),
            run_event(
                4,
                c,
                None,
                card(CardEvent::Reassigned {
                    from: Some(worker("codex")),
                    to: worker("agy"),
                    by: Party::Conductor,
                    rule_override: None,
                }),
            ),
            attempt(5, 2, AttemptCause::AfterFailure),
        ]);
        let record = cards.get(&id("task-001")).unwrap();
        assert_eq!(cards.progress(&id("task-001")).gate_retries, 0);
        assert_eq!(record.reassigned, 1);
        assert_eq!(record.attempts, 2);
    }

    #[test]
    fn only_an_automatic_move_of_a_failed_card_is_a_ladder_retry() {
        let c = Some("task-001");
        let state = |seq, from, to| {
            run_event(
                seq,
                c,
                None,
                card(CardEvent::State {
                    from: Some(from),
                    to,
                    reason: String::new(),
                }),
            )
        };
        let reassigned = |seq, to: &str, by| {
            run_event(
                seq,
                c,
                None,
                card(CardEvent::Reassigned {
                    from: None,
                    to: worker(to),
                    by,
                    rule_override: None,
                }),
            )
        };
        let created = run_event(
            1,
            c,
            None,
            card(CardEvent::Created(Box::new(card_spec(
                "task-001",
                &[],
                None,
            )))),
        );
        let cards = fold(vec![
            created.clone(),
            state(2, CardState::Running, CardState::NeedsDecision),
            reassigned(3, "agy", Party::Conductor),
            state(4, CardState::NeedsDecision, CardState::Ready),
            state(5, CardState::Running, CardState::Failed),
            reassigned(6, "codex", Party::User),
        ]);
        let record = cards.get(&id("task-001")).unwrap();
        assert_eq!(record.reassigned, 2);
        assert_eq!(cards.progress(&id("task-001")).ladder_retries, 0);

        let cards = fold(vec![
            created,
            state(2, CardState::Running, CardState::Failed),
            reassigned(3, "agy", Party::Rules),
            state(4, CardState::Failed, CardState::Ready),
        ]);
        assert_eq!(cards.progress(&id("task-001")).ladder_retries, 1);
    }

    #[test]
    fn integration_returns_count_per_owner() {
        let c = Some("task-001");
        let back = |seq| {
            run_event(
                seq,
                c,
                None,
                card(CardEvent::State {
                    from: Some(CardState::Integrating),
                    to: CardState::Running,
                    reason: String::new(),
                }),
            )
        };
        let created = run_event(
            1,
            c,
            None,
            card(CardEvent::Created(Box::new(card_spec(
                "task-001",
                &[],
                None,
            )))),
        );
        let cards = fold(vec![created.clone(), back(2), back(3)]);
        assert_eq!(cards.progress(&id("task-001")).integration_returns, 2);
        let reassigned = run_event(
            4,
            c,
            None,
            card(CardEvent::Reassigned {
                from: None,
                to: worker("agy"),
                by: Party::User,
                rule_override: None,
            }),
        );
        let cards = fold(vec![created, back(2), back(3), reassigned]);
        assert_eq!(cards.progress(&id("task-001")).integration_returns, 0);
    }

    #[test]
    fn gates_reviews_and_reports_are_kept_per_task() {
        let c = Some("task-002");
        let gate = GateOutcome {
            name: "test".into(),
            kind: GateKind::Command,
            passed: false,
            command: Some(vec!["cargo".into(), "test".into()]),
            exit_code: Some(101),
            duration_ms: 10,
            summary: "1 failed".into(),
            output_tail: "boom".into(),
            violations: Vec::new(),
            sandboxed: true,
            skipped: false,
        };
        let written = ReviewEvent::TestsWritten {
            reviewer: worker("agy"),
            for_card: id("task-001"),
            files: vec!["tests/a.rs".into()],
        };
        let cards = fold(vec![
            run_event(
                1,
                c,
                None,
                card(CardEvent::Created(Box::new(card_spec(
                    "task-002",
                    &[],
                    None,
                )))),
            ),
            run_event(2, c, None, RunEventKind::Gate(gate.clone())),
            run_event(3, c, None, RunEventKind::Review(written.clone())),
            run_event(4, None, None, RunEventKind::Gate(gate.clone())),
            run_event(5, Some("task-404"), None, RunEventKind::Gate(gate.clone())),
        ]);
        let record = cards.get(&id("task-002")).unwrap();
        assert_eq!(record.gates, [gate]);
        assert_eq!(record.reviews, [written]);
        assert_eq!(cards.by_id.len(), 1);
        assert_eq!(cards.progress(&id("task-002")).changes, 3);
        assert_eq!(
            cards.progress(&id("task-404")),
            &CardProgress::default(),
            "a task the log never named has no progress"
        );
    }

    #[test]
    fn handoff_checkpoints_debt_and_revert_fold() {
        let c = Some("task-001");
        let debt = QualityDebt {
            since: 2,
            by: worker("agy"),
            needs: worker("claude-code"),
        };
        let base = vec![
            run_event(
                1,
                c,
                None,
                card(CardEvent::Created(Box::new(card_spec(
                    "task-001",
                    &[],
                    Some("claude-code"),
                )))),
            ),
            run_event(
                2,
                c,
                None,
                card(CardEvent::CheckpointTagged {
                    n: 1,
                    commit: "a".into(),
                    from: worker("claude-code"),
                    to: vec![worker("codex")],
                    reason: "usage limit".into(),
                }),
            ),
            run_event(
                3,
                c,
                None,
                card(CardEvent::CheckpointTagged {
                    n: 2,
                    commit: "b".into(),
                    from: worker("codex"),
                    to: vec![worker("agy")],
                    reason: "usage limit".into(),
                }),
            ),
            run_event(4, c, None, card(CardEvent::QualityDebt(debt.clone()))),
            run_event(
                5,
                None,
                None,
                RunEventKind::Handoff(HandoffEvent::Packet {
                    card: id("task-001"),
                    path: "/p/packet.md".into(),
                    sha256: "00".into(),
                }),
            ),
            run_event(
                6,
                None,
                None,
                RunEventKind::Handoff(HandoffEvent::Waiting {
                    card: id("task-001"),
                    vendor: VendorId("claude-code".into()),
                    until: None,
                }),
            ),
        ];
        let cards = fold(base.clone());
        let record = cards.get(&id("task-001")).unwrap();
        assert_eq!(record.handoffs, 2);
        assert_eq!(record.debt, Some(debt.clone()));
        assert_eq!(
            cards.progress(&id("task-001")).packet,
            Some(PathBuf::from("/p/packet.md"))
        );
        assert_eq!(
            cards.progress(&id("task-001")).waiting_for,
            Some(VendorId("claude-code".into()))
        );

        let mut handed = base.clone();
        handed.push(run_event(
            7,
            None,
            None,
            RunEventKind::Handoff(HandoffEvent::HandedOff {
                card: id("task-001"),
                to: worker("agy"),
                mode: HandoffMode::Auto,
                by: Party::Rules,
                context: None,
            }),
        ));
        handed.push(run_event(
            8,
            c,
            None,
            card(CardEvent::DebtCleared {
                by: worker("claude-code"),
            }),
        ));
        let cards = fold(handed);
        let record = cards.get(&id("task-001")).unwrap();
        assert_eq!(record.owner, Some(worker("agy")));
        assert_eq!(cards.progress(&id("task-001")).waiting_for, None);
        assert_eq!(record.debt, None);

        let mut reverted = base.clone();
        reverted.push(run_event(
            7,
            c,
            None,
            card(CardEvent::Reverted {
                to: 3,
                kept_on: "x".into(),
            }),
        ));
        assert_eq!(
            fold(reverted).get(&id("task-001")).unwrap().debt,
            Some(debt.clone())
        );
        let mut reverted = base;
        reverted.push(run_event(
            7,
            c,
            None,
            card(CardEvent::Reverted {
                to: 2,
                kept_on: "x".into(),
            }),
        ));
        assert_eq!(fold(reverted).get(&id("task-001")).unwrap().debt, None);
    }
}
