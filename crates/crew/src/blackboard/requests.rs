//! Change requests through the decision rules: dependents paused, each affected owner asked for
//! one vote, a tie to the phase lead or to you, and a decision record that settles it.

use argonauts_core::CardId;
use argonauts_core::events::card::CardState;
use argonauts_core::events::checkpoint::{
    AnswerAction, ChangeRequestPacket, CheckpointKind, CheckpointPacket, FreeText,
};
use argonauts_core::events::goal::GoalChangeResolution;
use argonauts_core::events::run::Party;
use argonauts_core::events::shared::{BlackboardKind, BlackboardMessage, Recipient};
use argonauts_core::names::{MessageId, WorkerRef};
use argonauts_core::policy::EvidenceFacts;
use argonauts_core::steps::card::ChangeRequestOut;

use crate::blackboard::replies::Reply;
use crate::blackboard::rules::{self, Decider, Tally};
use crate::blackboard::{Post, delivery, post_once, questions, records, reply_to};
use crate::card::machine::Trigger;
use crate::card::pipeline;
use crate::checkpoints::{self, CheckpointRequest};
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::goal;
use crate::lead;
use crate::state::RunState;
use crate::state::blackboard::{Board, same_worker};

/// In the refs of the copy of a tied request sent to the phase lead.
pub const TIE_BREAK: &str = "tie-break";

/// How an owner's vote starts, as the conductor records it.
const ACCEPT: &str = "accept";
const REJECT: &str = "reject";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Accepted,
    Rejected,
    /// Back to the plan checkpoint.
    Replan,
}

pub async fn file(
    host: &Host,
    card: &CardId,
    from: &Party,
    request: &ChangeRequestOut,
) -> EngineResult<()> {
    let decider =
        host.read(|state| rules::decider_for(state, host.settings(), Some(card), request));
    let to = match &decider {
        Decider::Owner(owner) => Recipient::Worker {
            worker: owner.clone(),
        },
        Decider::Owners(owners) => Recipient::Worker {
            worker: owners[0].clone(),
        },
        Decider::User | Decider::Goal | Decider::Replan => Recipient::User,
    };
    let id = post_once(
        host,
        Post {
            kind: BlackboardKind::ChangeRequest,
            from: from.clone(),
            to,
            text: request.text.clone(),
            in_reply_to: None,
            refs: request.paths.clone(),
            from_card: Some(card.clone()),
        },
    )?;
    if host.read(|state| state.blackboard.settled(&id).is_some()) {
        return Ok(());
    }
    if let Some(record) = questions::settled_earlier(host, &id) {
        return questions::answer_with_record(host, &id, &record);
    }
    pause_dependents(host, card, request)?;
    match decider {
        Decider::Owner(_) => {
            delivery::to_running(host, &id)?;
        }
        Decider::Owners(owners) => {
            delivery::to_running(host, &id)?;
            for owner in &owners[1..] {
                let copy = post_once(
                    host,
                    Post {
                        kind: BlackboardKind::ChangeRequest,
                        from: from.clone(),
                        to: Recipient::Worker {
                            worker: owner.clone(),
                        },
                        text: request.text.clone(),
                        in_reply_to: Some(id.clone()),
                        refs: request.paths.clone(),
                        from_card: Some(card.clone()),
                    },
                )?;
                delivery::to_running(host, &copy)?;
            }
        }
        Decider::User => {
            decide_yourself(
                host,
                &id,
                "It's irreversible, or no other card owns what it touches",
            )
            .await?;
        }
        Decider::Goal => {
            let resolution =
                goal::changes::request(host, from.clone(), request.text.clone(), false).await?;
            match resolution {
                GoalChangeResolution::Changed => {
                    settle(
                        host,
                        &id,
                        Outcome::Accepted,
                        Party::User,
                        "the goal changed",
                    )?;
                }
                GoalChangeResolution::Recorded => {
                    settle(
                        host,
                        &id,
                        Outcome::Accepted,
                        Party::Config,
                        "recorded as a clarification",
                    )?;
                }
                GoalChangeResolution::Refused => {
                    settle(
                        host,
                        &id,
                        Outcome::Rejected,
                        Party::Config,
                        "goal changes are refused",
                    )?;
                }
                // The goal change checkpoint has it now; the request stays open until then.
                GoalChangeResolution::AskedYou => {}
            }
        }
        Decider::Replan => {
            settle(
                host,
                &id,
                Outcome::Replan,
                Party::Conductor,
                &format!("it would move {card} into a protected path"),
            )?;
        }
    }
    Ok(())
}

/// An owner's reply in its report: the answer to a question for its piece, or its vote on a
/// change request.
pub async fn take_reply(
    host: &Host,
    card: &CardId,
    from: &Party,
    reply: Reply,
) -> EngineResult<()> {
    match reply {
        Reply::Answer { to, text } => answer(host, card, from, &to, text),
        Reply::Vote { on, accept, reason } => vote(host, from, &on, accept, &reason).await,
    }
}

fn answer(
    host: &Host,
    card: &CardId,
    from: &Party,
    question: &MessageId,
    text: String,
) -> EngineResult<()> {
    let reply = host.read(|state| {
        let asked = state.blackboard.get(question)?;
        let for_this_card = match (&asked.to, from) {
            (Recipient::Card { card: to }, _) => to == card,
            (Recipient::Worker { worker }, Party::Agent { worker: author, .. }) => {
                same_worker(worker, author)
            }
            (
                Recipient::Session { session },
                Party::Agent {
                    session: author, ..
                },
            ) => session == author,
            _ => false,
        };
        let open =
            asked.kind == BlackboardKind::Question && state.blackboard.settled(question).is_none();
        (for_this_card && open).then(|| Post {
            kind: BlackboardKind::Answer,
            from: from.clone(),
            to: reply_to(state, asked),
            refs: records::record_refs(asked, &text),
            text: text.clone(),
            in_reply_to: Some(question.clone()),
            from_card: Some(card.clone()),
        })
    });
    let Some(reply) = reply else {
        tracing::warn!(%card, %question, "an answer to a question that isn't open for this card");
        return Ok(());
    };
    let posted = post_once(host, reply)?;
    delivery::to_running(host, &posted)?;
    Ok(())
}

async fn vote(
    host: &Host,
    from: &Party,
    on: &MessageId,
    accept: bool,
    reason: &str,
) -> EngineResult<()> {
    let Party::Agent { worker: voter, .. } = from else {
        return Ok(());
    };
    let primary = host.read(|state| {
        let asked = state.blackboard.get(on)?;
        let Recipient::Worker { worker } = &asked.to else {
            return None;
        };
        let counts = asked.kind == BlackboardKind::ChangeRequest && same_worker(worker, voter);
        counts.then(|| {
            asked
                .in_reply_to
                .clone()
                .unwrap_or_else(|| asked.id.clone())
        })
    });
    let Some(primary) = primary else {
        tracing::warn!(%on, %voter, "a vote on a change request this owner wasn't asked about");
        return Ok(());
    };
    if host.read(|state| state.blackboard.settled(&primary).is_some()) {
        return Ok(());
    }
    let verdict = if accept { ACCEPT } else { REJECT };
    let text = if reason.is_empty() {
        verdict.to_owned()
    } else {
        format!("{verdict}: {reason}")
    };
    post_once(
        host,
        Post {
            kind: BlackboardKind::Decision,
            from: from.clone(),
            to: Recipient::User,
            text,
            in_reply_to: Some(on.clone()),
            refs: Vec::new(),
            from_card: None,
        },
    )?;
    count(host, &primary).await
}

/// Settles the request once every affected owner has voted.
async fn count(host: &Host, primary: &MessageId) -> EngineResult<()> {
    let ballot = host.read(|state| Ballot::of(&state.blackboard, primary));
    if ballot.votes.len() < ballot.voters.len() {
        return Ok(());
    }
    let lead = host.read(|state| lead::phase_lead(state, host.settings()));
    let accepted = ballot.votes.iter().filter(|(_, accepts)| *accepts).count();
    let tally_note = format!("{accepted} of {} owners accepted", ballot.votes.len());
    match rules::tally(&ballot.votes, lead.as_ref()) {
        Tally::Accepted => {
            settle(
                host,
                primary,
                Outcome::Accepted,
                Party::Conductor,
                &tally_note,
            )?;
        }
        Tally::Rejected => {
            settle(
                host,
                primary,
                Outcome::Rejected,
                Party::Conductor,
                &tally_note,
            )?;
        }
        Tally::LeadBreaks(lead) => match ballot.lead_vote {
            Some(accepts) => {
                let outcome = if accepts {
                    Outcome::Accepted
                } else {
                    Outcome::Rejected
                };
                settle(
                    host,
                    primary,
                    outcome,
                    Party::Conductor,
                    "the phase lead broke a tie",
                )?;
            }
            None if ballot.tie_break.is_none() => {
                let asked = host
                    .read(|state| state.blackboard.get(primary).cloned())
                    .ok_or_else(|| EngineError::Refused(format!("There's no request {primary}")))?;
                let mut cited = asked.refs.clone();
                cited.push(TIE_BREAK.to_owned());
                let copy = post_once(
                    host,
                    Post {
                        kind: BlackboardKind::ChangeRequest,
                        from: asked.from.clone(),
                        to: Recipient::Worker { worker: lead },
                        text: format!(
                            "The owners tied on this. As phase lead, your vote decides.\n{}",
                            asked.text
                        ),
                        in_reply_to: Some(primary.clone()),
                        refs: cited,
                        from_card: None,
                    },
                )?;
                delivery::to_running(host, &copy)?;
            }
            None => {}
        },
        Tally::YouDecide => {
            decide_yourself(
                host,
                primary,
                "The owners tied, and the phase lead is one of them",
            )
            .await?;
        }
    }
    Ok(())
}

/// The owners asked to vote on a request, and their votes so far.
#[derive(Debug, Default)]
struct Ballot {
    voters: Vec<WorkerRef>,
    votes: Vec<(WorkerRef, bool)>,
    tie_break: Option<MessageId>,
    lead_vote: Option<bool>,
}

impl Ballot {
    fn of(board: &Board, primary: &MessageId) -> Self {
        let mut ballot = Self::default();
        let asked = board.messages.iter().filter(|message| {
            message.kind == BlackboardKind::ChangeRequest
                && (&message.id == primary || message.in_reply_to.as_ref() == Some(primary))
        });
        for message in asked {
            let Recipient::Worker { worker } = &message.to else {
                continue;
            };
            let vote = first_vote(board, message, worker);
            if message.refs.iter().any(|mark| mark == TIE_BREAK) {
                ballot.tie_break = Some(message.id.clone());
                ballot.lead_vote = vote;
                continue;
            }
            if ballot.voters.contains(worker) {
                continue;
            }
            ballot.voters.push(worker.clone());
            if let Some(accepts) = vote {
                ballot.votes.push((worker.clone(), accepts));
            }
        }
        ballot
    }
}

fn first_vote(board: &Board, asked: &BlackboardMessage, voter: &WorkerRef) -> Option<bool> {
    board
        .replies(&asked.id)
        .find_map(|reply| match &reply.from {
            Party::Agent { worker, .. }
                if reply.kind == BlackboardKind::Decision && same_worker(voter, worker) =>
            {
                Some(reply.text.starts_with(ACCEPT))
            }
            _ => None,
        })
}

/// A `ChangeRequest` checkpoint; your answer settles it.
async fn decide_yourself(host: &Host, primary: &MessageId, why: &str) -> EngineResult<()> {
    let (asked, card, paused) = host
        .read(|state| {
            let asked = state.blackboard.get(primary)?.clone();
            let card = state.blackboard.posted_from.get(primary)?.clone();
            let paused = paused_cards(state, &asked, &card);
            Some((asked, card, paused))
        })
        .ok_or_else(|| {
            EngineError::Refused(format!("{primary} isn't a change request from a card"))
        })?;
    // The approver mustn't come from the company whose agent asked for the change.
    let authors = match &asked.from {
        Party::Agent { worker, .. } => vec![worker.clone()],
        _ => Vec::new(),
    };
    let decided = checkpoints::open_and_wait(
        host,
        CheckpointRequest {
            at: None,
            kind: CheckpointKind::ChangeRequest,
            title: format!("Change request from {card}"),
            card: Some(card.clone()),
            packet: CheckpointPacket::ChangeRequest(ChangeRequestPacket {
                from: asked.from.clone(),
                card,
                text: asked.text.clone(),
                paths: asked.refs.clone(),
                why_you: why.to_owned(),
                paused,
            }),
            facts: EvidenceFacts {
                irreversible_actions: Some(rules::irreversible_actions(&asked.refs)),
                ..EvidenceFacts::default()
            },
            agent_outputs: Vec::new(),
            documents: Vec::new(),
            options: None,
            free_text: FreeText::Guidance,
            authors,
        },
    )
    .await?;
    let outcome = match decided.action {
        AnswerAction::Approve | AnswerAction::ApproveWithEdits => Outcome::Accepted,
        _ => Outcome::Rejected,
    };
    let note = decided.text.clone().unwrap_or_default();
    settle(host, primary, outcome, decided.by.clone(), &note)?;
    Ok(())
}

/// Records the decision, sends it to the requesting card, and lets paused cards go on.
pub fn settle(
    host: &Host,
    primary: &MessageId,
    outcome: Outcome,
    by: Party,
    note: &str,
) -> EngineResult<MessageId> {
    let (asked, to) = host
        .read(|state| {
            let asked = state.blackboard.get(primary)?.clone();
            let to = reply_to(state, &asked);
            Some((asked, to))
        })
        .ok_or_else(|| EngineError::Refused(format!("There's no request {primary}")))?;
    let verdict = match outcome {
        Outcome::Accepted => "Accepted",
        Outcome::Rejected => "Rejected",
        Outcome::Replan => "Sent back to the plan checkpoint",
    };
    let mut text = format!("{verdict}: {}", asked.text);
    if !note.trim().is_empty() {
        text.push_str(&format!(" ({})", note.trim()));
    }
    let record = post_once(
        host,
        Post {
            kind: BlackboardKind::Decision,
            from: by,
            to,
            text,
            in_reply_to: Some(primary.clone()),
            refs: asked.refs.clone(),
            from_card: None,
        },
    )?;
    delivery::to_running(host, &record)?;
    // Sent back to planning, its cards wait for the new plan.
    if outcome != Outcome::Replan {
        resume_dependents(host, &asked)?;
    }
    Ok(record)
}

fn pause_dependents(
    host: &Host,
    requester: &CardId,
    request: &ChangeRequestOut,
) -> EngineResult<()> {
    let cards = host.read(|state| rules::dependents(state, request));
    for card in cards.iter().filter(|card| *card != requester) {
        move_if(
            host,
            card,
            CardState::Running,
            Trigger::Question,
            &format!("waits for a change request from {requester}"),
        )?;
    }
    Ok(())
}

fn resume_dependents(host: &Host, asked: &BlackboardMessage) -> EngineResult<()> {
    let cards = host.read(|state| {
        let requester = state.blackboard.posted_from.get(&asked.id).cloned();
        rules::dependents(state, &as_request(asked))
            .into_iter()
            .filter(|card| Some(card) != requester.as_ref() && !paused(state, card))
            .collect::<Vec<_>>()
    });
    for card in &cards {
        move_if(
            host,
            card,
            CardState::Blocked,
            Trigger::Answered,
            &format!("change request {} was decided", asked.id),
        )?;
    }
    Ok(())
}

/// Whether an open change request still pauses this card. The scheduler shouldn't start it.
pub fn paused(state: &RunState, card: &CardId) -> bool {
    let board = &state.blackboard;
    board.messages.iter().any(|message| {
        message.kind == BlackboardKind::ChangeRequest
            && message.in_reply_to.is_none()
            && board.settled(&message.id).is_none()
            && board.posted_from.get(&message.id) != Some(card)
            && rules::dependents(state, &as_request(message)).contains(card)
    })
}

fn paused_cards(state: &RunState, asked: &BlackboardMessage, requester: &CardId) -> Vec<CardId> {
    rules::dependents(state, &as_request(asked))
        .into_iter()
        .filter(|card| {
            card != requester
                && state
                    .cards
                    .get(card)
                    .is_some_and(|record| record.state == CardState::Blocked)
        })
        .collect()
}

/// A posted change request's paths are its refs.
fn as_request(message: &BlackboardMessage) -> ChangeRequestOut {
    ChangeRequestOut {
        text: message.text.clone(),
        paths: message.refs.clone(),
    }
}

/// Moves the card by the table when it's in `from`. True when it moved.
pub(crate) fn move_if(
    host: &Host,
    card: &CardId,
    from: CardState,
    trigger: Trigger,
    reason: &str,
) -> EngineResult<bool> {
    let now = host.read(|state| state.cards.get(card).map(|record| record.state));
    if now != Some(from) {
        return Ok(false);
    }
    pipeline::transition(host, card, trigger, reason)?;
    Ok(true)
}
