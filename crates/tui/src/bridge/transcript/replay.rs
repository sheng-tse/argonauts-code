//! A CLI's own session carried on prints its turns as they were: your words as you typed them,
//! and each thing its agent did, in order. An Argonauts Code conversation needs none of this: its
//! own record comes back through the printers that drew it live.

use argonauts_core::app::ReplayTurn;
use argonauts_core::cli_sessions::{CliCall, CliStep};
use argonauts_core::events::run::Party;

use crate::bridge::tools;
use crate::cell::{
    AssistantMessage, Cell, CollapsedGroup, Thought, ToolCall, ToolItem, UserMessage,
};

use super::conversation::folds;
use super::{Cx, quiet};

/// The conversation takes the screen from the picker that chose it, with the session's turns,
/// if it came with any.
pub(super) fn conversation_resumed(turns: &[ReplayTurn], cx: &mut Cx<'_>) {
    cx.out.close_views = true;
    for turn in turns {
        print_turn(turn, cx);
    }
}

/// Your words as you typed them, and the agents' answers as they wrote them.
fn print_turn(turn: &ReplayTurn, cx: &mut Cx<'_>) {
    for reprint in turn_cells(turn, cx) {
        if reprint.prints {
            cx.model.history.push(reprint.cell);
        } else {
            cx.model.history.keep(reprint.cell);
        }
    }
}

/// A cell of a reprinted turn. One that doesn't print joins the Ctrl+O history alone.
struct Reprint {
    cell: Cell,
    prints: bool,
}

/// A turn as it printed when it happened: the words alone, or for a CLI session's turn each
/// thing the agent did, in order. Its calls follow the verbosity as live ones do, and what it
/// thought is kept for Ctrl+O, as Claude Code keeps it.
fn turn_cells(turn: &ReplayTurn, cx: &Cx<'_>) -> Vec<Reprint> {
    if turn.steps.is_empty() {
        let cell = turn_cell(turn);
        return cell
            .map(|cell| Reprint { cell, prints: true })
            .into_iter()
            .collect();
    }
    let agent_id = match &turn.from {
        Party::Agent { worker, .. } => worker.agent.as_str(),
        _ => "",
    };
    let places = tools::Places::of_conversation(cx.state);
    let calls_print = !quiet(cx);
    let mut cells: Vec<Reprint> = Vec::new();
    for step in &turn.steps {
        match step {
            CliStep::Wrote { text } => cells.push(Reprint {
                cell: AssistantMessage::new(text.trim_end()).into(),
                prints: true,
            }),
            CliStep::Thought { text } if text.trim().is_empty() => {}
            CliStep::Thought { text } => cells.push(Reprint {
                cell: Cell::Custom(Box::new(Thought::new(text.as_str()))),
                prints: false,
            }),
            CliStep::Ran(call) => {
                let row = call_row(agent_id, call, &places);
                let group = match cells.last_mut() {
                    Some(Reprint {
                        cell: Cell::Group(group),
                        ..
                    }) => Some(group),
                    _ => None,
                };
                let cell = match (folds(&row, cx), group) {
                    (true, Some(group)) => {
                        group.push(row);
                        continue;
                    }
                    (true, None) => {
                        let mut group = CollapsedGroup::new();
                        group.push(row);
                        group.into()
                    }
                    (false, _) => row.into(),
                };
                cells.push(Reprint {
                    cell,
                    prints: calls_print,
                });
            }
        }
    }
    cells
}

/// A call's row as it stood once the call ended: its patches in place of the preview, then its
/// result. A call the transcript never saw end reads as stopped. What a subagent it started
/// ran folds into its result, as on Claude Code's screen, and Ctrl+O shows each call.
fn call_row(agent_id: &str, call: &CliCall, places: &tools::Places) -> ToolCall {
    let whole = whole_row(agent_id, call, places);
    let is_nested = |item: &ToolItem| matches!(item, ToolItem::Call(_) | ToolItem::Group(_));
    if !whole.body.iter().any(is_nested) {
        return whole;
    }
    let mut folded = whole.clone();
    folded.body.retain(|item| !is_nested(item));
    folded.full = Some(Box::new(whole));
    folded
}

/// The row with every call of its subagent under it, at every depth.
fn whole_row(agent_id: &str, call: &CliCall, places: &tools::Places) -> ToolCall {
    let mut row = tools::start_call_in(agent_id, &call.name, &call.input, places);
    row.kind = tools::kind_of(agent_id, &call.name);
    for step in &call.steps {
        if let CliStep::Ran(inner) = step {
            row.push_call(whole_row(agent_id, inner, places));
        }
    }
    for patch in &call.patches {
        if patch.numbered {
            tools::take_patch_in(&mut row, &patch.path, &patch.hunks, places);
        } else {
            tools::take_unnumbered_patch_in(&mut row, &patch.path, &patch.hunks, places);
        }
    }
    match call.ok {
        Some(ok) => tools::finish_call(&mut row, ok, call.output.as_deref()),
        None => row.stop(),
    }
    row
}

fn turn_cell(turn: &ReplayTurn) -> Option<Cell> {
    let text = turn.text.trim_end();
    if text.trim().is_empty() {
        return None;
    }
    Some(match &turn.from {
        Party::User => UserMessage::new(text).into(),
        Party::OutsideAgent { client } => UserMessage::new(format!("{client}: {text}")).into(),
        _ => AssistantMessage::new(text).into(),
    })
}
