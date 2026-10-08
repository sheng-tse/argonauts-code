//! What a card's journal says: its events, read cheaply from a long run's log, how many times it
//! was handed off, whether gates ran on its latest commit, and where the run's other cards stand.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use argonauts_core::CardId;
use argonauts_core::events::card::{AttemptCause, CardEvent, CardState};
use argonauts_core::events::vendor::HandoffEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{VendorId, WorkerRef};
use tokio::io::{AsyncBufReadExt, BufReader};

/// What the card's own history says about earlier handoffs and gates.
pub struct History {
    pub handoffs: u32,
    /// Gates ran on the card's latest commit.
    pub gates_ran: bool,
}

impl History {
    /// Checkpoint tags number handoffs exactly, so the highest one counts whenever the card has
    /// any. Without tags, handoff attempts count, where the racers of one race share an attempt
    /// number and count once.
    pub fn of(events: &[RunEvent], card: &CardId) -> Self {
        let mut tagged = 0;
        let mut attempts = BTreeSet::new();
        let mut last_commit = None;
        let mut last_gate = None;
        for event in events
            .iter()
            .filter(|event| event.card_id.as_ref() == Some(card))
        {
            match &event.kind {
                RunEventKind::Card(CardEvent::CheckpointTagged { n, .. }) => {
                    tagged = tagged.max(*n)
                }
                RunEventKind::Card(CardEvent::Attempt {
                    attempt,
                    cause: AttemptCause::Handoff,
                    ..
                }) => {
                    attempts.insert(*attempt);
                }
                RunEventKind::Card(CardEvent::Committed(info)) if info.sha.is_some() => {
                    last_commit = Some(event.seq);
                }
                RunEventKind::Gate(_) => last_gate = Some(event.seq),
                _ => {}
            }
        }
        let handoffs = if tagged > 0 {
            tagged
        } else {
            u32::try_from(attempts.len()).unwrap_or(u32::MAX)
        };
        Self {
            handoffs,
            gates_ran: last_gate.is_some_and(|gate| last_commit.is_none_or(|commit| gate > commit)),
        }
    }
}

/// What a stop reads from the run's log.
#[derive(Debug, Default)]
pub struct CardLog {
    /// The card's events and the run-wide ones its packet reads.
    pub events: Vec<RunEvent>,
    pub placements: Placements,
}

/// Who holds each of the run's cards, for `spread`.
#[derive(Debug, Clone, Default)]
pub struct Placements {
    cards: BTreeMap<CardId, Placement>,
    /// The newest event read, so a pick made after the read can tell when the log catches up.
    pub last_seq: u64,
}

#[derive(Debug, Clone, Default)]
struct Placement {
    worker: Option<WorkerRef>,
    /// A handoff gave the card to `worker`.
    handed_off: bool,
    state: Option<CardState>,
    /// The event that last placed the card.
    seq: u64,
}

impl Placements {
    fn observe(&mut self, event: &RunEvent) {
        self.last_seq = self.last_seq.max(event.seq);
        match (&event.kind, &event.card_id) {
            (RunEventKind::Card(CardEvent::Attempt { worker, cause, .. }), Some(card)) => {
                self.place(card, worker, *cause == AttemptCause::Handoff, event.seq);
            }
            (RunEventKind::Card(CardEvent::Reassigned { to, .. }), Some(card)) => {
                self.place(card, to, false, event.seq);
            }
            (RunEventKind::Handoff(HandoffEvent::HandedOff { card, to, .. }), _) => {
                self.place(card, to, true, event.seq);
            }
            (RunEventKind::Card(CardEvent::State { to, .. }), Some(card)) => {
                self.cards.entry(card.clone()).or_default().state = Some(*to);
            }
            _ => {}
        }
    }

    /// A retry on the same worker keeps how the card got there.
    fn place(&mut self, card: &CardId, worker: &WorkerRef, handed_off: bool, seq: u64) {
        let placement = self.cards.entry(card.clone()).or_default();
        if placement.worker.as_ref() == Some(worker) {
            placement.handed_off |= handed_off;
        } else {
            placement.worker = Some(worker.clone());
            placement.handed_off = handed_off;
        }
        placement.seq = seq;
    }

    /// A handoff decided after reading up to `seen`, which the log may not show yet. Returns
    /// false once the log has placed the card since, so the pick can be forgotten.
    pub fn pick(&mut self, card: &CardId, worker: &WorkerRef, seen: u64) -> bool {
        if self
            .cards
            .get(card)
            .is_some_and(|placement| placement.seq > seen)
        {
            return false;
        }
        self.place(card, worker, true, seen);
        true
    }

    /// The run's cards a handoff gave to `worker`.
    pub fn handed_to(&self, worker: &WorkerRef) -> u32 {
        let count = self
            .cards
            .values()
            .filter(|placement| placement.handed_off && placement.worker.as_ref() == Some(worker))
            .count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    /// The cards other than `except` whose session on `vendor` is running or just stopped.
    pub fn on_vendor(&self, vendor: &VendorId, except: &CardId) -> Vec<CardId> {
        self.cards
            .iter()
            .filter(|(card, placement)| {
                *card != except
                    && placement
                        .worker
                        .as_ref()
                        .is_some_and(|worker| &worker.vendor() == vendor)
                    && placement.state.is_none_or(|state| {
                        matches!(
                            state,
                            CardState::Running | CardState::Blocked | CardState::NeedsDecision
                        )
                    })
            })
            .map(|(card, _)| card.clone())
            .collect()
    }
}

/// Reads the card's events and the run's placements in one pass. Lines that can't be about
/// either are skipped before parsing, so a long run stays cheap, and a line that doesn't parse
/// is skipped too, since a crash can leave the last one half written.
pub async fn card_log(path: &Path, card: &CardId) -> std::io::Result<CardLog> {
    const RUN_WIDE: [&str; 3] = ["\"goal\"", "\"plan\"", "\"blackboard\""];
    const PLACING: [&str; 4] = [
        "\"attempt\"",
        "\"reassigned\"",
        "\"handed_off\"",
        "\"state\"",
    ];
    let file = tokio::fs::File::open(path).await?;
    let mut lines = BufReader::new(file).lines();
    let mut sessions: Vec<String> = Vec::new();
    let mut log = CardLog::default();
    while let Some(line) = lines.next_line().await? {
        let mine = line.contains(card.as_str())
            || RUN_WIDE.iter().any(|tag| line.contains(tag))
            || sessions
                .iter()
                .any(|session| line.contains(session.as_str()));
        let placing = PLACING.iter().any(|tag| line.contains(tag));
        if !mine && !placing {
            continue;
        }
        let Ok(event) = serde_json::from_str::<RunEvent>(&line) else {
            continue;
        };
        log.placements.observe(&event);
        if !mine {
            continue;
        }
        if event.card_id.as_ref() == Some(card)
            && let Some(session) = &event.session
        {
            let key = session.to_string();
            if !sessions.contains(&key) {
                sessions.push(key);
            }
        }
        log.events.push(event);
    }
    Ok(log)
}

#[cfg(test)]
mod tests {
    use argonauts_core::names::SessionKey;
    use argonauts_core::{AgentEvent, Note, NoteLevel};
    use time::OffsetDateTime;

    use super::*;
    use crate::handoff::testing::{CARD, run, vendor, worker};

    fn event(card: Option<&str>, session: Option<u32>, kind: RunEventKind) -> RunEvent {
        RunEvent {
            seq: 0,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: run(),
            card_id: card.map(|id| CardId::new(id).expect("a valid task id")),
            session: session.map(SessionKey::numbered),
            source: "conductor".to_owned(),
            kind,
        }
    }

    fn message(text: &str) -> RunEventKind {
        RunEventKind::Agent(AgentEvent::Message {
            text: text.to_owned(),
            parent: None,
        })
    }

    #[tokio::test]
    async fn card_log_keeps_the_cards_sessions_and_skips_the_rest() {
        let card = CardId::new(CARD).expect("a valid task id");
        let attempt = RunEventKind::Card(CardEvent::Attempt {
            attempt: 1,
            worker: worker("codex/gpt-5"),
            cause: AttemptCause::First,
        });
        let events = [
            event(Some(CARD), Some(1), attempt.clone()),
            event(None, Some(1), message("mine")),
            event(Some("task-004"), Some(2), attempt),
            event(None, Some(2), message("theirs")),
            event(
                None,
                None,
                RunEventKind::Note(Note {
                    level: NoteLevel::Info,
                    text: "run-wide".to_owned(),
                }),
            ),
        ];
        let mut text: String = events
            .iter()
            .map(|event| serde_json::to_string(event).expect("an event serializes") + "\n")
            .collect();
        text.push_str("{\"seq\": 9, \"half written");
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, text).expect("the journal is written");

        let read = card_log(&path, &card).await.expect("the journal reads");
        assert_eq!(read.events, events[..2]);
        // Another card's attempt still places that card.
        assert_eq!(
            read.placements.on_vendor(&vendor("codex"), &card),
            [CardId::new("task-004").expect("a valid task id")]
        );
        assert!(
            card_log(&dir.path().join("missing.jsonl"), &card)
                .await
                .is_err()
        );
    }

    fn attempt(n: u32, to: &str, cause: AttemptCause) -> RunEventKind {
        RunEventKind::Card(CardEvent::Attempt {
            attempt: n,
            worker: worker(to),
            cause,
        })
    }

    fn state(to: CardState) -> RunEventKind {
        RunEventKind::Card(CardEvent::State {
            from: None,
            to,
            reason: String::new(),
        })
    }

    #[test]
    fn placements_follow_handoffs_and_live_cards() {
        let at = |seq: u64, card: &str, kind: RunEventKind| RunEvent {
            seq,
            ..event(Some(card), None, kind)
        };
        let id = |card: &str| CardId::new(card).expect("a valid task id");
        let events = [
            at(
                1,
                "task-004",
                attempt(1, "claude-code/opus", AttemptCause::First),
            ),
            at(2, "task-004", state(CardState::Running)),
            at(
                3,
                "task-005",
                attempt(1, "claude-code/opus", AttemptCause::First),
            ),
            at(4, "task-005", state(CardState::Integrated)),
            at(
                5,
                "task-006",
                RunEventKind::Handoff(HandoffEvent::HandedOff {
                    card: id("task-006"),
                    to: worker("codex/gpt-5"),
                    mode: argonauts_core::settings::handoff::HandoffMode::Auto,
                    by: argonauts_core::events::run::Party::Conductor,
                    context: None,
                }),
            ),
            at(
                6,
                "task-006",
                attempt(2, "codex/gpt-5", AttemptCause::Handoff),
            ),
            at(
                7,
                "task-006",
                attempt(3, "codex/gpt-5", AttemptCause::GateRetry),
            ),
        ];
        let mut placements = Placements::default();
        for event in &events {
            placements.observe(event);
        }
        let claude = vendor("claude-code");
        assert_eq!(placements.last_seq, 7);
        // A finished card has no session to move.
        assert_eq!(placements.on_vendor(&claude, &id(CARD)), [id("task-004")]);
        assert!(placements.on_vendor(&claude, &id("task-004")).is_empty());
        // A gate retry on the same worker keeps the handoff that put the card there.
        assert_eq!(placements.handed_to(&worker("codex/gpt-5")), 1);

        let gemini = worker("antigravity-cli/gemini-3");
        assert!(placements.pick(&id("task-004"), &gemini, 7));
        assert_eq!(placements.handed_to(&gemini), 1);
        assert!(placements.on_vendor(&claude, &id(CARD)).is_empty());

        // Once the log places the card after the pick, the log wins.
        placements.observe(&at(
            8,
            "task-004",
            attempt(2, "codex/gpt-5", AttemptCause::Handoff),
        ));
        assert!(!placements.pick(&id("task-004"), &gemini, 7));
        assert_eq!(placements.handed_to(&gemini), 0);
        assert_eq!(placements.handed_to(&worker("codex/gpt-5")), 2);
    }

    #[test]
    fn a_race_counts_as_one_handoff() {
        let card = CardId::new(CARD).expect("a valid task id");
        let racers = ["codex/gpt-5", "antigravity-cli/gemini-3", "opencode/glm"];
        let at = |seq: u64, kind: RunEventKind| RunEvent {
            seq,
            ..event(Some(CARD), None, kind)
        };
        let tag = at(
            2,
            RunEventKind::Card(CardEvent::CheckpointTagged {
                n: 1,
                commit: "c1".to_owned(),
                from: worker("claude-code/opus"),
                to: racers.iter().map(|racer| worker(racer)).collect(),
                reason: "usage limit".to_owned(),
            }),
        );
        // Each racer logs its own attempt, even with its own number.
        let mut checkpointed = vec![
            at(1, attempt(1, "claude-code/opus", AttemptCause::First)),
            tag,
        ];
        checkpointed.extend((0u32..).zip(racers).map(|(n, racer)| {
            at(
                u64::from(3 + n),
                attempt(2 + n, racer, AttemptCause::Handoff),
            )
        }));
        assert_eq!(History::of(&checkpointed, &card).handoffs, 1);

        // Without checkpoints, racers that share an attempt number count once.
        let untagged: Vec<RunEvent> = (0u32..)
            .zip(racers)
            .map(|(n, racer)| at(u64::from(n), attempt(2, racer, AttemptCause::Handoff)))
            .collect();
        assert_eq!(History::of(&untagged, &card).handoffs, 1);
    }

    #[test]
    fn gates_count_only_after_the_latest_commit() {
        let card = CardId::new(CARD).expect("a valid task id");
        let at = |seq: u64, kind: RunEventKind| RunEvent {
            seq,
            ..event(Some(CARD), None, kind)
        };
        let gate = || {
            RunEventKind::Gate(argonauts_core::events::card::GateOutcome {
                name: "tests".to_owned(),
                kind: argonauts_core::events::card::GateKind::Command,
                passed: true,
                command: None,
                exit_code: Some(0),
                duration_ms: 1,
                summary: "ok".to_owned(),
                output_tail: String::new(),
                violations: Vec::new(),
                sandboxed: true,
                skipped: false,
            })
        };
        let commit = RunEventKind::Card(CardEvent::Committed(argonauts_core::CommitInfo {
            branch: "argonauts/run-01/task-003".to_owned(),
            sha: Some("abc".to_owned()),
            message: "work".to_owned(),
            stat: argonauts_core::DiffStat::default(),
            by: None,
        }));
        let before = [at(1, gate()), at(2, commit.clone())];
        assert!(!History::of(&before, &card).gates_ran);
        let after = [at(1, commit), at(2, gate())];
        assert!(History::of(&after, &card).gates_ran);
        assert_eq!(History::of(&after, &card).handoffs, 0);
    }
}
