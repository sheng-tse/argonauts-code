//! The consultant: a `Question` addressed to it (from an owner's report, `@consultant`, or the
//! coordinator) starts its session read-only with its configured lifetime, the question, and
//! the status summary. The answer is posted as `Answer` and delivered at the asker's next turn.
//! It owns no card and never writes.

use argonauts_core::events::run::Party;
use argonauts_core::events::session::{InputPart, InputPartKind, SessionEnd};
use argonauts_core::events::shared::{BlackboardKind, BlackboardMessage};
use argonauts_core::names::{MessageId, RoleName, SessionKey, WorkerRef};
use argonauts_core::settings::Settings;
use argonauts_core::worker::Access;

use crate::blackboard::{Post, delivery, post_once, records, reply_to};
use crate::engine::host::{Host, SessionPlan};
use crate::error::{EngineError, EngineResult};
use crate::prompts;
use crate::status::Viewer;

/// Answers the question and returns the answer's message id. A question already answered
/// returns that answer, so a re-entered step never starts a second session.
pub async fn answer(host: &Host, question: &MessageId) -> EngineResult<MessageId> {
    let (asked, answered) = host.read(|state| {
        let board = &state.blackboard;
        let answered = board
            .replies(question)
            .find(|reply| reply.kind == BlackboardKind::Answer)
            .map(|reply| reply.id.clone());
        (board.get(question).cloned(), answered)
    });
    if let Some(answer) = answered {
        return Ok(answer);
    }
    let asked = asked
        .filter(|asked| asked.kind == BlackboardKind::Question)
        .ok_or_else(|| EngineError::Refused(format!("{question} isn't a question")))?;

    let worker = consultant(host.settings());
    let (session, text) = session_answer(host, &asked, &worker, RoleName::Consultant).await?;
    let reply = host.read(|state| Post {
        kind: BlackboardKind::Answer,
        from: Party::Agent {
            session,
            worker: worker.clone(),
        },
        to: reply_to(state, &asked),
        refs: records::record_refs(&asked, &text),
        text,
        in_reply_to: Some(question.clone()),
        from_card: None,
    });
    let id = post_once(host, reply)?;
    delivery::to_running(host, &id)?;
    Ok(id)
}

/// One read-only session in which `worker`, as `role`, answers the question: the session and
/// what it said. The consultant answers as itself; in full auto the lead answers in your place.
pub(crate) async fn session_answer(
    host: &Host,
    asked: &BlackboardMessage,
    worker: &WorkerRef,
    role: RoleName,
) -> EngineResult<(SessionKey, String)> {
    let prompt = match role {
        RoleName::Consultant => prompts::board::CONSULTANT,
        _ => prompts::board::FOR_YOU,
    };
    let running = host
        .start(SessionPlan {
            at: None,
            role,
            worker: worker.clone(),
            card: None,
            access: Access::ReadOnly,
            step: None,
            workdir: host.repo().base_worktree.clone(),
            parts: parts(host, asked, role, prompt),
            json_schema: None,
            resume: None,
            slot: slot(&asked.id),
            label: None,
        })
        .await?;
    let session = running.key.clone();
    let end = running.ended().await?;
    Ok((session, answer_text(&end)))
}

/// `roles.consultant.workers`, else the default agent.
pub fn consultant(settings: &Settings) -> WorkerRef {
    settings
        .roles
        .get(&RoleName::Consultant)
        .and_then(|role| role.workers.first().cloned())
        .unwrap_or_else(|| settings.main_agent.clone())
}

fn parts(host: &Host, asked: &BlackboardMessage, role: RoleName, prompt: &str) -> Vec<InputPart> {
    let viewer = Viewer::Session {
        role,
        card: None,
        label: None,
    };
    let mut question = format!(
        "Question {} from {}:\n{}",
        asked.id,
        delivery::party_label(&asked.from),
        asked.text
    );
    if !asked.refs.is_empty() {
        question.push_str(&format!("\nCites: {}", asked.refs.join(", ")));
    }
    vec![
        part(InputPartKind::Prompt, role.as_str(), prompt, None),
        part(
            InputPartKind::Message,
            "question",
            &question,
            Some(asked.id.to_string()),
        ),
        part(
            InputPartKind::Status,
            "status",
            &host.status_text(&viewer),
            None,
        ),
    ]
}

fn part(kind: InputPartKind, label: &str, text: &str, source: Option<String>) -> InputPart {
    InputPart {
        kind,
        label: label.to_owned(),
        text: text.to_owned(),
        approx_tokens: u32::try_from(text.len().div_ceil(4)).unwrap_or(u32::MAX),
        source,
    }
}

fn answer_text(end: &SessionEnd) -> String {
    end.result_text
        .as_deref()
        .or(end.last_message.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            format!(
                "The consultant ended without an answer ({}).",
                crate::status::folder::words(&end.reason)
            )
        })
}

/// Tells one question's consultant session from another's, for resuming the right one.
fn slot(question: &MessageId) -> u32 {
    question
        .as_str()
        .rsplit('-')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}
