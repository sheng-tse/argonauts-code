//! What you do to a run: answers, guidance, mentions, pauses, stops, reassigns, take-overs, the
//! approval mode, and the run commands and views. Anything that decides or raises autonomy goes
//! through `Approvals`, and without it the app says why it can't.

mod control;
mod tasks;
mod views;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use argonauts_core::api::{ApiError, ApprovalAnswer, Approvals, DiffScope};
use argonauts_core::app::{AppEvent, NOT_CHANGED, ViewData};
use argonauts_core::events::approval::AnswerAction;
use argonauts_core::events::goal::{GoalAction, LET_THE_MAIN_AGENT_ANSWER};
use argonauts_core::names::{ApprovalId, ChoiceId};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::{NoteLevel, RunId, TaskId};

use super::attach::Stage;
use super::runs::{RunChoice, RunsState};
use super::{Ctx, Effect};

pub(crate) use control::{
    cancel_pause, continue_run, follow_up, guidance, mention, pause, restart_agent, resume, steer,
    stop, stop_agent, take_back,
};
pub(crate) use tasks::{checkpoints, reassign, remember_pick, rewind, slash_reassign};
pub(crate) use views::{
    all_repos, ask_narrator, context, context_report, save_workflow, status, summary, view,
    workflow_view,
};

/// Why nothing that decides or raises autonomy can leave this app.
pub(super) const NO_APPROVALS: &str = "This app can't approve anything or change autonomy: it \
     has no terminal, or it runs inside an agent session. Nothing was sent.";
const NO_RUN: &str = "No run is on screen. /resume picks one.";

/// What you see when you act on an agent of a run that already merged.
const RUN_MERGED: &str = "That run already merged, so its agents are done. To build \
     on its result, type /follow-up and what you want next.";

/// A refusal you can meet by acting on a run that ended or went, in plain words: what happened
/// and what you can do. `None` for errors whose own detail says more.
pub(crate) fn plain_refusal(err: &ApiError) -> Option<String> {
    match err {
        ApiError::Merged(_) => Some(RUN_MERGED.to_owned()),
        ApiError::NoRun(_) => {
            Some("That run isn't in this folder any more, so nothing is left to act on.".to_owned())
        }
        ApiError::NotNow(why) => Some(sentence(why)),
        ApiError::Refused(_) | ApiError::RunHost(_) => None,
    }
}

/// Says plainly why the run refused when it can. Else the error goes on as one plain line
/// under `doing`, such as "Couldn't pause the run", never an internal error line.
pub(crate) fn refused(ctx: &Ctx, err: ApiError, doing: impl FnOnce() -> String) -> Handled {
    match plain_refusal(&err) {
        Some(words) => {
            ctx.notice(NoteLevel::Info, words);
            Ok(Vec::new())
        }
        None => Err(anyhow::anyhow!(failure(&err, &doing()))),
    }
}

/// One plain line for an error the run's own words explain: what you were `doing`, then why.
pub(crate) fn failure(err: &ApiError, doing: &str) -> String {
    let why = match err {
        ApiError::Refused(why) => why.trim().to_owned(),
        ApiError::RunHost(why) => format!("its run didn't answer ({})", why.trim()),
        ApiError::Merged(_) => "that run already merged".to_owned(),
        ApiError::NoRun(_) => "that run isn't in this folder any more".to_owned(),
        ApiError::NotNow(why) => why.trim().to_owned(),
    };
    let mut line = format!("{doing}: {why}");
    if !line.ends_with(['.', '!', '?']) {
        line.push('.');
    }
    line
}

/// `the run isn't running` as a sentence of its own.
fn sentence(text: &str) -> String {
    let text = text.trim();
    let mut chars = text.chars();
    let mut out: String = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => return String::new(),
    };
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

/// `UiCommand::Answer` for a run's approval.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ApprovalReply {
    pub run: RunId,
    pub id: ApprovalId,
    pub action: AnswerAction,
    pub text: Option<String>,
    /// From the question the TUI drew, passed on unchanged.
    pub evidence_sha256: Option<String>,
    pub tasks: Vec<TaskId>,
    /// Save a fallback pick as a fallback rule; the scope question follows.
    pub remember: bool,
}

type Handled = anyhow::Result<Vec<Effect>>;

/// `Approvals`, or a notice saying why there are none.
fn approvals(ctx: &Ctx) -> Option<Arc<dyn Approvals>> {
    let approvals = ctx.services.approvals.clone();
    if approvals.is_none() {
        ctx.notice(NoteLevel::Warn, NO_APPROVALS);
    }
    approvals
}

/// The run on screen, or a notice that there is none.
fn on_screen(ctx: &Ctx, runs: &RunsState) -> Option<RunId> {
    let run = super::runs::attached(runs).cloned();
    if run.is_none() {
        ctx.notice(NoteLevel::Info, NO_RUN);
    }
    run
}

pub(crate) async fn answer(ctx: &mut Ctx, runs: &mut RunsState, reply: ApprovalReply) -> Handled {
    // The question shows its own diff; the engine never sees this action.
    if reply.action == AnswerAction::ShowDiff {
        return Ok(Vec::new());
    }
    let Some(approvals) = approvals(ctx) else {
        return Ok(Vec::new());
    };
    // The engine binds an answer to the evidence you saw, so one without it can't be sent.
    let Some(evidence_sha256) = reply.evidence_sha256 else {
        ctx.notice(
            NoteLevel::Warn,
            format!(
                "The answer to {} carries no evidence hash, so it wasn't sent.",
                reply.id
            ),
        );
        return Ok(Vec::new());
    };
    let pick = match &reply.action {
        AnswerAction::Candidate { worker } => Some(worker.clone()),
        _ => None,
    };
    let main_agent_answers = matches!(
        &reply.action,
        AnswerAction::Choose { value } if value == LET_THE_MAIN_AGENT_ANSWER
    );
    let answer = ApprovalAnswer {
        id: reply.id.clone(),
        action: reply.action,
        text: reply.text,
        evidence_sha256,
        tasks: reply.tasks,
    };
    if let Err(err) = approvals.answer(&reply.run, answer).await {
        return refused(ctx, err, || "Couldn't send your answer".to_owned());
    }
    if main_agent_answers {
        return super::settings::let_the_main_agent_answer(ctx);
    }
    if reply.remember {
        return tasks::remember(ctx, runs, &reply.run, &reply.id, pick);
    }
    Ok(Vec::new())
}

/// Esc on a run's question or approval means "carry on without me": the engine
/// takes an option in your place, as `approvals.when_declined` says, so the run never waits on
/// it. One it can't take anything for stays open, and a line says why.
pub(crate) async fn carry_on(ctx: &mut Ctx, run: RunId, id: ApprovalId) -> Handled {
    let Some(approvals) = approvals(ctx) else {
        return Ok(Vec::new());
    };
    let left = ApprovalAnswer {
        id: id.clone(),
        action: AnswerAction::CarryOn,
        text: None,
        // Leaving it isn't an answer to what was shown, so it's bound to no evidence.
        evidence_sha256: String::new(),
        tasks: Vec::new(),
    };
    if let Err(err) = approvals.answer(&run, left).await {
        return refused(ctx, err, || format!("{id} still waits for you"));
    }
    Ok(Vec::new())
}

/// Choices this part opened, such as a reassign that overrides a rule, or `/rewind`'s confirm.
pub(crate) async fn answer_choice(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    id: ChoiceId,
    action: AnswerAction,
    text: Option<String>,
) -> Handled {
    // Neither question takes a note.
    let _ = text;
    let Some(choice) = runs.choices.remove(&id) else {
        return Ok(Vec::new());
    };
    if let (RunChoice::Rewind { run, task, .. }, AnswerAction::ShowDiff) = (&choice, &action) {
        let diff = ctx
            .services
            .conductor
            .diff(run, task, DiffScope::Task)
            .await;
        let task = task.clone();
        // The question stays open, so you can still rewind after reading the diff.
        runs.choices.insert(id, choice);
        return match diff {
            Ok(diff) => {
                ctx.send(AppEvent::View {
                    data: Box::new(ViewData::Diff { diff }),
                });
                Ok(Vec::new())
            }
            Err(err) => refused(ctx, err, || format!("Couldn't show the diff of {task}")),
        };
    }
    let outcome = match (choice, action) {
        (
            RunChoice::OverrideRule {
                run,
                task,
                to,
                rule,
            },
            AnswerAction::Approve,
        ) => {
            let Some(approvals) = approvals(ctx) else {
                close(ctx, id, NOT_CHANGED);
                return Ok(Vec::new());
            };
            let overridden = approvals
                .reassign_overriding(&run, &task, to.clone(), rule.clone())
                .await;
            if let Err(err) = overridden {
                close(ctx, id, NOT_CHANGED);
                return refused(ctx, err, || format!("Couldn't reassign {task}"));
            }
            let name = ctx.names.worker(&to);
            format!("Reassigned {task} to {name}, overriding {rule} for this task.")
        }
        (
            RunChoice::Rewind {
                run,
                task,
                checkpoint,
            },
            AnswerAction::Approve,
        ) => {
            let rewound = ctx.services.conductor.rewind(&run, &task, checkpoint).await;
            if let Err(err) = rewound {
                close(ctx, id, "Not rewound.");
                return refused(ctx, err, || format!("Couldn't rewind {task}"));
            }
            format!(
                "Rewound {task} to fallback-{checkpoint}. The work after it is on \
                 argonauts/{run}/{task}-replaced-{checkpoint}."
            )
        }
        (RunChoice::Rewind { .. }, _) => "Not rewound.".to_owned(),
        (RunChoice::OverrideRule { .. }, _) => NOT_CHANGED.to_owned(),
    };
    close(ctx, id, outcome);
    Ok(Vec::new())
}

pub(crate) fn set_aside(ctx: &mut Ctx, runs: &mut RunsState, id: ChoiceId) -> Handled {
    let outcome = match runs.choices.remove(&id) {
        Some(RunChoice::OverrideRule { .. }) => NOT_CHANGED,
        Some(RunChoice::Rewind { .. }) => "Not rewound.",
        None => return Ok(Vec::new()),
    };
    close(ctx, id, outcome);
    Ok(Vec::new())
}

fn close(ctx: &Ctx, id: ChoiceId, outcome: impl Into<String>) {
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: outcome.into(),
    });
}

pub(crate) async fn take_over(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: RunId,
    task: TaskId,
) -> Handled {
    let _ = runs;
    let Some(approvals) = approvals(ctx) else {
        return Ok(Vec::new());
    };
    // The engine answers once it has paused the task, which can take as long as the agent's
    // current tool call, so the screen goes on meanwhile.
    let place = ctx.settings().ui.take_over;
    let out = ctx.out.clone();
    tokio::spawn(async move {
        match approvals.take_over(&run, &task).await {
            Ok(ticket) => out.send(AppEvent::TakeOver { ticket, place, run }),
            Err(err) => out.send(match plain_refusal(&err) {
                Some(text) => AppEvent::Hint { text },
                None => AppEvent::Notice {
                    level: NoteLevel::Error,
                    text: format!("Couldn't take over {task}: {err}"),
                },
            }),
        }
    });
    Ok(Vec::new())
}

pub(crate) async fn take_over_ended(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: RunId,
    task: TaskId,
    exit_code: Option<i32>,
) -> Handled {
    let _ = runs;
    let Some(approvals) = approvals(ctx) else {
        return Ok(Vec::new());
    };
    if let Err(err) = approvals.take_over_ended(&run, &task).await {
        return refused(ctx, err, || format!("Couldn't hand {task} back"));
    }
    let how = match exit_code {
        Some(0) | None => String::new(),
        Some(code) => format!("The agent's UI exited with code {code}. "),
    };
    ctx.notice(
        NoteLevel::Info,
        format!(
            "{how}Your changes to {task} are committed, and its return question decides who goes on."
        ),
    );
    Ok(Vec::new())
}

/// Shift+Tab: for the run on screen, and for runs started later in this app session.
pub(crate) async fn approval_mode(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    mode: ApprovalMode,
) -> Handled {
    let Some(approvals) = approvals(ctx) else {
        return Ok(Vec::new());
    };
    runs.approval_mode = Some(mode);
    // Kept for the next start, which `approvals.start_mode = "last"` brings back.
    if ctx.env.interactive && !ctx.env.agent_session {
        let approvals = &ctx.settings().approvals;
        if let Err(err) = super::start_mode::remember(&ctx.env.state_dir, mode, approvals) {
            ctx.notice(
                NoteLevel::Warn,
                format!("Couldn't keep the approval mode for the next start: {err:#}"),
            );
        }
    }
    if let Some(attached) = runs
        .attached
        .as_mut()
        .filter(|attached| !matches!(attached.stage, Stage::Finished(_)))
    {
        if let Err(err) = approvals.set_approval_mode(&attached.run, mode).await {
            return refused(ctx, err, || {
                format!("Couldn't put the run in {} mode", mode.label())
            });
        }
        attached.approval_mode = Some(mode);
    }
    Ok(vec![Effect::Status])
}

/// Your verdict on an auto-approval. A right one can raise autonomy, so it needs you at the
/// terminal, as an approval does.
/// The goal view's keys and the goal panels': clear the goal, pause it, rewrite its lines, run
/// its checks now, turn the run into one, or change an assumption. Only you do these, so an
/// app that can't approve can't either.
pub(crate) async fn goal(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: Option<RunId>,
    action: GoalAction,
) -> Handled {
    if approvals(ctx).is_none() {
        return Ok(Vec::new());
    }
    let Some(run) = run.or_else(|| on_screen(ctx, runs)) else {
        return Ok(Vec::new());
    };
    let done = match &action {
        GoalAction::Clear { land: true } => {
            "Stopped pursuing the goal. Merge takes what holds once every check passes."
        }
        GoalAction::Clear { land: false } => {
            "Stopped pursuing the goal. The run stops and keeps its branch."
        }
        GoalAction::Pause { .. } => {
            "Goal paused. The try in progress finishes and is checked, and nothing is sent \
             round again until /goal resume."
        }
        GoalAction::Resume { .. } => "Goal resumed.",
        GoalAction::EditLines { .. } => {
            "Changed the goal's lines. The tries so far stay, and the next check reads the \
             new lines."
        }
        GoalAction::CheckNow { .. } => {
            "Running the goal's checks now, beside the try under way. What they find shows \
             as the last check, and the try's own check still decides what follows."
        }
        GoalAction::TurnIntoGoal => {
            "The run takes its done-when list as a goal: its check judges this workflow when \
             it ends, and the crew goes round again for what doesn't hold."
        }
        GoalAction::ChangeAssumption { .. } => {
            "Changed the assumption. The agents working now are told, and the next round \
             plans with it."
        }
    };
    let doing = match &action {
        GoalAction::CheckNow { .. } => "Couldn't run the goal's checks",
        _ => "Couldn't change the goal",
    };
    if let Err(err) = ctx.services.conductor.goal(&run, action).await {
        return refused(ctx, err, || doing.to_owned());
    }
    ctx.notice(NoteLevel::Info, done);
    Ok(Vec::new())
}

pub(crate) async fn spot_check(
    ctx: &mut Ctx,
    run: RunId,
    id: ApprovalId,
    correct: bool,
) -> Handled {
    if approvals(ctx).is_none() {
        return Ok(Vec::new());
    }
    if let Err(err) = ctx.services.conductor.spot_check(&run, &id, correct).await {
        return refused(ctx, err, || format!("Couldn't mark {id}"));
    }
    let verdict = if correct {
        "right, so it counts toward autonomy"
    } else {
        "wrong, so that decision point goes back to auto preview"
    };
    ctx.notice(NoteLevel::Info, format!("Marked {id} of {run} {verdict}."));
    Ok(Vec::new())
}
