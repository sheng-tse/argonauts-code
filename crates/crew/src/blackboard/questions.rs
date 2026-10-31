//! A question from a report: answered from an earlier record when it's settled, else sent to
//! the worker of the piece it concerns, the consultant, or you. On autopilot nobody is asked: a
//! question that stops the work gets the main agent's answer, and one in a report that finished
//! stands as what its agent decided. Either way one line says so, and the task carries on.

use argonauts_core::base::names::{MessageId, ModelRef, RoleName, SessionKey};
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::approval::{
    ApprovalKind, ApprovalPacket, FreeText, MergeQuestion, QuestionPacket,
};
use argonauts_core::runs::events::run::Party;
use argonauts_core::runs::events::shared::{
    AnsweredForYou, BlackboardEvent, BlackboardKind, BlackboardMessage, Recipient,
};
use argonauts_core::runs::events::task::TaskState;
use argonauts_core::safety::policy::EvidenceFacts;
use argonauts_core::{Note, NoteLevel, TaskId};

use crate::approvals::autopilot::{self, OWN_ANSWER, OWN_CALL};
use crate::approvals::{self, ApprovalRequest};
use crate::blackboard::requests::move_if;
use crate::blackboard::{Post, delivery, post_once, records, refs, reply_to, route};
use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::Host;
use crate::roles;
use crate::roles::consultant;
use crate::state::RunState;
use crate::status::folder::words;
use crate::task::machine::Trigger;
use crate::understand::materiality::one_line;

/// Posts the question and sends it where it belongs. `blocks` says the asking report stopped for
/// the answer; a report that finished only mentions its question.
pub async fn ask(
    host: &Host,
    task: &TaskId,
    from: &Party,
    text: &str,
    blocks: bool,
) -> EngineResult<()> {
    let cited = refs::refs_in(text);
    let to = match host.read(|state| route::question(state, text, &cited)) {
        // A worker asking about its own piece asks you.
        Recipient::Task { task: asked } if &asked == task => Recipient::User,
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
            from_task: Some(task.clone()),
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
        // Nothing waits for you on autopilot: the main agent answers what stops the work, and
        // the rest stands as its agent's own call.
        Recipient::User if autopilot::applies_now(host, ApprovalKind::Question) => {
            if blocks {
                answer_for_you(host, task, &id).await?;
            } else {
                own_call(host, task, &id)?;
            }
        }
        // A report that finished asks in passing: its task goes on to its reviews, and the
        // question stays open for the merge question to show.
        Recipient::User if !blocks => carried(host, task, &id)?,
        Recipient::User => ask_you(host, task, &id).await?,
        _ => {
            delivery::to_running(host, &id)?;
        }
    }
    Ok(())
}

/// A question in a report that finished, outside autopilot: nothing waits on it. One line says
/// the task goes on, and the question stays open on the board, where the merge question finds it.
fn carried(host: &Host, task: &TaskId, id: &MessageId) -> EngineResult<()> {
    let (question, _) = asked(host, task, id)?;
    let line = format!(
        "{} asked, and goes on to its reviews: {} · the merge question shows it",
        task_label(host, task),
        one_line(&question.text)
    );
    note(host, task, NoteLevel::Info, line)
}

/// Whether the task's report left a question for you that nothing waits on, so a report that
/// needs a decision goes on to its reviews with it.
pub fn carried_for_the_merge(state: &RunState, task: &TaskId) -> bool {
    let board = &state.blackboard;
    board.messages.iter().any(|message| {
        message.kind == BlackboardKind::Question
            && message.to == Recipient::User
            && board.posted_from.get(&message.id) == Some(task)
            && board.settled(&message.id).is_none()
    }) && state
        .approvals
        .open_for(ApprovalKind::Question, Some(task))
        .is_none()
}

/// What a task's agent asked you that the merge question shows: each question still open, and
/// each one answered in your place with its answer. One you answered yourself, or left with
/// Esc, you have already seen.
pub fn for_the_merge(state: &RunState, task: &TaskId) -> Vec<MergeQuestion> {
    let board = &state.blackboard;
    board
        .messages
        .iter()
        .filter(|message| {
            message.kind == BlackboardKind::Question
                && message.to == Recipient::User
                && board.posted_from.get(&message.id) == Some(task)
        })
        .filter_map(|question| {
            let open = MergeQuestion {
                text: question.text.clone(),
                answer: None,
                answered_by: None,
            };
            let Some(answer) = board.settled(&question.id) else {
                return Some(open);
            };
            let (text, by) = match &answer.from {
                Party::User | Party::Declined => return None,
                Party::Agent { worker, .. } => (answer.text.clone(), Some(worker.clone())),
                Party::Judge { model } => (answer.text.clone(), Some(model.clone())),
                _ if answer.text == OWN_CALL => ("its agent's own call".to_owned(), None),
                _ if answer.text == OWN_ANSWER => ("its agent's best judgment".to_owned(), None),
                _ => (answer.text.clone(), None),
            };
            Some(MergeQuestion {
                answer: Some(text),
                answered_by: by,
                ..open
            })
        })
        .collect()
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
            from: Party::ArgonautsCode,
            to: reply_to(state, asked),
            text: format!("Already settled by {record}: {}", settled.text),
            in_reply_to: Some(message.clone()),
            refs: cited,
            from_task: None,
        })
    });
    let reply = reply.ok_or_else(|| {
        EngineError::Refused(format!(
            "{message} or its record {record} isn't on the board"
        ))
    })?;
    let answer = post_once(host, reply)?;
    delivery::to_running(host, &answer)?;
    resume_asker(host, message)?;
    Ok(())
}

/// A task blocked on its questions goes back to work once the last of them is answered, so its
/// session takes the answers at its next turn. An answer that comes while its report is still
/// being read finds it running, and the report's end sees the answer instead.
pub fn resume_asker(host: &Host, question: &MessageId) -> EngineResult<()> {
    let task = host.read(|state| {
        let task = state.blackboard.posted_from.get(question)?;
        (!waits_on_the_board(state, task)).then(|| task.clone())
    });
    if let Some(task) = task {
        move_if(
            host,
            &task,
            TaskState::Blocked,
            Trigger::Answered,
            &format!("{question} was answered"),
        )?;
    }
    Ok(())
}

/// Whether a question the task asked is still open, or a change request of another task still
/// holds it.
pub fn waits_on_the_board(state: &RunState, task: &TaskId) -> bool {
    let board = &state.blackboard;
    let asked_and_open = board.messages.iter().any(|message| {
        message.kind == BlackboardKind::Question
            && board.posted_from.get(&message.id) == Some(task)
            && board.settled(&message.id).is_none()
    });
    asked_and_open || crate::blackboard::requests::paused(state, task)
}

/// The question and who asked it: the agent, else the task's worker, else the main agent.
fn asked(
    host: &Host,
    task: &TaskId,
    id: &MessageId,
) -> EngineResult<(BlackboardMessage, ModelRef)> {
    let (question, asker) = host
        .read(|state| {
            let question = state.blackboard.get(id)?.clone();
            let asker = match &question.from {
                Party::Agent { worker, .. } => Some(worker.clone()),
                _ => state.tasks.get(task).and_then(|record| {
                    record.worker.clone().or_else(|| record.spec.worker.clone())
                }),
            };
            Some((question, asker))
        })
        .ok_or_else(|| EngineError::Refused(format!("There's no question {id}")))?;
    Ok((
        question,
        asker.unwrap_or_else(|| host.settings().crew.main.clone()),
    ))
}

/// A `Question` approval. The task waits in `Blocked` until you answer, then the answer goes
/// to it for its next turn.
async fn ask_you(host: &Host, task: &TaskId, id: &MessageId) -> EngineResult<()> {
    let (question, asker) = asked(host, task, id)?;
    let blocked = move_if(
        host,
        task,
        TaskState::Running,
        Trigger::Question,
        &format!("waits for your answer to {id}"),
    )?;
    let from = task_label(host, task);
    let request = question_approval(task, &from, &question.text, asker);
    let decided = approvals::open_and_wait(host, request).await?;
    pass_on(host, task, &question, &decided, blocked)
}

/// Your answer to a question, posted to the board, the task waiting on it moved on, and the
/// answer delivered for its next turn.
fn pass_on(
    host: &Host,
    task: &TaskId,
    question: &BlackboardMessage,
    decided: &argonauts_core::runs::events::approval::ApprovalDecided,
    blocked: bool,
) -> EngineResult<()> {
    let id = &question.id;
    let text = decided
        .text
        .clone()
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| words(&decided.action));
    let reply = host.read(|state| Post {
        kind: BlackboardKind::Answer,
        from: decided.by.clone(),
        to: reply_to(state, question),
        refs: records::record_refs(question, &text),
        text,
        in_reply_to: Some(id.clone()),
        from_task: None,
    });
    let answer = post_once(host, reply)?;
    if blocked {
        move_if(
            host,
            task,
            TaskState::Blocked,
            Trigger::Answered,
            &format!("you answered {id}"),
        )?;
    }
    delivery::to_running(host, &answer)?;
    Ok(())
}

/// A task still blocked on a question you've answered. A run that stops to wait for you ends
/// the wait that would have passed the answer on, and the answer comes in as it resumes, so
/// the task takes it here, when its step goes on. Whether there was one.
pub fn answered_meanwhile(host: &Host, task: &TaskId) -> EngineResult<bool> {
    let found = host.read(|state| {
        if state.tasks.get(task)?.state != TaskState::Blocked {
            return None;
        }
        let record = state
            .approvals
            .last_decided(ApprovalKind::Question, Some(task))?;
        let ApprovalPacket::Question(packet) = &record.opened.packet else {
            return None;
        };
        let question = state.blackboard.messages.iter().rev().find(|message| {
            message.kind == BlackboardKind::Question
                && message.text == packet.text
                && state.blackboard.settled(&message.id).is_none()
        })?;
        Some((question.clone(), record.decided.clone()?))
    });
    let Some((question, decided)) = found else {
        return Ok(false);
    };
    pass_on(host, task, &question, &decided, true)?;
    Ok(true)
}

/// A task still blocked on a question nobody answered, as after something other than the
/// answer relaunched the run that waited on it: the task waits on the same question again, and
/// on autopilot the main agent answers it at once. Whether there was one.
pub async fn wait_again(host: &Host, task: &TaskId) -> EngineResult<bool> {
    let open = host.read(|state| {
        if state.tasks.get(task)?.state != TaskState::Blocked {
            return None;
        }
        let record = state
            .approvals
            .open_for(ApprovalKind::Question, Some(task))?;
        let ApprovalPacket::Question(packet) = &record.opened.packet else {
            return None;
        };
        let question = state.blackboard.messages.iter().rev().find(|message| {
            message.kind == BlackboardKind::Question
                && message.text == packet.text
                && state.blackboard.posted_from.get(&message.id) == Some(task)
                && state.blackboard.settled(&message.id).is_none()
        })?;
        Some((record.opened.id.clone(), question.clone()))
    });
    let Some((approval, question)) = open else {
        return Ok(false);
    };
    let decided = approvals::wait_again(host, &approval).await?;
    pass_on(host, task, &question, &decided, true)?;
    Ok(true)
}

/// Autopilot, a question that stops the work: the model `approvals.answers` picked answers it,
/// unless it's of the asker's own provider, else the main agent, or a model of another provider
/// than the asker's when the main agent is of the asker's own. The answer goes straight to the
/// agent, which carries on. One line says who answered and what, and the run's status keeps it
/// for you to review.
async fn answer_for_you(host: &Host, task: &TaskId, id: &MessageId) -> EngineResult<()> {
    let (question, asker) = asked(host, task, id)?;
    let from = task_label(host, task);
    let Some((answerer, session, answer)) =
        proposed_answer(host, task, &question, &asker, &from).await?
    else {
        // Nothing could answer, so the agent goes on with its own judgment.
        return reply(host, id, &question, OWN_ANSWER.to_owned(), Party::Autopilot);
    };
    let answered = AnsweredForYou {
        question: id.clone(),
        asker: from.clone(),
        answer: one_line(&answer),
        by: Some(answerer.clone()),
        role: answerer_role(host, &answerer),
        why: None,
        finished: false,
    };
    host.record(
        Some(task),
        None,
        RunEventKind::Blackboard(BlackboardEvent::AnsweredForYou(answered)),
    )?;
    let by = Party::Agent {
        session,
        worker: answerer,
    };
    reply(host, id, &question, answer, by)
}

/// Autopilot, a question in a report that finished its work: its agent already decided, so the
/// question stands as its own call. One line says so, and the closing answer lists it.
fn own_call(host: &Host, task: &TaskId, id: &MessageId) -> EngineResult<()> {
    let (question, asker) = asked(host, task, id)?;
    let line = format!(
        "Decided on autopilot · {} ({}) kept its own call on: {}",
        task_label(host, task),
        roles::display_name(&asker),
        one_line(&question.text)
    );
    note(host, task, NoteLevel::Info, line)?;
    reply(host, id, &question, OWN_CALL.to_owned(), Party::Autopilot)
}

/// The answer a model gives in your place, with who gave it and in which session: the model
/// [`answerer_for`] picks, else the tie-breaker, else the main agent though it shares the
/// asker's provider. Only a session that finished with text answers. Each one that couldn't
/// gets one line, and `None` means none could, so the agent goes with its own judgment.
async fn proposed_answer(
    host: &Host,
    task: &TaskId,
    question: &BlackboardMessage,
    asker: &ModelRef,
    from: &str,
) -> EngineResult<Option<(ModelRef, SessionKey, String)>> {
    let mut in_line = vec![answerer_for(host, asker)];
    for next in [
        tie_breaker_answerer(host, asker),
        host.settings().crew.main.clone(),
    ] {
        if !in_line.contains(&next) {
            in_line.push(next);
        }
    }
    for (n, answerer) in in_line.iter().enumerate() {
        let failed = match consultant::session_answer(
            host,
            question,
            answerer,
            RoleName::TieBreaker,
        )
        .await
        {
            Ok((session, text)) => return Ok(Some((answerer.clone(), session, text))),
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            Err(err) => err,
        };
        let name = roles::display_name(answerer);
        let text = match in_line.get(n + 1) {
            Some(next) if roles::provider(host, next) == roles::provider(host, asker) => format!(
                "{name} couldn't answer the question from {from}, so the main agent, {}, answers \
                 it though it shares the asker's provider: {failed}",
                roles::display_name(next)
            ),
            Some(next) => format!(
                "{name} couldn't answer the question from {from}, so {} answers it: {failed}",
                roles::display_name(next)
            ),
            None => format!(
                "{name} couldn't answer the question from {from}, so its agent goes with its \
                 best judgment: {failed}"
            ),
        };
        note(host, task, NoteLevel::Warn, text)?;
    }
    Ok(None)
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
        from_task: None,
    });
    let answer = post_once(host, reply)?;
    delivery::to_running(host, &answer)?;
    resume_asker(host, id)
}

fn note(host: &Host, task: &TaskId, level: NoteLevel, text: String) -> EngineResult<()> {
    host.record(Some(task), None, RunEventKind::Note(Note { level, text }))?;
    Ok(())
}

/// What the model that answered in your place is to the run, when it is more than the model
/// `approvals.answers` picked.
fn answerer_role(host: &Host, answerer: &ModelRef) -> Option<RoleName> {
    if *answerer == host.settings().crew.main {
        return Some(RoleName::Main);
    }
    let tie_breaker =
        host.read(|state| crate::roles::tie_breaker::tie_breaker(state, host.settings()));
    (tie_breaker.as_ref() == Some(answerer)).then_some(RoleName::TieBreaker)
}

/// Who answers an agent's question in your place: the model `approvals.answers` picked,
/// unless it's of the asker's own provider, else [`tie_breaker_answerer`].
fn answerer_for(host: &Host, asker: &ModelRef) -> ModelRef {
    let picked = host
        .settings()
        .approvals
        .answers
        .as_ref()
        .and_then(|answerer| answerer.model());
    let provider = |worker: &ModelRef| roles::provider(host, worker);
    match picked {
        Some(picked) if provider(picked) != provider(asker) => picked.clone(),
        _ => tie_breaker_answerer(host, asker),
    }
}

/// The phase's tie-breaker, the main agent before the run has a phase, unless it's of the asker's
/// own provider: then the first crew member, or other agent you can run, of another provider.
fn tie_breaker_answerer(host: &Host, asker: &ModelRef) -> ModelRef {
    let provider = |worker: &ModelRef| roles::provider(host, worker);
    let tie_breaker = host.read(|state| {
        crate::roles::tie_breaker::tie_breaker(state, host.settings())
            .unwrap_or_else(|| host.settings().crew.main.clone())
    });
    let theirs = provider(asker);
    if provider(&tie_breaker) != theirs {
        return tie_breaker;
    }
    let settings = host.settings();
    settings
        .crew
        .members()
        .into_iter()
        .chain(host.services().workers.available())
        .find(|worker| provider(worker) != theirs)
        .unwrap_or(tie_breaker)
}

/// What the tasks that ask about `task` call it, its worker's label such as `build:flag`, never
/// its id.
fn task_label(host: &Host, task: &TaskId) -> String {
    host.read(|state| crate::sessions::labels::task_label(state, task))
}

/// A question approval, titled by the asking task's label `from`.
fn question_approval(task: &TaskId, from: &str, text: &str, asker: ModelRef) -> ApprovalRequest {
    ApprovalRequest {
        at: None,
        kind: ApprovalKind::Question,
        title: format!("Question from {from}"),
        task: Some(task.clone()),
        packet: ApprovalPacket::Question(QuestionPacket {
            from: asker,
            task: task.clone(),
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
