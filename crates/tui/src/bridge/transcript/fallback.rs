//! Tasks changing hands: fallbacks with their checkpoint tags, waits, rewinds, reassignments,
//! rotations to a fresh session, cleared review debt, and an agent coming back.

use argonauts_core::RunId;
use argonauts_core::base::names::{AgentId, ModelRef, RoleName};
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::models::limits::{AgentHealth, ModelHealth, ModelState, Trouble};
use argonauts_core::runs::events::agent_status::FallbackPlan;
use argonauts_core::runs::events::run::Party;
use argonauts_core::runs::events::task::TaskState;
use argonauts_core::settings::fallback::{FallbackContext, FallbackMode};
use time::OffsetDateTime;

use crate::base::format::WallClock;
use crate::cells::cell::{Notice, Tone, ToolState};
use crate::frame::state::SessionStatus;

use super::steps::{party_words, plural, step};
use super::{Cx, emit};

/// `Claude Code out until 3:45 PM`, or the stop's own reason when no reset time is known.
fn why(plan: &FallbackPlan, clock: &WallClock, names: &DisplayNames) -> String {
    let cli = names.cli(&plan.stopped.agent);
    match plan.resets_at {
        Some(at) => format!("{cli} out until {}", clock.clock(at)),
        None if plan.reason.trim().is_empty() => format!("{cli} stopped"),
        None => format!("{cli} stopped: {}", plan.reason.trim()),
    }
}

/// A fallback as the log records it, with the task's newest checkpoint tag.
pub(super) struct Recorded<'a> {
    /// The task's label, such as `build:flag`.
    pub task: &'a str,
    pub to: &'a ModelRef,
    pub mode: FallbackMode,
    /// What the new worker is told. Older logs don't say.
    pub context: Option<FallbackContext>,
    pub tag: Option<u32>,
}

/// An auto fallback is a step, `● Fallback(build:limiter Middleware · auto)` with who took it,
/// what it was told, and why. One you picked on the fallback question is one line, `● build:export
/// fell back to GPT-6.1 Sol on Codex · note + transcript · checkpoint fallback-1 · Claude Code out
/// until 3:45 PM`.
pub(super) fn fell_back(
    recorded: Recorded<'_>,
    plan: Option<&FallbackPlan>,
    clock: &WallClock,
    cx: &mut Cx<'_>,
) {
    let Recorded {
        task,
        to,
        mode,
        context,
        tag,
    } = recorded;
    let told = context.map(|context| context.label().to_owned());
    let checkpoint = tag.map(|n| format!("checkpoint fallback-{n}"));
    let names = &cx.state.names;
    let why = plan.map(|plan| why(plan, clock, names));
    if mode == FallbackMode::Auto {
        let title = plan
            .map(|plan| plan.title.trim().to_owned())
            .filter(|title| !title.is_empty());
        let args = match title {
            Some(title) => format!("{task} {title} · auto"),
            None => format!("{task} · auto"),
        };
        let mut took = format!("{} took it", names.worker(to));
        for part in [told, checkpoint].into_iter().flatten() {
            took.push_str(&format!(" · {part}"));
        }
        let details: Vec<String> = [Some(took), why].into_iter().flatten().collect();
        emit(cx, step("Fallback", &args, ToolState::Done, &details));
        return;
    }
    let mut text = format!("{task} fell back to {}", names.worker(to));
    for part in [told, checkpoint, why].into_iter().flatten() {
        text.push_str(&format!(" · {part}"));
    }
    emit(cx, Notice::new(Tone::Success, text));
}

/// A task that waits for its agent to come back instead of changing hands.
pub(super) struct Wait<'a> {
    /// The task's label, such as `build:due-flag`.
    pub task: &'a str,
    pub agent_id: &'a AgentId,
    pub until: Option<OffsetDateTime>,
    /// The CLI it would have gone to, where staying on its own CLI is why it waits.
    pub kept_off: Option<&'a AgentId>,
}

/// `build:due-flag waits for Claude Code · next try 2:05 PM`, and under it that its work so far
/// is kept: `work committed in its folder · same_cli keeps it off Codex`.
pub(super) fn waiting(wait: &Wait<'_>, clock: &WallClock, cx: &mut Cx<'_>) {
    let names = &cx.state.names;
    let mut text = format!("{} waits for {}", wait.task, names.cli(&wait.agent_id.0));
    if let Some(at) = wait.until {
        text.push_str(&format!(" · next try {}", clock.clock(at)));
    }
    let mut kept = "work committed in its folder".to_owned();
    if let Some(other) = wait.kept_off {
        kept.push_str(&format!(" · same_cli keeps it off {}", names.cli(&other.0)));
    }
    let notice = Notice {
        detail: vec![kept],
        ..Notice::new(Tone::Normal, text)
    };
    emit(cx, notice);
}

/// `/rewind`: the task went back to a checkpoint, and the work after it was kept aside.
pub(super) fn rewound(task: &str, to: u32, kept_on: &str, cx: &mut Cx<'_>) {
    let mut notice = Notice::new(
        Tone::Success,
        format!("Rewound {task} to checkpoint fallback-{to}"),
    );
    if !kept_on.trim().is_empty() {
        notice = notice.with_detail(format!("The later work is kept on {}", kept_on.trim()));
    }
    emit(cx, notice);
}

pub(super) fn reassigned(
    task: &str,
    from: Option<&ModelRef>,
    to: &ModelRef,
    by: &Party,
    cx: &mut Cx<'_>,
) {
    let names = &cx.state.names;
    let text = format!("Reassigned {task} to {}", names.worker(to));
    let mut notice = Notice::new(Tone::Success, text);
    let parts: Vec<String> = [
        from.map(|from| format!("was {}", names.worker(from))),
        party_words(by, names).map(|who| format!("by {who}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !parts.is_empty() {
        notice = notice.with_detail(parts.join(" · "));
    }
    emit(cx, notice);
}

/// A full session gave way to a fresh one of the same agent, which goes on from a fallback note.
pub(super) fn rotated(task: &str, worker: &ModelRef, cx: &mut Cx<'_>) {
    let text = format!(
        "{task} rotated to a fresh {} session",
        cx.state.names.worker(worker)
    );
    let detail = "its context filled up, so the new session starts from a fallback note";
    emit(cx, Notice::new(Tone::Normal, text).with_detail(detail));
}

/// You started the task over: its worker goes on in a session of its own.
pub(super) fn restarted(task: &str, worker: &ModelRef, cx: &mut Cx<'_>) {
    let text = format!(
        "{task} restarted in a new {} session",
        cx.state.names.worker(worker)
    );
    let detail = "its work so far is kept, and the new session starts from a fallback note";
    emit(cx, Notice::new(Tone::Normal, text).with_detail(detail));
}

/// A review by the original, or a model at least as strong, cleared a fallback's debt.
pub(super) fn debt_cleared(task: &str, by: &ModelRef, cx: &mut Cx<'_>) {
    let text = format!(
        "{task} needs no more review · checked by {}",
        cx.state.names.worker(by)
    );
    emit(cx, Notice::new(Tone::Success, text));
}

pub(super) fn conflict(task: &str, files: usize, assigned_to: &ModelRef, cx: &mut Cx<'_>) {
    let text = format!(
        "Merging {task} conflicted in {} · {} resolves it",
        plural(files, "file", "files"),
        cx.state.names.worker(assigned_to)
    );
    emit(cx, Notice::new(Tone::Warning, text));
}

/// `Claude Code is back · build:limiter stays with GPT-6.1 Sol on Codex · /reassign task-003
/// claude-code/opus moves it back`, for each task another agent still holds for it. The
/// command keeps the id, since that's what you type. A task that waited for the CLI instead
/// reads `Claude Code is back · its test task passed · build:due-flag carries on with Sonnet
/// 5.5`. `tested` says the CLI came back by passing its test task.
pub(super) fn agent_back(run: &RunId, agent_id: &AgentId, tested: bool, cx: &mut Cx<'_>) {
    let Some(state) = cx.state.runs.get(run) else {
        return;
    };
    let names = &cx.state.names;
    let label = names.cli(&agent_id.0);
    let lines: Vec<String> = state
        .tasks
        .values()
        .filter_map(|task| {
            let worker = task.worker.as_ref()?;
            let waited = task
                .waiting_on_agent
                .as_ref()
                .is_some_and(|wait| wait.agent_id == agent_id.0);
            if waited {
                let task = state.task_label(&task.id);
                return Some(carries_on(&label, tested, &task, &model_name(names, worker)));
            }
            let original = task.fallback_for.as_ref()?;
            let still_held = original.agent_id() == *agent_id && worker != original;
            still_held.then(|| {
                format!(
                    "{label} is back · {task} stays with {worker} · /reassign {id} {original} moves it back",
                    task = state.task_label(&task.id),
                    id = task.id,
                    worker = names.worker(worker),
                )
            })
        })
        .collect();
    for line in lines {
        emit(cx, Notice::new(Tone::Success, line));
    }
}

/// What a task that waited for its CLI says once the CLI is back.
fn carries_on(cli: &str, tested: bool, task: &str, model: &str) -> String {
    let how = if tested {
        " · its test task passed"
    } else {
        ""
    };
    format!("{cli} is back{how} · {task} carries on with {model}")
}

/// One model going out while its CLI's other models go on: `Opus 5.5 on Claude Code is off for
/// new tasks: overloaded · tested again at 2:03 PM`, with who keeps working on that CLI under
/// it.
pub(super) fn model_out(health: &ModelHealth, run: &RunId, clock: &WallClock, cx: &mut Cx<'_>) {
    let names = &cx.state.names;
    let why = health.trouble.map_or("errors", Trouble::words);
    let mut text = format!(
        "{} is off for new tasks: {why}",
        names.worker(&health.model)
    );
    if let Some(at) = health.next_test_at {
        text.push_str(&format!(" · tested again at {}", clock.clock(at)));
    }
    let mut detail = format!("only {} is out", model_name(names, &health.model));
    if let Some(still_working) = works_beside(run, &health.model, cx) {
        detail.push_str(&format!(" · {still_working}"));
    }
    let notice = Notice {
        plain_words: true,
        detail: vec![detail],
        ..Notice::new(Tone::Warning, text)
    };
    emit(cx, notice);
}

/// A model that went out on its own is back, its test task passed: `Opus 5.5 is back · the
/// next jobs asked for Opus 5.5 get it again`, and under it what its fallbacks keep.
pub(super) fn model_back(model: &ModelRef, run: &RunId, cx: &mut Cx<'_>) {
    let name = model_name(&cx.state.names, model);
    let notice = Notice {
        detail: kept_work(run, model, cx),
        ..Notice::new(
            Tone::Success,
            format!("{name} is back · the next jobs asked for {name} get it again"),
        )
    };
    emit(cx, notice);
}

/// `review:due-tests keeps the review Sonnet 5.5 gave`: each job another model did in `back`'s
/// place, which isn't done again now that `back` returned. A build task says so on its own
/// line, with the command that moves it.
fn kept_work(run: &RunId, back: &ModelRef, cx: &Cx<'_>) -> Vec<String> {
    let Some(state) = cx.state.runs.get(run) else {
        return Vec::new();
    };
    let mut kept: Vec<String> = Vec::new();
    for session in state
        .session_order
        .iter()
        .filter_map(|key| state.sessions.get(key))
    {
        let in_its_place = session.fallback_for.as_ref() == Some(back) && session.worker != *back;
        let of_a_build_task = session
            .task
            .as_ref()
            .and_then(|task| state.tasks.get(task))
            .is_some_and(|task| task.fallback_for.as_ref() == Some(back));
        let gave_up = matches!(
            session.status,
            SessionStatus::Failed | SessionStatus::Stopped
        );
        let Some(label) = session.label.as_deref() else {
            continue;
        };
        if !in_its_place || of_a_build_task || gave_up {
            continue;
        }
        let what = match session.role {
            RoleName::Reviewer => "the review",
            RoleName::Judge => "the verdict",
            _ => "the answer",
        };
        let line = format!(
            "{label} keeps {what} {} gave",
            model_name(&cx.state.names, &session.worker)
        );
        if !kept.contains(&line) {
            kept.push(line);
        }
    }
    kept
}

/// `Opus 5.5`: the model without its CLI, or the CLI's name for its default model.
fn model_name(names: &DisplayNames, model: &ModelRef) -> String {
    names
        .model(&model.agent, model.model.as_deref())
        .unwrap_or_else(|| names.cli(&model.agent))
}

/// `build:due-flag keeps building on Sonnet 5.5`: a task of the run still at work on another
/// model of the CLI that `out` belongs to.
fn works_beside(run: &RunId, out: &ModelRef, cx: &Cx<'_>) -> Option<String> {
    let state = cx.state.runs.get(run)?;
    state.tasks.values().find_map(|task| {
        let worker = task.worker.as_ref()?;
        let beside = worker.agent == out.agent && worker != out;
        let building = task.state == Some(TaskState::Running);
        (beside && building).then(|| {
            format!(
                "{} keeps building on {}",
                state.task_label(&task.id),
                model_name(&cx.state.names, worker)
            )
        })
    })
}

/// A whole CLI going down: `Claude Code is down: two of its models are out · Haiku 5.5 isn't
/// tried`, and under it what each model that is out has, `Opus 5.5 overloaded · Sonnet 5.5
/// server errors`. `models` is what was last heard of the CLI's models.
pub(super) fn agent_down(
    run: &RunId,
    health: &AgentHealth,
    models: &[&ModelHealth],
    cx: &mut Cx<'_>,
) {
    let names = &cx.state.names;
    let mut text = format!("{} is down", names.cli(&health.agent_id.0));
    if let Some(reason) = &health.reason {
        text.push_str(&format!(": {reason}"));
    }
    let out: Vec<&ModelHealth> = models
        .iter()
        .copied()
        .filter(|model| model.state == ModelState::Out)
        .collect();
    if !out.is_empty()
        && let Some(spared) = spared(run, &health.agent_id, &out, cx)
    {
        text.push_str(&format!(" · {spared}"));
    }
    let troubles: Vec<String> = out
        .iter()
        .map(|model| {
            let trouble = model.trouble.map_or("out", Trouble::words);
            format!("{} {trouble}", model_name(names, &model.model))
        })
        .collect();
    let notice = Notice {
        plain_words: true,
        detail: if troubles.is_empty() {
            Vec::new()
        } else {
            vec![troubles.join(" · ")]
        },
        ..Notice::new(Tone::Error, text)
    };
    emit(cx, notice);
}

/// `Haiku 5.5 isn't tried`: the models of a CLI that went down which never failed themselves,
/// and still get no work while it is down. They are the ones the run used and the ones the
/// catalog lists for the CLI.
fn spared(run: &RunId, cli: &AgentId, out: &[&ModelHealth], cx: &Cx<'_>) -> Option<String> {
    let names = &cx.state.names;
    let state = cx.state.runs.get(run);
    let used = state.into_iter().flat_map(|state| {
        let tasks = state.tasks.values().filter_map(|task| task.worker.as_ref());
        let sessions = state.sessions.values().map(|session| &session.worker);
        tasks.chain(sessions)
    });
    let listed = names.catalog.models.iter().map(|entry| &entry.id);
    let failed: Vec<String> = out
        .iter()
        .map(|model| model_name(names, &model.model))
        .collect();
    let mut spared: Vec<String> = Vec::new();
    for model in used.chain(listed) {
        if model.agent_id() != *cli || model.model.is_none() {
            continue;
        }
        let name = model_name(names, model);
        if !failed.contains(&name) && !spared.contains(&name) {
            spared.push(name);
        }
    }
    match spared.as_slice() {
        [] => None,
        [one] => Some(format!("{one} isn't tried")),
        [one, other] => Some(format!("{one} and {other} aren't tried")),
        _ => Some("its other models aren't tried".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::carries_on;

    #[test]
    fn a_task_that_waited_carries_on_and_says_how_its_cli_came_back() {
        assert_eq!(
            carries_on("Claude Code", true, "build:due-flag", "Sonnet 5.5"),
            "Claude Code is back · its test task passed · build:due-flag carries on with Sonnet 5.5"
        );
        assert_eq!(
            carries_on("Codex", false, "build:due-tests", "GPT 5.6"),
            "Codex is back · build:due-tests carries on with GPT 5.6"
        );
    }
}
