//! One line in scrollback after a question is answered, passed on, or withdrawn, such as
//! `◆ Approved merging build:redis into main · you, 10:58 AM`.

use argonauts_core::app::NOT_CHANGED;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::approval::{
    AnswerAction, ApprovalDecided, ApprovalKind, ApprovalOpened, ApprovalPacket,
};
use argonauts_core::events::run::Party;
use argonauts_core::hidden_git::MergedInto;
use argonauts_core::{RunId, TaskId};
use ratatui::text::{Line, Span};
use time::{OffsetDateTime, UtcOffset};

use super::words;
use crate::cell::{Cell, HistoryCell, Notice, ResultLine, Tone, UserMessage};
use crate::render::RenderCtx;
use crate::text::{fit_lines, sanitize_line, str_width, wrap_spans};
use crate::theme::Role;

/// The record with agents by the names their ids give and tasks by their ids, for callers that
/// don't know the app's names or the run.
pub fn record_cell(opened: &ApprovalOpened, decided: &ApprovalDecided) -> Cell {
    let names = DisplayNames::default();
    Cell::Notice(record(
        opened,
        decided,
        None,
        None,
        UtcOffset::UTC,
        &names,
        &by_id,
    ))
}

/// A task by its id, for a record made where its run isn't on screen to give its label.
pub fn by_id(task: &TaskId) -> String {
    task.to_string()
}

/// The record, naming the run for a merge without a task when `run` is given (lists that
/// span runs) or as "the run", each task as `tasks` names it, and the time it was decided in
/// the terminal's zone.
pub fn record(
    opened: &ApprovalOpened,
    decided: &ApprovalDecided,
    run: Option<&RunId>,
    at: Option<OffsetDateTime>,
    offset: UtcOffset,
    names: &DisplayNames,
    tasks: &dyn Fn(&TaskId) -> String,
) -> Notice {
    let mut who = words::party(&decided.by, names);
    if let Some(at) = at {
        who.push_str(&format!(", {}", words::clock(at, offset)));
    }
    // Esc: "carry on without me". What the agent was told isn't your words, so it isn't quoted.
    if decided.by == Party::Declined {
        let left = match opened.kind {
            ApprovalKind::Question | ApprovalKind::Understand => {
                "Question left to the agent".to_owned()
            }
            _ => format!(
                "Left without you: {}",
                outcome(opened, decided, run, names, tasks)
            ),
        };
        return Notice::answered(left, who);
    }
    let mut notice = Notice::answered(outcome(opened, decided, run, names, tasks), who);
    let own_words = decided.action == AnswerAction::Answer;
    if let Some(text) = decided.text.as_deref().map(str::trim)
        && !text.is_empty()
        && !own_words
    {
        notice = notice.with_detail(format!("“{text}”"));
    }
    notice
}

/// What the answer did, in a few words: `Approved merging build:redis into main`, `Plan
/// approved`, each task as `tasks` names it.
pub fn outcome(
    opened: &ApprovalOpened,
    decided: &ApprovalDecided,
    run: Option<&RunId>,
    names: &DisplayNames,
    tasks: &dyn Fn(&TaskId) -> String,
) -> String {
    use AnswerAction as A;
    use ApprovalKind as K;

    let subject = subject(opened, run, tasks);
    let task = packet_task(opened)
        .map(tasks)
        .unwrap_or_else(|| subject.clone());
    let title = opened.title.trim();
    let text = decided.text.as_deref().map(str::trim).unwrap_or_default();
    match (&decided.action, opened.kind) {
        // The decision, not the landing: the run's last line says when it merged.
        (A::Merge, _) => match merge_target(opened) {
            Some(target) => MergedInto::of(target).approved(&subject),
            None => format!("Approved merging {subject}"),
        },
        // The answer, not what came of it: the run's own line says whether one was opened.
        (A::PullRequest, _) => format!("Asked for a pull request for {subject}"),
        (A::RequestChanges, _) if decided.tasks.is_empty() => {
            "Sent every task back with changes".to_owned()
        }
        (A::RequestChanges, _) => {
            let sent: Vec<String> = decided.tasks.iter().map(tasks).collect();
            format!("Sent {} back with changes", words::joined(&sent))
        }
        (A::KeepBranch, _) => format!("Kept {subject}'s work on its branch, not merged"),
        (A::Abandon, K::Understand) => "Abandoned the goal".to_owned(),
        (A::Abandon, _) => format!("Abandoned {subject}"),
        (A::Approve, K::Understand) => "Goal approved".to_owned(),
        (A::Approve, K::Plan) => "Plan approved".to_owned(),
        (A::Approve, K::GoalChange) => "Goal change accepted".to_owned(),
        (A::Approve, K::ChangeRequest) => format!("Change to {task} accepted"),
        (A::Approve, K::WorkflowPromotion) => "Workflow promoted".to_owned(),
        (A::Approve, _) => format!("{title} · approved"),
        (A::ApproveWithEdits, K::Understand) => "Goal approved with edits".to_owned(),
        (A::ApproveWithEdits, K::Plan) => "Plan approved with edits".to_owned(),
        (A::ApproveWithEdits, _) => format!("{title} · approved with edits"),
        (A::SendBack, K::Understand) => "Goal sent back for another round".to_owned(),
        (A::SendBack, K::Plan) => "Plan sent back".to_owned(),
        (A::SendBack, _) => format!("{title} · sent back"),
        (A::Stop, _) => format!("Stopped {subject}"),
        (A::Choose { value }, K::Understand) => format!("Goal question answered · {value}"),
        // A question is titled by the asking agent's label, never its task id.
        (A::Choose { value }, K::Question) => format!("{title} answered · {value}"),
        (A::Choose { value }, _) => format!("{title} · {value}"),
        (A::Answer, K::Understand) => format!("Goal question answered · {text}"),
        (A::Answer, _) => format!("{title} answered · {text}"),
        (A::Candidate { worker }, _) => format!("{task} fell back to {}", names.worker(worker)),
        (A::OtherCandidates, _) => format!("Listing every agent for {task}"),
        (A::TakeOver, K::TakeOverReturn) => format!("You keep {task}"),
        (A::TakeOver, _) => format!("You took over {task}"),
        (A::Wait, K::Fallback) => format!("{task} waits for the reset"),
        (A::Wait, K::AgentDown) => format!("Waiting for {}", agent_id(opened, names)),
        (A::Wait, K::Merge) => "The merge waits for a stronger review".to_owned(),
        (A::Wait, _) => format!("{title} · waiting"),
        (A::OneForAll, _) => "One agent takes every paused task".to_owned(),
        (A::PerTask, _) => "Choosing an agent per paused task".to_owned(),
        (A::RaiseLimit, _) => format!("Raised the limit for {task}"),
        (A::Fallback, _) => format!("{task} fell back"),
        (A::AskYou, _) => format!("{task} stopped and needs you"),
        (A::Retry, _) => format!("Retrying {task}"),
        (A::Split, _) => format!("Splitting {task}"),
        (A::Cancel, K::GoalChange) => "Kept the goal".to_owned(),
        (A::Cancel, K::ChangeRequest) => format!("Change to {task} declined"),
        (A::Cancel, K::Stuck) => format!("Cancelled {task}"),
        (A::Cancel, _) => format!("{title} · declined"),
        (A::Defer, _) => format!("Deferred {task}"),
        (A::KeepWork, _) => format!("Kept {task}'s work for the checks"),
        (A::LetContinue, _) => match &opened.packet {
            ApprovalPacket::TakeOverReturn(back) => {
                format!("{} continues {task}", names.worker(&back.worker))
            }
            _ => format!("{task} continues"),
        },
        (A::SwitchToCaptain, _) => "Running the plan as Captain".to_owned(),
        (A::ShowDiff, _) => title.to_owned(),
        (A::UpgradeReview, _) => match &opened.packet {
            ApprovalPacket::OriginalReturned(back) => {
                format!("{} reviews {task}", names.worker(&back.original))
            }
            _ => format!("{subject} waits for a stronger review"),
        },
        (A::GiveBack, _) => match &opened.packet {
            ApprovalPacket::OriginalReturned(back) => {
                format!("{task} given back to {}", names.worker(&back.original))
            }
            _ => format!("{task} given back"),
        },
        (A::Redo, _) => match &opened.packet {
            ApprovalPacket::OriginalReturned(back) => format!(
                "{} redoes {task} from fallback-{}",
                names.worker(&back.original),
                back.since
            ),
            _ => format!("{task} redone"),
        },
        (A::KeepFallback, _) => match &opened.packet {
            ApprovalPacket::OriginalReturned(back) => {
                format!("{} keeps {task}", names.worker(&back.fallback))
            }
            _ => format!("{task} keeps its worker"),
        },
        // Esc is never recorded as itself: the option taken in your place is.
        (A::CarryOn, _) => format!("Left {title} without you"),
    }
}

/// Esc on a question or an approval a run waits on: who decides it now, and when you said
/// so. An agent's own question goes back to that agent, and the rest to the main agent. The
/// `◆` stays dim, since nothing is settled yet.
pub fn passed_on_cell(kind: ApprovalKind, at: OffsetDateTime, offset: UtcOffset) -> Cell {
    let what = match kind {
        ApprovalKind::Question => "Question left to the agent".to_owned(),
        ApprovalKind::Understand => "Goal question left to the main agent".to_owned(),
        ApprovalKind::Plan => "Plan approval left to the main agent".to_owned(),
        ApprovalKind::Merge => "Merge approval left to the main agent".to_owned(),
        _ => "Question left to the main agent".to_owned(),
    };
    Cell::Notice(Notice {
        tone: Tone::Dim,
        ..Notice::answered(what, format!("you, {}", words::clock(at, offset)))
    })
}

/// The dim line left when a question no longer applies.
pub fn withdrawn_cell(title: &str, reason: &str) -> Cell {
    let reason = reason.trim();
    let text = if reason.is_empty() {
        format!("{} · no longer needed", title.trim())
    } else {
        format!("{} · no longer needed: {reason}", title.trim())
    };
    Cell::Custom(Box::new(DimRecord { text }))
}

/// Between the command and its one line, in the outcome of a dialog that closes as Claude Code
/// closes one: `/setup ⎿  Captain · Claude Code · …` draws as `/setup` typed, with the line
/// under it. A screen reader reads it as it stands.
const COMMAND_SAID: &str = " ⎿  ";

/// A closed app choice: its title, then the controller's outcome line. The `◆` takes the
/// success color for an answer, and stays dim when nothing changed. An empty outcome leaves no
/// row, as each question of a flow that ends in one row of its own does, and an outcome that
/// names a command draws as that command with its line under it.
pub fn choice_closed_cell(title: Option<&str>, outcome: &str) -> Cell {
    let outcome = outcome.trim();
    if outcome.is_empty() {
        return Cell::Custom(Box::new(NoRow));
    }
    if let Some((command, line)) = outcome.split_once(COMMAND_SAID)
        && command.starts_with('/')
        && !command.contains(char::is_whitespace)
    {
        return Cell::Custom(Box::new(CommandSaid {
            command: command.to_owned(),
            line: line.trim().to_owned(),
        }));
    }
    let tone = if outcome == NOT_CHANGED {
        Tone::Dim
    } else {
        Tone::Success
    };
    let notice = match title.map(str::trim).filter(|title| !title.is_empty()) {
        Some(title) => Notice {
            tone,
            ..Notice::answered(title, outcome)
        },
        None => Notice {
            question: true,
            ..Notice::new(tone, outcome)
        },
    };
    Cell::Notice(notice)
}

/// What a question folded into its flow's one row leaves.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NoRow;

impl HistoryCell for NoRow {
    fn lines(&self, _ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        Vec::new()
    }
}

/// The command as you typed it, then its one dim line under it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandSaid {
    command: String,
    line: String,
}

impl HistoryCell for CommandSaid {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let mut lines = UserMessage::command(self.command.clone()).lines(ctx);
        lines.extend(ResultLine::new(Tone::Dim, self.line.clone()).lines(ctx));
        lines
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DimRecord {
    text: String,
}

impl HistoryCell for DimRecord {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let width = usize::from(ctx.width);
        let dim = theme.style(Role::Dim);
        let glyph = theme.glyphs.question;
        let first = [Span::styled(format!("{glyph} "), dim)];
        let rest = [Span::raw(" ".repeat(str_width(glyph) + 1))];
        let spans = [Span::styled(sanitize_line(&self.text).into_owned(), dim)];
        let mut lines = wrap_spans(&spans, width, &first, &rest);
        fit_lines(&mut lines, width);
        lines
    }
}

/// What a merge or a stop acts on: the task, or else the run.
fn subject(
    opened: &ApprovalOpened,
    run: Option<&RunId>,
    tasks: &dyn Fn(&TaskId) -> String,
) -> String {
    match (&opened.task, run) {
        (Some(task), _) => tasks(task),
        (None, Some(run)) => run.to_string(),
        (None, None) => "the run".to_owned(),
    }
}

fn packet_task(opened: &ApprovalOpened) -> Option<&TaskId> {
    let task = match &opened.packet {
        ApprovalPacket::Fallback(plan) => &plan.task,
        ApprovalPacket::Stuck(stuck) => &stuck.task,
        ApprovalPacket::Limit(limit) => limit.task.as_ref()?,
        ApprovalPacket::ChangeRequest(request) => &request.task,
        ApprovalPacket::TakeOverReturn(back) => &back.task,
        ApprovalPacket::Question(question) => &question.task,
        ApprovalPacket::OriginalReturned(back) => &back.task,
        _ => opened.task.as_ref()?,
    };
    Some(task)
}

/// The branch a merge question names, `None` for a run that started on no branch.
fn merge_target(opened: &ApprovalOpened) -> Option<&str> {
    match &opened.packet {
        ApprovalPacket::Merge(merge) => merge.target.as_deref(),
        _ => None,
    }
}

fn agent_id(opened: &ApprovalOpened, names: &DisplayNames) -> String {
    match &opened.packet {
        ApprovalPacket::AgentDown(down) => names.cli(&down.agent_id.0),
        _ => "the agent".into(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::bridge::transcript::picture_check::assert_rows_match;
    use crate::render::Clock;
    use crate::theme::{Glyphs, Theme};

    const PICTURE: (&str, &str) = (
        include_str!("../../tests/golden/questions/setup_after.txt"),
        include_str!("../../tests/golden/questions/setup_after.html"),
    );

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    fn drawn(cell: &Cell, theme: &Theme) -> Vec<Line<'static>> {
        cell.lines(&RenderCtx::new(118, theme, Clock::still(Instant::now())))
    }

    /// A second `/setup` closes as `setup_after` draws it: the command, with its one line under it.
    #[test]
    fn a_later_setup_closes_in_one_row() {
        let theme = theme();
        let outcome = "/setup ⎿  Captain · Claude Code · auto mode · Usage reports on · saved to \
                       ~/.argonauts/config.toml";
        let lines = drawn(&choice_closed_cell(None, outcome), &theme);
        assert_rows_match("setup_after", &lines, PICTURE, 8..10, &theme, &[]);
    }

    /// Each of its questions leaves nothing, so the row stands alone.
    #[test]
    fn a_question_folded_into_its_flow_leaves_no_row() {
        let theme = theme();
        let title = Some("Who you talk to");
        assert!(drawn(&choice_closed_cell(title, ""), &theme).is_empty());
        assert!(drawn(&choice_closed_cell(title, "  "), &theme).is_empty());
    }

    /// Only a command names a row of its own; words that happen to hold the mark stay a record.
    #[test]
    fn only_a_command_draws_as_a_command() {
        let theme = theme();
        let lines = drawn(
            &choice_closed_cell(Some("Approvals"), "auto ⎿  later"),
            &theme,
        );
        let text: String = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains("Approvals"), "{text}");
    }
}
