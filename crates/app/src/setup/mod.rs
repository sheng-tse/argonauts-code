//! The guided setup (`argonauts setup`, `/setup`, and the first start): the terms note, your
//! agents, who you talk to, how the crew works and the roles that workflow has, a local model
//! server when it has a reviewer, approvals, and this folder. Every answer is written at the
//! end, so a setup cut off leaves no file.

mod finish;
pub(crate) mod probe;
mod questions;
pub(crate) mod reference;
mod roles;
mod steps;
mod templates;

use std::collections::{BTreeMap, VecDeque};

use anyhow::bail;
use argonauts_conductor::{DoctorReport, doctor};
use argonauts_core::NoteLevel;
use argonauts_core::app::AppEvent;
use argonauts_core::events::approval::AnswerAction;
use argonauts_core::layout::user_files;
use argonauts_core::model_catalog::display_name_from_id;
use argonauts_core::names::{ChoiceId, ModelRef};
use argonauts_core::paths::{create_private_dir_all, write_private_file};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::text::shown_name;

use crate::builtin_adapters;
use crate::choices::app_choice;
use crate::controller::{Ctx, Effect, Internal};
use crate::startup::{RunCounts, run_counts, settings_warnings, welcome_from};
use crate::trust::cant_trust;
use argonauts_registry::SettingsWarning;

pub(crate) use finish::project_dir as project_folder;
pub(crate) use probe::{FoundAgent, LocalServer, LoginState};
use questions::{SetupView, Step};
use steps::Answers;
#[allow(unused_imports)]
pub(crate) use templates::guide;
#[cfg(test)]
pub(crate) use templates::guide_topics;

/// What Esc on a setup question leaves in the conversation, so you know how to get back to the
/// questions.
const TOOK_THE_RECOMMENDED: &str =
    "Took the recommended answers. /setup (or argonauts setup) walks through the setup again.";

/// Between `/setup` and its one line, which the screen draws as the command with the line
/// under it, as Claude Code closes a dialog.
const COMMAND_SAID: &str = " ⎿  ";

/// The one row a setup run over your settings ends in, in place of a row per question.
pub(crate) fn setup_row(line: &str) -> String {
    format!("/setup{COMMAND_SAID}{line}")
}

pub(crate) struct Setup {
    open: Option<ChoiceId>,
    view: SetupView,
    answers: Answers,
    /// The steps still to go, the open one first.
    steps: VecDeque<Step>,
    /// So a new main agent starts a fresh conversation.
    agent_before: ModelRef,
    header: Header,
    done: bool,
    /// You had settings already, so the questions leave no rows and the setup ends in one.
    one_row: bool,
}

/// What the header showed besides your settings, so it can print again with the new ones.
pub(crate) struct Header {
    pub report: DoctorReport,
    pub runs: RunCounts,
}

/// Results of the setup's probes, which run on their own tasks.
#[derive(Debug)]
pub(crate) enum SetupUpdate {
    Servers(Vec<LocalServer>),
    Logins(Vec<(String, LoginState)>),
}

/// Whether the probes run on their own, or a test feeds their results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probing {
    Live,
    #[cfg(test)]
    Fed,
}

impl Setup {
    /// Shows the first step. Run again, it preselects your current values. Without a terminal
    /// it takes the recommended answers at once, writes no safety key, and returns
    /// `SetupDone`.
    pub(crate) async fn begin(ctx: &mut Ctx) -> anyhow::Result<(Self, Vec<Effect>)> {
        let adapters = builtin_adapters();
        let report = doctor(&adapters, ctx.settings()).await;
        let agents = probe::found_agents(&adapters, &report, ctx.settings());
        let runs = run_counts(ctx.services.conductor.as_ref()).await;
        let header = Header { report, runs };
        Self::begin_with(ctx, agents, header, Probing::Live).await
    }

    async fn begin_with(
        ctx: &mut Ctx,
        agents: Vec<FoundAgent>,
        header: Header,
        probing: Probing,
    ) -> anyhow::Result<(Self, Vec<Effect>)> {
        let one_row = ctx.env.global_config().exists();
        let current = read_table(&ctx.env.global_config());
        let untrusted =
            finish::untrusted_files(&ctx.env, |file| ctx.settings.trust().is_trusted(file));
        let view = SetupView {
            agents,
            logins_known: false,
            servers: None,
            current,
            untrusted,
            guides_answer: templates::guides_answer(&ctx.env),
            role_keys: crate::controller::settings::model_keys(),
            models: models_by_agent(ctx),
            workflows: crate::controller::settings::workflow_list(ctx)
                .into_iter()
                .map(|workflow| workflow.name)
                .collect(),
            captain_reviews: ctx.settings().captain.review,
        };
        let mut steps = VecDeque::new();
        if !terms_shown(ctx) {
            steps.push_back(Step::Terms);
        }
        // The workflow's own steps follow its answer, and the local servers after them.
        steps.extend([
            Step::Agents,
            Step::MainAgent,
            Step::Workflow,
            Step::Approvals,
            Step::Trust,
        ]);
        let mut setup = Self {
            open: None,
            view,
            answers: Answers::default(),
            steps,
            agent_before: ctx.settings().crew.main.clone(),
            header,
            done: false,
            one_row,
        };
        if !ctx.env.interactive {
            if probing == Probing::Live {
                let logins = probe::logins(&setup.view.agents).await;
                setup.learn_logins(logins);
            }
            setup.view.logins_known = true;
            // Nobody picks a server's model without a terminal, so none is probed.
            setup.view.servers = Some(Vec::new());
            let effects = setup.take_the_rest(ctx)?;
            return Ok((setup, effects));
        }
        if probing == Probing::Live {
            setup.start_probes(ctx);
        }
        let effects = setup.advance(ctx)?;
        Ok((setup, effects))
    }

    pub(crate) fn owns(&self, id: &ChoiceId) -> bool {
        self.open.as_ref() == Some(id)
    }

    pub(crate) async fn answer(
        &mut self,
        ctx: &mut Ctx,
        id: &ChoiceId,
        action: AnswerAction,
        text: Option<String>,
    ) -> anyhow::Result<Vec<Effect>> {
        // Setup questions take no note.
        let _ = text;
        let Some(step) = self.steps.front().cloned() else {
            bail!("the setup has no open question");
        };
        let Some(question) = questions::question(&step, &self.view, &self.answers) else {
            bail!("the setup has no open question");
        };
        let value = questions::value_of(&action).filter(|value| question.offers(value));
        let Some(value) = value else {
            bail!("that isn't one of this question's answers");
        };
        let mut outcome = self.settle_step(ctx, &step, &value);
        if self.one_row {
            outcome.clear();
            // How to log an agent back in is still its own line.
            if step == Step::Agents {
                for fix in questions::logged_out_fixes(&self.view) {
                    ctx.notice(NoteLevel::Warn, fix);
                }
            }
        }
        self.open = None;
        ctx.send(AppEvent::ChoiceClosed {
            id: id.clone(),
            outcome,
        });
        let mut effects = self.advance(ctx)?;
        // The bottom line shows the mode just picked.
        if step == Step::Approvals {
            effects.insert(0, Effect::Status);
        }
        Ok(effects)
    }

    /// The approval mode the approvals step picked, until the setup writes it.
    pub(crate) fn approval_mode(&self) -> Option<ApprovalMode> {
        self.answers.approvals
    }

    /// Esc: the recommended answers for this step and the rest.
    pub(crate) async fn set_aside(
        &mut self,
        ctx: &mut Ctx,
        id: &ChoiceId,
    ) -> anyhow::Result<Vec<Effect>> {
        self.open = None;
        let outcome = if self.one_row {
            String::new()
        } else {
            TOOK_THE_RECOMMENDED.to_owned()
        };
        ctx.send(AppEvent::ChoiceClosed {
            id: id.clone(),
            outcome,
        });
        self.take_the_rest(ctx)
    }

    pub(crate) async fn on_update(
        &mut self,
        ctx: &mut Ctx,
        update: SetupUpdate,
    ) -> anyhow::Result<Vec<Effect>> {
        match update {
            SetupUpdate::Logins(logins) => {
                self.learn_logins(logins);
                self.view.logins_known = true;
            }
            SetupUpdate::Servers(servers) => self.view.servers = Some(servers),
        }
        if self.done || self.open.is_some() {
            return Ok(Vec::new());
        }
        self.advance(ctx)
    }

    fn learn_logins(&mut self, logins: Vec<(String, LoginState)>) {
        for (id, state) in logins {
            if let Some(agent) = self.view.agents.iter_mut().find(|agent| agent.id == id) {
                agent.login = state;
            }
        }
    }

    /// The login checks and the server probe, each on its own task, so no step waits on a
    /// slow CLI longer than it must.
    fn start_probes(&self, ctx: &Ctx) {
        let agents = self.view.agents.clone();
        let internal = ctx.internal.clone();
        tokio::spawn(async move {
            let logins = probe::logins(&agents).await;
            let _ = internal.send(Internal::Setup(SetupUpdate::Logins(logins)));
        });
        let settings = ctx.settings.shared();
        let internal = ctx.internal.clone();
        tokio::spawn(async move {
            let servers = probe::local_servers(&settings).await;
            let _ = internal.send(Internal::Setup(SetupUpdate::Servers(servers)));
        });
    }

    /// Asks the next question, skips steps with nothing to ask, and finishes after the last.
    /// A step whose probe isn't back yet waits for [`Self::on_update`].
    fn advance(&mut self, ctx: &mut Ctx) -> anyhow::Result<Vec<Effect>> {
        while let Some(step) = self.steps.front().cloned() {
            if !self.view.can_ask(&step) {
                return Ok(Vec::new());
            }
            if step == Step::Servers {
                // A reviewer chosen in "Who does what" is the one written, so a local model
                // would have nothing to fill.
                if self.answers.roles.contains_key(roles::REVIEWER_KEY) {
                    self.steps.pop_front();
                } else {
                    self.expand_servers();
                }
                continue;
            }
            // Nothing to ask, such as who you talk to when no agent is ready.
            let Some(question) = questions::question(&step, &self.view, &self.answers) else {
                self.steps.pop_front();
                continue;
            };
            let id = ctx.next_choice_id();
            ctx.send(AppEvent::Choice {
                choice: app_choice(
                    id.clone(),
                    question.kind,
                    question.title,
                    question.body,
                    question.options,
                ),
            });
            self.open = Some(id);
            return Ok(Vec::new());
        }
        self.finish(ctx)
    }

    fn expand_servers(&mut self) {
        self.steps.pop_front();
        let servers = self.view.servers.clone().unwrap_or_default();
        for server in servers.into_iter().rev() {
            self.steps.push_front(Step::Server(server));
        }
    }

    /// Records the step's answer, removes it, and returns the transcript's line.
    fn settle_step(&mut self, ctx: &Ctx, step: &Step, value: &str) -> String {
        let outcome = questions::apply(step, value, &self.view, &mut self.answers);
        self.steps.pop_front();
        match step {
            Step::Terms => {
                if let Err(err) = mark_terms_shown(ctx) {
                    ctx.error(&err);
                }
            }
            Step::Workflow => {
                // Each goes in front of the one before, so they're asked in the reverse order:
                // who orchestrates, who does what, then a local model for the reviewer.
                let workflow = self.answers.workflow.clone();
                if questions::has_reviewer(&workflow, &self.view) {
                    self.steps.push_front(Step::Servers);
                }
                if workflow != steps::CAPTAIN {
                    self.steps.push_front(Step::WhoDoesWhat);
                } else if self.view.captain_reviews {
                    // Captain's one other role is its reviewer.
                    let key = roles::REVIEWER_KEY.to_owned();
                    self.steps.push_front(Step::RoleAgent(key));
                }
                if workflow == steps::ORCHESTRATOR {
                    self.steps.push_front(Step::Orchestrator);
                }
            }
            Step::Orchestrator => {
                let key = roles::ORCHESTRATOR_KEY.to_owned();
                self.steps.push_front(Step::RoleModel(key));
            }
            // The list comes back after each role you change, until you keep them all.
            Step::WhoDoesWhat if value != roles::KEEP => {
                self.steps.push_front(Step::WhoDoesWhat);
                self.steps.push_front(Step::RoleAgent(value.to_owned()));
            }
            Step::RoleAgent(key) if value != roles::KEEP => {
                self.steps.push_front(Step::RoleModel(key.clone()));
            }
            _ => {}
        }
        outcome
    }

    /// Takes the first answer of every step left, then finishes.
    fn take_the_rest(&mut self, ctx: &mut Ctx) -> anyhow::Result<Vec<Effect>> {
        while let Some(step) = self.steps.front().cloned() {
            match step {
                Step::Terms if !ctx.env.interactive => {
                    ctx.notice(NoteLevel::Info, argonauts_registry::TERMS_NOTE);
                    self.settle_step(ctx, &step, questions::CONTINUE);
                }
                // A server not probed yet stays unused, as Not now would leave it.
                Step::Servers => {
                    self.steps.pop_front();
                }
                _ => match questions::question(&step, &self.view, &self.answers)
                    .and_then(|question| question.first_value())
                {
                    Some(first) => {
                        self.settle_step(ctx, &step, &first);
                    }
                    None => {
                        self.steps.pop_front();
                    }
                },
            }
        }
        self.finish(ctx)
    }

    /// Writes everything at once and hands back to the conversation. A failure is a notice, and the
    /// conversation opens anyway.
    fn finish(&mut self, ctx: &mut Ctx) -> anyhow::Result<Vec<Effect>> {
        self.done = true;
        let env = std::sync::Arc::clone(&ctx.env);
        let keys = steps::keys(&self.answers, env.interactive);
        // Held until the header changes, so they follow the settings now in effect.
        let mut notices = Vec::new();
        let written = finish::write_config(&env.global_config(), &keys);
        if let Err(err) = &written {
            notices.push((
                NoteLevel::Error,
                format!("Couldn't save your answers, so the setup runs again next time: {err:#}"),
            ));
        }
        notices.extend(finish::write_workflows(&env.paths.global_workflows()));
        if self.answers.trust_folder {
            // `trusted.json` lives in this repo's state folder, which a first start hasn't made.
            if let Err(err) = create_private_dir_all(&env.state_dir) {
                notices.push((
                    NoteLevel::Error,
                    format!("Couldn't trust this folder: {err}"),
                ));
            }
            for file in &self.view.untrusted {
                // A file whose safety settings don't resolve would break the next start.
                if let Err(err) = ctx.settings.check_trust(&env, file) {
                    notices.push((NoteLevel::Warn, cant_trust(&env.shown(file), &err)));
                    continue;
                }
                if let Err(err) = ctx.settings.trust_mut().trust(file) {
                    notices.push((NoteLevel::Error, format!("{err:#}")));
                }
            }
        }
        // The guide pages go into this repo only where you once said yes to them; this
        // folder's `.argonauts/` is made when a change first needs it.
        let in_project = self.view.guides_answer == Some(true);
        let pages = templates::write(&env, in_project);
        notices.extend(pages.notices);
        if let Err(err) = ctx.settings.reload(&env) {
            notices.push((NoteLevel::Error, format!("{err:#}")));
        }
        // The header printed before you answered, so it changes in place to the new agent and
        // workflow, followed by the warnings about the settings now in effect.
        ctx.send(welcome_from(ctx, &self.header.report, self.header.runs));
        let wrote: Vec<String> = pages
            .in_project
            .iter()
            .map(|page| env.shown(page))
            .collect();
        if self.one_row && written.is_ok() {
            let id = ctx.next_choice_id();
            let outcome = setup_row(&self.result_line(ctx, &wrote));
            ctx.send(AppEvent::ChoiceClosed { id, outcome });
        }
        let warnings = ctx.settings.resolved().warnings.iter();
        let said = |warning: &&SettingsWarning| {
            crate::controller::settings::said_by_a_question(ctx, warning)
        };
        notices.splice(0..0, settings_warnings(warnings.filter(|w| !said(w))));
        for (level, text) in notices {
            ctx.notice(level, text);
        }
        if self.view.ready().next().is_none() {
            ctx.notice(
                NoteLevel::Warn,
                finish::nothing_ready_line(&self.view.agents),
            );
        }
        if written.is_ok() && !self.one_row {
            let also = if wrote.is_empty() {
                String::new()
            } else {
                format!(" Wrote {} here.", wrote.join(", "))
            };
            ctx.notice(
                NoteLevel::Info,
                format!(
                    "Saved to {}.{also} {}",
                    env.display(&env.global_config()),
                    finish::CLOSING
                ),
            );
        }
        let mut effects = Vec::new();
        if written.is_ok() {
            effects.push(Effect::SettingsApplied {
                keys: keys.into_iter().map(|(key, _)| key).collect(),
            });
        }
        if ctx.settings().crew.main != self.agent_before {
            effects.push(Effect::FreshConversation);
        }
        effects.push(Effect::SetupDone);
        Ok(effects)
    }

    /// What the setup left you with, in one line: `Captain · Claude Code · auto mode · Usage
    /// reports on · saved to ~/.argonauts/config.toml`.
    fn result_line(&self, ctx: &Ctx, wrote: &[String]) -> String {
        let settings = ctx.settings();
        let mode = match settings.approvals.mode() {
            ApprovalMode::Autopilot => "autopilot".to_owned(),
            mode => format!("{} mode", mode.label()),
        };
        // CI, DO_NOT_TRACK or an agent's session keep reports off whatever the setting says. The
        // tests run in CI, and speak of a machine where nothing does.
        let quiet =
            !cfg!(test) && crate::telemetry::notice::quiet(&|name| std::env::var(name).ok());
        let usage = usage_words(settings.telemetry.enabled, quiet);
        let mut parts = vec![
            shown_name(&settings.workflow),
            self.view.agent_name(&settings.crew.main.agent),
            mode,
            usage.to_owned(),
        ];
        if !wrote.is_empty() {
            parts.push(format!("wrote {} here", wrote.join(", ")));
        }
        parts.push(format!(
            "saved to {}",
            ctx.env.display(&ctx.env.global_config())
        ));
        parts.join(" · ")
    }
}

/// A missing or unreadable file reads as empty, so nothing is preselected.
/// Each agent's models the registry knows, as `/model` lists them, without the older ones.
fn models_by_agent(ctx: &Ctx) -> BTreeMap<String, Vec<roles::ModelOption>> {
    let mut models: BTreeMap<String, Vec<roles::ModelOption>> = BTreeMap::new();
    for choice in crate::controller::settings::model_choices(ctx, None) {
        if choice.older {
            continue;
        }
        let name = match &choice.worker.model {
            None => "Default".to_owned(),
            Some(model) if choice.name.is_empty() => display_name_from_id(model),
            Some(_) => choice.name.clone(),
        };
        models
            .entry(choice.worker.agent.clone())
            .or_default()
            .push(roles::ModelOption {
                model: choice.worker.model.clone(),
                name,
                note: choice.note.or(choice.description),
            });
    }
    models
}

fn read_table(path: &std::path::Path) -> toml::Table {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .unwrap_or_default()
}

fn terms_shown(ctx: &Ctx) -> bool {
    ctx.env.paths.home().join(user_files::TERMS_SHOWN).exists()
}

fn mark_terms_shown(ctx: &Ctx) -> anyhow::Result<()> {
    let home = ctx.env.paths.home();
    create_private_dir_all(home)?;
    write_private_file(&home.join(user_files::TERMS_SHOWN), b"")?;
    Ok(())
}

/// Whether usage reports go, as setup's row says it.
fn usage_words(enabled: bool, quiet: bool) -> &'static str {
    if enabled && !quiet {
        "Usage reports on"
    } else {
        "Usage reports off"
    }
}

#[cfg(test)]
mod tests;
