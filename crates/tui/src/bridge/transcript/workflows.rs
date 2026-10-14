//! A crew run's workflow in the chat, as Claude Code prints its own: `● Workflow(<name>)` when
//! it starts, with the description and the phases it declares, a line when the lead adds a
//! phase, and `● Workflow(<name>) · done` when it ends, or `· failed` or
//! `· stopped` as the run ended.
//!
//! A run whose step runs another workflow's steps declares only that they run. Its start item
//! waits for that workflow's first report, and lists the phases the report names in place of
//! the step's own, so the chat shows the workflow that runs, as the view does.

use std::time::Duration;

use argonauts_core::RunId;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::run::{RunOutcome, RunStarted};
use argonauts_core::events::workflow::{WorkflowDeclared, WorkflowReport};
use argonauts_core::names::WorkerRef;
use argonauts_core::steps::intake::InstructionKind;
use argonauts_core::text::sentence_case;

use crate::bottom::WorkflowRowState;
use crate::cell::{PhaseLine, ToolCall, ToolItem, ToolKind, ToolState};
use crate::format;
use crate::text::sanitize_line;

use super::milestones::RunStory;
use super::steps::plural;
use super::{Cx, emit, quiet};

/// The run's own workflow declared its phases. The first time, the start item prints; a later
/// declaration follows an approved change, which its own line tells.
pub(super) fn declared(
    run: &RunId,
    declared: &WorkflowDeclared,
    story: &mut RunStory,
    cx: &mut Cx<'_>,
) {
    let state = cx.state.runs.get(run);
    let own = state.is_some_and(|run| {
        run.workflows
            .iter()
            .any(|workflow| workflow.id == declared.id && workflow.parent.is_none())
    });
    if !story.crew || !own || story.workflow.is_some() {
        return;
    }
    let named = state.and_then(|run| run.named.as_ref());
    let name = named
        .map(|named| named.name.trim())
        .filter(|name| !name.is_empty())
        .unwrap_or(declared.name.trim())
        .to_owned();
    let description = named
        .map(|named| named.description.trim())
        .filter(|text| !text.is_empty())
        .unwrap_or(declared.description.trim())
        .to_owned();
    let based_on = named
        .and_then(|named| named.based_on.as_deref())
        .map(str::trim)
        .filter(|based_on| !based_on.is_empty() && *based_on != name)
        .map(|based_on| sanitize_line(based_on).into_owned());
    // The workflow in use, as your settings name it, unless the run goes by that name.
    let in_use = state
        .and_then(|run| run.workflow.as_deref())
        .map(str::trim)
        .filter(|in_use| !in_use.is_empty() && *in_use != name)
        .map(|in_use| sentence_case(&sanitize_line(in_use)));
    let start = Start {
        name: name.clone(),
        description,
        declared: declared.clone(),
        in_use,
        based_on,
        reviewers_asked: story.reviewers_asked.clone(),
    };
    let waits = state.is_some_and(|run| run.reported.expected_from(&declared.id));
    story.workflow = Some(name);
    if waits {
        story.workflow_start = Some(start);
    } else {
        emit(cx, start.item(None));
    }
}

/// The workflow one of the run's steps runs reported. Its first report prints the start item
/// that waited for it, with the phases it names.
pub(super) fn reported(report: &WorkflowReport, story: &mut RunStory, cx: &mut Cx<'_>) {
    // A report that names nothing yet leaves the item waiting for one that does.
    let named = !report.workflow.phases.is_empty() || !report.workflow.agents.is_empty();
    let ours = named
        && story
            .workflow_start
            .as_ref()
            .is_some_and(|start| start.declared.id == report.step.workflow);
    if let Some(start) = story.workflow_start.take_if(|_| ours) {
        emit(cx, start.item(Some(report)));
    }
}

/// A run's start item that hasn't printed: what it says, and the phases its workflow declared.
#[derive(Debug)]
pub(super) struct Start {
    name: String,
    description: String,
    declared: WorkflowDeclared,
    /// The workflow your settings have in use, when the run goes by another name.
    in_use: Option<String>,
    /// The learned workflow it started from.
    based_on: Option<String>,
    /// The reviewers your words named, by name, said beside the phases.
    reviewers_asked: Option<String>,
}

impl Start {
    /// `● Workflow(<name>) · <workflow in use> · ctrl+t to open`, then its phases on one line.
    /// The description stands in for phases it hasn't named.
    fn item(&self, report: Option<&WorkflowReport>) -> ToolCall {
        let mut tail = String::new();
        for part in [&self.in_use, &self.based_on].into_iter().flatten() {
            tail.push_str(" · ");
            if Some(part) == self.based_on.as_ref() {
                tail.push_str("started from ");
            }
            tail.push_str(part);
        }
        tail.push_str(" · ctrl+t to open");
        let mut call = ToolCall::call(
            ToolKind::Other,
            "Workflow",
            sanitize_line(&self.name).into_owned(),
        )
        .with_state(ToolState::Done)
        .with_tail(tail);
        // The size your request asked for, which the orchestrator keeps to.
        if let Some(size) = report.and_then(|report| report.size_asked.as_deref()) {
            call.push(ToolItem::text(format!(
                "As you asked: {}",
                sanitize_line(size)
            )));
        }
        let phases = phase_lines(&self.declared, report);
        let beside = self
            .reviewers_asked
            .as_ref()
            .map(|names| format!("reviewers: {names} (you asked)"));
        if !phases.is_empty() {
            call.push(match beside {
                Some(beside) => ToolItem::PhasesBeside { phases, beside },
                None => ToolItem::Phases(phases),
            });
        } else if !self.description.is_empty() {
            call.push(ToolItem::text(sanitize_line(&self.description)));
        }
        call
    }
}

/// The reviewers a run's request named, as "GPT 5.6 Luna on Codex", when it asked for them:
/// an instruction to review was followed, and the run's settings hold who reviews.
pub(super) fn reviewers_asked(started: &RunStarted, names: &DisplayNames) -> Option<String> {
    let asked = started
        .instructions
        .iter()
        .any(|instruction| instruction.kind == InstructionKind::Review && instruction.followed());
    if !asked {
        return None;
    }
    let reviewers: Vec<String> = started.settings["roles"]["reviewer"]["workers"]
        .as_array()?
        .iter()
        .filter_map(|worker| worker.as_str()?.parse::<WorkerRef>().ok())
        .map(|worker| names.worker(&names.running(&worker)))
        .collect();
    (!reviewers.is_empty()).then(|| reviewers.join(", "))
}

/// The phases as the workflow declares them, in its order, each saying whether it is a step
/// you require. The phases `report` names stand in for the phase whose step runs them.
fn phase_lines(declared: &WorkflowDeclared, report: Option<&WorkflowReport>) -> Vec<PhaseLine> {
    let report = report
        .filter(|report| report.step.workflow == declared.id && !report.workflow.phases.is_empty());
    let mut lines = Vec::new();
    for phase in &declared.phases {
        match report.filter(|report| report.step.phase == phase.index) {
            Some(report) => {
                lines.extend(report.workflow.phases.iter().map(|reported| PhaseLine {
                    title: reported.title.clone(),
                    detail: reported.detail.clone(),
                    required: declared.requires_step(&reported.title),
                }));
            }
            None => lines.push(PhaseLine {
                title: phase.title.clone(),
                detail: phase.detail.clone(),
                required: declared.requires(phase),
            }),
        }
    }
    lines
}

/// `● The lead changed the workflow · added Fix again`, with why under it.
pub(super) fn phase_added(title: &str, why: &str, story: &RunStory, cx: &mut Cx<'_>) {
    if story.workflow.is_none() || quiet(cx) {
        return;
    }
    let mut call = ToolCall::described(
        ToolKind::Other,
        format!(
            "The main agent changed the workflow · added {}",
            sanitize_line(title.trim())
        ),
    )
    .with_state(ToolState::Done);
    let why = sanitize_line(why.trim());
    if !why.is_empty() {
        call.push(ToolItem::text(why.into_owned()));
    }
    emit(cx, call);
}

/// `● Workflow(<name>) · done`, with its agents, time, tokens, and spend, as Claude Code's
/// tool call returns, before the run's own end. It ends as the run's panel row does, so a
/// stopped run's workflow reads `· stopped`, never `· done`. A failed run is one block, "The
/// run failed", so its workflow adds no row of its own.
pub(super) fn done(
    run: &RunId,
    story: &mut RunStory,
    outcome: RunOutcome,
    elapsed: Option<Duration>,
    cx: &mut Cx<'_>,
) {
    // A start item still waiting for a report prints as declared: none will come now.
    if let Some(start) = story.workflow_start.take() {
        emit(cx, start.item(None));
    }
    let Some(name) = &story.workflow else {
        return;
    };
    if outcome == RunOutcome::Failed {
        return;
    }
    let mut parts = Vec::new();
    if let Some(state) = cx.state.runs.get(run) {
        parts.push(plural(state.sessions.len(), "agent", "agents"));
        if let Some(elapsed) = elapsed {
            parts.push(format::duration(elapsed));
        }
        let tokens: u64 = state
            .sessions
            .values()
            .map(|session| session.tokens_out)
            .sum();
        if tokens > 0 {
            parts.push(format!("{} tokens", format::tokens(tokens)));
        }
        parts.push(format::spend(&state.spend()));
    }
    let ended = WorkflowRowState::ended(outcome);
    let state = match ended {
        WorkflowRowState::Failed => ToolState::Failed,
        WorkflowRowState::Stopped => ToolState::Stopped,
        _ => ToolState::Done,
    };
    let mut call = ToolCall::described(
        ToolKind::Other,
        format!("Workflow({}) · {}", sanitize_line(name), ended.word()),
    )
    .with_state(state);
    if !parts.is_empty() {
        call.push(ToolItem::text(parts.join(" · ")));
    }
    emit(cx, call);
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::workflow::DeclaredPhase;
    use argonauts_core::names::WorkflowId;
    use argonauts_core::workflow::StandardPhase;

    use super::*;

    /// The chat lists the phases a workflow declares, whatever they are called and however
    /// often a name comes, and marks the ones it says you require.
    #[test]
    fn the_phases_listed_are_the_workflows_own_with_your_steps_marked() {
        let titles = ["Draft", "Critique", "Draft", "secrets scan"];
        let declared = WorkflowDeclared {
            id: WorkflowId::numbered(1),
            name: "write-the-paper".into(),
            description: String::new(),
            phases: titles
                .iter()
                .zip(0..)
                .map(|(title, index)| DeclaredPhase {
                    index,
                    title: (*title).into(),
                    detail: format!("part {index}"),
                    standard: StandardPhase::Do,
                })
                .collect(),
            required: vec!["Secrets scan".into()],
        };
        let lines = phase_lines(&declared, None);
        let listed: Vec<(&str, bool)> = lines
            .iter()
            .map(|line| (line.title.as_str(), line.required))
            .collect();
        assert_eq!(
            listed,
            [
                ("Draft", false),
                ("Critique", false),
                ("Draft", false),
                ("secrets scan", true)
            ]
        );
        assert_eq!(lines[2].detail, "part 2");
    }
}
