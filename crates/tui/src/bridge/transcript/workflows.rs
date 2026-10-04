//! A crew run's workflow in the chat, as Claude Code prints its own: `● Workflow(<name>)` when
//! it starts, with the description and the phases it declares, a line when the lead adds a
//! phase, and `● Workflow(<name>) · done` when it ends.

use std::time::Duration;

use argonauts_core::RunId;
use argonauts_core::events::workflow::{DeclaredPhase, WorkflowDeclared};

use crate::cell::{ToolCall, ToolItem, ToolKind, ToolState};
use crate::format;
use crate::text::{sanitize_line, str_width};
use crate::views::layout::left;

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
    let mut call = ToolCall::call(
        ToolKind::Other,
        "Workflow",
        sanitize_line(&name).into_owned(),
    )
    .with_state(ToolState::Done);
    if !description.is_empty() {
        call.push(ToolItem::text(sanitize_line(&description)));
    }
    for phase in phase_lines(&declared.phases) {
        call.push(phase);
    }
    let watch = match named
        .and_then(|named| named.based_on.as_deref())
        .map(str::trim)
        .filter(|based_on| !based_on.is_empty() && *based_on != name)
    {
        Some(based_on) => format!("Started from {} · ctrl+t to watch", sanitize_line(based_on)),
        None => "ctrl+t to watch".to_owned(),
    };
    call.push(ToolItem::text(watch));
    story.workflow = Some(name);
    emit(cx, call);
}

/// `1. Understand  three models read the issue`, the titles padded so the details line up, and
/// a detail too long for one row wraps under its own column.
fn phase_lines(phases: &[DeclaredPhase]) -> Vec<ToolItem> {
    let titles: Vec<String> = phases
        .iter()
        .map(|phase| sanitize_line(phase.title.trim()).into_owned())
        .collect();
    let widest = titles
        .iter()
        .map(|title| str_width(title))
        .max()
        .unwrap_or(0);
    phases
        .iter()
        .zip(&titles)
        .enumerate()
        .map(|(i, (phase, title))| {
            let detail = sanitize_line(phase.detail.trim()).into_owned();
            if detail.is_empty() {
                ToolItem::text(format!("{}. {title}", i + 1))
            } else {
                ToolItem::Hanging {
                    lead: format!("{}. {}  ", i + 1, left(title, widest)),
                    text: detail,
                }
            }
        })
        .collect()
}

/// `● The lead changed the workflow · added Fix again`, with why under it.
pub(super) fn phase_added(title: &str, why: &str, story: &RunStory, cx: &mut Cx<'_>) {
    if story.workflow.is_none() || quiet(cx) {
        return;
    }
    let mut call = ToolCall::described(
        ToolKind::Other,
        format!(
            "The lead changed the workflow · added {}",
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
/// tool call returns, before the run's own end.
pub(super) fn done(run: &RunId, story: &RunStory, elapsed: Option<Duration>, cx: &mut Cx<'_>) {
    let Some(name) = &story.workflow else {
        return;
    };
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
    let mut call = ToolCall::described(
        ToolKind::Other,
        format!("Workflow({}) · done", sanitize_line(name)),
    )
    .with_state(ToolState::Done);
    if !parts.is_empty() {
        call.push(ToolItem::text(parts.join(" · ")));
    }
    emit(cx, call);
}
