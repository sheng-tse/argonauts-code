//! `council.same_model_check = "ask"`: before a fresh session of the model that made a task
//! checks it, you're asked once for the council whether that may go ahead. Yes lets it, as
//! "allow" does; no makes each of the council's checks yours, with the change in front of you.

use std::collections::BTreeSet;
use std::path::Path;

use argonauts_core::base::names::{ApprovalId, ModelRef};
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::approval::{
    AnswerAction, AnswerOption, ApprovalKind, ApprovalPacket, FreeText, SecondAgentPacket,
};
use argonauts_core::runs::events::council::{CheckPick, CheckRung, CouncilEvent, CouncilEventKind};
use argonauts_core::safety::policy::{EvidenceDocument, EvidenceFacts};
use argonauts_runner::{AgentCall, AgentEnd};
use serde_json::json;

use super::Bridge;
use super::reading::Change;
use crate::approvals::{self, ApprovalRequest, autopilot};
use crate::engine::error::EngineResult;
use crate::engine::job_fallback;
use crate::roles::display_name;
use crate::script::tasks;

const YES: &str = "yes";
const NO: &str = "no";
const ACCEPT: &str = "accept";

/// The council's question, the same for every check that would need it, so the first answer
/// stands for them all.
pub(super) const ASK_TITLE: &str = "Check each task with the model that made it?";

/// Sent back with nothing typed, so the maker still has a line to read.
const SENT_BACK: &str = "Sent back without a note";

impl Bridge<'_> {
    /// Whether a fresh session of the model that made a task may check it. Asked once for the
    /// council, as a decided question with the same title and evidence is found again in the
    /// step. In Autopilot nobody is asked, and a merge still needs its checks to pass.
    pub(super) async fn same_maker_may_check(
        &self,
        call: &AgentCall,
        maker: &ModelRef,
    ) -> EngineResult<bool> {
        if autopilot::applies_now(self.host, ApprovalKind::Question) {
            return Ok(true);
        }
        let text = "No model from another maker is set up, so only a fresh session of the \
                    model that made a task can check it. It never learns who made the work, but \
                    a model tends to favour its own style. If not, you check each task yourself.";
        let request = ApprovalRequest {
            at: Some(self.call.at.clone()),
            kind: ApprovalKind::Question,
            title: ASK_TITLE.to_owned(),
            task: None,
            packet: ApprovalPacket::SecondAgent(SecondAgentPacket {
                job: call.label.clone(),
                stopped: maker.clone(),
                taker: maker.clone(),
                text: text.to_owned(),
            }),
            facts: EvidenceFacts::default(),
            agent_outputs: Vec::new(),
            documents: Vec::new(),
            options: Some(vec![
                choice(
                    'y',
                    "Yes, a fresh session of the same model checks each task",
                    YES,
                    true,
                ),
                choice('n', "No, I check each task myself", NO, false),
            ]),
            free_text: FreeText::None,
            authors: Vec::new(),
        };
        let decided = approvals::open_and_wait_yours(
            self.host,
            request,
            "council.same_model_check asks you".into(),
        )
        .await?;
        Ok(matches!(&decided.action, AnswerAction::Choose { value } if value == YES))
    }

    /// Your own check of `call`'s task, which `maker` made: accept it, or send it back with
    /// what to change. Your answer is the check's, in the shape the script asked for.
    pub(super) async fn yours_to_check(
        &self,
        call: &AgentCall,
        maker: &ModelRef,
        workdir: &Path,
        change: Option<&Change>,
        checks: Option<String>,
    ) -> EngineResult<AgentEnd> {
        let host = self.host;
        let Some(task) = tasks::task_of(self, call.task.as_deref()) else {
            return Ok(AgentEnd::failed("there's no task to check yet"));
        };
        let title = host.read(|state| crate::sessions::labels::task_label(state, &task));
        let maker_name =
            job_fallback::model_name(host, maker).unwrap_or_else(|| display_name(maker));
        let request = ApprovalRequest {
            at: Some(self.call.at.clone()),
            kind: ApprovalKind::Question,
            title: format!("Accept {title}?"),
            task: Some(task.clone()),
            packet: ApprovalPacket::SecondAgent(SecondAgentPacket {
                job: call.label.clone(),
                stopped: maker.clone(),
                taker: maker.clone(),
                text: your_check_text(&maker_name, workdir, checks.as_deref()),
            }),
            facts: EvidenceFacts::default(),
            // Each check of the task asks afresh, so a send back is never answered for you
            // by the one before it.
            agent_outputs: vec![format!("check {}", call.index)],
            documents: change
                .map(|change| EvidenceDocument {
                    label: "the change".to_owned(),
                    text: change.for_a_reader_without_tools(None),
                    path: None,
                    sha256: None,
                })
                .into_iter()
                .collect(),
            options: Some(vec![
                choice('a', "Accept it", ACCEPT, false),
                AnswerOption {
                    key: 's',
                    label: "Send it back".to_owned(),
                    action: AnswerAction::SendBack,
                    recommended: false,
                    detail: None,
                },
            ]),
            free_text: FreeText::Guidance,
            authors: Vec::new(),
        };
        let known: BTreeSet<ApprovalId> =
            host.read(|state| state.approvals.by_id.keys().cloned().collect());
        let opening = approvals::open_yours(
            host,
            request,
            "you said you'd check this council's tasks yourself".into(),
        )?;
        if !known.contains(opening.id()) {
            let pick = CheckPick {
                task: task.clone(),
                checker: None,
                rung: CheckRung::You,
            };
            let event = CouncilEvent {
                talk: self.council_talk(),
                kind: CouncilEventKind::Check(pick),
            };
            host.record(Some(&task), None, RunEventKind::Council(event))?;
        }
        let decided = opening.decided(host).await?;
        let verdict = match &decided.action {
            AnswerAction::Choose { value } if value == ACCEPT => {
                json!({"accept": true, "findings": []})
            }
            _ => {
                let note = decided
                    .text
                    .as_deref()
                    .map(str::trim)
                    .filter(|note| !note.is_empty())
                    .unwrap_or(SENT_BACK);
                json!({"accept": false, "findings": [note]})
            }
        };
        Ok(AgentEnd::answered(verdict))
    }
}

fn choice(key: char, label: &str, value: &str, recommended: bool) -> AnswerOption {
    AnswerOption {
        key,
        label: label.to_owned(),
        action: AnswerAction::Choose {
            value: value.to_owned(),
        },
        recommended,
        detail: None,
    }
}

/// Who made the work, where it is, and what its checks said, for the question that hands you
/// its check.
fn your_check_text(maker: &str, workdir: &Path, checks: Option<&str>) -> String {
    let made = format!("{maker} made it, and its work is in {}.", workdir.display());
    let checks = checks.unwrap_or("No checks have run on it.");
    format!("{made}\n{checks}\nAccept it, or send it back with what to change.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn your_check_names_the_maker_the_folder_and_the_checks() {
        let folder = Path::new("/Users/you/runs/due/tasks/due-field");
        assert_eq!(
            your_check_text("5.6 Luna", folder, Some("tests passed: 3 passed, 0 failed")),
            "5.6 Luna made it, and its work is in /Users/you/runs/due/tasks/due-field.\n\
             tests passed: 3 passed, 0 failed\n\
             Accept it, or send it back with what to change."
        );
        assert!(
            your_check_text("5.6 Luna", folder, None).contains("\nNo checks have run on it.\n")
        );
    }
}
