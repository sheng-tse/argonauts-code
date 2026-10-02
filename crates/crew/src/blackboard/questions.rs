//! A question from a report: answered from an earlier record when it's settled, else sent to
//! the owner of the piece it concerns, the consultant, or you.

use argonauts_core::CardId;
use argonauts_core::events::card::CardState;
use argonauts_core::events::checkpoint::{
    CheckpointKind, CheckpointPacket, FreeText, QuestionPacket,
};
use argonauts_core::events::run::Party;
use argonauts_core::events::shared::{BlackboardKind, Recipient};
use argonauts_core::names::{MessageId, RoleName, WorkerRef};
use argonauts_core::policy::EvidenceFacts;

use crate::blackboard::requests::move_if;
use crate::blackboard::{Post, delivery, post_once, records, refs, reply_to, route};
use crate::card::machine::Trigger;
use crate::checkpoints::{self, CheckpointRequest};
use crate::consultant;
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::status::folder::words;

pub async fn ask(host: &Host, card: &CardId, from: &Party, text: &str) -> EngineResult<()> {
    let cited = refs::refs_in(text);
    let to = match host.read(|state| route::question(state, text, &cited)) {
        // An owner asking about its own piece asks you.
        Recipient::Card { card: asked } if &asked == card => Recipient::User,
        other => other,
    };
    let id = post_once(
        host,
        Post {
            kind: BlackboardKind::Question,
            from: from.clone(),
            to: to.clone(),
            text: text.to_owned(),
            in_reply_to: None,
            refs: cited,
            from_card: Some(card.clone()),
        },
    )?;
    if host.read(|state| state.blackboard.settled(&id).is_some()) {
        return Ok(());
    }
    if let Some(record) = settled_earlier(host, &id) {
        return answer_with_record(host, &id, &record);
    }
    match to {
        Recipient::Role {
            role: RoleName::Consultant,
        } => {
            consultant::answer(host, &id).await?;
        }
        Recipient::User => ask_you(host, card, &id).await?,
        _ => {
            delivery::to_running(host, &id)?;
        }
    }
    Ok(())
}

/// The earlier record that settles this message, unless it cites evidence the record lacked.
pub fn settled_earlier(host: &Host, message: &MessageId) -> Option<MessageId> {
    host.read(|state| {
        let asked = state.blackboard.get(message)?;
        let record = records::settled_by(state, asked)?;
        let settled = state.blackboard.get(&record)?;
        (!records::reopens(settled, asked)).then_some(record)
    })
}

/// Answers a question, or decides a change request, with the id of the record that already
/// settled it.
pub fn answer_with_record(
    host: &Host,
    message: &MessageId,
    record: &MessageId,
) -> EngineResult<()> {
    let reply = host.read(|state| {
        let asked = state.blackboard.get(message)?;
        let settled = state.blackboard.get(record)?;
        let kind = match asked.kind {
            BlackboardKind::ChangeRequest => BlackboardKind::Decision,
            _ => BlackboardKind::Answer,
        };
        let mut cited = settled.refs.clone();
        cited.push(record.to_string());
        Some(Post {
            kind,
            from: Party::Conductor,
            to: reply_to(state, asked),
            text: format!("Already settled by {record}: {}", settled.text),
            in_reply_to: Some(message.clone()),
            refs: cited,
            from_card: None,
        })
    });
    let reply = reply.ok_or_else(|| {
        EngineError::Refused(format!(
            "{message} or its record {record} isn't on the board"
        ))
    })?;
    let answer = post_once(host, reply)?;
    delivery::to_running(host, &answer)?;
    Ok(())
}

/// A `Question` checkpoint. The card waits in `Blocked` until you answer, then the answer goes
/// to it for its next turn.
async fn ask_you(host: &Host, card: &CardId, id: &MessageId) -> EngineResult<()> {
    let (question, asker) = host
        .read(|state| {
            let question = state.blackboard.get(id)?.clone();
            let asker = match &question.from {
                Party::Agent { worker, .. } => Some(worker.clone()),
                _ => state
                    .cards
                    .get(card)
                    .and_then(|record| record.owner.clone().or_else(|| record.spec.owner.clone())),
            };
            Some((question, asker))
        })
        .ok_or_else(|| EngineError::Refused(format!("There's no question {id}")))?;
    let asker = asker.unwrap_or_else(|| host.settings().default_agent.clone());
    let blocked = move_if(
        host,
        card,
        CardState::Running,
        Trigger::Question,
        &format!("waits for your answer to {id}"),
    )?;
    let decided =
        checkpoints::open_and_wait(host, question_checkpoint(card, &question.text, asker)).await?;
    let text = decided
        .text
        .clone()
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| words(&decided.action));
    let reply = host.read(|state| Post {
        kind: BlackboardKind::Answer,
        from: decided.by.clone(),
        to: reply_to(state, &question),
        refs: records::record_refs(&question, &text),
        text,
        in_reply_to: Some(id.clone()),
        from_card: None,
    });
    let answer = post_once(host, reply)?;
    if blocked {
        move_if(
            host,
            card,
            CardState::Blocked,
            Trigger::Answered,
            &format!("you answered {id}"),
        )?;
    }
    delivery::to_running(host, &answer)?;
    Ok(())
}

fn question_checkpoint(card: &CardId, text: &str, asker: WorkerRef) -> CheckpointRequest {
    CheckpointRequest {
        at: None,
        kind: CheckpointKind::Question,
        title: format!("Question from {card}"),
        card: Some(card.clone()),
        packet: CheckpointPacket::Question(QuestionPacket {
            from: asker,
            card: card.clone(),
            text: text.to_owned(),
            options: Vec::new(),
        }),
        facts: EvidenceFacts::default(),
        agent_outputs: Vec::new(),
        documents: Vec::new(),
        options: None,
        free_text: FreeText::Answer,
        authors: Vec::new(),
    }
}
