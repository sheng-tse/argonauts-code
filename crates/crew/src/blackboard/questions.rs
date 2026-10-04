//! A question from a report: answered from an earlier record when it's settled, else sent to
//! the owner of the piece it concerns, the consultant, or you. In full auto the lead answers
//! what was meant for you, and the approver checks its answer before it goes.

use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::CardState;
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointKind, CheckpointPacket, FreeText, QuestionPacket,
};
use argonauts_core::events::run::Party;
use argonauts_core::events::shared::{BlackboardKind, BlackboardMessage, Recipient};
use argonauts_core::names::{MessageId, RoleName, SessionKey, WorkerRef};
use argonauts_core::policy::{EvidenceDocument, EvidenceFacts};
use argonauts_core::registry::company_of;
use argonauts_core::{CardId, Note, NoteLevel};

use crate::blackboard::requests::move_if;
use crate::blackboard::{Post, delivery, post_once, records, refs, reply_to, route};
use crate::card::machine::Trigger;
use crate::checkpoints::full_auto::{self, OWN_ANSWER};
use crate::checkpoints::{self, CheckpointRequest, options};
use crate::consultant;
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::roles;
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
        // Nothing waits for you in full auto, so the lead answers in your place.
        Recipient::User if full_auto::applies_now(host, CheckpointKind::Question) => {
            answer_for_you(host, card, &id).await?;
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

/// The question and who asked it: the agent, else the card's owner, else the default agent.
fn asked(
    host: &Host,
    card: &CardId,
    id: &MessageId,
) -> EngineResult<(BlackboardMessage, WorkerRef)> {
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
    Ok((
        question,
        asker.unwrap_or_else(|| host.settings().default_agent.clone()),
    ))
}

/// A `Question` checkpoint. The card waits in `Blocked` until you answer, then the answer goes
/// to it for its next turn.
async fn ask_you(host: &Host, card: &CardId, id: &MessageId) -> EngineResult<()> {
    let (question, asker) = asked(host, card, id)?;
    let blocked = move_if(
        host,
        card,
        CardState::Running,
        Trigger::Question,
        &format!("waits for your answer to {id}"),
    )?;
    let from = card_label(host, card);
    let request = question_checkpoint(card, &from, &question.text, asker);
    let decided = checkpoints::open_and_wait(host, request).await?;
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

/// Full auto: the model `approvals.answered_by` picked answers a question meant for you, unless
/// it's of the asker's own company, else the lead, or a model of another company than the
/// asker's when the lead is of the asker's own. The approver, of another company than the one
/// that answered, checks the answer before it goes, as it checks the lead's answers at the
/// goal; one it turns down, or couldn't check, goes as "use your best judgment" instead. One
/// line says what was answered, and the run's status keeps it for you to review.
async fn answer_for_you(host: &Host, card: &CardId, id: &MessageId) -> EngineResult<()> {
    let (question, asker) = asked(host, card, id)?;
    let from = card_label(host, card);
    let Some((answerer, session, proposed)) =
        proposed_answer(host, card, &question, &asker, &from).await?
    else {
        // Nothing could answer, so the agent goes on with its own judgment.
        return reply(host, id, &question, OWN_ANSWER.to_owned(), Party::FullAuto);
    };
    let answerer_name = roles::display_name(&answerer);
    let mut request = question_checkpoint(card, &from, &question.text, asker.clone());
    request.options = Some(options::lead_answer(&answerer_name));
    request.agent_outputs = vec![format!(
        "The run is in full auto, so {answerer} answered this question from {asker} in the \
         user's place. Approve only an answer that keeps the work safe and within the request"
    )];
    request.documents = vec![
        EvidenceDocument {
            label: "question".to_owned(),
            text: question.text.clone(),
            path: None,
            sha256: None,
        },
        EvidenceDocument {
            label: format!("{answerer}'s answer"),
            text: proposed.clone(),
            path: None,
            sha256: None,
        },
    ];
    request.authors = vec![answerer.clone()];
    let decided = checkpoints::open_and_wait(host, request).await?;
    let (text, by) = match decided.action {
        AnswerAction::Approve => (
            proposed,
            Party::Agent {
                session,
                worker: answerer.clone(),
            },
        ),
        _ => (
            decided
                .text
                .clone()
                .filter(|text| !text.trim().is_empty())
                .unwrap_or_else(|| OWN_ANSWER.to_owned()),
            decided.by.clone(),
        ),
    };
    // Full auto's own pick already said what it chose and why; a checked answer says so here.
    let asked_by = format!("{from} ({})", roles::display_name(&asker));
    let line = match &decided.by {
        Party::Approver { model } => Some(format!(
            "Full auto answered the question from {asked_by} for you with {answerer_name}'s \
             answer, which {} checked.",
            roles::display_name(model)
        )),
        Party::Rules => Some(format!(
            "Full auto answered the question from {asked_by} for you with {answerer_name}'s \
             answer, which your rules let through."
        )),
        _ => None,
    };
    if let Some(text) = line {
        note(host, card, NoteLevel::Info, text)?;
    }
    reply(host, id, &question, text, by)
}

/// The answer a model gives in your place, with who gave it and in which session: the model
/// [`answerer_for`] picks, else the lead when that one couldn't answer, with one line saying
/// so. `None` when neither could, after a line that says why.
async fn proposed_answer(
    host: &Host,
    card: &CardId,
    question: &BlackboardMessage,
    asker: &WorkerRef,
    from: &str,
) -> EngineResult<Option<(WorkerRef, SessionKey, String)>> {
    let picked = answerer_for(host, asker);
    let failed =
        match consultant::session_answer(host, question, &picked, RoleName::PhaseLead).await {
            Ok((session, text)) => return Ok(Some((picked, session, text))),
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            Err(err) => err,
        };
    let lead = lead_answerer(host, asker);
    let picked_name = roles::display_name(&picked);
    if lead == picked {
        let text = format!(
            "{picked_name} couldn't answer the question from {from}, so its agent goes with its \
             best judgment: {failed}"
        );
        note(host, card, NoteLevel::Warn, text)?;
        return Ok(None);
    }
    let lead_name = roles::display_name(&lead);
    let text = format!(
        "{picked_name} couldn't answer the question from {from}, so {lead_name} answers it: \
         {failed}"
    );
    note(host, card, NoteLevel::Warn, text)?;
    match consultant::session_answer(host, question, &lead, RoleName::PhaseLead).await {
        Ok((session, text)) => Ok(Some((lead, session, text))),
        Err(EngineError::Interrupted) => Err(EngineError::Interrupted),
        Err(err) => {
            let text = format!(
                "{lead_name} couldn't answer the question from {from} either, so its agent goes \
                 with its best judgment: {err}"
            );
            note(host, card, NoteLevel::Warn, text)?;
            Ok(None)
        }
    }
}

/// Posts `text` as the answer to question `id`, and sends it to the agent that asked.
fn reply(
    host: &Host,
    id: &MessageId,
    question: &BlackboardMessage,
    text: String,
    from: Party,
) -> EngineResult<()> {
    let reply = host.read(|state| Post {
        kind: BlackboardKind::Answer,
        from,
        to: reply_to(state, question),
        refs: records::record_refs(question, &text),
        text,
        in_reply_to: Some(id.clone()),
        from_card: None,
    });
    let answer = post_once(host, reply)?;
    delivery::to_running(host, &answer)?;
    Ok(())
}

fn note(host: &Host, card: &CardId, level: NoteLevel, text: String) -> EngineResult<()> {
    host.record(Some(card), None, RunEventKind::Note(Note { level, text }))?;
    Ok(())
}

/// Who answers an agent's question in your place: the model `approvals.answered_by` picked,
/// unless it's of the asker's own company, else [`lead_answerer`].
fn answerer_for(host: &Host, asker: &WorkerRef) -> WorkerRef {
    let picked = host
        .settings()
        .approvals
        .answered_by
        .as_ref()
        .and_then(|answerer| answerer.model());
    let company = |worker: &WorkerRef| company_of(&roles::family(host, worker));
    match picked {
        Some(picked) if company(picked) != company(asker) => picked.clone(),
        _ => lead_answerer(host, asker),
    }
}

/// The lead, unless it's of the asker's own company, then the first crew member, or other agent
/// you can run, of another company.
fn lead_answerer(host: &Host, asker: &WorkerRef) -> WorkerRef {
    let company = |worker: &WorkerRef| company_of(&roles::family(host, worker));
    let lead = host.read(|state| {
        crate::lead::phase_lead(state, host.settings())
            .unwrap_or_else(|| host.settings().default_agent.clone())
    });
    let theirs = company(asker);
    if company(&lead) != theirs {
        return lead;
    }
    let settings = host.settings();
    settings
        .crew
        .iter()
        .cloned()
        .chain(host.services().workers.available())
        .find(|worker| company(worker) != theirs)
        .unwrap_or(lead)
}

/// What the cards that ask about `card` call it, its owner's label such as `build:flag`, never
/// its id.
fn card_label(host: &Host, card: &CardId) -> String {
    host.read(|state| crate::labels::card_label(state, card))
}

/// A question checkpoint, titled by the asking card's label `from`.
fn question_checkpoint(
    card: &CardId,
    from: &str,
    text: &str,
    asker: WorkerRef,
) -> CheckpointRequest {
    CheckpointRequest {
        at: None,
        kind: CheckpointKind::Question,
        title: format!("Question from {from}"),
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
