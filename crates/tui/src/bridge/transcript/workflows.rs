//! A crew run's workflow in the conversation, as Claude Code prints its own Workflow call:
//! `● Workflow(<name> · <kind> workflow)` with `Running in background · /workflows to monitor
//! and save` under it when it starts, a line when the main agent adds a phase, and the same
//! call with `✔ Completed in 6m 20s · 6 agents · 177k tokens · merged onto main` when it ends,
//! or `✘ Stopped in` as the run ended.
//!
//! A run whose step runs another workflow's steps declares only that they run. Its start item
//! waits for that workflow's first report, so the conversation says nothing of a workflow whose
//! script doesn't exist yet.

use std::time::Duration;

use argonauts_core::RunId;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::run::{RunOutcome, RunStarted};
use argonauts_core::events::workflow::{WorkflowDeclared, WorkflowReport};
use argonauts_core::hidden_git::MergedInto;
use argonauts_core::names::ModelRef;
use argonauts_core::steps::intake::InstructionKind;
use argonauts_core::text::sentence_case;

use time::OffsetDateTime;

use crate::bottom::WorkflowRowState;
use crate::cell::{ToolCall, ToolItem, ToolKind, ToolState, WorkflowEnd, WorkflowResult};
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
        name,
        declared: declared.clone(),
        in_use,
        based_on,
        reviewers_asked: story.reviewers_asked.clone(),
    };
    let waits = state.is_some_and(|run| run.reported.expected_from(&declared.id));
    story.workflow = Some(start.title());
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

/// A run's start item that hasn't printed.
#[derive(Debug)]
pub(super) struct Start {
    name: String,
    declared: WorkflowDeclared,
    /// The workflow your settings have in use, when the run goes by another name.
    in_use: Option<String>,
    /// The learned workflow it started from.
    based_on: Option<String>,
    /// The reviewers your words named, by name.
    reviewers_asked: Option<String>,
}

impl Start {
    /// The run's name and what kind of workflow it is, as its view's header says them: `Add
    /// due dates · orchestrator workflow`.
    fn title(&self) -> String {
        let kind = self
            .based_on
            .as_deref()
            .or(self.in_use.as_deref())
            .map(|kind| format!(" · {} workflow", kind.to_lowercase()));
        format!("{}{}", sanitize_line(&self.name), kind.unwrap_or_default())
    }

    /// `● Workflow(<name> · <kind> workflow)`, and under it that it runs in the background and
    /// what opens it. What your own words asked of it follows on lines of their own.
    fn item(&self, report: Option<&WorkflowReport>) -> ToolCall {
        let mut call =
            ToolCall::call(ToolKind::Other, "Workflow", self.title()).with_state(ToolState::Done);
        call.push(ToolItem::Workflow(WorkflowResult::Running));
        // The size your request asked for, which the orchestrator keeps to.
        if let Some(size) = report.and_then(|report| report.size_asked.as_deref()) {
            call.push(ToolItem::text(format!(
                "As you asked: {}",
                sanitize_line(size)
            )));
        }
        if let Some(names) = &self.reviewers_asked {
            call.push(ToolItem::text(format!("reviewers: {names} (you asked)")));
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
    let reviewers: Vec<String> = started.settings["roles"]["reviewer"]["models"]
        .as_array()?
        .iter()
        .filter_map(|worker| worker.as_str()?.parse::<ModelRef>().ok())
        .map(|worker| names.worker(&names.running(&worker)))
        .collect();
    (!reviewers.is_empty()).then(|| reviewers.join(", "))
}

/// `● The main agent changed the workflow · added Fix again`, with why under it.
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

/// `● Workflow(<name> · <kind> workflow)` again, with `✔ Completed in 6m 20s · 6 agents · 177k
/// tokens` under it as Claude Code's call returns, and how the run ended after them, before
/// the run's own end. It ends as the run's panel row does, so a stopped run's workflow reads
/// `✘ Stopped in`, never completed. A failed run is one block, "The run failed", so its workflow
/// adds no row of its own.
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
    let Some(title) = &story.workflow else {
        return;
    };
    if outcome == RunOutcome::Failed {
        return;
    }
    let mut counts = Vec::new();
    if let Some(elapsed) = elapsed {
        counts.push(format::duration(elapsed));
    }
    if let Some(state) = cx.state.runs.get(run) {
        // The workflow's agents and tokens, as its view's header and its panel row count them.
        let totals = crate::bridge::panel::run_totals(state, OffsetDateTime::now_utc());
        let agents = usize::try_from(totals.agents_total).unwrap_or(usize::MAX);
        counts.push(plural(agents, "agent", "agents"));
        if totals.tokens > 0 {
            counts.push(format!("{} tokens", format::tokens(totals.tokens)));
        }
        // How it ended, as the run's row in the bottom list says it.
        match (outcome, state.merged_onto.as_deref()) {
            (RunOutcome::Merged, Some(target)) => counts.push(MergedInto::of(target).ended()),
            (RunOutcome::Merged, None) => counts.push("merged".to_owned()),
            (RunOutcome::PullRequest, _) => counts.push("a pull request is open".to_owned()),
            (RunOutcome::BranchLeft, _) => counts.push("its branch was kept".to_owned()),
            (RunOutcome::Stopped, _) => counts.push("/resume carries it on".to_owned()),
            (RunOutcome::Abandoned | RunOutcome::Failed, _) => {}
        }
    }
    let (how, state) = match WorkflowRowState::ended(outcome) {
        WorkflowRowState::Failed => (WorkflowEnd::Failed, ToolState::Failed),
        WorkflowRowState::Stopped => (WorkflowEnd::Stopped, ToolState::Stopped),
        _ => (WorkflowEnd::Completed, ToolState::Done),
    };
    let mut call = ToolCall::call(ToolKind::Other, "Workflow", title.clone()).with_state(state);
    call.push(ToolItem::Workflow(WorkflowResult::Ended {
        how,
        counts: counts.join(" · "),
    }));
    emit(cx, call);
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use argonauts_core::names::WorkflowId;

    use super::*;
    use crate::cell::HistoryCell;
    use crate::render::{Clock, RenderCtx};
    use crate::theme::Role;
    use crate::views::workflow_pictures::{buffer_of, dark, picture};

    fn start() -> Start {
        Start {
            name: "Add due dates".to_owned(),
            declared: WorkflowDeclared {
                id: WorkflowId::numbered(1),
                name: "orchestrator".to_owned(),
                description: String::new(),
                phases: Vec::new(),
                required: Vec::new(),
            },
            in_use: None,
            based_on: Some("orchestrator".to_owned()),
            reviewers_asked: None,
        }
    }

    /// The run's two items as their picture draws them, cell for cell: Claude Code's Workflow
    /// call with where to watch it, then the same call with how it ended.
    #[test]
    fn the_workflows_items_read_as_their_picture() {
        let theme = dark();
        let ctx = RenderCtx::new(118, &theme, Clock::still(Instant::now()));
        let mut picture = picture!("workflow_transcript");
        // Every row of the conversation opens with the theme's bullet, in green once its call
        // returned. The picture draws Claude Code's own glyph, plain.
        for row in [0, 3] {
            picture.reword(row, "⏺", "●");
            picture.recolour(row, 0, theme.style(Role::Success).fg);
        }
        let started = start().item(None);
        picture.assert_drawn(0..2, &buffer_of(&started.lines(&ctx), 118), 0);

        let mut ended = ToolCall::call(ToolKind::Other, "Workflow", start().title())
            .with_state(ToolState::Done);
        ended.push(ToolItem::Workflow(WorkflowResult::Ended {
            how: WorkflowEnd::Completed,
            counts: "6m 20s · 6 agents · 177k tokens · merged onto main".to_owned(),
        }));
        picture.assert_drawn(3..5, &buffer_of(&ended.lines(&ctx), 118), 0);
    }
}
