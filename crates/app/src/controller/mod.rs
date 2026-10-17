//! The app controller: turns what you type into conversation turns, runs, and settings changes, and
//! everything that happens into `AppEvent`s for the screen.
//!
//! Each part keeps its own state (`ConversationState`, `RunsState`, `SettingsState`, the setup) and
//! handles its commands in its own module. A part that needs another returns an [`Effect`],
//! and the loop applies it, so no part reaches into another's state.

mod account;
mod agent_panel;
mod agents_add;
pub(crate) mod ask_role;
mod attach;
mod banner;
mod captain_or_crew;
mod cli_commands;
pub(crate) mod cli_updates;
mod connect;
pub(crate) mod conversation;
mod crew_lines;
mod ctx;
mod doctor;
mod files;
#[cfg(test)]
mod fixture_replay;
mod folder_git;
mod header_and_bottom_line;
pub(crate) mod instructions;
mod mod_places;
pub(crate) mod names;
mod narrator_host;
mod needed_update;
mod notify;
mod prompt_templates;
mod prompt_trust;
mod recap;
mod route;
mod run_commands;
pub(crate) mod runs;
pub(crate) mod settings;
mod setup_again;
mod shell;
mod start_mode;
mod status_health;
mod status_tab;
mod templates;
#[cfg(test)]
pub(crate) mod test_support;
mod tries;
pub(crate) mod turn;
pub(crate) mod ultra;
mod workflows;
mod workspace;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use argonauts_core::app::{AnswerTarget, AppEvent, UiCommand, ViewRequest};
use argonauts_core::events::approval::AnswerAction;
use argonauts_core::events::context::ContextDetail;
use argonauts_core::events::run::RunMode;
use argonauts_core::input::{
    AgentsAction, PromptsAction, ResumeTarget, SlashCommand, ViewAction, WorkflowVerb,
};
use argonauts_core::names::{ChoiceId, RoleName};
use argonauts_core::session_runner::RunnerEvent;
use argonauts_core::settings::approvals::{AUTOPILOT_BACK, ApprovalMode, StartMode};
use argonauts_core::settings::ui::Place;
use argonauts_core::views::{LearningArea, LearningCommand, conversation_run};
use argonauts_core::{AgentEvent, NoteLevel, RunId};
use tokio::sync::mpsc;

pub(crate) use ctx::{AppServices, Ctx, Effect, Internal, Outbox, ScopeRequest};
pub(crate) use header_and_bottom_line::{agent_label, header_agent, plan_usage_chips};
pub(crate) use route::Route;
pub(crate) use run_commands::ApprovalReply;

use crate::FirstView;
use crate::repo_files;
use crate::setup::Setup;

/// How often waiting runs are checked for a wake while the app is open.
const WAKE_EVERY: Duration = Duration::from_secs(60);

/// How long closing waits for cancelled conversation steps to stop their agent processes.
const STOP_GRACE: Duration = Duration::from_secs(3);

/// The slash command that opens `area`'s picker.
fn area_command(area: LearningArea) -> SlashCommand {
    match area {
        LearningArea::Records => SlashCommand::Records { agent: None },
        LearningArea::LearnedWorkflows => SlashCommand::LearnedWorkflows { name: None },
        LearningArea::Skills => SlashCommand::Skills,
        LearningArea::Memory => SlashCommand::Memory,
    }
}

/// The `@` list's files, those of the folder the conversation works in. Without them the list
/// still offers the run's agents, so a failure is only logged.
fn send_repo_files(ctx: &Ctx, folder: &Path) {
    match repo_files::list(folder) {
        Ok(paths) => ctx.send(AppEvent::RepoFiles { paths }),
        Err(err) => tracing::warn!("listing the repo's files: {err:#}"),
    }
}

/// `/resume <run>`, or a run's row: the run comes on screen inside the conversation that
/// started it, which comes back first when another is on screen.
async fn open_run(
    ctx: &mut Ctx,
    conversation: &mut conversation::ConversationState,
    runs: &mut runs::RunsState,
    run: RunId,
) -> anyhow::Result<Vec<Effect>> {
    let Some(mut effects) = conversation::come_home(ctx, conversation, &run).await? else {
        return Ok(Vec::new());
    };
    effects.extend(run_commands::resume(ctx, runs, Some(run)).await?);
    Ok(effects)
}

/// What the controller does before it reads input.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Opening {
    /// Runs the guided setup first: the first start, `argonauts setup`.
    pub guided_setup: bool,
    pub first: FirstView,
}

pub(crate) struct Controller {
    ctx: Ctx,
    conversation: conversation::ConversationState,
    runs: runs::RunsState,
    settings: settings::SettingsState,
    connect: connect::ConnectState,
    workflows: workflows::State,
    prompt_trust: prompt_trust::PromptTrustState,
    renamed: settings::renamed::RenamedState,
    setup_again: setup_again::SetupAgainState,
    git: folder_git::GitState,
    templates: templates::TemplatesState,
    needed_update: needed_update::NeededUpdateState,
    shell: shell::ShellState,
    /// The conversation's CLI's own commands.
    agent_id: cli_commands::CliCommands,
    /// The mod files, and what their commands last printed.
    mods: crate::mods::ModsState,
    /// The folder the screen's `@` list holds the files of.
    files_folder: Option<PathBuf>,
    setup: Option<Setup>,
    /// What to open once the setup is done; taken on the first start only.
    first: Option<FirstView>,
    internal: mpsc::UnboundedReceiver<Internal>,
}

impl Controller {
    pub(crate) fn new(
        ctx: Ctx,
        internal: mpsc::UnboundedReceiver<Internal>,
    ) -> anyhow::Result<Self> {
        let conversation = conversation::ConversationState::new(&ctx)?;
        let runs = runs::RunsState::new(&ctx);
        // Autopilot coming back by itself says so once; supervised and auto show on the
        // bottom line.
        let last = ctx.settings().approvals.start_mode == StartMode::Last;
        if last && runs.approval_mode == Some(ApprovalMode::Autopilot) {
            ctx.notice(NoteLevel::Caution, AUTOPILOT_BACK);
        }
        let settings = settings::SettingsState::new(&ctx);
        let mods = crate::mods::ModsState::new(&ctx);
        Ok(Self {
            ctx,
            mods,
            conversation,
            runs,
            settings,
            connect: connect::ConnectState::default(),
            workflows: workflows::State::default(),
            prompt_trust: prompt_trust::PromptTrustState::default(),
            renamed: settings::renamed::RenamedState::default(),
            setup_again: setup_again::SetupAgainState::default(),
            git: folder_git::GitState::default(),
            templates: templates::TemplatesState::default(),
            needed_update: needed_update::NeededUpdateState::default(),
            shell: shell::ShellState::default(),
            agent_id: cli_commands::CliCommands::default(),
            files_folder: None,
            setup: None,
            first: None,
            internal,
        })
    }

    /// Runs until `Exit`, or until the screen closes its channel. Runs keep going either way.
    pub(crate) async fn run(
        mut self,
        mut commands: mpsc::Receiver<UiCommand>,
        opening: Opening,
    ) -> anyhow::Result<()> {
        let effects = self.open(opening).await;
        if self.apply_all(effects).await {
            return self.close().await;
        }
        self.keep_runs();
        self.follow_conversation();
        header_and_bottom_line::read_stale_at_start(&self.ctx);
        // Opening already checked, so the first check comes a while later.
        let mut next_wake = tokio::time::Instant::now() + WAKE_EVERY;
        loop {
            let attaches = runs::attaches(&self.runs);
            let conversation = conversation::id(&self.conversation).clone();
            let effects = tokio::select! {
                command = commands.recv() => {
                    let Some(command) = command else { break };
                    self.ctx.out.trace().command(&command);
                    if !keeps_orchestrator_hold(&command) {
                        settings::interrupt(&mut self.settings);
                    }
                    let result = self.command(command).await;
                    self.settle(result)
                }
                Some(message) = self.internal.recv() => {
                    let result = self.internal_message(message).await;
                    self.settle(result)
                }
                () = tokio::time::sleep_until(next_wake) => {
                    self.kick_replays();
                    runs::refresh_background(&self.ctx);
                    let result = runs::wake_waiting(&mut self.ctx, &mut self.runs).await;
                    next_wake = tokio::time::Instant::now() + self.next_wake_in();
                    self.settle(result.map(|()| Vec::new()))
                }
            };
            let mut exit = self.apply_all(effects).await;
            if !exit {
                let settled = self.settle_on_autopilot();
                exit = self.apply_all(settled).await;
            }
            if runs::attaches(&self.runs) != attaches {
                conversation::run_attached(&mut self.conversation);
            }
            if !exit {
                self.hear_runs();
            }
            if conversation::id(&self.conversation) != &conversation {
                runs::follow_conversation(
                    &self.ctx,
                    &mut self.runs,
                    conversation::own_runs(&self.conversation),
                );
            }
            self.keep_runs();
            self.follow_conversation();
            if exit {
                break;
            }
        }
        self.close().await
    }

    /// Cancels the conversation's turn, lets the screen close, and waits a little for the turn's
    /// step to end, so its agent process doesn't outlive the app.
    async fn close(mut self) -> anyhow::Result<()> {
        // A command in the conversation's folder stops before that folder may go.
        shell::close(&mut self.shell);
        conversation::close(&mut self.ctx, &mut self.conversation);
        self.ctx.send(AppEvent::Exit);
        let deadline = tokio::time::Instant::now() + STOP_GRACE;
        while !conversation::settled(&self.conversation) {
            let message = tokio::time::timeout_at(deadline, self.internal.recv()).await;
            let Ok(Some(message)) = message else {
                break;
            };
            if let Internal::Conversation(update) = message {
                let result =
                    conversation::on_update(&mut self.ctx, &mut self.conversation, update).await;
                self.settle(result);
            }
        }
        if !conversation::settled(&self.conversation) {
            tracing::warn!("a conversation step was still stopping when the app closed");
        }
        Ok(())
    }

    /// A run that came on screen is the conversation's, saved with it for `argonauts -c`.
    /// How long until waiting runs are checked again: at the soonest retry one waits for, at
    /// most [`WAKE_EVERY`] on, and a second at least, so a time just passed is checked once.
    fn next_wake_in(&self) -> Duration {
        let now = self.ctx.services.clock.now();
        self.runs
            .waiting_soonest(now)
            .and_then(|at| Duration::try_from(at - now).ok())
            .map_or(WAKE_EVERY, |left| {
                left.clamp(Duration::from_secs(1), WAKE_EVERY)
            })
    }

    fn keep_runs(&mut self) {
        conversation::keep_runs(&self.conversation, runs::take_opened(&mut self.runs));
    }

    /// The `/` menu, its keywords, and the `@` list follow the conversation: its CLI, and the
    /// folder it works in. Both change when it carries on an agent's session from a run.
    /// The main session hears how the runs it started got on: its start tool's answer waits
    /// for that, and a run that failed later is told to it once no turn runs. Nothing is told
    /// while the guided setup is open.
    fn hear_runs(&mut self) {
        if self.setup.is_some() {
            return;
        }
        let news = runs::take_news(&mut self.runs);
        conversation::hear_runs(&mut self.ctx, &mut self.conversation, news);
    }

    fn follow_conversation(&mut self) {
        cli_commands::follow(&self.ctx, &mut self.agent_id, &self.conversation);
        let folder = self.conversation.folder(&self.ctx);
        if self.files_folder.as_deref() != Some(folder) {
            let folder = folder.to_path_buf();
            send_repo_files(&self.ctx, &folder);
            self.files_folder = Some(folder);
        }
    }

    /// Autopilot coming on, by Shift+Tab, a setting, or a run that brings its own mode: the
    /// questions already open get what autopilot gives them, since nothing waits for you there.
    fn settle_on_autopilot(&mut self) -> Vec<Effect> {
        let shown = runs::status_parts(&self.runs)
            .approval_mode
            .unwrap_or_else(|| self.ctx.settings().approvals.mode());
        if !settings::follow_mode(&mut self.settings, shown == ApprovalMode::Autopilot) {
            return Vec::new();
        }
        settings::settle_on_autopilot(&self.ctx, &mut self.settings);
        let settled = conversation::autopilot_came_on(&mut self.ctx, &mut self.conversation);
        self.settle(settled)
    }

    /// Every error a handler returns reaches you as a notice, and the app goes on.
    fn settle(&self, result: anyhow::Result<Vec<Effect>>) -> Vec<Effect> {
        result.unwrap_or_else(|err| {
            self.ctx.error(&err);
            Vec::new()
        })
    }

    async fn open(&mut self, opening: Opening) -> Vec<Effect> {
        self.first = Some(opening.first);
        // The app sent them before its header; a controller started on its own sends them now.
        names::send(&mut self.ctx);
        settings::refresh_models(&self.ctx, &mut self.settings);
        self.follow_conversation();
        if let Err(why) = crate::setup::reference::refresh(&self.ctx.env) {
            self.ctx.notice(NoteLevel::Warn, why);
        }
        settings::renamed::ask_at_start(&mut self.ctx, &mut self.renamed);
        let result = if opening.guided_setup {
            // The bottom line shows the mode in force while the setup asks, and the setup asks
            // about the folder's settings itself.
            self.send_status();
            self.open_setup().await
        } else {
            settings::ask_trust_at_start(&mut self.ctx, &mut self.settings);
            prompt_trust::ask_at_start(&mut self.ctx, &mut self.prompt_trust);
            self.after_setup().await
        };
        self.settle(result)
    }

    async fn open_setup(&mut self) -> anyhow::Result<Vec<Effect>> {
        let (setup, effects) = Setup::begin(&mut self.ctx).await?;
        self.setup = Some(setup);
        Ok(effects)
    }

    /// Opens what `argonauts -c` or `argonauts resume` asked for, the first time only. A plain
    /// start opens a new conversation and leaves runs where they are.
    async fn after_setup(&mut self) -> anyhow::Result<Vec<Effect>> {
        let mut effects = vec![Effect::Status];
        if let Some(first) = self.first.take() {
            let ctx = &mut self.ctx;
            let opened = match &first {
                FirstView::PastConversation(id) => {
                    conversation::resume(ctx, &mut self.conversation, id).await
                }
                FirstView::CliSession(session) => {
                    conversation::resume_cli_session(ctx, &mut self.conversation, session).await
                }
                FirstView::Agent(agent) => {
                    conversation::resume_agent(ctx, &mut self.conversation, agent).await
                }
                FirstView::Continue => {
                    conversation::continue_newest(ctx, &mut self.conversation).await
                }
                // A run opens in the conversation that started it.
                FirstView::Resume(run) => conversation::come_home(ctx, &mut self.conversation, run)
                    .await
                    .map(Option::unwrap_or_default),
                _ => Ok(Vec::new()),
            };
            // A conversation that isn't there still leaves the app open on a new one.
            effects.extend(self.settle(opened));
            // Worktrees made again for earlier conversations go, now that the one on screen is
            // known.
            conversation::tidy_worktrees(&self.ctx, &self.conversation);
            // `argonauts -c` brings the conversation's own runs back into the panel. A new one
            // has none, so earlier runs show only under the header and in /resume.
            runs::adopt(&mut self.runs, conversation::own_runs(&self.conversation));
            effects.extend(runs::open(&mut self.ctx, &mut self.runs, first).await?);
        }
        Ok(effects)
    }

    /// True when the app should close.
    async fn apply_all(&mut self, effects: Vec<Effect>) -> bool {
        let mut queue = VecDeque::from(effects);
        while let Some(effect) = queue.pop_front() {
            let result = match effect {
                Effect::Exit => return true,
                Effect::StartRun(request) => self.start_run(*request).await,
                Effect::FreshConversation => {
                    // A new main agent brings its own commands, unless the conversation goes to
                    // the agent whose session it carries on.
                    let fresh = conversation::fresh_session(&mut self.ctx, &mut self.conversation);
                    cli_commands::refresh(&self.ctx, &mut self.agent_id, &self.conversation);
                    fresh
                }
                Effect::CommandsChanged => {
                    cli_commands::refresh(&self.ctx, &mut self.agent_id, &self.conversation);
                    Ok(Vec::new())
                }
                Effect::SettingsApplied { keys } => {
                    names::send(&mut self.ctx);
                    conversation::saved(&self.ctx, &mut self.conversation, &keys);
                    let applied = runs::settings_applied(&mut self.ctx, &mut self.runs, &keys);
                    applied.await.map(|mut effects| {
                        effects.push(Effect::Status);
                        effects
                    })
                }
                Effect::AskScope(request) => {
                    settings::ask_scope(&mut self.ctx, &mut self.settings, *request).await
                }
                Effect::SaveSettings { pairs, scope } => {
                    settings::save(&mut self.ctx, &mut self.settings, pairs, scope)
                }
                Effect::SaveConfirmedSettings { pairs, scope } => {
                    settings::save_confirmed(&mut self.ctx, &mut self.settings, pairs, scope)
                }
                Effect::Status => {
                    self.send_status();
                    Ok(Vec::new())
                }
                Effect::OpenSetup => self.open_setup().await,
                Effect::Input(command) => self.command(*command).await,
                Effect::Queued {
                    command,
                    typed_after,
                } => {
                    conversation::sending_queued(&mut self.conversation, Some(typed_after));
                    let sent = self.command(*command).await;
                    conversation::sending_queued(&mut self.conversation, None);
                    sent
                }
                Effect::SetupDone => {
                    if let Some(mode) = self.setup.as_ref().and_then(Setup::approval_mode) {
                        runs::setup_answered(&mut self.runs, mode);
                    }
                    self.setup = None;
                    prompt_templates::write_for_setup(&self.ctx);
                    // The setup asked about the folder's settings, not about its prompt files.
                    prompt_trust::ask_at_start(&mut self.ctx, &mut self.prompt_trust);
                    self.after_setup().await
                }
            };
            queue.extend(self.settle(result));
        }
        false
    }

    async fn start_run(
        &mut self,
        request: argonauts_core::api::StartRun,
    ) -> anyhow::Result<Vec<Effect>> {
        let mode = self.approval_mode_shown();
        let held = &mut self.needed_update;
        let request = match needed_update::before_run(&mut self.ctx, held, request, mode).await {
            needed_update::Start::Now(request) => *request,
            needed_update::Start::Waits => return Ok(Vec::new()),
            needed_update::Start::Refused(why) => return self.not_started(&why),
        };
        if request.continues.is_some() {
            return run_commands::continue_run(&mut self.ctx, &mut self.runs, request).await;
        }
        let offered = settings::offer_checks(&mut self.ctx, &mut self.settings).await;
        let mut effects = self.settle(offered);
        let from_conversation = request.conversation_turn.is_some();
        match runs::start(&mut self.ctx, &mut self.runs, request).await {
            Ok(started) => effects.extend(started),
            // The main session that asked says why, so the failure is said once.
            Err(err) if conversation::run_not_started(&mut self.conversation, &err) => {
                return Ok(effects);
            }
            Err(err) => return Err(err),
        }
        if let Some(run) = runs::attached(&self.runs) {
            conversation::run_started_here(&mut self.conversation, run, from_conversation);
        }
        conversation::run_started(&mut self.conversation);
        conversation::goal_run_started(
            &self.ctx,
            &mut self.conversation,
            runs::attached(&self.runs),
        );
        Ok(effects)
    }

    /// A run that didn't start, said once: by the main session where it asked for the run,
    /// else as an error.
    fn not_started(&mut self, why: &str) -> anyhow::Result<Vec<Effect>> {
        let err = anyhow::anyhow!("{why}");
        if conversation::run_not_started(&mut self.conversation, &err) {
            return Ok(Vec::new());
        }
        Err(err)
    }

    /// The approval mode on screen: the run's you watch, else this session's, else the
    /// settings'.
    fn approval_mode_shown(&self) -> argonauts_core::settings::approvals::ApprovalMode {
        runs::status_parts(&self.runs)
            .approval_mode
            .unwrap_or_else(|| self.ctx.settings().approvals.mode())
    }

    fn send_status(&self) {
        let mut parts = runs::status_parts(&self.runs);
        // The setup writes your answers at its end, so until then the line shows the mode its
        // approvals step picked.
        if let Some(mode) = self.setup.as_ref().and_then(Setup::approval_mode) {
            parts.approval_mode = Some(mode);
        }
        let status = header_and_bottom_line::header_and_bottom_line(
            &self.ctx,
            conversation::title(&self.conversation),
            parts,
        );
        self.ctx.send(AppEvent::Status { status });
    }

    async fn command(&mut self, command: UiCommand) -> anyhow::Result<Vec<Effect>> {
        // A direct turn asks by the mode on screen when it starts.
        let shown = self.approval_mode_shown();
        conversation::set_approval(&mut self.conversation, shown);
        let ctx = &mut self.ctx;
        let runs = &mut self.runs;
        match command {
            UiCommand::Submit { .. } | UiCommand::Mention { .. } => self.route(command).await,
            UiCommand::Slash { command } => self.slash(command).await,
            UiCommand::Answer {
                to: AnswerTarget::Approval { run, id },
                action,
                text,
                evidence_sha256,
                tasks,
                remember,
            } => {
                let reply = ApprovalReply {
                    run,
                    id,
                    action,
                    text,
                    evidence_sha256,
                    tasks,
                    remember,
                };
                run_commands::answer(ctx, runs, reply).await
            }
            UiCommand::Answer {
                to: AnswerTarget::Choice { id },
                action,
                text,
                ..
            } => self.answer_choice(id, action, text).await,
            UiCommand::SetAside {
                to: AnswerTarget::Choice { id },
            } => self.set_aside(id).await,
            // Esc on a run's question or approval: carry on without me.
            UiCommand::SetAside {
                to: AnswerTarget::Approval { run, id },
            } => run_commands::carry_on(ctx, run, id).await,
            UiCommand::Steer {
                run,
                session,
                text,
                now,
            } => run_commands::steer(ctx, runs, run, session, text, now).await,
            UiCommand::TakeOver { run, task } => {
                run_commands::take_over(ctx, runs, run, task).await
            }
            UiCommand::TakeOverEnded {
                run,
                task,
                exit_code,
            } => run_commands::take_over_ended(ctx, runs, run, task, exit_code).await,
            UiCommand::TerminalReturned { argv } => {
                account::terminal_returned(ctx, &argv);
                Ok(Vec::new())
            }
            UiCommand::Reassign { run, task, to } => {
                run_commands::reassign(ctx, runs, run, task, to).await
            }
            UiCommand::Pause { now } => run_commands::pause(ctx, runs, now).await,
            UiCommand::CancelPause => run_commands::cancel_pause(ctx, runs).await,
            UiCommand::Resume { run: Some(run) } => {
                open_run(ctx, &mut self.conversation, runs, run).await
            }
            UiCommand::Resume { run: None } => run_commands::resume(ctx, runs, None).await,
            UiCommand::Stop => run_commands::stop(ctx, runs, None).await,
            UiCommand::StopAgent { run, session } => {
                run_commands::stop_agent(ctx, runs, run, session).await
            }
            UiCommand::RestartAgent { run, session } => {
                run_commands::restart_agent(ctx, runs, run, session).await
            }
            UiCommand::Rewind { task, checkpoint } => {
                run_commands::rewind(ctx, runs, task, checkpoint).await
            }
            UiCommand::Summary => run_commands::summary(ctx, runs).await,
            UiCommand::Learning { area, command } => self.learning(area, command).await,
            UiCommand::WorkflowFile { name, action } => {
                files::workflow(ctx, &workflows::taken(&self.agent_id), name, action).await
            }
            UiCommand::WorkflowPage { name, change } => {
                workflows::page_change(ctx, &mut self.workflows, &name, change)
            }
            UiCommand::SaveWorkflow { run, workflow } => {
                run_commands::save_workflow(ctx, runs, &self.agent_id, run, workflow).await
            }
            UiCommand::TakeBack { run, messages } => {
                run_commands::take_back(ctx, runs, run, messages).await
            }
            UiCommand::ApprovalMode { mode } => run_commands::approval_mode(ctx, runs, mode).await,
            UiCommand::Verbosity { level } => {
                settings::verbosity(ctx, &mut self.settings, level).await
            }
            UiCommand::Vim { on } => settings::vim(ctx, &mut self.settings, on).await,
            UiCommand::Narrator { mode } => {
                settings::narrator_mode(ctx, &mut self.settings, mode).await
            }
            UiCommand::CancelTurn => conversation::cancel_turn(ctx, &mut self.conversation),
            UiCommand::Focus { focused } => {
                runs::focus(runs, focused);
                Ok(Vec::new())
            }
            UiCommand::SpotCheck { run, id, correct } => {
                run_commands::spot_check(ctx, run, id, correct).await
            }
            UiCommand::Goal { run, action } => run_commands::goal(ctx, runs, run, action).await,
            UiCommand::Request { view } => self.view(view).await,
            UiCommand::SetSetting { key, value, scope } => {
                settings::set(ctx, &mut self.settings, key, value, scope).await
            }
            UiCommand::ResetSetting { key, scope } => {
                settings::reset(ctx, &mut self.settings, key, scope).await
            }
            UiCommand::Mod { change } => {
                let context = conversation::context_row(&self.conversation);
                crate::mods::change(ctx, &self.mods, context, change).await
            }
            UiCommand::Shell { command } => {
                let folder = self.conversation.folder(ctx).to_path_buf();
                shell::run(ctx, &mut self.shell, command, &folder);
                Ok(Vec::new())
            }
            UiCommand::StopShell => {
                shell::stop(&mut self.shell);
                Ok(Vec::new())
            }
            UiCommand::SendNow => {
                conversation::send_now(&mut self.conversation);
                Ok(Vec::new())
            }
            UiCommand::StopSubagent { id } => {
                conversation::stop_subagent(ctx, &mut self.conversation, id);
                Ok(Vec::new())
            }
            UiCommand::MessageSubagent { name, text } => {
                conversation::message_subagent(ctx, &mut self.conversation, name, text)
            }
            UiCommand::TakeBackConversation => {
                conversation::take_back(ctx, &mut self.conversation);
                Ok(Vec::new())
            }
            UiCommand::PressNotice { plugin, handle } => {
                conversation::press_notice(ctx, &mut self.conversation, &plugin, handle);
                Ok(Vec::new())
            }
            UiCommand::WatchContext { detail } => {
                conversation::watch_context(&mut self.conversation, detail);
                Ok(Vec::new())
            }
            UiCommand::Exit => Ok(vec![Effect::Exit]),
            UiCommand::CommandTyped { text } => {
                conversation::command_typed(ctx, &mut self.conversation, text);
                Ok(Vec::new())
            }
            UiCommand::CommandReplied { reply } => {
                conversation::command_replied(ctx, &mut self.conversation, reply);
                Ok(Vec::new())
            }
            UiCommand::RememberPick { run, id } => {
                run_commands::remember_pick(ctx, runs, run, id).await
            }
        }
    }

    /// Plain text and mentions mean what the app mode says.
    async fn route(&mut self, mut command: UiCommand) -> anyhow::Result<Vec<Effect>> {
        // Each message's pictures go with its own turn, and a message without any clears them.
        if let UiCommand::Submit { pictures, .. } = &command {
            conversation::send_pictures(&mut self.conversation, pictures.clone());
        }
        // With the trigger off, `ultracode` in a prompt is only a word, whoever sent it.
        if let UiCommand::Submit {
            workflow_requested, ..
        } = &mut command
            && !self.ctx.settings().workflows.ultracode_trigger
        {
            *workflow_requested = false;
        }
        let root = self.conversation.folder(&self.ctx).to_path_buf();
        let ctx = &mut self.ctx;
        let runs = &mut self.runs;
        let starts_with_file = |text: &str| repo_files::starts_with_file(&root, text);
        match route::route(
            &ctx.mode,
            command,
            self.agent_id.set(),
            starts_with_file,
            repo_files::starts_with_path,
        ) {
            Route::NewRequest { text } => {
                conversation::new_request(ctx, &mut self.conversation, text).await
            }
            Route::NextTurn { text } => conversation::queue_turn(ctx, &mut self.conversation, text),
            Route::WorkflowRequest { text } => {
                conversation::workflow_request(ctx, &mut self.conversation, text).await
            }
            Route::Guidance { run, text } => {
                let ran = conversation::take_shell(&mut self.conversation);
                let sent = run_commands::guidance(ctx, runs, run, text, &ran).await;
                if sent.is_err() {
                    conversation::give_back_shell(&mut self.conversation, ran);
                }
                sent
            }
            Route::CancelPause => run_commands::cancel_pause(ctx, runs).await,
            Route::Resume { run } => run_commands::resume(ctx, runs, Some(run)).await,
            Route::Mention { run, target, text } => {
                let ran = conversation::take_shell(&mut self.conversation);
                let sent = run_commands::mention(ctx, runs, run, target, text, &ran).await;
                if sent.is_err() {
                    conversation::give_back_shell(&mut self.conversation, ran);
                }
                sent
            }
            Route::Notice { text } => {
                ctx.notice(NoteLevel::Info, text);
                Ok(Vec::new())
            }
            Route::Slash { command } => self.slash(command).await,
            Route::Nothing => Ok(Vec::new()),
        }
    }

    async fn slash(&mut self, command: SlashCommand) -> anyhow::Result<Vec<Effect>> {
        let agent_id = &self.agent_id;
        let ctx = &mut self.ctx;
        let runs = &mut self.runs;
        let conversation = &mut self.conversation;
        let settings = &mut self.settings;
        match command {
            SlashCommand::Clear => conversation::clear(ctx, conversation),
            SlashCommand::Resume { target: None } => run_commands::resume(ctx, runs, None).await,
            SlashCommand::Resume {
                target: Some(ResumeTarget::Run(run)),
            } => open_run(ctx, conversation, runs, run).await,
            SlashCommand::Resume {
                target: Some(ResumeTarget::Conversation(id)),
            } => conversation::resume(ctx, conversation, &id).await,
            SlashCommand::Resume {
                target: Some(ResumeTarget::CliSession(session)),
            } => conversation::resume_cli_session(ctx, conversation, &session).await,
            SlashCommand::Resume {
                target: Some(ResumeTarget::BranchConversation(id)),
            } => conversation::branch_from(ctx, conversation, &id).await,
            SlashCommand::Resume {
                target: Some(ResumeTarget::BranchCliSession(session)),
            } => conversation::branch_from_cli_session(ctx, conversation, &session).await,
            SlashCommand::Resume {
                target: Some(ResumeTarget::Agent(agent)),
            } => conversation::resume_agent(ctx, conversation, &agent).await,
            SlashCommand::Exit => Ok(vec![Effect::Exit]),
            SlashCommand::Ask { question } => {
                if runs::narrator_answers(runs, ctx) {
                    run_commands::ask_narrator(ctx, runs, question).await
                } else {
                    conversation::ask(ctx, conversation, question).await
                }
            }
            SlashCommand::Captain { task } => {
                conversation::start_forced(ctx, conversation, RunMode::Captain, task).await
            }
            SlashCommand::Crew { task } => {
                conversation::start_forced(ctx, conversation, RunMode::Crew, task).await
            }
            SlashCommand::WorkflowRun { task: Some(task) } if !task.trim().is_empty() => {
                conversation::start_workflow(ctx, conversation, task).await
            }
            SlashCommand::WorkflowRun { .. } if runs::attached(runs).is_some() => {
                run_commands::workflow_view(ctx, runs, None).await
            }
            SlashCommand::WorkflowRun { .. } => {
                files::library(ctx, &workflows::taken(agent_id), false);
                Ok(Vec::new())
            }
            SlashCommand::FollowUp { run, request } => {
                let asked = run_commands::follow_up(ctx, runs, run, request).await?;
                let start = asked.map(|request| Effect::StartRun(Box::new(request)));
                Ok(start.into_iter().collect())
            }
            // Typed, it opens the Status tab, which always draws, so it prints nothing.
            SlashCommand::Status {
                action: ViewAction::Open,
            } => status_tab::show(ctx, runs, conversation).await,
            SlashCommand::Status {
                action: ViewAction::Print,
            } => run_commands::status(ctx, runs).await,
            SlashCommand::Status { action } => mod_places::tab(ctx, Place::Status, action),
            SlashCommand::Workflows if runs::attached(runs).is_some() => {
                run_commands::workflow_view(ctx, runs, None).await
            }
            SlashCommand::Workflows => {
                files::library(ctx, &workflows::taken(agent_id), false);
                Ok(Vec::new())
            }
            // The tab lists everything in full, so `all` asks for nothing more. The main
            // session counts as its own /context does, which the tab draws when it answers.
            SlashCommand::Context {
                action: ViewAction::Open,
                ..
            } => {
                conversation::ask_context(conversation, ContextDetail::Full);
                let row = conversation::context_row(conversation);
                run_commands::context(ctx, runs, row).await
            }
            // A session at work is counted first, and the rows print once its count is in.
            SlashCommand::Context {
                action: ViewAction::Print,
                ..
            } if conversation::count_then_print(ctx, conversation) => Ok(Vec::new()),
            SlashCommand::Context {
                action: ViewAction::Print,
                ..
            } => {
                let row = conversation::context_row(conversation);
                run_commands::context_report(ctx, runs, row).await
            }
            SlashCommand::Context { action, .. } => mod_places::tab(ctx, Place::Context, action),
            // In a direct conversation, the turns that changed files; in a run, its approvals.
            SlashCommand::Rewind { task: None } if conversation::rewind(ctx, conversation) => {
                Ok(Vec::new())
            }
            SlashCommand::Rewind { task } => run_commands::checkpoints(ctx, runs, task).await,
            SlashCommand::Reassign { task, from, to } => {
                run_commands::slash_reassign(ctx, runs, task, from, to).await
            }
            SlashCommand::Stop { run } => run_commands::stop(ctx, runs, run).await,
            SlashCommand::Agents {
                action: AgentsAction::Add { name },
            } => agents_add::add(ctx, &name).await,
            SlashCommand::Workflow {
                name,
                action: WorkflowVerb::Use,
            } => {
                let command = SlashCommand::Workflow {
                    name,
                    action: WorkflowVerb::Use,
                };
                settings::command(ctx, settings, command).await
            }
            SlashCommand::Workflow { name, action } => files::chosen(ctx, name, action).await,
            SlashCommand::WorkflowNew { name, from } => {
                workflows::new(ctx, agent_id, &name, from.as_deref())
            }
            SlashCommand::WorkflowTry { name, request } => {
                let kept = conversation::conversation_steps(conversation);
                workflows::try_out(ctx, &mut self.workflows, &name, &request, kept)
            }
            SlashCommand::WorkflowEdit { name } => workflows::edit(ctx, &mut self.workflows, &name),
            SlashCommand::WorkflowRequest { workflow, request } => {
                workflows::run(ctx, conversation, &workflow, request).await
            }
            SlashCommand::Prompts {
                action: PromptsAction::Writer { role, who },
            } => {
                let change = prompt_templates::writer_setting(role.as_deref(), &who)?;
                settings::command(ctx, settings, change).await
            }
            SlashCommand::Prompts { action } => prompt_templates::command(ctx, action),
            SlashCommand::Templates { name, global } => {
                templates::command(ctx, &mut self.templates, name, global)
            }
            SlashCommand::Verbosity { level: Some(level) } => {
                settings::verbosity(ctx, settings, level).await
            }
            SlashCommand::Vim => settings::toggle_vim(ctx, settings).await,
            SlashCommand::Setup => Ok(setup_again::ask(&mut self.ctx, &mut self.setup_again)),
            SlashCommand::Connect { outside_agent } => {
                connect::connect(ctx, &mut self.connect, outside_agent)
            }
            SlashCommand::Git => folder_git::command(ctx, &mut self.git),
            SlashCommand::GitClean => folder_git::clean(ctx).await,
            SlashCommand::Runs { archive } => runs::archive::command(ctx, archive).await,
            SlashCommand::Doctor => doctor::doctor(ctx),
            SlashCommand::Awake { time } => crate::keep_awake::command(ctx, time),
            SlashCommand::Update => doctor::update(ctx),
            SlashCommand::Limits {
                action: ViewAction::Print,
                ..
            } => {
                let spent = conversation::context_row(&self.conversation).map(|row| row.spent);
                account::limits_report(ctx, spent).await
            }
            SlashCommand::Limits { action, .. } if !action.is_open() => {
                mod_places::tab(ctx, Place::Limits, action)
            }
            SlashCommand::ModFile { name, words } => {
                let context = conversation::context_row(&self.conversation);
                crate::mods::file_command(ctx, &self.mods, context, name, words).await
            }
            // /mods and a mod's page read the mod files, which the controller keeps.
            SlashCommand::Mods { action, .. } if mod_places::opens_mods(&action) => {
                let context = conversation::context_row(&self.conversation);
                let page = mod_places::page_asked(&action);
                crate::mods::open(ctx, &self.mods, context, page, false).await
            }
            SlashCommand::Mods { action, scope } => mod_places::command(ctx, action, scope),
            // The screen handles these itself.
            SlashCommand::Help | SlashCommand::Artifacts => Ok(Vec::new()),
            command @ (SlashCommand::Model { .. }
            | SlashCommand::Effort { .. }
            | SlashCommand::Agents { .. }
            | SlashCommand::Fallback { .. }
            | SlashCommand::Approvals { .. }
            | SlashCommand::Limits { .. }
            | SlashCommand::Permissions { .. }
            | SlashCommand::Verbosity { .. }
            | SlashCommand::Narrator { .. }
            | SlashCommand::Theme { .. }
            | SlashCommand::Config { .. }
            | SlashCommand::Records { .. }
            | SlashCommand::LearnedWorkflows { .. }
            | SlashCommand::Skills
            | SlashCommand::Memory) => settings::command(ctx, settings, command).await,
            SlashCommand::Compact { instructions } => {
                conversation::compact(ctx, conversation, instructions)
            }
            SlashCommand::Rename { name } => conversation::rename(ctx, conversation, name),
            SlashCommand::Branch { name } => conversation::branch(ctx, conversation, name),
            SlashCommand::Fork { prompt } => conversation::fork(ctx, conversation, prompt),
            SlashCommand::Btw { question } => conversation::btw(ctx, conversation, question),
            SlashCommand::Recap => conversation::recap(ctx, conversation).await,
            // The screen copies itself; an outside agent without one gets the text.
            SlashCommand::Copy { nth } => conversation::copy(ctx, conversation, nth),
            SlashCommand::Export { file } => conversation::export(ctx, conversation, file),
            SlashCommand::Cd { path } => {
                workspace::cd(ctx, path, conversation::settled(conversation))
            }
            SlashCommand::AddDir { path } => workspace::add_dir(ctx, &path),
            SlashCommand::Plan { task } => conversation::plan(ctx, conversation, task),
            SlashCommand::Goal { text } => conversation::goal(ctx, conversation, text),
            SlashCommand::Fast { on } => conversation::fast(ctx, conversation, on),
            SlashCommand::OutputStyle { style } => {
                conversation::output_style(ctx, conversation, style)
            }
            SlashCommand::Diff => workspace::diff(ctx),
            SlashCommand::Review { target } => {
                conversation::review(ctx, conversation, false, target)
            }
            SlashCommand::SecurityReview { target } => {
                conversation::review(ctx, conversation, true, target)
            }
            // The tab opens as a panel, and never prints into a running answer.
            SlashCommand::Usage {
                period,
                action: ViewAction::Open,
            } => {
                let spent = conversation::context_row(conversation).map(|row| row.spent);
                let opens_on = ctx.settings().ui.views.usage.period.usage();
                let period = account::parse_period(period.as_deref(), opens_on)?;
                account::usage_view(ctx, spent, period).await
            }
            SlashCommand::Usage {
                period,
                action: ViewAction::Print,
            } => {
                let spent = conversation::context_row(conversation).map(|row| row.spent);
                account::usage(ctx, spent, period).await
            }
            SlashCommand::Usage { action, .. } => mod_places::tab(ctx, Place::Usage, action),
            // The Stats tab draws from the usage numbers, which carry the crew's stats.
            SlashCommand::Stats {
                period,
                action: ViewAction::Open,
            } => {
                let spent = conversation::context_row(conversation).map(|row| row.spent);
                let period = account::stats_period(ctx, period.as_deref());
                account::usage_view(ctx, spent, period).await
            }
            SlashCommand::Stats {
                action: ViewAction::Pin { scope },
                ..
            } => mod_places::pin_stats(ctx, scope),
            SlashCommand::Stats { .. } => Ok(Vec::new()),
            SlashCommand::Login { agent } => account::login(ctx, agent, true),
            SlashCommand::Logout { agent } => account::login(ctx, agent, false),
            SlashCommand::ReleaseNotes => account::release_notes(ctx),
            SlashCommand::Feedback { text } => account::feedback(ctx, text),
            SlashCommand::Cli { name, args } => {
                cli_commands::run(ctx, conversation, &self.agent_id, &name, &args)
            }
        }
    }

    /// A Learning picker's key: does it, says what it did, and sends the area's rows again
    /// for the picker still open.
    async fn learning(
        &mut self,
        area: LearningArea,
        command: LearningCommand,
    ) -> anyhow::Result<Vec<Effect>> {
        let ctx = &mut self.ctx;
        // The screen is you at the terminal, unless it runs inside an agent's session.
        let asker = crate::learning::act::Asker {
            at_terminal: !ctx.env.agent_session,
        };
        match crate::learning::act::act(&ctx.env, ctx.settings(), &command, asker) {
            Ok(said) => ctx.notice(NoteLevel::Info, said),
            Err(err) => ctx.notice(NoteLevel::Warn, format!("{err:#}")),
        }
        settings::command(ctx, &mut self.settings, area_command(area)).await
    }

    /// One background replay at a time, while candidates wait and the month's limit lasts.
    fn kick_replays(&self) {
        let settings = self.ctx.settings.shared();
        let policy = crate::commands::policy_for(&self.ctx.env, std::sync::Arc::clone(&settings));
        crate::learning::replay::kick(std::sync::Arc::clone(&self.ctx.env), settings, policy);
    }

    async fn view(&mut self, view: ViewRequest) -> anyhow::Result<Vec<Effect>> {
        let ctx = &mut self.ctx;
        match view {
            ViewRequest::Config
            | ViewRequest::Settings { .. }
            | ViewRequest::Models { .. }
            | ViewRequest::Approvals => settings::view(ctx, &mut self.settings, view).await,
            ViewRequest::StatusTab => {
                status_tab::show(ctx, &mut self.runs, &self.conversation).await
            }
            ViewRequest::Context => {
                conversation::ask_context(&self.conversation, ContextDetail::Full);
                let row = conversation::context_row(&self.conversation);
                run_commands::context(ctx, &mut self.runs, row).await
            }
            ViewRequest::WorkflowLibrary => {
                files::library(ctx, &workflows::taken(&self.agent_id), false);
                Ok(Vec::new())
            }
            ViewRequest::Usage { period } => {
                let spent = conversation::context_row(&self.conversation).map(|row| row.spent);
                account::usage_view(ctx, spent, period).await
            }
            ViewRequest::ReadPlanUsage => {
                account::read_plan_usage(ctx);
                Ok(Vec::new())
            }
            ViewRequest::Overview { period, import } => {
                crate::overview::view(ctx, period, import).await
            }
            ViewRequest::Crew => agent_panel::show(ctx, &self.conversation).await,
            ViewRequest::Mods { open, refresh } => {
                let context = conversation::context_row(&self.conversation);
                crate::mods::open(ctx, &self.mods, context, open, refresh).await
            }
            // A workflow the conversation's own CLI runs, mirrored as a run's.
            ViewRequest::Workflow { run, workflow } if run == conversation_run() => {
                conversation::workflow_view(ctx, &self.conversation, workflow.as_ref());
                Ok(Vec::new())
            }
            ViewRequest::InnerAgent { run, agent, .. } if run == conversation_run() => {
                conversation::inner_agent_view(ctx, &self.conversation, &agent);
                Ok(Vec::new())
            }
            ViewRequest::RepoFiles => {
                send_repo_files(ctx, self.conversation.folder(ctx));
                Ok(Vec::new())
            }
            ViewRequest::Learning { area } => {
                settings::command(ctx, &mut self.settings, area_command(area)).await
            }
            view => run_commands::view(ctx, &mut self.runs, view).await,
        }
    }

    /// The part that asked the question answers it.
    async fn answer_choice(
        &mut self,
        id: ChoiceId,
        action: AnswerAction,
        text: Option<String>,
    ) -> anyhow::Result<Vec<Effect>> {
        let ctx = &mut self.ctx;
        if let Some(setup) = self.setup.as_mut()
            && setup.owns(&id)
        {
            return setup.answer(ctx, &id, action, text).await;
        }
        if conversation::owns_choice(&self.conversation, &id) {
            return conversation::answer_choice(ctx, &mut self.conversation, id, action, text)
                .await;
        }
        if settings::owns_choice(&self.settings, &id) {
            return settings::answer_choice(ctx, &mut self.settings, id, action, text).await;
        }
        if connect::owns_choice(&self.connect, &id) {
            return connect::answer_choice(ctx, &mut self.connect, id, action);
        }
        if workflows::owns_choice(&self.workflows, &id) {
            let (conversation, workflows) = (&mut self.conversation, &mut self.workflows);
            return workflows::answer_choice(ctx, conversation, workflows, id, action).await;
        }
        if prompt_trust::owns_choice(&self.prompt_trust, &id) {
            return prompt_trust::answer_choice(ctx, &mut self.prompt_trust, id, action);
        }
        if settings::renamed::owns_choice(&self.renamed, &id) {
            return settings::renamed::answer_choice(ctx, &mut self.renamed, id, action);
        }
        if setup_again::owns_choice(&self.setup_again, &id) {
            return Ok(setup_again::answer_choice(
                ctx,
                &mut self.setup_again,
                id,
                &action,
            ));
        }
        if folder_git::owns_choice(&self.git, &id) {
            return folder_git::answer_choice(ctx, &mut self.git, id, action);
        }
        if templates::owns_choice(&self.templates, &id) {
            return templates::answer_choice(ctx, &mut self.templates, id, action);
        }
        if needed_update::owns_choice(&self.needed_update, &id) {
            return needed_update::answer_choice(ctx, &mut self.needed_update, id, action);
        }
        if runs::owns_choice(&self.runs, &id) {
            return run_commands::answer_choice(ctx, &mut self.runs, id, action, text).await;
        }
        ctx.notice(NoteLevel::Info, "That question is already closed.");
        Ok(Vec::new())
    }

    async fn set_aside(&mut self, id: ChoiceId) -> anyhow::Result<Vec<Effect>> {
        let ctx = &mut self.ctx;
        if let Some(setup) = self.setup.as_mut()
            && setup.owns(&id)
        {
            return setup.set_aside(ctx, &id).await;
        }
        if conversation::owns_choice(&self.conversation, &id) {
            return conversation::set_aside(ctx, &mut self.conversation, id);
        }
        if settings::owns_choice(&self.settings, &id) {
            return settings::set_aside(ctx, &mut self.settings, id);
        }
        if connect::owns_choice(&self.connect, &id) {
            return connect::set_aside(ctx, &mut self.connect, id);
        }
        if workflows::owns_choice(&self.workflows, &id) {
            return workflows::set_aside(ctx, &mut self.workflows, id);
        }
        if prompt_trust::owns_choice(&self.prompt_trust, &id) {
            return prompt_trust::set_aside(ctx, &mut self.prompt_trust, id);
        }
        if settings::renamed::owns_choice(&self.renamed, &id) {
            return settings::renamed::set_aside(ctx, &mut self.renamed, id);
        }
        if setup_again::owns_choice(&self.setup_again, &id) {
            setup_again::set_aside(ctx, &mut self.setup_again, id);
            return Ok(Vec::new());
        }
        if folder_git::owns_choice(&self.git, &id) {
            return folder_git::set_aside(ctx, &mut self.git, id);
        }
        if templates::owns_choice(&self.templates, &id) {
            return templates::set_aside(ctx, &mut self.templates, id);
        }
        if let Some(why) = needed_update::set_aside(ctx, &mut self.needed_update, id.clone()) {
            // The main session that asked for the run hears that it didn't start.
            conversation::run_not_started(&mut self.conversation, &anyhow::anyhow!("{why}"));
            return Ok(Vec::new());
        }
        if runs::owns_choice(&self.runs, &id) {
            return run_commands::set_aside(ctx, &mut self.runs, id);
        }
        Ok(Vec::new())
    }

    async fn internal_message(&mut self, message: Internal) -> anyhow::Result<Vec<Effect>> {
        match message {
            Internal::Conversation(update) => {
                // The CLI's own command list, which the `/` menu mirrors.
                if let conversation::TurnUpdate::Event {
                    turn,
                    event: RunnerEvent::Agent(AgentEvent::CommandsChanged { commands }),
                } = &update
                    && self
                        .conversation
                        .speaks_for_the_conversation(&self.ctx, turn)
                {
                    cli_commands::reported(
                        &self.ctx,
                        &mut self.agent_id,
                        &self.conversation,
                        commands,
                    );
                }
                let effects =
                    conversation::on_update(&mut self.ctx, &mut self.conversation, update).await?;
                if conversation::print_is_due(&mut self.conversation) {
                    let row = conversation::context_row(&self.conversation);
                    run_commands::context_report(&mut self.ctx, &mut self.runs, row).await?;
                }
                Ok(effects)
            }
            Internal::Run(update) => runs::on_update(&mut self.ctx, &mut self.runs, update).await,
            Internal::Setup(update) => match self.setup.as_mut() {
                Some(setup) => setup.on_update(&mut self.ctx, update).await,
                None => Ok(Vec::new()),
            },
            Internal::Mods => {
                let context = conversation::context_row(&self.conversation);
                crate::mods::changed(&self.ctx, &self.mods, context).await
            }
            Internal::Shell(update) => {
                if let Some(ran) = shell::on_update(&mut self.ctx, &mut self.shell, update) {
                    conversation::shell_ran(&mut self.conversation, ran);
                }
                Ok(Vec::new())
            }
            Internal::ModelsRefreshed => {
                names::send(&mut self.ctx);
                settings::models_refreshed(&self.ctx, &mut self.settings)
            }
            Internal::WorkflowSaved { name } => {
                workflows::saved(&self.ctx, &mut self.workflows, &name)
            }
            Internal::Goal(checked) => Ok(conversation::goal_checked(
                &mut self.ctx,
                &mut self.conversation,
                checked,
            )),
            Internal::PlanUsageRead => Ok(vec![Effect::Status]),
            Internal::CliUpdated(done) => {
                let waiting = &mut self.needed_update;
                Ok(needed_update::updated(&mut self.ctx, waiting, *done))
            }
        }
    }
}

/// A workflow held for its orchestrator waits through the pick and its picker asking for rows
/// again. Anything else you do drops it, so a later pick asks where to save.
fn keeps_orchestrator_hold(command: &UiCommand) -> bool {
    matches!(
        command,
        UiCommand::Slash {
            command: SlashCommand::Model {
                role: Some(RoleName::Orchestrator),
                ..
            }
        } | UiCommand::Request {
            view: ViewRequest::Models {
                role: Some(RoleName::Orchestrator)
            }
        } | UiCommand::Focus { .. }
    )
}
