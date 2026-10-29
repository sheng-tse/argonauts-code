//! The runs this app watches: starting and attaching, forwarding their events, noticing a
//! crashed run host, and waking waiting runs.

mod alongside;
pub(crate) mod archive;
pub(super) mod told;
mod track;

#[cfg(test)]
pub(crate) mod rig;
#[cfg(test)]
mod tests;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use anyhow::Context;

use argonauts_core::api::{ApiError, RunState, RunSummary, StartRun};
use argonauts_core::app::{AppEvent, AppMode};
use argonauts_core::events::RunEvent;
use argonauts_core::events::failure::run_failure_line;
use argonauts_core::events::run::{RunMode, RunOutcome};
use argonauts_core::names::{ChoiceId, MessageId, ModelRef};
use argonauts_core::policy::NarratorLine;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::ui::{ConversationTrigger, NarratorMode};
use argonauts_core::task::title_from_task;
use argonauts_core::{NoteLevel, RunId, Spend, TaskId};

use super::attach::{self, Attached, Opening, Stage};
use super::header_and_bottom_line::RunStatusParts;
use super::notify::{self, NeedsYou};
use super::recap;
use super::{Ctx, Effect, Internal};
use crate::FirstView;
use crate::conversation_store::ConversationRun;
use crate::wake::{WaitingRuns, WakeCause};

pub(crate) struct RunsState {
    pub(super) attached: Option<Attached>,
    /// Counts every attach, so the loop can tell that a run came on screen.
    pub(super) attaches: u64,
    focused: bool,
    /// Shift+Tab's pick this app session, or the one it starts in. `None` until either, so
    /// each run's settings decide.
    pub(super) approval_mode: Option<ApprovalMode>,
    /// Guidance and mentions sent from here and not delivered yet, oldest first.
    pub(super) queued: HashMap<RunId, Vec<Queued>>,
    /// Questions this part opened, waiting for your answer.
    pub(super) choices: HashMap<ChoiceId, RunChoice>,
    /// Waiting runs whose run host is starting for a wake.
    waking: HashSet<RunId>,
    /// `argonauts run` closes the app once this run finishes.
    exit_when_done: Option<RunId>,
    waiting: WaitingRuns,
    /// This repo's runs going or waiting on you that aren't on screen, in any conversation, from
    /// the last listing, so a change in one is said once.
    listed: Vec<RunSummary>,
    /// The agent panel's rows for this conversation's runs not on screen, as last sent.
    background: Vec<RunSummary>,
    /// Runs this screen has shown, so one you left isn't announced as new when it gets its row.
    shown: HashSet<RunId>,
    /// This conversation's runs: started or opened here, or brought back with the conversation,
    /// and replaced when another conversation comes on screen. Only they get panel rows and
    /// count in the bottom line, as Claude Code's panel holds only its own session's tasks.
    /// Any other run in the repo, from an earlier conversation, another terminal, or an outside
    /// agent over MCP, is counted under the header at start, listed in /resume, and said in one
    /// line instead: when it needs you, wakes, or ends, and at start when it crashed.
    pub(super) ours: BTreeSet<RunId>,
    /// Runs that came on screen since the conversation last saved its runs.
    pub(super) opened: Vec<ConversationRun>,
    /// How the runs on screen got on since the loop last asked, oldest first.
    news: Vec<(RunId, RunNews)>,
    /// Runs that left the screen while working, and still stream into the conversation.
    streaming: alongside::Streaming,
}

/// How far a run got, which the main session that started it hears of.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RunNews {
    /// It is under way: an agent started, a step got through, or it waits for you.
    Began,
    /// It waits for its CLIs before any agent started, so nothing runs. `line` says why.
    Waiting { line: String },
    /// It ended, before it began or after. `line` says why a failed run failed.
    Ended { outcome: RunOutcome, line: String },
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Queued {
    pub id: MessageId,
    pub text: String,
}

/// A question this part asked, and what its answer acts on.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum RunChoice {
    /// The worker breaks one of the role's rules; overriding it needs your yes.
    OverrideRule {
        run: RunId,
        task: TaskId,
        to: ModelRef,
        rule: String,
    },
    Rewind {
        run: RunId,
        task: TaskId,
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
    /// The run host serving the stream came or went.
    Live {
        run: RunId,
        live: bool,
    },
    Narrator {
        run: RunId,
        line: NarratorLine,
    },
    /// A wake's run host started, or couldn't.
    Woke {
        run: RunId,
        cause: WakeCause,
        started: Result<(), ApiError>,
    },
    /// This repo's runs, listed for the agent panel's rows.
    Listed(Result<Vec<RunSummary>, ApiError>),
}

impl RunsState {
    /// The soonest time a run waiting out an outage tries again, after `now`.
    pub(crate) fn waiting_soonest(
        &self,
        now: time::OffsetDateTime,
    ) -> Option<time::OffsetDateTime> {
        self.waiting.soonest(now)
    }

    pub(crate) fn new(ctx: &Ctx) -> Self {
        // The app at your terminal starts in the mode `approvals.start_mode` picks, such as
        // Shift+Tab's last pick; anything else starts as each run's settings say.
        let at_terminal = ctx.env.interactive && !ctx.env.agent_session;
        let approval_mode = at_terminal
            .then(|| super::start_mode::at_start(&ctx.env.state_dir, &ctx.settings().approvals))
            .flatten();
        Self {
            attached: None,
            attaches: 0,
            focused: true,
            approval_mode,
            queued: HashMap::new(),
            choices: HashMap::new(),
            waking: HashSet::new(),
            exit_when_done: None,
            waiting: WaitingRuns::default(),
            listed: Vec::new(),
            background: Vec::new(),
            shown: HashSet::new(),
            ours: BTreeSet::new(),
            opened: Vec::new(),
            news: Vec::new(),
            streaming: alongside::Streaming::default(),
        }
    }
}

/// How the runs on screen got on since the last call.
pub(crate) fn take_news(runs: &mut RunsState) -> Vec<(RunId, RunNews)> {
    std::mem::take(&mut runs.news)
}

/// The run on screen gives way to another. One still working goes on streaming into the
/// conversation beside it; any other leaves the screen.
pub(super) async fn give_way(ctx: &mut Ctx, runs: &mut RunsState, mut old: Attached) {
    if alongside::keeps_working(&old) {
        match alongside::follow(ctx, runs, &mut old).await {
            Ok(()) => return,
            Err(err) => tracing::warn!("{err:#}"),
        }
    }
    ctx.send(AppEvent::Detached {
        run: old.run.clone(),
    });
}

/// A run that comes on screen streams there, and no longer beside it.
pub(super) fn came_on_screen(runs: &mut RunsState, run: &RunId) {
    alongside::on_screen(runs, run);
}

pub(crate) fn attached(runs: &RunsState) -> Option<&RunId> {
    runs.attached.as_ref().map(|attached| &attached.run)
}

/// The run the Context tab lists: the one on screen, going or finished, until another
/// conversation comes on screen after it finished. Before any comes on screen, the newest of
/// the conversation's own, so carrying a conversation on lists the run it ended with.
pub(crate) fn followed(runs: &RunsState) -> Option<&RunId> {
    match &runs.attached {
        Some(attached) => (!attached.left_behind).then_some(&attached.run),
        None => runs.ours.last(),
    }
}

pub(crate) fn attaches(runs: &RunsState) -> u64 {
    runs.attaches
}

/// At start, a conversation carried on brings its own runs back into the panel.
pub(crate) fn adopt(runs: &mut RunsState, theirs: impl IntoIterator<Item = RunId>) {
    runs.ours.extend(theirs);
}

/// The runs that came on screen since the last call, for the conversation to keep.
pub(crate) fn take_opened(runs: &mut RunsState) -> Vec<ConversationRun> {
    std::mem::take(&mut runs.opened)
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

/// The approval mode the guided setup's approvals step picked, which the setup has just
/// written: the mode in force from now on, over Shift+Tab's pick and the one the start brought
/// back from the last.
pub(crate) fn setup_answered(runs: &mut RunsState, mode: ApprovalMode) {
    runs.approval_mode = Some(mode);
}

pub(crate) fn status_parts(runs: &RunsState) -> RunStatusParts {
    let attached = runs.attached.as_ref();
    RunStatusParts {
        run: attached.map(|attached| attached.run.clone()),
        spent: attached.and_then(|attached| attached.spent),
        limit: attached.and_then(|attached| attached.limit),
        // A run that goes on shows its own mode, which is the settings' until Shift+Tab sets
        // one. Shift+Tab's pick for this session shows once none does.
        approval_mode: match attached {
            Some(attached) if !matches!(attached.stage, Stage::Finished(_)) => {
                attached.approval_mode
            }
            _ => runs.approval_mode,
        },
    }
}

pub(crate) fn focus(runs: &mut RunsState, focused: bool) {
    runs.focused = focused;
}

/// At start: a new conversation, as `claude` opens one, the newest one `argonauts -c` carries
/// on, or what `argonauts resume` asked for. The conversation's own runs going or waiting on you
/// get their panel rows, and waiting runs are checked for a wake.
pub(crate) async fn open(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    first: FirstView,
) -> anyhow::Result<Vec<Effect>> {
    let effects = vec![Effect::Status];
    // A list that fails still opens what you asked for.
    let list = match ctx.services.conductor.list().await {
        Ok(list) => list,
        Err(err) => {
            ctx.error(&anyhow::Error::new(err).context("listing this repo's runs"));
            Vec::new()
        }
    };
    match first {
        // A run that waits on you keeps its question until you open that run, from its panel row or
        // /resume, so nothing on it is answered from a screen that opened for something else.
        // The controller opened the conversation, or the agent session it carries on.
        FirstView::Conversation
        | FirstView::Continue
        | FirstView::PastConversation(_)
        | FirstView::CliSession(_)
        | FirstView::Agent(_) => {}
        FirstView::ResumePicker => show_runs(ctx, false).await?,
        FirstView::Resume(run) => resume_and_attach(ctx, runs, &run).await?,
        FirstView::Attach(run) => {
            runs.exit_when_done = Some(run.clone());
            let summary = summary_of(ctx, &run).await;
            super::conversation::redraw::run(ctx, &run);
            attach::attach(ctx, runs, summary, Opening::Watch).await?;
        }
    }
    // Said once each, then left to Status: the oldest first, as they happened.
    let runs_dir = ctx.env.runs_dir();
    for run in list.iter().rev() {
        if let Some((level, line)) = told::once(&runs_dir.join(run.run.as_str()), run) {
            ctx.notice(level, line);
        }
    }
    send_background(ctx, runs, list);
    wake_waiting(ctx, runs).await?;
    Ok(effects)
}

/// A run the header counts, and the agent panel shows a row for when it's this conversation's
/// and isn't on screen.
pub(crate) fn going_or_waiting(run: &RunSummary) -> bool {
    run.live
        || matches!(run.state, RunState::Running | RunState::Pausing)
        || (run.state == RunState::Waiting && !run.waiting.is_empty())
}

/// Once a minute: the panel's rows for runs not on screen, sent again when they changed, such
/// as a run answered from another terminal. Listing reads every run's journal, so it runs on a
/// blocking thread and comes back as [`RunUpdate::Listed`].
pub(crate) fn refresh_background(ctx: &Ctx) {
    let conductor = Arc::clone(&ctx.services.conductor);
    let internal = ctx.internal.clone();
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let listed = runtime.block_on(conductor.list());
        // After Exit nobody listens.
        let _ = internal.send(Internal::Run(RunUpdate::Listed(listed)));
    });
}

fn on_listed(ctx: &mut Ctx, runs: &mut RunsState, listed: Result<Vec<RunSummary>, ApiError>) {
    match listed {
        Ok(list) => {
            announce_background(ctx, runs, &list);
            send_background(ctx, runs, list);
        }
        Err(err) => tracing::warn!("listing this repo's runs for the agent panel: {err}"),
    }
}

/// Says when a run off screen starts in another window, starts waiting on you, or finishes, as
/// one line and, while you're away, a notification. Every run in the repo does, so one from an
/// earlier conversation, another window, or an outside agent can't start, wait, or merge unseen,
/// though only this conversation's get panel rows, which the bottom line counts.
fn announce_background(ctx: &mut Ctx, runs: &RunsState, list: &[RunSummary]) {
    let on_screen = attached(runs);
    for run in list.iter().filter(|run| Some(&run.run) != on_screen) {
        let before = runs.listed.iter().find(|earlier| earlier.run == run.run);
        if before.is_none() && started_elsewhere(runs, run) {
            let line = started_line(run);
            ctx.notice(NoteLevel::Info, line.clone());
            if !runs.focused {
                notify::send_os_line(ctx, &repo_name(ctx), &line);
            }
            continue;
        }
        let (trigger, kind, text) = if run.needs_you() {
            let waits_more = match before {
                Some(before) => run.waiting.iter().any(|id| !before.waiting.contains(id)),
                None => !runs.shown.contains(&run.run),
            };
            if !waits_more {
                continue;
            }
            let kind = match run.stopped_at.trim() {
                "" => "a task".to_owned(),
                at => at.to_owned(),
            };
            // One of this conversation's has its row, which opens it; one from elsewhere opens
            // by its number alone.
            let text = if runs.ours.contains(&run.run) {
                format!("{} needs you · ↓  to manage", run_called(run))
            } else {
                format!("{} needs you · /resume {}", run_called(run), run.run)
            };
            (ConversationTrigger::NeedsYou, kind, text)
        } else if let (Some(_), Some(outcome)) = (before, outcome_of(run.state))
            && !alongside::ended_here(runs, &run.run)
        {
            let open = if runs.ours.contains(&run.run) {
                "/resume".to_owned()
            } else {
                format!("/resume {}", run.run)
            };
            let text = format!(
                "{} {open} shows it.",
                track::outcome_sentence(run_called(run), outcome),
            );
            (
                ConversationTrigger::Finished,
                track::outcome_label(outcome).to_owned(),
                text,
            )
        } else {
            continue;
        };
        ctx.notice(NoteLevel::Info, text.clone());
        let needs = NeedsYou {
            repo: repo_name(ctx),
            run: run.run.clone(),
            approval: run.waiting.last().cloned(),
            kind,
            spent: Some(run.spent),
            trigger,
        };
        notify_if_away(ctx, runs, &needs);
        // A run that ended in another window is told on the desktop too: its merge changed
        // your checkout while you looked elsewhere. One of this conversation's has its row.
        if trigger == ConversationTrigger::Finished
            && !runs.focused
            && !runs.ours.contains(&run.run)
        {
            notify::send_os_line(ctx, &needs.repo, &text);
        }
    }
}

/// A run this listing shows for the first time, going, that this conversation neither started
/// nor opened: it was started in another window, or by an outside agent, since the last listing.
fn started_elsewhere(runs: &RunsState, run: &RunSummary) -> bool {
    run.live
        && run.is_open()
        && !run.needs_you()
        && !runs.ours.contains(&run.run)
        && !runs.shown.contains(&run.run)
}

/// The one line for a run that started in another window, by its own name once it has one.
fn started_line(run: &RunSummary) -> String {
    match named(run) {
        Some(name) => format!(
            "A run started in another window: {name} · /resume {} shows it",
            run.run
        ),
        None => format!(
            "A run started in another window · /resume {} shows it",
            run.run
        ),
    }
}

/// A run's own name, else its title.
fn named(run: &RunSummary) -> Option<&str> {
    [run.name.as_deref(), Some(run.title.as_str())]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|name| !name.is_empty())
}

/// A run as a line about it names it: by its own name or title, as its panel row does, and by
/// its number only when it has neither.
fn run_called(run: &RunSummary) -> String {
    named(run).map_or_else(|| run.run.to_string(), str::to_owned)
}

/// How a run ended, from its listed state. `None` while it's open, or when it crashed.
fn outcome_of(state: RunState) -> Option<RunOutcome> {
    match state {
        RunState::Merged => Some(RunOutcome::Merged),
        RunState::PullRequest => Some(RunOutcome::PullRequest),
        RunState::BranchLeft => Some(RunOutcome::BranchLeft),
        RunState::Stopped => Some(RunOutcome::Stopped),
        RunState::Failed => Some(RunOutcome::Failed),
        RunState::Abandoned => Some(RunOutcome::Abandoned),
        RunState::Running
        | RunState::Pausing
        | RunState::Waiting
        | RunState::Crashed
        | RunState::NeverStarted
        | RunState::FromAnotherBuild => None,
    }
}

/// Keeps this repo's runs going or waiting on you that aren't on screen, and sends this
/// conversation's among them as the agent panel's rows. Their tasks stay with them until you
/// open one.
fn send_background(ctx: &Ctx, runs: &mut RunsState, list: Vec<RunSummary>) {
    let on_screen = attached(runs).cloned();
    if let Some(run) = &on_screen {
        runs.shown.insert(run.clone());
    }
    runs.listed = list
        .into_iter()
        .filter(|run| going_or_waiting(run) && Some(&run.run) != on_screen.as_ref())
        .collect();
    send_rows(ctx, runs);
}

/// The agent panel's rows, this conversation's runs among those listed, sent when they changed.
fn send_rows(ctx: &Ctx, runs: &mut RunsState) {
    let on_screen = attached(runs);
    let background: Vec<RunSummary> = runs
        .listed
        .iter()
        .filter(|run| runs.ours.contains(&run.run) && Some(&run.run) != on_screen)
        .cloned()
        .collect();
    if background == runs.background {
        return;
    }
    runs.background.clone_from(&background);
    ctx.send(AppEvent::BackgroundRuns { runs: background });
}

/// Another conversation came on screen, by `/clear`, `/resume <conversation>`, `/branch`, or
/// carrying on a session: its own runs take the panel, and the ones it left go to the header and
/// /resume. The run on screen stays, as it's this conversation's too now. One that has finished
/// stays the last conversation's to look back on, and leaves the Context tab.
pub(crate) fn follow_conversation(ctx: &Ctx, runs: &mut RunsState, theirs: Vec<RunId>) {
    runs.ours = theirs.into_iter().collect();
    if let Some(attached) = runs.attached.as_mut()
        && matches!(attached.stage, Stage::Finished(_))
    {
        attached.left_behind = true;
    }
    if let Some(attached) = &runs.attached
        && runs.ours.insert(attached.run.clone())
    {
        runs.opened.push(ConversationRun {
            run: attached.run.clone(),
            title: attached.title.clone(),
        });
    }
    send_rows(ctx, runs);
    // The last listing may be a minute old.
    refresh_background(ctx);
}

/// The `/resume` picker: this repo's runs with their agents' sessions, earlier conversations,
/// and the CLIs' own sessions, for this folder or for every folder.
pub(super) async fn show_runs(ctx: &mut Ctx, every_folder: bool) -> anyhow::Result<()> {
    let runs = ctx.services.conductor.list().await?;
    let conversations = super::conversation::list(ctx, &runs);
    let sessions = super::conversation::cli_sessions(ctx, every_folder).await;
    let in_cli = super::conversation::in_cli::listed(ctx, &conversations, &sessions).await;
    ctx.send(AppEvent::View {
        data: Box::new(argonauts_core::app::ViewData::ResumeList {
            conversations,
            sessions,
            in_cli,
        }),
    });
    Ok(())
}

/// Starts a run host for `run`, then shows it, keeping the stream when it is still open. A
/// stopped run on screen goes on where its stream ended.
pub(super) async fn resume_and_attach(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    run: &RunId,
) -> anyhow::Result<()> {
    // A run another build wrote can't be taken up: that is said plainly, and nothing starts.
    if let Some(why) = crate::client::cant_be_resumed(&ctx.env.run_dir(run)) {
        ctx.notice(NoteLevel::Info, why);
        return Ok(());
    }
    ctx.services
        .conductor
        .resume(run)
        .await
        .map_err(resume_error)?;
    let stopped_here = runs.attached.as_mut().filter(|attached| {
        &attached.run == run
            && attached.ended
            && attached.replay_done
            && attached.stage == Stage::Finished(RunOutcome::Stopped)
    });
    if let Some(attached) = stopped_here {
        attach::follow_on(ctx, attached).await?;
    } else {
        let streaming = runs
            .attached
            .as_ref()
            .is_some_and(|attached| &attached.run == run && !attached.ended);
        if !streaming {
            let summary = summary_of(ctx, run).await;
            super::conversation::redraw::run(ctx, run);
            attach::attach(ctx, runs, summary, Opening::Reattach).await?;
        }
    }
    raise_to_chosen(ctx, runs, run).await;
    Ok(())
}

/// A run that comes back on screen to do more work takes the mode Shift+Tab chose in this
/// session, as a run started now does.
async fn raise_to_chosen(ctx: &Ctx, runs: &mut RunsState, run: &RunId) {
    let Some(mode) = runs.approval_mode else {
        return;
    };
    let Some(attached) = runs
        .attached
        .as_mut()
        .filter(|attached| &attached.run == run)
    else {
        return;
    };
    if attached.approval_mode != Some(mode) && set_approval_mode(ctx, run, mode).await {
        attached.approval_mode = Some(mode);
    }
}

/// Why a run didn't resume, in plain words and without its id, which the screen already shows.
fn resume_error(err: ApiError) -> anyhow::Error {
    match err {
        ApiError::Merged(_) => anyhow::anyhow!(
            "That run already merged, so it can't resume. To build on its result, type \
             /follow-up and what you want next."
        ),
        err => anyhow::anyhow!(
            super::run_commands::plain_refusal(&err)
                .unwrap_or_else(|| super::run_commands::failure(&err, "Couldn't resume that run"))
        ),
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
    let handle = match ctx.services.conductor.start(request).await {
        Ok(handle) => handle,
        // Said in the run's own words, such as a folder whose history can't be written now.
        Err(ApiError::Refused(why)) => {
            let why = why.trim().trim_end_matches('.');
            ctx.notice(NoteLevel::Warn, format!("\"{title}\" didn't start: {why}"));
            return Ok(Vec::new());
        }
        Err(err) => return Err(err).context("starting the run"),
    };
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
        workflow: ctx.settings().workflow.clone(),
        state: RunState::Running,
        stopped_at: String::new(),
        updated: ctx.services.clock.now(),
        spent: Spend::default(),
        sessions_to_resume: 0,
        waiting: Vec::new(),
        live: true,
        remind: false,
        name: None,
        shape_known: true,
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
        RunUpdate::Listed(listed) => {
            on_listed(ctx, runs, listed);
            Vec::new()
        }
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
        alongside::on_event(ctx, runs, run, event).await;
        return Vec::new();
    };
    let history = !attached.replay_done;
    let replay_ends = attached.replay_ends_at(event.seq);
    attached.narrator.observe(&event);
    let seen = track::track(attached, &event);
    ctx.send(AppEvent::Run {
        event: Box::new(crate::views::required::for_screen(&event, &ctx.env)),
    });
    attached.events.push(event);
    if history {
        if replay_ends {
            attach::finish_replay(ctx, attached);
            let mut effects = vec![Effect::Status];
            effects.extend(exit_if_done(runs, run));
            return effects;
        }
        return Vec::new();
    }
    let spent = attached.spent;
    if seen.began {
        runs.news.push((run.clone(), RunNews::Began));
    }
    if let Some(line) = seen.held.clone() {
        runs.news.push((run.clone(), RunNews::Waiting { line }));
    }
    if let Some(outcome) = seen.finished {
        let line = run_failure_line(seen.detail.as_deref());
        runs.news
            .push((run.clone(), RunNews::Ended { outcome, line }));
    }
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
            approval: Some(waiting.approval),
            kind: track::approval_label(waiting.kind).to_owned(),
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
/// waiting at an approval keeps the app open.
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

/// A recap built by code, and a message when you're away.
async fn finished(ctx: &mut Ctx, runs: &mut RunsState, run: &RunId, outcome: RunOutcome) {
    let Some(attached) = runs.attached.as_ref() else {
        return;
    };
    let spent = attached.spent;
    send_recap(ctx, run, &attached.events, outcome).await;
    runs.queued.remove(run);
    notify_finished(ctx, runs, run, outcome, spent);
    ctx.set_mode(AppMode::Idle);
    crate::behaviors::deep::tab::after_run(ctx);
}

/// The run's recap, when recaps are on, from its events and its closing answer.
async fn send_recap(ctx: &mut Ctx, run: &RunId, events: &[RunEvent], outcome: RunOutcome) {
    // A failed run is said once, in its own block, which also says how many tasks got done.
    if !ctx.settings().ui.recaps || outcome == RunOutcome::Failed {
        return;
    }
    let summary = attach::status_summary(ctx, run, events).await;
    let mut text = recap::recap_from_summary(&summary, Some(outcome), events);
    if text.trim().is_empty() {
        text = format!(
            "{} Ask for another change, or /resume an older run.",
            track::outcome_sentence(run, outcome)
        );
    }
    ctx.send(AppEvent::Recap { text, model: None });
}

fn notify_finished(
    ctx: &Ctx,
    runs: &RunsState,
    run: &RunId,
    outcome: RunOutcome,
    spent: Option<Spend>,
) {
    let needs = NeedsYou {
        repo: repo_name(ctx),
        run: run.clone(),
        approval: None,
        kind: track::outcome_label(outcome).to_owned(),
        spent,
        trigger: ConversationTrigger::Finished,
    };
    notify_if_away(ctx, runs, &needs);
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
        let line = "its run host exited before waiting".to_owned();
        let outcome = RunOutcome::Failed;
        runs.news
            .push((run.clone(), RunNews::Ended { outcome, line }));
        return vec![Effect::Status];
    }
    Vec::new()
}

async fn on_ended(ctx: &mut Ctx, runs: &mut RunsState, run: &RunId) -> Vec<Effect> {
    let Some(attached) = watching(runs, run) else {
        alongside::on_ended(runs, run);
        return Vec::new();
    };
    attached.ended = true;
    let mut effects = Vec::new();
    if !attached.replay_done {
        attach::finish_replay(ctx, attached);
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

/// Once a minute: starts the run host of each waiting run whose wake condition holds. Each
/// start runs on its own task, since it may wait seconds for the run host's welcome.
pub(crate) async fn wake_waiting(ctx: &mut Ctx, runs: &mut RunsState) -> anyhow::Result<()> {
    let now = ctx.services.clock.now();
    let mut waiting = std::mem::take(&mut runs.waiting);
    let env = Arc::clone(&ctx.env);
    let settings = ctx.settings().clone();
    let health = Arc::clone(&ctx.services.health);
    // A journal that changed is parsed whole, which can take a while, so the check runs on a
    // blocking thread and the runs' streams keep flowing.
    let (waiting, due) = tokio::task::spawn_blocking(move || {
        let due = waiting.due(&env, &settings, health.as_ref(), now);
        (waiting, due)
    })
    .await
    .context("checking waiting runs for a wake")?;
    runs.waiting = waiting;
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
    let summary = summary_of(ctx, &run).await;
    let called = run_called(&summary);
    if let Err(err) = started {
        ctx.notice(NoteLevel::Warn, format!("Couldn't wake {called}: {err}"));
        return Ok(Vec::new());
    }
    let why = match cause {
        // Its agent is healthy again, or the time to try it once more has come.
        WakeCause::AgentBack => "the agent it waited for is back",
        WakeCause::Timer => "its wait ended, so it tries again",
        WakeCause::AskTimeout => "its fallback question timed out, so the role's fallback applies",
    };
    ctx.notice(NoteLevel::Info, format!("{called} woke up: {why}."));
    let reattach = runs
        .attached
        .as_ref()
        .is_some_and(|attached| attached.run == run && attached.ended);
    if !reattach {
        return Ok(Vec::new());
    }
    attach::attach(ctx, runs, summary, Opening::Reattach).await?;
    raise_to_chosen(ctx, runs, &run).await;
    Ok(vec![Effect::Status])
}

/// The narrator hears of a new mode or model. The loop sends a new header and bottom line
/// after.
pub(crate) async fn settings_applied(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    keys: &[String],
) -> anyhow::Result<Vec<Effect>> {
    // Any key can move the narrator's model, such as a workflow or the main agent, so the
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
