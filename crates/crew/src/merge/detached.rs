//! A run that started with your checkout on no branch. It has no branch to merge into by
//! itself, so unless `merge.target` names one, you're asked once which branch takes it. The
//! answer stays in the run's record, and everything after reads it as the run's target. Your
//! checkout stays where it is: only the branch you named moves. Only you can name it, so no
//! rule or judge weighs the question, and where nobody is asked the run keeps its branch.

use argonauts_core::base::names::ApprovalId;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::approval::{
    AnswerAction, AnswerOption, ApprovalKind, ApprovalPacket, FreeText,
};
use argonauts_core::safety::policy::EvidenceFacts;
use argonauts_core::{Note, NoteLevel};

use crate::approvals::{self, ApprovalRequest};
use crate::engine::error::EngineResult;
use crate::engine::host::Host;
use crate::merge::branch::PREFIX;
use crate::merge::{in_the_way, merge_packet};
use crate::state::RunState;

pub const TITLE: &str = "This checkout isn't on a branch. Which branch takes the run?";

/// The most branches the question lists.
const LISTED: usize = 8;

/// The branch you named for this run, once you have.
pub fn chosen(state: &RunState) -> Option<String> {
    state
        .approvals
        .by_id
        .values()
        .filter(|record| record.opened.title == TITLE)
        .filter_map(|record| record.decided.as_ref())
        .find_map(|decided| match &decided.action {
            AnswerAction::Choose { value } => Some(value.clone()),
            _ => None,
        })
}

/// What you answered when asked for the run's branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Named {
    Branch(String),
    /// The run ends with its work on its own branch.
    KeepBranch,
    /// The question went away unanswered.
    Withdrawn,
}

/// Asks which branch the run merges into. `approved` is the merge question you answered,
/// whose evidence the question shows again.
pub async fn ask(host: &Host, approved: &ApprovalId) -> EngineResult<Named> {
    let branches = branches(host).await?;
    let run_branch = host.read(merge_packet::landing_branch_of);
    if branches.is_empty() {
        // Nothing to ask: no answer could name a branch, so asking again would never end.
        let on = run_branch.as_deref().unwrap_or("its own branch");
        let note = Note {
            level: NoteLevel::Warn,
            text: format!(
                "This checkout has no branch, so the run's work stays on {on}. merge.target \
                 names the branch a run merges into."
            ),
        };
        host.record(None, None, RunEventKind::Note(note))?;
        return Ok(Named::KeepBranch);
    }
    let mut options: Vec<AnswerOption> = branches
        .iter()
        .zip('a'..='z')
        .map(|(branch, key)| AnswerOption {
            key,
            label: branch.clone(),
            action: AnswerAction::Choose {
                value: branch.clone(),
            },
            recommended: false,
            detail: Some("the run's work goes on it, and your checkout stays where it is".into()),
        })
        .collect();
    options.push(AnswerOption {
        key: 'k',
        label: "Keep the run on its branch".to_owned(),
        action: AnswerAction::KeepBranch,
        recommended: false,
        detail: Some(match run_branch {
            Some(branch) => format!("the run ends with its work on {branch}, not merged"),
            None => "the run ends with its work on its own branch, not merged".to_owned(),
        }),
    });
    let mut packet = in_the_way::packet_shown(host, approved, "");
    packet.target = None;
    packet.asks_which_branch = true;
    let request = ApprovalRequest {
        at: None,
        kind: ApprovalKind::Merge,
        title: TITLE.to_owned(),
        task: None,
        packet: ApprovalPacket::Merge(packet),
        facts: EvidenceFacts::default(),
        agent_outputs: Vec::new(),
        documents: Vec::new(),
        options: Some(options),
        free_text: FreeText::None,
        authors: Vec::new(),
    };
    let decided = approvals::open(host, request)?
        .decided_or_withdrawn(host)
        .await?;
    Ok(match decided.map(|decided| decided.action) {
        Ok(AnswerAction::Choose { value }) if branches.contains(&value) => Named::Branch(value),
        Ok(AnswerAction::KeepBranch) => Named::KeepBranch,
        _ => Named::Withdrawn,
    })
}

/// Your branches, the usual default ones first. The run's own are never offered.
async fn branches(host: &Host) -> EngineResult<Vec<String>> {
    let mut branches = host.git().branches_under(&host.repo().root, "").await?;
    branches.retain(|branch| !branch.starts_with(PREFIX));
    branches.sort_by_key(|branch| {
        (
            !matches!(branch.as_str(), "main" | "master"),
            branch.clone(),
        )
    });
    branches.truncate(LISTED);
    Ok(branches)
}
