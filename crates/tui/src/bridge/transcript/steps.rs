//! The run's planning steps as tool-call-shaped lines: what its start says, the goal, the plan,
//! and contracts.

use std::collections::BTreeSet;

use argonauts_core::PhaseStatus;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::goal::{GoalChangeResolution, GoalFile, GoalSource};
use argonauts_core::events::run::{
    CheckedInstruction, InstructionOutcome, Party, RunPhase, RunStarted,
};
use argonauts_core::log::RUN_REVIEW_FAILED;
use argonauts_core::steps::intake::InstructionKind;

use crate::cell::{Notice, Tone, ToolCall, ToolItem, ToolKind, ToolState};

use super::milestones::RunStory;
use super::{Cx, emit, quiet};
use crate::views::layout::cut;

/// `● Name(args)` with its details under one `⎿`.
pub(super) fn step(name: &str, args: &str, state: ToolState, details: &[String]) -> ToolCall {
    let mut call = ToolCall::call(ToolKind::Other, name, args).with_state(state);
    let details: Vec<&str> = details
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect();
    if !details.is_empty() {
        call.push(ToolItem::Text {
            text: details.join("\n"),
            tone: Tone::Dim,
        });
    }
    call
}

/// Who decided, in a few words, or `None` for a fixed rule.
pub(super) fn party_words(party: &Party, names: &DisplayNames) -> Option<String> {
    match party {
        Party::User | Party::Declined => Some("you".to_owned()),
        Party::Rules => Some("your rules".to_owned()),
        Party::Config => Some("your settings".to_owned()),
        Party::Autopilot => Some("autopilot".to_owned()),
        Party::Judge { model } | Party::Agent { worker: model, .. } => Some(names.worker(model)),
        Party::OutsideAgent { client } => Some(client.clone()),
        Party::ArgonautsCode => None,
    }
}

pub(super) fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}
/// What a run's start says in the conversation. It has no row of its own: the workflow's
/// own row marks the start, as Claude Code's does. Only what you would otherwise never hear is
/// said, in one quiet item: what your words asked and what of it wasn't followed, the files
/// left out, and who else started it.
///
/// `reviewers_beside` is true when the workflow's row names the reviewers you asked for, so
/// this line leaves them out.
pub(super) fn run_started(started: &RunStarted, reviewers_beside: bool, cx: &mut Cx<'_>) {
    let instructions: Vec<CheckedInstruction> = started
        .instructions
        .iter()
        .filter(|instruction| {
            !(reviewers_beside
                && instruction.kind == InstructionKind::Review
                && instruction.followed())
        })
        .cloned()
        .collect();
    let mut lines = as_you_asked(&instructions);
    if !started.uncommitted.is_empty() {
        let files = plural(
            started.uncommitted.len(),
            "uncommitted file",
            "uncommitted files",
        );
        let fate = if started.uncommitted_included {
            "included"
        } else {
            "left out"
        };
        lines.push(format!("{files} {fate}"));
    }
    if let Some(client) = &started.outside_agent {
        lines.push(format!("Started by {client}"));
    }
    start_note(lines, cx);
}

/// What your words asked of a Captain that works in your checkout, which has no run to say it.
pub(super) fn direct_asked(instructions: &[CheckedInstruction], cx: &mut Cx<'_>) {
    start_note(as_you_asked(instructions), cx);
}

/// The first line stands as the item, and the rest go under it.
fn start_note(mut lines: Vec<String>, cx: &mut Cx<'_>) {
    if lines.is_empty() {
        return;
    }
    let mut note = Notice::new(Tone::Dim, lines.remove(0));
    note.detail = lines;
    emit(cx, note);
}

/// `As you asked: Codex orchestrates · Claude Code reviews`, then a `Not followed:` line for
/// each instruction that wasn't, or was only in part, with its words and why. Instructions
/// whose words you didn't write show only in Ctrl+O.
pub(super) fn as_you_asked(instructions: &[CheckedInstruction]) -> Vec<String> {
    let followed: Vec<&str> = instructions
        .iter()
        .filter_map(|instruction| match &instruction.outcome {
            InstructionOutcome::Followed { said, .. } => Some(said.trim()),
            _ => None,
        })
        .filter(|said| !said.is_empty())
        .collect();
    let mut lines = Vec::new();
    if !followed.is_empty() {
        lines.push(format!("As you asked: {}", followed.join(" · ")));
    }
    for instruction in instructions {
        let why = match &instruction.outcome {
            InstructionOutcome::NotFollowed { why } => why,
            InstructionOutcome::Followed {
                note: Some(note), ..
            } => note,
            _ => continue,
        };
        lines.push(format!(
            "Not followed: \"{}\" · {}",
            cut(instruction.says.trim(), SAYS_CELLS),
            why.trim()
        ));
    }
    lines
}

/// The longest an instruction's own words show on a `Not followed:` line, in cells.
const SAYS_CELLS: usize = 40;

/// A failed phase, and the contracts step, whose events no other line sums up.
pub(super) fn phase(phase: RunPhase, status: PhaseStatus, summary: Option<&str>, cx: &mut Cx<'_>) {
    let summary = summary.map(str::trim).filter(|summary| !summary.is_empty());
    match status {
        PhaseStatus::Failed => {
            let mut text = format!("{} failed", phase_name(phase));
            if let Some(summary) = summary {
                text.push_str(&format!(" · {summary}"));
            }
            emit(cx, Notice::new(Tone::Error, text));
        }
        PhaseStatus::Finished if phase == RunPhase::Contracts => {
            emit(
                cx,
                step("Contracts", summary.unwrap_or(""), ToolState::Done, &[]),
            );
        }
        _ => {}
    }
}

/// `● The run review failed. The run's result stands.` Why is often a raw schema error, so it waits
/// for Ctrl+O.
pub(super) fn run_review_failed(reason: Option<&str>, cx: &mut Cx<'_>) {
    let mut notice = Notice::new(Tone::Warning, RUN_REVIEW_FAILED);
    if let Some(reason) = reason.map(str::trim).filter(|reason| !reason.is_empty()) {
        notice = notice.with_more(reason);
    }
    emit(cx, notice);
}

fn phase_name(phase: RunPhase) -> &'static str {
    match phase {
        RunPhase::Understand => "Understand",
        RunPhase::Planning => "Planning",
        RunPhase::Dividing => "Dividing the plan",
        RunPhase::Contracts => "The contracts step",
        RunPhase::Build => "The build",
        RunPhase::FinalCheck => "The final check",
        RunPhase::Merge => "The merge",
        RunPhase::RunReview => "The run review",
    }
}
/// `● Goal(Add a --json flag to export)` then `⎿  Agreed by 3 agents · 1 question answered`,
/// with the questions the main agent answered for you counted apart from the other assumptions.
/// A Captain run's goal is your prompt, which the estimate already said.
pub(super) fn goal(story: &RunStory, file: &GoalFile, by: &Party, cx: &mut Cx<'_>) {
    if file.source == GoalSource::Prompt {
        return;
    }
    let mut parts = Vec::new();
    if story.readings > 0 {
        let agents = usize::try_from(story.readings).unwrap_or(usize::MAX);
        parts.push(format!("Agreed by {}", plural(agents, "agent", "agents")));
    }
    if !file.answers.is_empty() {
        parts.push(format!(
            "{} answered",
            plural(file.answers.len(), "question", "questions")
        ));
    }
    let by_main_agent = file
        .assumptions
        .iter()
        .filter(|assumption| assumption.by_main_agent())
        .count();
    if by_main_agent > 0 {
        parts.push(format!(
            "{} answered by the main agent",
            plural(by_main_agent, "question", "questions")
        ));
    }
    let stated = file.assumptions.len() - by_main_agent;
    if stated > 0 {
        parts.push(format!(
            "{} stated",
            plural(stated, "assumption", "assumptions")
        ));
    }
    if parts.is_empty() {
        parts.push(plural(file.criteria.len(), "criterion", "criteria"));
    }
    if !matches!(by, Party::User)
        && let Some(who) = party_words(by, &cx.state.names)
    {
        parts.push(format!("approved by {who}"));
    }
    let details = [parts.join(" · ")];
    emit(
        cx,
        step("Goal", file.summary.trim(), ToolState::Done, &details),
    );
}

pub(super) fn goal_change(resolution: GoalChangeResolution, cx: &mut Cx<'_>) {
    let (tone, text) = match resolution {
        GoalChangeResolution::Recorded => (Tone::Dim, "Recorded a clarification of the goal"),
        GoalChangeResolution::Refused => (
            Tone::Warning,
            "Kept the approved goal · the change was refused",
        ),
        GoalChangeResolution::Changed => (Tone::Success, "Changed the goal"),
        GoalChangeResolution::AskedYou => return,
    };
    emit(cx, Notice::new(tone, text));
}

/// `● Plan(3 drafts, judged)`, the winner and the task count, then the disputed topics.
pub(super) fn plan(story: &RunStory, tasks: usize, cx: &mut Cx<'_>) {
    let drafts = usize::try_from(story.drafts).unwrap_or(usize::MAX);
    let tasks = plural(tasks, "task", "tasks");
    let (args, outcome) = if let Some(orchestrator) = &story.orchestrated {
        let who = cx.state.names.worker(orchestrator);
        (format!("orchestrated by {who}"), tasks)
    } else if let Some(merger) = &story.fast_path {
        let who = cx.state.names.worker(merger);
        (
            format!("{}, merged", plural(drafts, "draft", "drafts")),
            format!("No disputed topics · merged by {who} · {tasks}"),
        )
    } else if let Some(judged) = &story.judged {
        let won = match judged.winner {
            Some(label) if !judged.near_tie => format!("Plan {label} won"),
            _ => "A near tie, so you pick at the plan approval".to_owned(),
        };
        (
            format!("{}, judged", plural(drafts, "draft", "drafts")),
            format!("{won} · {tasks}"),
        )
    } else if drafts > 0 {
        (plural(drafts, "draft", "drafts"), tasks)
    } else {
        return;
    };
    let mut call = step("Plan", &args, ToolState::Done, &[outcome]);
    if !quiet(cx)
        && let Some(table) = disputed_table(story)
    {
        call.push(ToolItem::Markdown(table));
    }
    emit(cx, call);
}

/// The disputed topics from the disagreement map, one row per topic and one column per plan.
fn disputed_table(story: &RunStory) -> Option<String> {
    let topics: Vec<_> = story.mapped.iter().filter(|topic| topic.disputed).collect();
    if topics.is_empty() {
        return None;
    }
    let labels: BTreeSet<u8> = topics
        .iter()
        .flat_map(|topic| topic.positions.iter().map(|position| position.label))
        .collect();
    let cell = |text: &str| text.replace('|', "\\|").replace('\n', " ");
    let mut table = String::from("| Disputed |");
    for label in &labels {
        table.push_str(&format!(" Plan {label} |"));
    }
    table.push_str("\n|---|");
    table.push_str(&"---|".repeat(labels.len()));
    for topic in topics {
        table.push_str(&format!("\n| {} |", cell(topic.name.trim())));
        for label in &labels {
            let summary = topic
                .positions
                .iter()
                .find(|position| position.label == *label)
                .map(|position| cell(position.summary.trim()))
                .unwrap_or_default();
            table.push_str(&format!(" {summary} |"));
        }
    }
    table.push('\n');
    Some(table)
}
