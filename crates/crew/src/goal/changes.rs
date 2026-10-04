//! Goal changes mid-run, per `approvals.goal_changes`: `ask` opens a `GoalChange` checkpoint at
//! the next safe point; `allow_clarifications` records a clarification that leaves the criteria
//! unchanged and asks for anything else; `deny` refuses. In auto mode no rule or model may
//! redefine the goal.

use argonauts_core::events::RunEventKind;
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointKind, CheckpointPacket, FreeText, GoalChangePacket,
};
use argonauts_core::events::goal::{GoalChangeResolution, GoalEvent, GoalFile};
use argonauts_core::events::run::Party;
use argonauts_core::log::NoteLevel;
use argonauts_core::names::MessageId;
use argonauts_core::policy::{EvidenceDocument, EvidenceFacts};
use argonauts_core::settings::approvals::GoalChanges;

use crate::checkpoints::{self, CheckpointRequest};
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::integrate::branch;
use crate::state::ids::IdKind;

use super::checkpoint::approve;
use super::materiality::one_line;
use super::{draft, file, round};

/// What `approvals.goal_changes` does with a request, before anyone answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// A clarification recorded as a decision; the run goes on.
    Record,
    Refuse,
    /// The `GoalChange` checkpoint, which only you can accept.
    AskYou,
}

pub fn route(setting: GoalChanges, clarification: bool) -> Route {
    match setting {
        GoalChanges::Deny => Route::Refuse,
        GoalChanges::AllowClarifications if clarification => Route::Record,
        GoalChanges::AllowClarifications | GoalChanges::Ask => Route::AskYou,
    }
}

/// Call it at a safe point. The same request asked again gets the answer it already got.
pub async fn request(
    host: &Host,
    from: Party,
    text: String,
    clarification: bool,
) -> EngineResult<GoalChangeResolution> {
    let text = text.trim().to_owned();
    if text.is_empty() {
        return Err(EngineError::Refused(
            "a goal change needs some text".to_owned(),
        ));
    }
    let (approved, earlier) = host.read(|state| {
        let earlier = state
            .goal
            .changes
            .iter()
            .rev()
            .find(|change| {
                change.text == text && change.from == from && change.clarification == clarification
            })
            .cloned();
        (state.goal.approved.clone(), earlier)
    });
    let approved = approved.ok_or_else(|| {
        EngineError::Refused("there is no approved goal to change yet".to_owned())
    })?;

    let (id, asked) = match earlier {
        Some(change) => match change.resolved() {
            Some(resolution) => return Ok(resolution),
            None => {
                let asked = change.resolutions.contains(&GoalChangeResolution::AskedYou);
                (change.id, asked)
            }
        },
        None => {
            let id = MessageId::numbered(host.allocate(IdKind::Message)?);
            host.record(
                None,
                None,
                RunEventKind::Goal(GoalEvent::ChangeRequested {
                    id: id.clone(),
                    from: from.clone(),
                    text: text.clone(),
                    clarification,
                }),
            )?;
            (id, false)
        }
    };

    match route(host.settings().approvals.goal_changes, clarification) {
        Route::Refuse => resolve(host, &id, GoalChangeResolution::Refused, None),
        Route::Record => resolve(host, &id, GoalChangeResolution::Recorded, None),
        Route::AskYou => {
            if !asked {
                resolve(host, &id, GoalChangeResolution::AskedYou, None)?;
            }
            ask_you(host, &id, from, &text, clarification, approved).await
        }
    }
}

async fn ask_you(
    host: &Host,
    id: &MessageId,
    from: Party,
    text: &str,
    clarification: bool,
    approved: GoalFile,
) -> EngineResult<GoalChangeResolution> {
    let changed = changed_goal(&approved, text);
    let request = CheckpointRequest {
        at: None,
        kind: CheckpointKind::GoalChange,
        title: "Change the goal?".to_owned(),
        card: None,
        packet: CheckpointPacket::GoalChange(GoalChangePacket {
            from,
            text: text.to_owned(),
            clarification,
            goal: approved.clone(),
        }),
        facts: facts(&approved, &changed, clarification),
        agent_outputs: Vec::new(),
        documents: vec![
            EvidenceDocument {
                label: "approved goal".to_owned(),
                text: file::render(&approved),
                path: Some(approved.path.clone()),
                sha256: Some(approved.sha256.clone()),
            },
            EvidenceDocument {
                label: "goal with the change".to_owned(),
                text: file::render(&changed),
                path: None,
                sha256: Some(changed.sha256.clone()),
            },
        ],
        options: None,
        free_text: FreeText::None,
        authors: Vec::new(),
    };
    let decided = checkpoints::open_and_wait(host, request).await?;
    let accepted = decided.action == AnswerAction::Approve;
    if accepted && decided.by != Party::User {
        round::note(
            host,
            NoteLevel::Warn,
            "Only you can change the goal, so the change was refused.".to_owned(),
        )?;
        return resolve(host, id, GoalChangeResolution::Refused, None);
    }
    if !accepted {
        return resolve(host, id, GoalChangeResolution::Refused, None);
    }
    let tree = branch::ensure_integration(host).await?;
    let written = file::write(host, &tree, changed).await?;
    resolve(
        host,
        id,
        GoalChangeResolution::Changed,
        Some(written.sha256.clone()),
    )?;
    approve(host, written, decided.by)?;
    Ok(GoalChangeResolution::Changed)
}

/// The change counts as an open question, so auto mode never decides it. Whether it keeps the
/// success criteria is measured on the goal it would write, not taken from the request.
fn facts(approved: &GoalFile, changed: &GoalFile, clarification: bool) -> EvidenceFacts {
    EvidenceFacts {
        open_questions: Some(1),
        criteria_unchanged: Some(clarification && changed.criteria == approved.criteria),
        ..EvidenceFacts::default()
    }
}

/// The next version of the goal with the change written in as an answer.
pub fn changed_goal(approved: &GoalFile, text: &str) -> GoalFile {
    let mut changed = draft::with_answers(approved, &[format!("Goal change: {}", one_line(text))]);
    changed.version = approved.version + 1;
    file::hashed(changed)
}

fn resolve(
    host: &Host,
    id: &MessageId,
    resolution: GoalChangeResolution,
    new_sha256: Option<String>,
) -> EngineResult<GoalChangeResolution> {
    host.record(
        None,
        None,
        RunEventKind::Goal(GoalEvent::ChangeResolved {
            id: id.clone(),
            resolution,
            new_sha256,
        }),
    )?;
    Ok(resolution)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use argonauts_core::RunId;
    use argonauts_core::events::goal::{Criterion, GoalSource};

    use super::*;

    #[test]
    fn the_setting_routes_each_request() {
        let table = [
            (GoalChanges::Ask, false, Route::AskYou),
            (GoalChanges::Ask, true, Route::AskYou),
            (GoalChanges::AllowClarifications, true, Route::Record),
            (GoalChanges::AllowClarifications, false, Route::AskYou),
            (GoalChanges::Deny, true, Route::Refuse),
            (GoalChanges::Deny, false, Route::Refuse),
        ];
        for (setting, clarification, expected) in table {
            assert_eq!(route(setting, clarification), expected, "{setting:?}");
        }
    }

    #[test]
    fn a_clarification_keeps_the_criteria_and_nothing_else_claims_to() {
        let approved = file::hashed(GoalFile {
            run: RunId::new("run-01").unwrap(),
            version: 1,
            summary: "s".to_owned(),
            criteria: vec![Criterion::new("G1", "Requests over the limit get a 429")],
            in_scope: Vec::new(),
            out_of_scope: Vec::new(),
            assumptions: Vec::new(),
            answers: Vec::new(),
            source: GoalSource::Restatements,
            path: PathBuf::new(),
            sha256: String::new(),
        });
        let changed = changed_goal(&approved, "the limit is per key");
        assert_eq!(
            facts(&approved, &changed, true).criteria_unchanged,
            Some(true)
        );
        assert_eq!(
            facts(&approved, &changed, false).criteria_unchanged,
            Some(false)
        );
        let mut rewritten = changed.clone();
        rewritten.criteria.clear();
        assert_eq!(
            facts(&approved, &rewritten, true).criteria_unchanged,
            Some(false)
        );
        assert_eq!(facts(&approved, &changed, true).open_questions, Some(1));
    }

    #[test]
    fn a_change_is_the_next_version() {
        let approved = file::hashed(GoalFile {
            run: RunId::new("run-01").unwrap(),
            version: 1,
            summary: "s".to_owned(),
            criteria: Vec::new(),
            in_scope: Vec::new(),
            out_of_scope: Vec::new(),
            assumptions: Vec::new(),
            answers: Vec::new(),
            source: GoalSource::Restatements,
            path: PathBuf::new(),
            sha256: String::new(),
        });
        let changed = changed_goal(&approved, "also cover\nthe CLI");
        assert_eq!(changed.version, 2);
        assert_eq!(changed.answers, ["Goal change: also cover the CLI"]);
        assert_ne!(changed.sha256, approved.sha256);
        assert_eq!(changed.sha256, file::sha256(&changed));
    }
}
