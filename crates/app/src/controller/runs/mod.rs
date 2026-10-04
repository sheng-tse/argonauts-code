//! The runs this app watches: starting and attaching, forwarding their events, noticing a
//! crashed supervisor, and waking parked runs.

mod track;

#[cfg(test)]
pub(crate) mod rig;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Context;

use argonauts_core::api::{ApiError, RunState, RunSummary, StartRun};
use argonauts_core::app::{AppEvent, AppMode};
use argonauts_core::card::title_from_task;
use argonauts_core::events::RunEvent;
use argonauts_core::events::run::{RunMode, RunOutcome};
use argonauts_core::names::{ChoiceId, MessageId, WorkerRef};
use argonauts_core::policy::NarratorLine;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::ui::{ChatTrigger, NarratorMode};
use argonauts_core::{CardId, NoteLevel, RunId, Spend};

use super::attach::{self, Attached, Opening, Stage};
use super::notify::{self, NeedsYou};
use super::recap;
use super::status_line::RunStatusParts;
use super::{Ctx, Effect, Internal};
use crate::FirstView;
use crate::wake::{ParkedRuns, WakeCause};

pub(crate) struct RunsState {
    pub(super) attached: Option<Attached>,
    /// Counts every attach, so the loop can tell that a run came on screen.
    pub(super) attaches: u64,
    focused: bool,
    /// Shift+Tab's pick this app session. `None` until you press it, so each run's settings
    /// decide.
    pub(super) approval_mode: Option<ApprovalMode>,
    /// Guidance and mentions sent from here and not delivered yet, oldest first.
    pub(super) queued: HashMap<RunId, Vec<Queued>>,
    /// Cards this part opened, waiting for your answer.
    pub(super) choices: HashMap<ChoiceId, RunChoice>,
    /// Parked runs whose supervisor is starting for a wake.
    waking: HashSet<RunId>,
    /// `argonauts run` closes the app once this run finishes.
    exit_when_done: Option<RunId>,
    parked: ParkedRuns,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Queued {
    pub id: MessageId,
    pub text: String,
}

/// A card this part asked, and what its answer acts on.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum RunChoice {
    /// The worker breaks one of the role's rules; overriding it needs your yes.
    OverrideRule {
        run: RunId,
        card: CardId,
        to: WorkerRef,
        rule: String,
    },
    Revert {
        run: RunId,
        card: CardId,
        checkpoint: u32,
    },
}

/// From a run's stream or narrator to the loop.
#[derive(Debug)]
pub(crate) enum RunUpdate {
    Event {
        run: RunId,
        event: Box<RunEvent>,
    },
    /// The stream ended.
    Ended {
        run: RunId,
    },
    /// The supervisor serving the stream came or went.
    Live {
        run: RunId,
        live: bool,
    },
    Narrator {
        run: RunId,
        line: NarratorLine,
    },
    /// A wake's supervisor started, or couldn't.
    Woke {
        run: RunId,
        cause: WakeCause,
        started: Result<(), ApiError>,
    },
}

impl RunsState {
    pub(crate) fn new(ctx: &Ctx) -> Self {
        // Shift+Tab is per app session; the settings file never picks it.
        let _ = ctx;
        Self {
            attached: None,
            attaches: 0,
            focused: true,
            approval_mode: None,
            queued: HashMap::new(),
            choices: HashMap::new(),
            waking: HashSet::new(),
            exit_when_done: None,
            parked: ParkedRuns::default(),
        }
    }
}

pub(crate) fn attached(runs: &RunsState) -> Option<&RunId> {
    runs.attached.as_ref().map(|attached| &attached.run)
}

pub(crate) fn attaches(runs: &RunsState) -> u64 {
    runs.attaches
}

pub(crate) fn owns_choice(runs: &RunsState, id: &ChoiceId) -> bool {
    runs.choices.contains_key(id)
}

/// `/ask` during a run goes to the narrator unless it's off.
pub(crate) fn narrator_answers(runs: &RunsState, ctx: &Ctx) -> bool {
    ctx.settings().ui.narrator != NarratorMode::Off
        && runs.attached.as_ref().is_some_and(|attached| {
            !matches!(attached.stage, Stage::Finished(_)) && attached.narrator.is_on()
        })
}

pub(crate) fn status_parts(runs: &RunsState) -> RunStatusParts {
    let attached = runs.attached.as_ref();
    RunStatusParts {
        run: attached.map(|attached| attached.run.clone()),
        spent: attached.and_then(|attached| attached.spent),
        limit: attached.and_then(|attached| attached.limit),
        approval_mode: attached
            .and_then(|attached| attached.approval_mode)
            .or(runs.approval_mode),
    }
}

pub(crate) fn focus(runs: &mut RunsState, focused: bool) {
    runs.focused = focused;
}

/// At start: attach to the newest live or waiting run, check parked runs for a wake, or open
/// what `argonauts resume` asked for.
pub(crate) async fn open(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    first: FirstView,
) -> anyhow::Result<Vec<Effect>> {
    let effects = vec![Effect::Status];
    match first {
        FirstView::Chat => reattach_newest(ctx, runs).await?,
        // The controller opened the conversation; a run would take the screen from it.
        FirstView::Conversation(_)
        | FirstView::VendorSession(_)
        | FirstView::Agent(_)
        | FirstView::Continue => {}
        FirstView::ResumePicker => show_runs(ctx).await?,
        FirstView::Resume(run) => resume_and_attach(ctx, runs, &run).await?,
        FirstView::Attach(run) => {
            runs.exit_when_done = Some(run.clone());
            let summary = summary_of(ctx, &run).await;
            attach::attach(ctx, runs, summary, Opening::Watch).await?;
        }
    }
    wake_parked(ctx, runs).await?;
    Ok(effects)
}

async fn reattach_newest(ctx: &mut Ctx, runs: &mut RunsState) -> anyhow::Result<()> {
    let list = ctx.services.conductor.list().await?;
    let going: Vec<&RunSummary> = list.iter().filter(|run| going_or_waiting(run)).collect();
    if let Some(newest) = going.first() {
        attach::attach(ctx, runs, (*newest).clone(), Opening::Reattach).await?;
        if going.len() > 1 {
            ctx.notice(
                NoteLevel::Info,
                format!(
                    "{} runs are going or waiting in this repo, and {} is on screen. /resume picks \
                     another.",
                    going.len(),
                    newest.run
                ),
            );
        }
    }
    for crashed in list.iter().filter(|run| run.state == RunState::Crashed) {
        ctx.notice(
            NoteLevel::Warn,
            format!(
                "{run} crashed: its supervisor exited before parking. /resume {run} recovers it.",
                run = crashed.run
            ),
        );
    }
    Ok(())
}

fn going_or_waiting(run: &RunSummary) -> bool {
    run.live
        || matches!(run.state, RunState::Running | RunState::Pausing)
        || (run.state == RunState::Parked && !run.waiting.is_empty())
}

/// The `/resume` picker: this repo's runs with their agents' sessions, earlier conversations,
/// and the main agent CLI's own sessions.
pub(super) async fn show_runs(ctx: &mut Ctx) -> anyhow::Result<()> {
    let runs = ctx.services.conductor.list().await?;
    let chats = super::chat::list(ctx);
    let sessions = super::chat::vendor_sessions(ctx).await;
    let (agents, agents_read) = super::chat::agent_sessions(ctx, &runs).await;
    ctx.send(AppEvent::View {
        data: Box::new(argonauts_core::app::ViewData::Runs {
            runs,
            chats,
            sessions,
            agents,
            agents_read,
        }),
    });
    Ok(())
}

/// Starts a supervisor for `run`, then shows it, keeping the stream when it is still open. A
/// stopped run on screen goes on where its stream ended.
pub(super) async fn resume_and_attach(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: &RunId,
) -> anyhow::Result<()> {
    ctx.services
        .conductor
        .resume(run)
        .await
        .map_err(|err| resume_error(run, err))?;
    let stopped_here = runs.attached.as_mut().filter(|attached| {
        &attached.run == run
            && attached.ended
            && attached.replay_done
            && attached.stage == Stage::Finished(RunOutcome::Stopped)
    });
    if let Some(attached) = stopped_here {
        return attach::follow_on(ctx, attached).await;
    }
    let streaming = runs
        .attached
        .as_ref()
        .is_some_and(|attached| &attached.run == run && !attached.ended);
    if !streaming {
        let summary = summary_of(ctx, run).await;
        attach::attach(ctx, runs, summary, Opening::Reattach).await?;
    }
    Ok(())
}

fn resume_error(run: &RunId, err: ApiError) -> anyhow::Error {
    match err {
        ApiError::Merged(_) => {
            anyhow::anyhow!("{run} is merged, so it can't resume. /follow-up builds on it.")
        }
        err => anyhow::anyhow!("couldn't resume {run}: {err}"),
    }
}

/// `ConductorApi::start`, this session's approval mode, then attach.
pub(crate) async fn start(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    mut request: StartRun,
) -> anyhow::Result<Vec<Effect>> {
    // A request whose own words asked approvals to wait for you stays supervised, whatever
    // Shift+Tab says.
    let asked_supervised = request.approval_mode == Some(ApprovalMode::Supervised);
    // `start` takes at most supervised; anything more autonomous goes through Approvals after.
    if runs.approval_mode == Some(ApprovalMode::Supervised) {
        request.approval_mode = Some(ApprovalMode::Supervised);
    } else if request.approval_mode != Some(ApprovalMode::Supervised) {
        request.approval_mode = None;
    }
    let title = title_from_task(&request.request);
    let mode = request.mode;
    let handle = ctx
        .services
        .conductor
        .start(request)
        .await
        .context("starting the run")?;
    if asked_supervised {
        return started_as_asked(ctx, runs, handle.run, title, mode).await;
    }
    started(ctx, runs, handle.run, title, mode).await
}

/// A run that stays supervised, as its request asked: attached without raising its mode.
async fn started_as_asked(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: RunId,
    title: String,
    mode: RunMode,
) -> anyhow::Result<Vec<Effect>> {
    let summary = match find_summary(ctx, &run).await {
        Some(summary) => summary,
        None => new_summary(ctx, &run, title, mode),
    };
    attach::attach(ctx, runs, summary, Opening::New).await?;
    if let Some(attached) = runs.attached.as_mut() {
        attached.approval_mode = Some(ApprovalMode::Supervised);
    }
    Ok(vec![Effect::Status])
}

/// After `start` or `continue_run`: raise the approval mode as Shift+Tab chose, and attach.
pub(super) async fn started(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: RunId,
    title: String,
    mode: RunMode,
) -> anyhow::Result<Vec<Effect>> {
    let raised = runs
        .approval_mode
        .filter(|mode| *mode != ApprovalMode::Supervised);
    let raised = match raised {
        Some(approval) => set_approval_mode(ctx, &run, approval)
            .await
            .then_some(approval),
        None => None,
    };
    let summary = match find_summary(ctx, &run).await {
        Some(summary) => summary,
        None => new_summary(ctx, &run, title, mode),
    };
    attach::attach(ctx, runs, summary, Opening::New).await?;
    if let (Some(approval), Some(attached)) = (raised, runs.attached.as_mut()) {
        attached.approval_mode = Some(approval);
    }
    Ok(vec![Effect::Status])
}

/// A failure is a notice, since the run itself started. True when the mode was set.
async fn set_approval_mode(ctx: &Ctx, run: &RunId, mode: ApprovalMode) -> bool {
    let Some(approvals) = ctx.services.approvals.as_ref() else {
        ctx.notice(NoteLevel::Warn, super::run_commands::NO_APPROVALS);
        return false;
    };
    match approvals.set_approval_mode(run, mode).await {
        Ok(()) => true,
        Err(err) => {
            ctx.notice(
                NoteLevel::Warn,
                format!("{run} stays supervised: setting the approval mode failed: {err}"),
            );
            false
        }
    }
}

/// This repo's summary of `run`, or one built from what the app knows.
pub(super) async fn summary_of(ctx: &Ctx, run: &RunId) -> RunSummary {
    match find_summary(ctx, run).await {
        Some(summary) => summary,
        None => new_summary(ctx, run, run.to_string(), RunMode::Crew),
    }
}

async fn find_summary(ctx: &Ctx, run: &RunId) -> Option<RunSummary> {
    let list = ctx.services.conductor.list().await.ok()?;
    list.into_iter().find(|summary| &summary.run == run)
}

fn new_summary(ctx: &Ctx, run: &RunId, title: String, mode: RunMode) -> RunSummary {
    RunSummary {
        run: run.clone(),
        title,
        mode,
        preset: ctx.settings().preset.clone(),
        state: RunState::Running,
        stopped_at: String::new(),
        updated: ctx.services.clock.now(),
        spent: Spend::default(),
        sessions_to_resume: 0,
        waiting: Vec::new(),
        live: true,
        remind: false,
    }
}

pub(crate) async fn on_update(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    update: RunUpdate,
) -> anyhow::Result<Vec<Effect>> {
    let effects = match update {
        RunUpdate::Event { run, event } => on_event(ctx, runs, &run, *event).await,
        RunUpdate::Live { run, live } => on_live(ctx, runs, &run, live),
        RunUpdate::Ended { run } => on_ended(ctx, runs, &run).await,
        RunUpdate::Narrator { run, line } => {
            on_narrator(ctx, runs, &run, line);
            Vec::new()
        }
        RunUpdate::Woke {
            run,
            cause,
            started,
        } => on_woke(ctx, runs, run, cause, started).await?,
    };
    Ok(effects)
}

/// The attached run, when `run` is it. Updates from a stream left behind are dropped.
fn watching<'a>(runs: &'a mut RunsState, run: &RunId) -> Option<&'a mut Attached> {
    runs.attached
        .as_mut()
        .filter(|attached| &attached.run == run)
}

async fn on_event(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: &RunId,
    event: RunEvent,
) -> Vec<Effect> {
    let Some(attached) = watching(runs, run) else {
        return Vec::new();
    };
    let history = !attached.replay_done;
    let replay_ends = attached.replay_ends_at(event.seq);
    attached.narrator.observe(&event);
    let seen = track::track(attached, &event);
    ctx.send(AppEvent::Run {
        event: Box::new(event.clone()),
    });
    attached.events.push(event);
    if history {
        if replay_ends {
            attach::finish_replay(ctx, attached).await;
            let mut effects = vec![Effect::Status];
            effects.extend(exit_if_done(runs, run));
            return effects;
        }
        return Vec::new();
    }
    let spent = attached.spent;
    if let Some(delivered) = &seen.delivered {
        forget_queued(runs, run, delivered);
    }
    let mut effects = Vec::new();
    // A finished run goes idle after its recap, so the recap reads as the run's last word.
    if let Some(mode) = seen.mode
        && seen.finished.is_none()
    {
        ctx.set_mode(mode);
    }
    if seen.status {
        effects.push(Effect::Status);
    }
    if let Some(waiting) = seen.waiting {
        let needs = NeedsYou {
            repo: repo_name(ctx),
            run: run.clone(),
            checkpoint: Some(waiting.checkpoint),
            kind: track::checkpoint_label(waiting.kind).to_owned(),
            spent,
            trigger: track::trigger(waiting.kind),
        };
        notify_if_away(ctx, runs, &needs);
    }
    if let Some(outcome) = seen.finished {
        finished(ctx, runs, run, outcome).await;
        effects.push(Effect::Status);
        effects.extend(exit_if_done(runs, run));
    }
    effects
}

/// `Exit` once the run `argonauts run` attached to has finished, after its recap. A run
/// waiting at a checkpoint keeps the app open.
fn exit_if_done(runs: &RunsState, run: &RunId) -> Option<Effect> {
    let attached = runs.attached.as_ref()?;
    let done = runs.exit_when_done.as_ref() == Some(run)
        && &attached.run == run
        && matches!(attached.stage, Stage::Finished(_));
    done.then_some(Effect::Exit)
}

fn forget_queued(runs: &mut RunsState, run: &RunId, delivered: &MessageId) {
    if let Some(queued) = runs.queued.get_mut(run) {
        queued.retain(|message| &message.id != delivered);
    }
}

/// A recap built by code, and a chat notification when you're away.
async fn finished(ctx: &mut Ctx, runs: &mut RunsState, run: &RunId, outcome: RunOutcome) {
    let Some(attached) = runs.attached.as_ref() else {
        return;
    };
    let spent = attached.spent;
    if ctx.settings().ui.recaps {
        let summary = attach::status_summary(ctx, run, &attached.events).await;
        let closing = recap::closing_in(&attached.events);
        let mut text = recap::recap_from_summary(&summary, Some(outcome), closing);
        if text.trim().is_empty() {
            text = format!(
                "{} Ask for another change, or /resume an older run.",
                track::outcome_sentence(run, outcome)
            );
        }
        ctx.send(AppEvent::Recap { text, model: None });
    }
    runs.queued.remove(run);
    let needs = NeedsYou {
        repo: repo_name(ctx),
        run: run.clone(),
        checkpoint: None,
        kind: track::outcome_label(outcome).to_owned(),
        spent,
        trigger: ChatTrigger::Finished,
    };
    notify_if_away(ctx, runs, &needs);
    ctx.set_mode(AppMode::Idle);
}

fn notify_if_away(ctx: &Ctx, runs: &RunsState, needs: &NeedsYou) {
    if !runs.focused {
        notify::send(ctx, needs);
    }
}

fn repo_name(ctx: &Ctx) -> String {
    ctx.env
        .repo_root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| ctx.env.display_repo())
}

fn on_live(ctx: &mut Ctx, runs: &mut RunsState, run: &RunId, live: bool) -> Vec<Effect> {
    let Some(attached) = watching(runs, run) else {
        return Vec::new();
    };
    attached.live = live;
    if attach::report_crash(ctx, attached) {
        return vec![Effect::Status];
    }
    Vec::new()
}

async fn on_ended(ctx: &mut Ctx, runs: &mut RunsState, run: &RunId) -> Vec<Effect> {
    let Some(attached) = watching(runs, run) else {
        return Vec::new();
    };
    attached.ended = true;
    let mut effects = Vec::new();
    if !attached.replay_done {
        attach::finish_replay(ctx, attached).await;
        effects.push(Effect::Status);
    }
    if attached.crashed || attach::report_crash(ctx, attached) {
        return effects;
    }
    if attached.stage == Stage::Going {
        ctx.notice(
            NoteLevel::Warn,
            format!("The event stream of {run} closed. /resume {run} reattaches."),
        );
    }
    effects.extend(exit_if_done(runs, run));
    effects
}

fn on_narrator(ctx: &mut Ctx, runs: &mut RunsState, run: &RunId, line: NarratorLine) {
    let Some(attached) = watching(runs, run) else {
        return;
    };
    if let Err(err) = attached.narrator.record(&line) {
        ctx.notice(
            NoteLevel::Warn,
            format!("Narrator lines aren't being saved: {err:#}"),
        );
    }
    ctx.send(AppEvent::Narrator { line });
}

/// Once a minute: starts the supervisor of each parked run whose wake condition holds. Each
/// start runs on its own task, since it may wait seconds for the supervisor's welcome.
pub(crate) async fn wake_parked(ctx: &mut Ctx, runs: &mut RunsState) -> anyhow::Result<()> {
    let now = ctx.services.clock.now();
    let mut parked = std::mem::take(&mut runs.parked);
    let env = Arc::clone(&ctx.env);
    let settings = ctx.settings().clone();
    let health = Arc::clone(&ctx.services.health);
    // A journal that changed is parsed whole, which can take a while, so the check runs on a
    // blocking thread and the runs' streams keep flowing.
    let (parked, due) = tokio::task::spawn_blocking(move || {
        let due = parked.due(&env, &settings, health.as_ref(), now);
        (parked, due)
    })
    .await
    .context("checking parked runs for a wake")?;
    runs.parked = parked;
    for (run, cause) in due {
        if !runs.waking.insert(run.clone()) {
            continue;
        }
        let conductor = Arc::clone(&ctx.services.conductor);
        let internal = ctx.internal.clone();
        tokio::spawn(async move {
            let started = conductor.resume(&run).await;
            let woke = RunUpdate::Woke {
                run,
                cause,
                started,
            };
            // After Exit nobody listens, and the run woke all the same.
            let _ = internal.send(Internal::Run(woke));
        });
    }
    Ok(())
}

async fn on_woke(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: RunId,
    cause: WakeCause,
    started: Result<(), ApiError>,
) -> anyhow::Result<Vec<Effect>> {
    runs.waking.remove(&run);
    if let Err(err) = started {
        ctx.notice(NoteLevel::Warn, format!("Couldn't wake {run}: {err}"));
        return Ok(Vec::new());
    }
    let why = match cause {
        WakeCause::VendorBack => "the vendor it waited for is back",
        WakeCause::AskTimeout => "its handoff question timed out, so the role's fallback applies",
    };
    ctx.notice(NoteLevel::Info, format!("{run} woke up: {why}."));
    let reattach = runs
        .attached
        .as_ref()
        .is_some_and(|attached| attached.run == run && attached.ended);
    if !reattach {
        return Ok(Vec::new());
    }
    let summary = summary_of(ctx, &run).await;
    attach::attach(ctx, runs, summary, Opening::Reattach).await?;
    Ok(vec![Effect::Status])
}

/// The narrator hears of a new mode or model. The loop sends a new status line after.
pub(crate) async fn settings_applied(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    keys: &[String],
) -> anyhow::Result<Vec<Effect>> {
    // Any key can move the narrator's model, such as a preset or the default agent, so the
    // host rereads both rather than matching keys.
    let _ = keys;
    if let Some(attached) = runs.attached.as_ref()
        && attached.narrator.apply(ctx)
    {
        ctx.send(AppEvent::NarratorStatus {
            status: attached.narrator.status(),
        });
    }
    Ok(Vec::new())
}
