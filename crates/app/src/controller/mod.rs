//! The app controller: turns what you type into chat turns, runs, and settings changes, and
//! everything that happens into `AppEvent`s for the screen.
//!
//! Each part keeps its own state (`ChatState`, `RunsState`, `SettingsState`, the setup) and
//! handles its commands in its own module. A part that needs another returns an [`Effect`],
//! and the loop applies it, so no part reaches into another's state.

mod account;
mod agents_add;
mod attach;
pub(crate) mod chat;
mod connect;
mod crew_panel;
mod ctx;
mod doctor;
mod files;
#[cfg(test)]
mod fixture_replay;
pub(crate) mod instructions;
mod journal_history;
pub(crate) mod names;
mod narrator_host;
mod notify;
mod recap;
mod route;
mod run_commands;
pub(crate) mod runs;
pub(crate) mod settings;
mod shell;
mod status_line;
#[cfg(test)]
pub(crate) mod test_support;
mod triage;
pub(crate) mod turn;
mod vendor_commands;
mod workspace;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use argonauts_core::app::{AnswerTarget, AppEvent, UiCommand, ViewData, ViewRequest};
use argonauts_core::events::checkpoint::AnswerAction;
use argonauts_core::events::run::RunMode;
use argonauts_core::input::{AgentsAction, PresetAction, ResumeTarget, SlashCommand};
use argonauts_core::names::{ChoiceId, RoleName};
use argonauts_core::views::{LearningArea, LearningCommand, conversation_run};
use argonauts_core::worker::WorkerEvent;
use argonauts_core::{AgentEvent, NoteLevel};
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

pub(crate) use ctx::{AppServices, Ctx, Effect, Internal, Outbox, ScopeRequest};
pub(crate) use route::Route;
pub(crate) use run_commands::CheckpointReply;
pub(crate) use status_line::header_agent;

use crate::FirstView;
use crate::repo_files;
use crate::setup::Setup;

/// How often parked runs are checked for a wake while the app is open.
const WAKE_EVERY: Duration = Duration::from_secs(60);

/// How long closing waits for cancelled chat steps to stop their vendor processes.
const STOP_GRACE: Duration = Duration::from_secs(3);

/// The slash command that opens `area`'s picker.
fn area_command(area: LearningArea) -> SlashCommand {
    match area {
        LearningArea::Records => SlashCommand::Records { agent: None },
        LearningArea::Recipes => SlashCommand::Recipes { name: None },
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

/// What the controller does before it reads input.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Opening {
    /// Runs the guided setup first: the first start, `argonauts init`.
    pub guided_setup: bool,
    pub first: FirstView,
}

pub(crate) struct Controller {
    ctx: Ctx,
    chat: chat::ChatState,
    runs: runs::RunsState,
    settings: settings::SettingsState,
    connect: connect::ConnectState,
    shell: shell::ShellState,
    /// The conversation's CLI's own commands.
    vendor: vendor_commands::VendorCommands,
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
        let chat = chat::ChatState::new(&ctx)?;
        let runs = runs::RunsState::new(&ctx);
        let settings = settings::SettingsState::new(&ctx);
        Ok(Self {
            ctx,
            chat,
            runs,
            settings,
            connect: connect::ConnectState::default(),
            shell: shell::ShellState::default(),
            vendor: vendor_commands::VendorCommands::default(),
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
        self.follow_chat();
        let mut wake = tokio::time::interval(WAKE_EVERY);
        wake.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // The first tick fires at once, and opening already checked.
        wake.tick().await;
        loop {
            let attaches = runs::attaches(&self.runs);
            let effects = tokio::select! {
                command = commands.recv() => {
                    let Some(command) = command else { break };
                    self.ctx.out.trace().command(&command);
                    if !keeps_coordinator_hold(&command) {
                        settings::interrupt(&mut self.settings);
                    }
                    let result = self.command(command).await;
                    self.settle(result)
                }
                Some(message) = self.internal.recv() => {
                    let result = self.internal_message(message).await;
                    self.settle(result)
                }
                _ = wake.tick() => {
                    self.kick_replays();
                    runs::refresh_background(&self.ctx);
                    let result = runs::wake_parked(&mut self.ctx, &mut self.runs).await;
                    self.settle(result.map(|()| Vec::new()))
                }
            };
            let exit = self.apply_all(effects).await;
            if runs::attaches(&self.runs) != attaches {
                chat::run_attached(&mut self.chat);
            }
            self.follow_chat();
            if exit {
                break;
            }
        }
        self.close().await
    }

    /// Cancels the chat's turn, lets the screen close, and waits a little for the turn's step
    /// to end, so its vendor process doesn't outlive the app.
    async fn close(mut self) -> anyhow::Result<()> {
        // A command in the conversation's folder stops before that folder may go.
        shell::close(&mut self.shell);
        chat::close(&mut self.ctx, &mut self.chat);
        self.ctx.send(AppEvent::Exit);
        let deadline = tokio::time::Instant::now() + STOP_GRACE;
        while !chat::settled(&self.chat) {
            let message = tokio::time::timeout_at(deadline, self.internal.recv()).await;
            let Ok(Some(message)) = message else {
                break;
            };
            if let Internal::Chat(update) = message {
                let result = chat::on_update(&mut self.ctx, &mut self.chat, update).await;
                self.settle(result);
            }
        }
        if !chat::settled(&self.chat) {
            tracing::warn!("a chat step was still stopping when the app closed");
        }
        Ok(())
    }

    /// The `/` menu, its keywords, and the `@` list follow the conversation: its CLI, and the
    /// folder it works in. Both change when it carries on an agent's session from a run.
    fn follow_chat(&mut self) {
        vendor_commands::follow(&self.ctx, &mut self.vendor, &self.chat);
        let folder = self.chat.folder(&self.ctx);
        if self.files_folder.as_deref() != Some(folder) {
            let folder = folder.to_path_buf();
            send_repo_files(&self.ctx, &folder);
            self.files_folder = Some(folder);
        }
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
        self.follow_chat();
        let result = if opening.guided_setup {
            // The setup asks about the folder's settings itself.
            self.open_setup().await
        } else {
            settings::ask_trust_at_start(&mut self.ctx, &mut self.settings);
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
                FirstView::Conversation(id) => chat::resume(ctx, &mut self.chat, id).await,
                FirstView::VendorSession(session) => {
                    chat::resume_vendor(ctx, &mut self.chat, session).await
                }
                FirstView::Agent(agent) => chat::resume_agent(ctx, &mut self.chat, agent).await,
                FirstView::Continue => chat::continue_newest(ctx, &mut self.chat).await,
                _ => Ok(Vec::new()),
            };
            // A conversation that isn't there still leaves the app open on a new one.
            effects.extend(self.settle(opened));
            // Copies made again for earlier conversations go, now that the one on screen is
            // known.
            chat::tidy_copies(&self.ctx, &self.chat);
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
                Effect::FreshChat => {
                    // A new main agent brings its own commands, unless the conversation goes to
                    // the agent whose session it carries on.
                    let fresh = chat::fresh_session(&mut self.ctx, &mut self.chat);
                    vendor_commands::refresh(&self.ctx, &mut self.vendor, &self.chat);
                    fresh
                }
                Effect::SettingsApplied { keys } => {
                    names::send(&mut self.ctx);
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
                    chat::sending_queued(&mut self.chat, Some(typed_after));
                    let sent = self.command(*command).await;
                    chat::sending_queued(&mut self.chat, None);
                    sent
                }
                Effect::SetupDone => {
                    self.setup = None;
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
        let offered = settings::offer_gates(&mut self.ctx, &mut self.settings).await;
        let mut effects = self.settle(offered);
        let text = request.request.clone();
        effects.extend(runs::start(&mut self.ctx, &mut self.runs, request).await?);
        chat::run_started(&mut self.chat);
        chat::goal_run_started(&mut self.chat, &text, runs::attached(&self.runs));
        Ok(effects)
    }

    fn send_status(&self) {
        let parts = runs::status_parts(&self.runs);
        let status = status_line::status_line(&self.ctx, chat::title(&self.chat), parts);
        self.ctx.send(AppEvent::Status { status });
    }

    async fn command(&mut self, command: UiCommand) -> anyhow::Result<Vec<Effect>> {
        // A direct turn asks by the mode on screen when it starts.
        let shown = runs::status_parts(&self.runs)
            .approval_mode
            .unwrap_or_else(|| self.ctx.settings().approvals.mode());
        chat::set_approval(&mut self.chat, shown);
        let ctx = &mut self.ctx;
        let runs = &mut self.runs;
        match command {
            UiCommand::Submit { .. } | UiCommand::Mention { .. } => self.route(command).await,
            UiCommand::Slash { command } => self.slash(command).await,
            UiCommand::Answer {
                to: AnswerTarget::Checkpoint { run, id },
                action,
                text,
                evidence_sha256,
                cards,
                remember,
            } => {
                let reply = CheckpointReply {
                    run,
                    id,
                    action,
                    text,
                    evidence_sha256,
                    cards,
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
            // A run's card set aside stays in the crew view; nothing to do here.
            UiCommand::SetAside {
                to: AnswerTarget::Checkpoint { .. },
            } => Ok(Vec::new()),
            UiCommand::Steer {
                run,
                session,
                text,
                now,
            } => run_commands::steer(ctx, runs, run, session, text, now).await,
            UiCommand::TakeOver { run, card } => {
                run_commands::take_over(ctx, runs, run, card).await
            }
            UiCommand::TakeOverEnded {
                run,
                card,
                exit_code,
            } => run_commands::take_over_ended(ctx, runs, run, card, exit_code).await,
            UiCommand::Reassign { run, card, to } => {
                run_commands::reassign(ctx, runs, run, card, to).await
            }
            UiCommand::Pause { now } => run_commands::pause(ctx, runs, now).await,
            UiCommand::CancelPause => run_commands::cancel_pause(ctx, runs).await,
            UiCommand::Resume { run } => run_commands::resume(ctx, runs, run).await,
            UiCommand::Stop => run_commands::stop(ctx, runs, None).await,
            UiCommand::StopAgent { run, session } => {
                run_commands::stop_agent(ctx, runs, run, session).await
            }
            UiCommand::Rewind { card, checkpoint } => {
                run_commands::revert(ctx, runs, card, checkpoint).await
            }
            UiCommand::Summary => run_commands::summary(ctx, runs).await,
            UiCommand::Learning { area, command } => self.learning(area, command).await,
            UiCommand::WorkflowFile { name, action } => files::workflow(ctx, name, action).await,
            UiCommand::SaveWorkflow { run, workflow } => {
                run_commands::save_workflow(ctx, runs, run, workflow).await
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
            UiCommand::CancelTurn => chat::cancel_turn(ctx, &mut self.chat),
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
            UiCommand::Shell { command } => {
                let folder = self.chat.folder(ctx).to_path_buf();
                shell::run(ctx, &mut self.shell, command, &folder);
                Ok(Vec::new())
            }
            UiCommand::StopShell => {
                shell::stop(&mut self.shell);
                Ok(Vec::new())
            }
            UiCommand::SendNow => {
                chat::send_now(&mut self.chat);
                Ok(Vec::new())
            }
            UiCommand::TakeBackChat => {
                chat::take_back(ctx, &mut self.chat);
                Ok(Vec::new())
            }
            UiCommand::PressHeadsUp { plugin, handle } => {
                chat::press_heads_up(ctx, &mut self.chat, &plugin, handle);
                Ok(Vec::new())
            }
            UiCommand::Exit => Ok(vec![Effect::Exit]),
            UiCommand::RememberPick { run, id } => {
                run_commands::remember_pick(ctx, runs, run, id).await
            }
        }
    }

    /// Plain text and mentions mean what the app mode says.
    async fn route(&mut self, command: UiCommand) -> anyhow::Result<Vec<Effect>> {
        let root = self.chat.folder(&self.ctx).to_path_buf();
        let ctx = &mut self.ctx;
        let runs = &mut self.runs;
        let leads_with_file = |text: &str| repo_files::leads_with_file(&root, text);
        match route::route(&ctx.mode, command, self.vendor.set(), leads_with_file) {
            Route::NewRequest { text } => chat::new_request(ctx, &mut self.chat, text).await,
            Route::NextTurn { text } => chat::queue_turn(ctx, &mut self.chat, text),
            Route::Guidance { run, text } => {
                let ran = chat::take_shell(&mut self.chat);
                let sent = run_commands::guidance(ctx, runs, run, text, &ran).await;
                if sent.is_err() {
                    chat::give_back_shell(&mut self.chat, ran);
                }
                sent
            }
            Route::CancelPause => run_commands::cancel_pause(ctx, runs).await,
            Route::Resume { run } => run_commands::resume(ctx, runs, Some(run)).await,
            Route::Mention { run, target, text } => {
                let ran = chat::take_shell(&mut self.chat);
                let sent = run_commands::mention(ctx, runs, run, target, text, &ran).await;
                if sent.is_err() {
                    chat::give_back_shell(&mut self.chat, ran);
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
        let ctx = &mut self.ctx;
        let runs = &mut self.runs;
        let chat = &mut self.chat;
        let settings = &mut self.settings;
        match command {
            SlashCommand::Clear => chat::clear(ctx, chat),
            SlashCommand::Resume { target: None } => run_commands::resume(ctx, runs, None).await,
            SlashCommand::Resume {
                target: Some(ResumeTarget::Run(run)),
            } => run_commands::resume(ctx, runs, Some(run)).await,
            SlashCommand::Resume {
                target: Some(ResumeTarget::Conversation(id)),
            } => chat::resume(ctx, chat, &id).await,
            SlashCommand::Resume {
                target: Some(ResumeTarget::VendorSession(session)),
            } => chat::resume_vendor(ctx, chat, &session).await,
            SlashCommand::Resume {
                target: Some(ResumeTarget::Agent(agent)),
            } => chat::resume_agent(ctx, chat, &agent).await,
            SlashCommand::Exit => Ok(vec![Effect::Exit]),
            SlashCommand::Ask { question } => {
                if runs::narrator_answers(runs, ctx) {
                    run_commands::ask_narrator(ctx, runs, question).await
                } else {
                    chat::ask(ctx, chat, question).await
                }
            }
            SlashCommand::Solo { task } => chat::start_forced(ctx, chat, RunMode::Solo, task).await,
            SlashCommand::Crew { task } => chat::start_forced(ctx, chat, RunMode::Crew, task).await,
            SlashCommand::Workflow { task: Some(task) } if !task.trim().is_empty() => {
                chat::start_workflow(ctx, chat, task).await
            }
            SlashCommand::Workflow { .. } if runs::attached(runs).is_some() => {
                run_commands::workflow_view(ctx, runs, None).await
            }
            SlashCommand::Workflow { .. } => {
                files::library(ctx, false);
                Ok(Vec::new())
            }
            SlashCommand::FollowUp { run, request } => {
                run_commands::continue_run(ctx, runs, run, request).await
            }
            SlashCommand::Status => run_commands::status(ctx, runs).await,
            SlashCommand::Workflows if runs::attached(runs).is_some() => {
                run_commands::workflow_view(ctx, runs, None).await
            }
            SlashCommand::Workflows => {
                files::library(ctx, false);
                Ok(Vec::new())
            }
            SlashCommand::Context => {
                let row = chat::context_row(chat);
                run_commands::context(ctx, runs, row).await
            }
            // In a direct conversation, the turns that changed files; in a run, its checkpoints.
            SlashCommand::Rewind { card: None } if chat::rewind(ctx, chat) => Ok(Vec::new()),
            SlashCommand::Rewind { card } => run_commands::checkpoints(ctx, runs, card).await,
            SlashCommand::Reassign { card, from, to } => {
                run_commands::slash_reassign(ctx, runs, card, from, to).await
            }
            SlashCommand::Stop { run } => run_commands::stop(ctx, runs, run).await,
            SlashCommand::Agents {
                action: AgentsAction::Add { name },
            } => agents_add::add(ctx, &name).await,
            SlashCommand::Preset {
                name,
                action: PresetAction::Use,
            } => {
                let command = SlashCommand::Preset {
                    name,
                    action: PresetAction::Use,
                };
                settings::command(ctx, settings, command).await
            }
            SlashCommand::Preset { name, action } => files::preset(ctx, name, action).await,
            SlashCommand::Verbosity { level: Some(level) } => {
                settings::verbosity(ctx, settings, level).await
            }
            SlashCommand::Vim => settings::toggle_vim(ctx, settings).await,
            SlashCommand::Setup => Ok(vec![Effect::OpenSetup]),
            SlashCommand::Connect { front_end } => {
                connect::connect(ctx, &mut self.connect, front_end)
            }
            SlashCommand::Doctor => doctor::doctor(ctx),
            // The screen handles this itself.
            SlashCommand::Help => Ok(Vec::new()),
            command @ (SlashCommand::Model { .. }
            | SlashCommand::Effort { .. }
            | SlashCommand::Agents { .. }
            | SlashCommand::Handoff { .. }
            | SlashCommand::Approvals { .. }
            | SlashCommand::Budget { .. }
            | SlashCommand::Permissions { .. }
            | SlashCommand::Verbosity { .. }
            | SlashCommand::Narrator { .. }
            | SlashCommand::Theme { .. }
            | SlashCommand::Style { .. }
            | SlashCommand::Config { .. }
            | SlashCommand::Records { .. }
            | SlashCommand::Recipes { .. }
            | SlashCommand::Skills
            | SlashCommand::Memory) => settings::command(ctx, settings, command).await,
            SlashCommand::Compact { instructions } => chat::compact(ctx, chat, instructions),
            SlashCommand::Rename { name } => chat::rename(ctx, chat, name),
            SlashCommand::Branch { name } => chat::branch(ctx, chat, name),
            SlashCommand::Fork { prompt } => chat::fork(ctx, chat, prompt),
            SlashCommand::Btw { question } => chat::btw(ctx, chat, question),
            SlashCommand::Recap => chat::recap(ctx, chat).await,
            // The screen copies itself; a front end without one gets the text.
            SlashCommand::Copy { nth } => chat::copy(ctx, chat, nth),
            SlashCommand::Export { file } => chat::export(ctx, chat, file),
            SlashCommand::Cd { path } => workspace::cd(ctx, path, chat::settled(chat)),
            SlashCommand::AddDir { path } => workspace::add_dir(ctx, &path),
            SlashCommand::Plan { task } => chat::plan(ctx, chat, task),
            SlashCommand::Goal { text } => chat::goal(ctx, chat, text),
            SlashCommand::Fast { on } => chat::fast(ctx, chat, on),
            SlashCommand::OutputStyle { style } => chat::output_style(ctx, chat, style),
            SlashCommand::Diff => workspace::diff(ctx),
            SlashCommand::Review { target } => chat::review(ctx, chat, false, target),
            SlashCommand::SecurityReview { target } => chat::review(ctx, chat, true, target),
            SlashCommand::Usage { period } => {
                let spent = chat::context_row(chat).map(|row| row.spent);
                account::usage(ctx, spent, period).await
            }
            SlashCommand::Login { agent } => account::login(ctx, agent, true),
            SlashCommand::Logout { agent } => account::login(ctx, agent, false),
            SlashCommand::ReleaseNotes => account::release_notes(ctx),
            SlashCommand::Feedback { text } => account::feedback(ctx, text),
            SlashCommand::Vendor { name, args } => {
                vendor_commands::run(ctx, chat, &self.vendor, &name, &args)
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

    /// One background replay at a time, while candidates wait and the month's budget lasts.
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
            ViewRequest::Context => {
                let row = chat::context_row(&self.chat);
                run_commands::context(ctx, &mut self.runs, row).await
            }
            ViewRequest::WorkflowLibrary => {
                files::library(ctx, false);
                Ok(Vec::new())
            }
            ViewRequest::Crew => crew_panel::show(ctx, &self.chat).await,
            // A workflow the conversation's own CLI runs, mirrored as a run's.
            ViewRequest::Workflow { run, workflow } if run == conversation_run() => {
                chat::workflow_view(ctx, &self.chat, workflow.as_ref());
                Ok(Vec::new())
            }
            ViewRequest::RepoFiles => {
                send_repo_files(ctx, self.chat.folder(ctx));
                Ok(Vec::new())
            }
            ViewRequest::AgentSessions { run } => {
                let agents = chat::run_agent_sessions(ctx, &run).await?;
                ctx.send(AppEvent::View {
                    data: Box::new(ViewData::AgentSessions { run, agents }),
                });
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
        if chat::owns_choice(&self.chat, &id) {
            return chat::answer_choice(ctx, &mut self.chat, id, action, text).await;
        }
        if settings::owns_choice(&self.settings, &id) {
            return settings::answer_choice(ctx, &mut self.settings, id, action, text).await;
        }
        if connect::owns_choice(&self.connect, &id) {
            return connect::answer_choice(ctx, &mut self.connect, id, action);
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
        if chat::owns_choice(&self.chat, &id) {
            return chat::set_aside(ctx, &mut self.chat, id);
        }
        if settings::owns_choice(&self.settings, &id) {
            return settings::set_aside(ctx, &mut self.settings, id);
        }
        if connect::owns_choice(&self.connect, &id) {
            return connect::set_aside(ctx, &mut self.connect, id);
        }
        if runs::owns_choice(&self.runs, &id) {
            return run_commands::set_aside(ctx, &mut self.runs, id);
        }
        Ok(Vec::new())
    }

    async fn internal_message(&mut self, message: Internal) -> anyhow::Result<Vec<Effect>> {
        match message {
            Internal::Chat(update) => {
                // The CLI's own command list, which the `/` menu mirrors.
                if let chat::TurnUpdate::Event {
                    turn,
                    event: WorkerEvent::Agent(AgentEvent::CommandsChanged { commands }),
                } = &update
                    && self.chat.speaks_for_the_conversation(&self.ctx, turn)
                {
                    vendor_commands::reported(&self.ctx, &mut self.vendor, &self.chat, commands);
                }
                chat::on_update(&mut self.ctx, &mut self.chat, update).await
            }
            Internal::Run(update) => runs::on_update(&mut self.ctx, &mut self.runs, update).await,
            Internal::Setup(update) => match self.setup.as_mut() {
                Some(setup) => setup.on_update(&mut self.ctx, update).await,
                None => Ok(Vec::new()),
            },
            Internal::Shell(update) => {
                if let Some(ran) = shell::on_update(&mut self.ctx, &mut self.shell, update) {
                    chat::shell_ran(&mut self.chat, ran);
                }
                Ok(Vec::new())
            }
            Internal::ModelsRefreshed => {
                names::send(&mut self.ctx);
                settings::models_refreshed(&self.ctx, &mut self.settings)
            }
        }
    }
}

/// A preset held for its coordinator waits through the pick and its picker asking for rows
/// again. Anything else you do drops it, so a later pick asks where to save.
fn keeps_coordinator_hold(command: &UiCommand) -> bool {
    matches!(
        command,
        UiCommand::Slash {
            command: SlashCommand::Model {
                role: Some(RoleName::Coordinator),
                ..
            }
        } | UiCommand::Request {
            view: ViewRequest::Models {
                role: Some(RoleName::Coordinator)
            }
        } | UiCommand::Focus { .. }
    )
}
