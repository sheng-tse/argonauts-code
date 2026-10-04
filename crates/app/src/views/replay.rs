use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::RunId;
use argonauts_core::app::{AppEvent, ReplayTurn};
use argonauts_core::events::card::{CardEvent, CardState};
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointEvent, CheckpointKind, CheckpointOpened, CheckpointPacket,
};
use argonauts_core::events::goal::GoalEvent;
use argonauts_core::events::run::{ParkReason, Party, ResumeCause, RunLifecycle, RunOutcome};
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::vendor::HandoffEvent;
use argonauts_core::events::workflow::WorkflowEvent;
use argonauts_core::events::{IntegrationEvent, RunEvent, RunEventKind};
use argonauts_core::names::{CheckpointId, MessageId, SessionKey, WorkerRef};
use argonauts_core::sanitize::sanitize_for_terminal;

/// Chat turns a resumed run repeats word for word; older ones are one Ctrl+O away.
const REPLAY_TURNS: usize = 6;

/// What a resumed run prints in place of its history: the last turns word for word, one line
/// per milestone, and `recap`. `None` when there's no history.
pub(crate) fn condensed_replay(
    events: &[RunEvent],
    run: &RunId,
    recap: String,
) -> Option<AppEvent> {
    if events.is_empty() {
        return None;
    }
    let turns = turns(events);
    Some(AppEvent::CondensedReplay {
        run: run.clone(),
        turns: turns[turns.len().saturating_sub(REPLAY_TURNS)..].to_vec(),
        milestones: milestones(events),
        recap,
        recap_model: None,
    })
}

/// What you and the front ends said to the run, and what its agents asked you. A question
/// still open is left out, since its card shows it.
fn turns(events: &[RunEvent]) -> Vec<ReplayTurn> {
    let mut workers: BTreeMap<SessionKey, WorkerRef> = BTreeMap::new();
    let mut seen: BTreeSet<MessageId> = BTreeSet::new();
    let mut open: BTreeMap<CheckpointId, usize> = BTreeMap::new();
    let mut turns = Vec::new();
    let say = |turns: &mut Vec<ReplayTurn>, from: Party, text: &str| {
        let text = sanitize_for_terminal(text.trim());
        if !text.is_empty() {
            turns.push(ReplayTurn { from, text });
        }
    };
    for event in events {
        match &event.kind {
            RunEventKind::Run(RunLifecycle::Started(started)) => {
                let from = match &started.front_end {
                    Some(client) => Party::FrontEnd {
                        client: client.clone(),
                    },
                    None => Party::User,
                };
                say(&mut turns, from, &started.request);
            }
            RunEventKind::Session(SessionEvent::Started(started)) => {
                if let Some(key) = &event.session {
                    workers.insert(key.clone(), started.worker.clone());
                }
            }
            RunEventKind::Session(SessionEvent::Queued(message))
                if from_a_person(&message.from) && seen.insert(message.id.clone()) =>
            {
                say(&mut turns, message.from.clone(), &message.text);
            }
            RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) => {
                if let CheckpointPacket::Question(question) = &opened.packet {
                    let from = match &event.session {
                        Some(session) => Party::Agent {
                            session: session.clone(),
                            worker: workers
                                .get(session)
                                .cloned()
                                .unwrap_or_else(|| question.from.clone()),
                        },
                        None => Party::Conductor,
                    };
                    let at = turns.len();
                    say(&mut turns, from, &question.text);
                    if turns.len() > at {
                        open.insert(opened.id.clone(), at);
                    }
                }
            }
            RunEventKind::Checkpoint(CheckpointEvent::Decided(decided)) => {
                open.remove(&decided.id);
                if from_a_person(&decided.by)
                    && let Some(text) = &decided.text
                {
                    say(&mut turns, decided.by.clone(), text);
                }
            }
            RunEventKind::Checkpoint(CheckpointEvent::Withdrawn { id, .. }) => {
                open.remove(id);
            }
            _ => {}
        }
    }
    // Turns only grow until here, so each question's place still holds it, and going from the
    // last keeps the earlier places right.
    let mut asked: Vec<usize> = open.into_values().collect();
    asked.sort_unstable();
    for at in asked.into_iter().rev() {
        if at < turns.len() {
            turns.remove(at);
        }
    }
    turns
}

fn from_a_person(party: &Party) -> bool {
    matches!(party, Party::User | Party::FrontEnd { .. })
}

/// One line per milestone, oldest first, such as "Plan approved · 5 cards".
pub(crate) fn milestones(events: &[RunEvent]) -> Vec<String> {
    let mut opened: BTreeMap<CheckpointId, (CheckpointKind, usize)> = BTreeMap::new();
    let mut titles: BTreeMap<String, String> = BTreeMap::new();
    let mut lines = Vec::new();
    for event in events {
        let card = event.card_id.as_ref().map(ToString::to_string);
        let line = match &event.kind {
            RunEventKind::Goal(GoalEvent::Approved { .. }) => Some("Goal approved".to_owned()),
            RunEventKind::Checkpoint(CheckpointEvent::Opened(checkpoint)) => {
                opened.insert(checkpoint.id.clone(), opened_entry(checkpoint));
                None
            }
            RunEventKind::Checkpoint(CheckpointEvent::Decided(decided)) => {
                match (opened.get(&decided.id), &decided.action) {
                    (
                        Some((CheckpointKind::Plan, cards)),
                        AnswerAction::Approve | AnswerAction::ApproveWithEdits,
                    ) => Some(format!("Plan approved · {}", count(*cards, "card"))),
                    _ => None,
                }
            }
            RunEventKind::Card(CardEvent::Created(spec)) => {
                titles.insert(spec.id.to_string(), sanitize_for_terminal(&spec.title));
                None
            }
            RunEventKind::Card(CardEvent::State { to, .. }) => {
                let card = card.unwrap_or_default();
                let title = titles
                    .get(&card)
                    .map(|title| format!(" · {title}"))
                    .unwrap_or_default();
                match to {
                    CardState::Integrated => Some(format!("{card} integrated{title}")),
                    CardState::Failed => Some(format!("{card} failed{title}")),
                    CardState::Escalated => Some(format!("{card} escalated to you{title}")),
                    _ => None,
                }
            }
            RunEventKind::Card(CardEvent::Reverted { to, .. }) => Some(format!(
                "{} reverted to handoff-{to}",
                card.unwrap_or_default()
            )),
            RunEventKind::Handoff(HandoffEvent::HandedOff { card, to, .. }) => {
                Some(format!("{card} handed off to {to}"))
            }
            RunEventKind::Integration(IntegrationEvent::FinalCheck { passed }) => Some(
                if *passed {
                    "Final check passed"
                } else {
                    "Final check failed"
                }
                .to_owned(),
            ),
            RunEventKind::Integration(IntegrationEvent::Landed { target, .. }) => {
                Some(format!("Landed on {}", sanitize_for_terminal(target)))
            }
            RunEventKind::Integration(IntegrationEvent::PullRequest { url, .. }) => {
                Some(match url {
                    Some(url) => format!("Pull request opened · {}", sanitize_for_terminal(url)),
                    None => "Pull request suggested".to_owned(),
                })
            }
            RunEventKind::Workflow(WorkflowEvent::Changed(change)) => Some(format!(
                "Workflow changed · {}",
                sanitize_for_terminal(&change.reason)
            )),
            // A stream that starts here never saw these open, so a later answer still counts.
            RunEventKind::Run(RunLifecycle::Snapshot(snapshot)) => {
                for checkpoint in &snapshot.open_checkpoints {
                    opened.insert(checkpoint.id.clone(), opened_entry(checkpoint));
                }
                None
            }
            RunEventKind::Run(lifecycle) => lifecycle_line(lifecycle),
            _ => None,
        };
        lines.extend(line);
    }
    lines
}

/// The checkpoint's kind, and a plan's card count.
fn opened_entry(checkpoint: &CheckpointOpened) -> (CheckpointKind, usize) {
    let cards = match &checkpoint.packet {
        CheckpointPacket::Plan(plan) => plan.graph.cards.len(),
        _ => 0,
    };
    (checkpoint.kind, cards)
}

fn lifecycle_line(lifecycle: &RunLifecycle) -> Option<String> {
    match lifecycle {
        RunLifecycle::Resumed(resumed) => Some(
            match resumed.cause {
                ResumeCause::User => "Resumed",
                ResumeCause::Answer => "Resumed for your answer",
                ResumeCause::VendorBack => "Resumed as an agent came back",
                ResumeCause::AskTimeout => "Resumed as a handoff question timed out",
                ResumeCause::Crash => "Resumed after a crash",
            }
            .to_owned(),
        ),
        RunLifecycle::ApprovalModeChanged { mode, .. } => {
            Some(format!("Approval mode set to {}", mode.label()))
        }
        RunLifecycle::Parked { reason, .. } => Some(format!(
            "Parked · {}",
            match reason {
                ParkReason::Checkpoint => "waiting for you".to_owned(),
                ParkReason::Paused => "paused".to_owned(),
                ParkReason::AllVendorsOut { .. } => "every agent is out".to_owned(),
                ParkReason::WaitingForVendor { vendor, .. } => format!("waiting for {vendor}"),
            }
        )),
        RunLifecycle::Finished { outcome, .. } => Some(
            match outcome {
                RunOutcome::Merged => "Merged",
                RunOutcome::PullRequest => "Finished with a pull request",
                RunOutcome::BranchLeft => "Finished with its branches kept",
                RunOutcome::Abandoned => "Abandoned",
                RunOutcome::Stopped => "Stopped",
                RunOutcome::Failed => "Failed",
            }
            .to_owned(),
        ),
        _ => None,
    }
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_support::{
        card_event, checkpoint_decided, event, plan_checkpoint, queued, run_id, run_started,
    };
    use argonauts_core::events::run::RunResumed;

    #[test]
    fn milestones_are_one_line_each_oldest_first() {
        let card = argonauts_core::CardId::new("card-001").unwrap();
        let plan = CheckpointId::numbered(1);
        let events = vec![
            run_started(0, "Add --json to export", None),
            plan_checkpoint(1, &plan, 5),
            checkpoint_decided(2, &plan, AnswerAction::Approve, Party::User, None),
            card_event(
                3,
                &card,
                CardEvent::State {
                    from: Some(CardState::Integrating),
                    to: CardState::Integrated,
                    reason: "merged".to_owned(),
                },
            ),
            event(
                4,
                RunEventKind::Run(RunLifecycle::Parked {
                    reason: ParkReason::Paused,
                    waiting_on: Vec::new(),
                }),
            ),
            event(
                5,
                RunEventKind::Run(RunLifecycle::Resumed(RunResumed {
                    cause: ResumeCause::User,
                    settings_changed: false,
                    harness_versions_changed: Vec::new(),
                    main_moved_by: None,
                    sessions: Vec::new(),
                })),
            ),
        ];
        assert_eq!(
            milestones(&events),
            [
                "Plan approved · 5 cards",
                "card-001 integrated",
                "Parked · paused",
                "Resumed"
            ]
        );
    }

    #[test]
    fn a_plan_open_at_the_snapshot_is_approved_after_it() {
        let plan = CheckpointId::numbered(1);
        let RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) =
            plan_checkpoint(1, &plan, 5).kind
        else {
            unreachable!()
        };
        let snapshot = argonauts_core::events::run::RunSnapshot {
            upto: 1,
            engine: serde_json::Value::Null,
            summary: argonauts_core::api::StatusSummary::default(),
            open_checkpoints: vec![*opened],
            running: Vec::new(),
        };
        let events = vec![
            event(
                2,
                RunEventKind::Run(RunLifecycle::Snapshot(Box::new(snapshot))),
            ),
            checkpoint_decided(3, &plan, AnswerAction::Approve, Party::User, None),
        ];
        assert_eq!(milestones(&events), ["Plan approved · 5 cards"]);
    }

    #[test]
    fn no_condensed_replay_without_history() {
        assert_eq!(
            condensed_replay(&[], &run_id(), "※ nothing yet".to_owned()),
            None
        );
    }

    #[test]
    fn a_condensed_replay_repeats_the_last_turns_word_for_word() {
        let plan = CheckpointId::numbered(1);
        let mut events = vec![run_started(0, "Add --json to export", Some("hermes"))];
        for seq in 1..=7 {
            events.push(queued(seq, &format!("note {seq}"), Party::User));
        }
        events.push(plan_checkpoint(8, &plan, 2));
        events.push(checkpoint_decided(
            9,
            &plan,
            AnswerAction::Approve,
            Party::User,
            Some("ship it"),
        ));
        let Some(AppEvent::CondensedReplay {
            run,
            turns,
            milestones,
            recap,
            recap_model,
        }) = condensed_replay(&events, &run_id(), "※ recap".to_owned())
        else {
            panic!("expected a condensed replay");
        };
        assert_eq!(run, run_id());
        let texts: Vec<_> = turns.iter().map(|turn| turn.text.as_str()).collect();
        assert_eq!(
            texts,
            ["note 3", "note 4", "note 5", "note 6", "note 7", "ship it"]
        );
        assert_eq!(milestones, ["Plan approved · 2 cards"]);
        assert_eq!((recap.as_str(), recap_model), ("※ recap", None));

        let short = condensed_replay(&events[..2], &run_id(), String::new()).unwrap();
        let AppEvent::CondensedReplay { turns, .. } = short else {
            unreachable!()
        };
        assert_eq!(
            turns[0].from,
            Party::FrontEnd {
                client: "hermes".to_owned()
            }
        );
        assert_eq!(turns[0].text, "Add --json to export");
    }

    /// An agent's question still open isn't repeated above the card that asks it; once
    /// answered, the question and your answer read as turns.
    #[test]
    fn an_open_question_is_left_to_its_card() {
        use argonauts_core::events::checkpoint::{FreeText, QuestionPacket};
        use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};

        let id = CheckpointId::numbered(1);
        let card = argonauts_core::CardId::new("card-001").unwrap();
        let opened = CheckpointOpened {
            id: id.clone(),
            kind: CheckpointKind::Question,
            title: "Question from build:greet".to_owned(),
            card: Some(card.clone()),
            packet: CheckpointPacket::Question(QuestionPacket {
                from: WorkerRef::new("codex", None),
                card,
                text: "Keep the old name as an alias?".to_owned(),
                options: Vec::new(),
            }),
            evidence_sha256: String::new(),
            mode: ApprovalMode::Supervised,
            decide_with: DecideWith::Rules,
            options: Vec::new(),
            free_text: FreeText::Answer,
            needs_you: true,
        };
        let mut events = vec![
            run_started(0, "Rename the config loader", None),
            event(
                1,
                RunEventKind::Checkpoint(CheckpointEvent::Opened(Box::new(opened))),
            ),
            queued(2, "rename it everywhere", Party::User),
        ];
        let texts = |events: &[RunEvent]| -> Vec<String> {
            turns(events).into_iter().map(|turn| turn.text).collect()
        };
        assert_eq!(
            texts(&events),
            ["Rename the config loader", "rename it everywhere"]
        );
        events.push(checkpoint_decided(
            3,
            &id,
            AnswerAction::Answer,
            Party::User,
            Some("yes, keep it"),
        ));
        assert_eq!(
            texts(&events),
            [
                "Rename the config loader",
                "Keep the old name as an alias?",
                "rename it everywhere",
                "yes, keep it"
            ]
        );
    }
}
