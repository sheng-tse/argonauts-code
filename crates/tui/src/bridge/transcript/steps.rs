//! The run's planning steps as tool-call-shaped lines: triage, goal, plan, and contracts.

use std::collections::BTreeSet;

use argonauts_core::PhaseStatus;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::goal::{GoalChangeResolution, GoalFile, GoalSource};
use argonauts_core::events::run::{Party, RunMode, RunPhase, RunStarted};

use crate::cell::{Notice, Tone, ToolCall, ToolItem, ToolKind, ToolState};

use super::milestones::RunStory;
use super::{Cx, emit, quiet};

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
        Party::User => Some("you".to_owned()),
        Party::Rules => Some("your rules".to_owned()),
        Party::Config => Some("your settings".to_owned()),
        Party::Approver { model } | Party::Agent { worker: model, .. } => Some(names.worker(model)),
        Party::FrontEnd { client } => Some(client.clone()),
        Party::Conductor => None,
    }
}

pub(super) fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}
/// `● Triage(solo)` and what the run starts from.
pub(super) fn triage(started: &RunStarted, cx: &mut Cx<'_>) {
    let mode = match started.mode {
        RunMode::Solo => "solo",
        RunMode::Crew => "crew",
    };
    let args = if started.forced {
        format!("{mode}, as you asked")
    } else {
        mode.to_owned()
    };
    let mut details = Vec::new();
    let branch = started
        .base_branch
        .as_deref()
        .map(str::trim)
        .filter(|branch| !branch.is_empty());
    let mut recorded = match branch {
        Some(branch) => format!("Starts from {branch}"),
        None if started.base_commit.trim().is_empty() => {
            "Starts from your work as it is".to_owned()
        }
        None => "Starts from the commit you're on".to_owned(),
    };
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
        recorded.push_str(&format!(" · {files} {fate}"));
    }
    details.push(recorded);
    if let Some(estimate) = &started.estimate {
        let files = usize::try_from(estimate.file_count).unwrap_or(usize::MAX);
        let mut line = format!("Estimate: {}", plural(files, "file", "files"));
        if !estimate.task_type.trim().is_empty() {
            line.push_str(&format!(" · {}", estimate.task_type.trim()));
        }
        line.push_str(if estimate.tests_exist {
            " · tests exist"
        } else {
            " · no tests yet"
        });
        if !estimate.risk_labels.is_empty() {
            line.push_str(&format!(" · touches {}", estimate.risk_labels.join(", ")));
        }
        details.push(line);
    }
    if let Some(triage) = &started.triage {
        details.push(triage.reason.clone());
        if triage.escalate_to_crew {
            details.push("Solo first, and a crew if it grows".to_owned());
        }
    }
    let restate_first = started
        .settings
        .pointer("/solo/restate_first")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if started.mode == RunMode::Solo && !restate_first {
        details.push("Goal: your prompt as you wrote it".to_owned());
    }
    if let Some(client) = &started.front_end {
        details.push(format!("Started by {client}"));
    }
    if let Some(earlier) = &started.continues {
        details.push(format!("Continues {earlier}"));
    }
    emit(cx, step("Triage", &args, ToolState::Done, &details));
}
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

fn phase_name(phase: RunPhase) -> &'static str {
    match phase {
        RunPhase::Goal => "The goal step",
        RunPhase::Planning => "Planning",
        RunPhase::Dividing => "Dividing the plan",
        RunPhase::Contracts => "The contracts step",
        RunPhase::Build => "The build",
        RunPhase::FinalCheck => "The final check",
        RunPhase::Merge => "The merge",
        RunPhase::Retro => "The retro",
    }
}
/// `● Goal(Add a --json flag to export)` then `⎿  Agreed by 3 agents · 1 question answered`.
/// A solo run's goal is your prompt, which triage already said.
pub(super) fn goal(story: &RunStory, file: &GoalFile, by: &Party, cx: &mut Cx<'_>) {
    if file.source == GoalSource::Prompt {
        return;
    }
    let mut parts = Vec::new();
    if story.restatements > 0 {
        let agents = usize::try_from(story.restatements).unwrap_or(usize::MAX);
        parts.push(format!("Agreed by {}", plural(agents, "agent", "agents")));
    }
    if !file.answers.is_empty() {
        parts.push(format!(
            "{} answered",
            plural(file.answers.len(), "question", "questions")
        ));
    }
    if !file.assumptions.is_empty() {
        parts.push(format!(
            "{} stated",
            plural(file.assumptions.len(), "assumption", "assumptions")
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

/// `● Plan(3 drafts, judged)`, the winner and the card count, then the disputed topics.
pub(super) fn plan(story: &RunStory, cards: usize, cx: &mut Cx<'_>) {
    let drafts = usize::try_from(story.drafts).unwrap_or(usize::MAX);
    let cards = plural(cards, "card", "cards");
    let (args, outcome) = if let Some(coordinator) = &story.coordinated {
        let who = cx.state.names.worker(coordinator);
        (format!("coordinated by {who}"), cards)
    } else if let Some(merger) = &story.fast_path {
        let who = cx.state.names.worker(merger);
        (
            format!("{}, merged", plural(drafts, "draft", "drafts")),
            format!("No disputed topics · merged by {who} · {cards}"),
        )
    } else if let Some(judged) = &story.judged {
        let won = match judged.winner {
            Some(label) if !judged.near_tie => format!("Plan {label} won"),
            _ => "A near tie, so you pick at the plan checkpoint".to_owned(),
        };
        (
            format!("{}, judged", plural(drafts, "draft", "drafts")),
            format!("{won} · {cards}"),
        )
    } else if drafts > 0 {
        (plural(drafts, "draft", "drafts"), cards)
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
