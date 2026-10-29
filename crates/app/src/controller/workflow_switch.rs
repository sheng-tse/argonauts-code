//! Switching workflows: a bare `/captain`, `/orchestrator`, `/council` or `/<your workflow>`,
//! `/workflow <name>`, `/config workflow=…` and the `/workflow` picker's pick. A workflow is ready when each role its steps
//! name has a ready agent; a ready one switches at once and says where it was saved. One that
//! isn't opens its setup sheet, with the pointer on what needs you: Enter saves everything in
//! one write and switches, Esc saves nothing.
//!
//! Logins are read as the command comes, so a CLI logged out since the app started counts as
//! it is now, and again every few seconds while a sheet is open, so a CLI you log in to in
//! another terminal shows up on its own. Without Doctor's check of the CLIs, as before the
//! app's start has made one, every CLI that's on counts as ready: nothing is known against it.
//!
//! The `/` menu says beside each workflow's command whether it is in use, ready, or needs
//! setting up first: from Doctor's check at start, then from the logins each time a switch or
//! a sheet reads them. The start runs no login check of its own, so opening the app starts no
//! CLI beyond Doctor's.

mod sheet;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use argonauts_conductor::{AgentStatus, DoctorReport};
use argonauts_core::NoteLevel;
use argonauts_core::app::{AppEvent, CommandReply, ViewData};
use argonauts_core::model_catalog::display_name_from_id;
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::settings::{SettingScope, Settings};
use argonauts_core::setup_sheet::{SheetAction, WorkflowStatus};
use argonauts_crew::roles::candidates;

use super::settings::save::{self, Change, TrustAfter};
use super::settings::{check_known, model_choices, workflow_list};
use super::{Ctx, Effect, Internal, workflows};
use crate::setup::probe::{self, LoginState};
use sheet::{Draft, Facts, Model};

type Handled = anyhow::Result<Vec<Effect>>;

const CAPTAIN: &str = "captain";
const ORCHESTRATOR: &str = "orchestrator";
const COUNCIL: &str = "council";

/// How often an open sheet reads the logins again.
const LOGINS_EVERY: Duration = Duration::from_secs(2);

#[derive(Debug, Default)]
pub(crate) struct State {
    /// The workflow a switch waits on the logins for, and the place it was asked to save.
    waiting: Option<(String, Option<SettingScope>)>,
    /// The sheet on screen.
    open: Option<Open>,
    /// Counts the sheets opened and closed, so logins read for one that closed are dropped.
    generation: u64,
    /// The logins as last read.
    logins: BTreeMap<String, LoginState>,
    /// The statuses the `/` menu has, so it hears again only of a change.
    told: Option<Vec<(String, WorkflowStatus)>>,
}

#[derive(Debug)]
struct Open {
    draft: Draft,
    facts: Facts,
    /// Where the switch was asked to save, as a picker's ctrl+s asks for this session.
    chosen: Option<SettingScope>,
}

/// What a login check sent back, for the sheet or switch of `generation`.
#[derive(Debug)]
pub(crate) struct Probed {
    logins: BTreeMap<String, LoginState>,
    generation: u64,
}

/// `/<workflow>` with nothing after it, `/workflow <name>`, `/config workflow=<name>`, or a
/// pick in the `/workflow` picker. `chosen` names a place to save on purpose; without it the
/// switch saves where changes save.
pub(crate) fn switch(
    ctx: &mut Ctx,
    state: &mut State,
    name: &str,
    chosen: Option<SettingScope>,
) -> Handled {
    let name = name.trim().trim_matches(['"', '\'']);
    let known = workflow_list(ctx)
        .into_iter()
        .any(|workflow| workflow.name == name);
    if !known {
        anyhow::bail!("There's no workflow named {name}. /workflow lists them");
    }
    state.open = None;
    state.generation += 1;
    let checks = login_checks(ctx);
    if checks.is_empty() {
        return decide(ctx, state, name, chosen, &BTreeMap::new());
    }
    state.waiting = Some((name.to_owned(), chosen));
    read_logins_after(ctx, checks, Duration::ZERO, state.generation);
    Ok(Vec::new())
}

/// The logins are in: the switch they were read for goes on, or the open sheet is drawn again
/// with them, and they're read again in a few seconds.
pub(crate) fn probed(ctx: &mut Ctx, state: &mut State, probed: Probed) -> Handled {
    // Logins read for a sheet that closed are still what they are now, for the menu.
    state.logins.clone_from(&probed.logins);
    tell_the_menu(ctx, state);
    if probed.generation != state.generation {
        return Ok(Vec::new());
    }
    if let Some((name, chosen)) = state.waiting.take() {
        return decide(ctx, state, &name, chosen, &probed.logins);
    }
    let Some(open) = state.open.as_mut() else {
        return Ok(Vec::new());
    };
    let present = Present::read(ctx, &probed.logins);
    if present != open.facts.present {
        open.facts.present = present;
        open.draft.learn(&open.facts);
        send(ctx, open, true);
    }
    read_logins_after(ctx, login_checks(ctx), LOGINS_EVERY, state.generation);
    Ok(Vec::new())
}

/// A key on the open sheet.
pub(crate) fn act(ctx: &mut Ctx, state: &mut State, action: SheetAction) -> Handled {
    let Some(open) = state.open.as_mut() else {
        return Ok(Vec::new());
    };
    match action {
        SheetAction::Change {
            row,
            column,
            forward,
        } => open.draft.change(&row, column, forward, &open.facts),
        SheetAction::Toggle { row } => open.draft.toggle(&row, &open.facts),
        SheetAction::Save { session } => {
            let open = state.open.take().expect("the sheet is open");
            state.generation += 1;
            return save_sheet(ctx, &open, session);
        }
        SheetAction::Close => {
            state.open = None;
            state.generation += 1;
            return Ok(Vec::new());
        }
    }
    send(ctx, open, true);
    Ok(Vec::new())
}

/// Sends each workflow command's status, when the menu has heard of none yet or one changed:
/// after a setting changes, a workflow file comes or goes, or the logins are read.
pub(crate) fn tell_the_menu(ctx: &Ctx, state: &mut State) {
    let statuses = statuses(ctx, &Present::read(ctx, &state.logins));
    if state.told.as_ref() == Some(&statuses) {
        return;
    }
    state.told = Some(statuses.clone());
    ctx.send(AppEvent::WorkflowStatuses { statuses });
}

/// Each workflow whose command reaches it, with its status: the built-ins, and each of yours
/// whose name isn't one of the app's own commands.
fn statuses(ctx: &Ctx, present: &Present) -> Vec<(String, WorkflowStatus)> {
    let provider = |worker: &ModelRef| ctx.services.registry.provider(worker);
    let library = workflows::library(ctx);
    workflow_list(ctx)
        .into_iter()
        .filter(|workflow| {
            [CAPTAIN, ORCHESTRATOR, COUNCIL].contains(&workflow.name.as_str())
                || argonauts_core::input::slash_spec(&workflow.name).is_none()
        })
        .map(|workflow| {
            let status = if ctx.settings().workflow == workflow.name {
                WorkflowStatus::InUse
            } else {
                let steps = library
                    .load(&workflow.name)
                    .ok()
                    .map(|loaded| roles_of(&step_roles(&loaded)));
                let settings = ctx.settings();
                match readiness(
                    settings,
                    present,
                    &provider,
                    &workflow.name,
                    steps.as_deref(),
                ) {
                    Readiness::Ready => WorkflowStatus::Ready,
                    Readiness::Needs(_) => WorkflowStatus::SetUpFirst,
                }
            };
            (workflow.name, status)
        })
        .collect()
}

fn decide(
    ctx: &mut Ctx,
    state: &mut State,
    name: &str,
    chosen: Option<SettingScope>,
    logins: &BTreeMap<String, LoginState>,
) -> Handled {
    let present = Present::read(ctx, logins);
    let provider = |worker: &ModelRef| ctx.services.registry.provider(worker);
    let steps = workflows::library(ctx)
        .load(name)
        .ok()
        .map(|loaded| step_roles(&loaded));
    let roles = steps.as_deref().map(roles_of);
    let labels: Option<Vec<(String, CrewRole)>> = steps.map(|steps| {
        steps
            .into_iter()
            .map(|(label, role, _)| (label, role))
            .collect()
    });
    match readiness(ctx.settings(), &present, &provider, name, roles.as_deref()) {
        Readiness::Ready if ctx.settings().workflow == name => {
            let facts = facts(ctx, present);
            let draft = Draft::new(ctx.settings(), name, &facts, labels.as_deref());
            let mut text = format!("{} is the workflow already", title(name));
            if let Some(who) = draft.who(&facts) {
                text.push_str(&format!(" · {who}"));
            }
            let reply = CommandReply {
                text,
                value: None,
                after: None,
                dim: false,
                error: false,
            };
            ctx.send(AppEvent::CommandReplied { reply });
            Ok(Vec::new())
        }
        Readiness::Ready => switch_now(ctx, name, chosen),
        // Nobody is at a terminal to fill a sheet in.
        Readiness::Needs(why) if !ctx.env.interactive => {
            ctx.notice(
                NoteLevel::Warn,
                format!("{} isn't ready: {why}", title(name)),
            );
            Ok(Vec::new())
        }
        Readiness::Needs(_) => {
            let facts = facts(ctx, present);
            let draft = Draft::new(ctx.settings(), name, &facts, labels.as_deref());
            let open = Open {
                draft,
                facts,
                chosen,
            };
            send(ctx, &open, false);
            state.open = Some(open);
            read_logins_after(ctx, login_checks(ctx), LOGINS_EVERY, state.generation);
            Ok(Vec::new())
        }
    }
}

/// The sheet as it stands, to the screen.
fn send(ctx: &Ctx, open: &Open, refresh: bool) {
    let saves_to = file_words(saves_to(ctx, &open.draft.workflow, open.chosen));
    let sheet = open.draft.sheet(&open.facts, saves_to, refresh);
    ctx.send(AppEvent::View {
        data: Box::new(ViewData::SetupSheet {
            sheet: Box::new(sheet),
        }),
    });
}

/// Enter: every setting the sheet changed and the workflow, in one write where changes save,
/// or this session's for ctrl+s. One line says so.
fn save_sheet(ctx: &mut Ctx, open: &Open, session: bool) -> Handled {
    if let Err(why) = open.draft.ready(&open.facts) {
        anyhow::bail!("{why}");
    }
    let pairs = open
        .draft
        .pairs()
        .into_iter()
        .map(|(key, value)| {
            let value = check_known(ctx, &key, &value)?;
            Ok((key, value))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let scope = if session {
        SettingScope::Session
    } else {
        saves_to(ctx, &open.draft.workflow, open.chosen)
    };
    write(ctx, pairs, scope, session || open.chosen.is_some())
}

/// Saves `workflow` where `chosen` says, else where `save_changes_to` says or where it takes
/// effect when a layer above that sets it, and says so.
fn switch_now(ctx: &mut Ctx, name: &str, chosen: Option<SettingScope>) -> Handled {
    let scope = saves_to(ctx, name, chosen);
    let pairs = vec![("workflow".to_owned(), quoted(name))];
    write(ctx, pairs, scope, chosen.is_some())
}

/// Writes `pairs` in one change, then says it with the workflow's line alone: the other keys
/// are the workflow's setup, which that line sums up. `asked` is a place named on purpose, by a
/// flag or ctrl+s.
fn write(ctx: &mut Ctx, pairs: Vec<(String, String)>, scope: SettingScope, asked: bool) -> Handled {
    let before = save::before(ctx, &["workflow".to_owned()]);
    let previous = save::value_of(ctx, "workflow");
    let main_before = ctx.settings().crew.main.clone();
    let value = pairs
        .iter()
        .find(|(key, _)| key == "workflow")
        .map(|(_, value)| value.clone())
        .unwrap_or_default();
    let change = Change::Set(pairs);
    save::make(ctx, &change, scope, TrustAfter::Carry)?;
    // What moved it from where your changes go, or still wins over it there, as every change's
    // line says, naming the workflow as what that file sets; else a workflow only this
    // repository has, which another one's file can't name.
    let why = save::why_saved(ctx, &before, "workflow", scope)
        .map(|why| why.replacen(" sets it", " sets the workflow", 1))
        .or_else(|| (!asked && only_here(ctx, &value, scope)).then(|| ONLY_HERE.to_owned()));
    ctx.send(AppEvent::SettingChanged {
        key: "workflow".to_owned(),
        value,
        scope,
        previous,
        reset: false,
        why,
        kept: false,
    });
    let mut effects = Vec::new();
    if ctx.settings().crew.main != main_before {
        effects.push(Effect::FreshConversation);
    }
    effects.push(Effect::SettingsApplied {
        keys: change.keys(),
    });
    Ok(effects)
}

/// What the sheet knows of the agents, read now.
fn facts(ctx: &Ctx, present: Present) -> Facts {
    let names = ctx.names.names();
    let models: Vec<Model> = model_choices(ctx, None)
        .into_iter()
        .filter(|choice| !choice.older)
        .map(|choice| {
            let worker = choice.worker;
            let name = match &worker.model {
                Some(model) => names
                    .model(&worker.agent, Some(model))
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| {
                        if choice.name.is_empty() {
                            display_name_from_id(model)
                        } else {
                            choice.name.clone()
                        }
                    }),
                None => names.cli(&worker.agent),
            };
            Model {
                name,
                provider: choice.provider,
                worker,
            }
        })
        .collect();
    let out_of_plan = ctx
        .settings()
        .crew
        .members()
        .into_iter()
        .filter_map(|worker| {
            let reading = ctx.services.plan_usage.get(&worker.agent_id())?;
            let spent = matches!(
                reading.status,
                argonauts_core::limits::PlanUsageStatus::Rejected
            ) || reading.used.is_some_and(|used| used >= 1.0);
            let clock = argonauts_tui::format::WallClock::local();
            spent.then(|| (worker, reading.resets_at.map(|at| reset_time(&clock, at))))
        })
        .collect();
    let agents = models
        .iter()
        .map(|model| model.worker.agent.clone())
        .chain(
            ctx.settings()
                .crew
                .members()
                .into_iter()
                .map(|worker| worker.agent),
        )
        .chain(std::iter::once(ctx.settings().crew.main.agent.clone()));
    let mut runs = BTreeMap::new();
    for agent in agents {
        if runs.contains_key(&agent) {
            continue;
        }
        let running = names.running(&ModelRef::new(agent.clone(), None));
        if let Some(name) = running
            .model
            .as_deref()
            .and_then(|id| names.model(&running.agent, Some(id)))
        {
            runs.insert(agent, name);
        }
    }
    let workflows = workflow_list(ctx)
        .into_iter()
        .map(|workflow| workflow.name)
        .filter(|name| ![CAPTAIN, ORCHESTRATOR, COUNCIL].contains(&name.as_str()))
        .filter(|name| !name.starts_with("ablate-"))
        .collect();
    Facts {
        present,
        models,
        out_of_plan,
        runs,
        workflows,
        no_nesting: ctx.settings().workflows.max_depth == 0,
    }
}

/// When a plan's window resets, in `clock`'s zone and as `ui.format.clock` writes times:
/// `3:40 PM`.
fn reset_time(clock: &argonauts_tui::format::WallClock, at: time::OffsetDateTime) -> String {
    clock.clock(at).to_string()
}

/// Where a scope saves, as the line under a sheet's title names it.
fn file_words(scope: SettingScope) -> &'static str {
    match scope {
        SettingScope::Global => "~/.argonauts/config.toml",
        SettingScope::Project => ".argonauts/config.toml",
        SettingScope::ProjectLocal => ".argonauts/config.local.toml",
        SettingScope::Session => "this session",
    }
}

/// Reads the logins after `delay` on a task of their own, and sends them back for
/// `generation`.
fn read_logins_after(
    ctx: &Ctx,
    checks: Vec<(String, PathBuf, Vec<String>)>,
    delay: Duration,
    generation: u64,
) {
    if checks.is_empty() {
        return;
    }
    let internal = ctx.internal.clone();
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        let logins = read_logins(checks).await;
        let _ = internal.send(Internal::WorkflowSwitch(Probed { logins, generation }));
    });
}

/// Where switching to `name` saves: the place asked for, when one was.
fn saves_to(ctx: &Ctx, name: &str, chosen: Option<SettingScope>) -> SettingScope {
    chosen.unwrap_or_else(|| saves_switch_to(ctx, name))
}

/// Where switching to `name` saves when no place was asked for: where changes save, except that
/// a workflow only this repository has goes to this project, just you, since another
/// repository's settings couldn't name it; and where a layer above would hide it, where it
/// takes effect.
fn saves_switch_to(ctx: &Ctx, name: &str) -> SettingScope {
    let mut wanted = super::settings::save::save_changes_to(ctx).scope();
    if only_this_repositorys(ctx, name) && wanted == SettingScope::Global {
        wanted = SettingScope::ProjectLocal;
    }
    where_it_takes_effect(wanted, &ctx.settings.layer("workflow"))
}

/// Why a workflow only this repository has saves for this project, just you.
const ONLY_HERE: &str = "since only this project has it";

/// Whether `name` is a workflow only this repository has.
fn only_this_repositorys(ctx: &Ctx, name: &str) -> bool {
    workflows::library(ctx)
        .list()
        .into_iter()
        .any(|listed| listed.name == name && listed.origin == argonauts_runner::Origin::Project)
}

/// A switch to the TOML text `value` went to this project, just you, where your changes go
/// everywhere, because only this repository has that workflow.
fn only_here(ctx: &Ctx, value: &str, scope: SettingScope) -> bool {
    let name = value.trim().trim_matches(['"', '\'']);
    scope == SettingScope::ProjectLocal
        && super::settings::save::save_changes_to(ctx).scope() == SettingScope::Global
        && only_this_repositorys(ctx, name)
}

/// `wanted`, unless the layer `workflow` comes from wins over it: then that layer, and just
/// you in place of the team's file, so the change takes effect without changing what the team
/// shares.
fn where_it_takes_effect(wanted: SettingScope, layer: &str) -> SettingScope {
    if !wanted.hidden_by(layer) {
        return wanted;
    }
    match SettingScope::from_layer(layer) {
        Some(SettingScope::Project) | Some(SettingScope::ProjectLocal) => {
            SettingScope::ProjectLocal
        }
        _ => SettingScope::Session,
    }
}

fn quoted(text: &str) -> String {
    toml::Value::String(text.to_owned()).to_string()
}

/// `Council` for `council`, `Spec-first` for `spec-first`.
pub(crate) fn title(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Each step's role and the model it names for itself.
fn roles_of(steps: &[(String, CrewRole, Option<ModelRef>)]) -> Vec<(CrewRole, Option<ModelRef>)> {
    steps
        .iter()
        .map(|(_, role, model)| (*role, model.clone()))
        .collect()
}

/// A workflow file's steps: each one's name, its role, and the model it names for itself.
fn step_roles(loaded: &argonauts_runner::Loaded) -> Vec<(String, CrewRole, Option<ModelRef>)> {
    match loaded {
        argonauts_runner::Loaded::Steps(file) => file
            .steps
            .iter()
            .map(|step| (step.name.clone(), step.role, step.model.clone()))
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether a workflow can run now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Readiness {
    Ready,
    /// What it needs, as a sentence's end: "it needs two seats that can answer, and only
    /// Claude Code can now".
    Needs(String),
}

/// Ready means each role a step names has a ready agent: Captain the main agent, and a
/// reviewer from another provider when Captain reviews; Orchestrator someone to plan and two
/// reviewers; Council two seats that can answer, not all of them skeptics; any other workflow
/// each role its steps name.
pub(crate) fn readiness(
    settings: &Settings,
    present: &Present,
    provider: &dyn Fn(&ModelRef) -> String,
    name: &str,
    steps: Option<&[(CrewRole, Option<ModelRef>)]>,
) -> Readiness {
    let ready = |role: RoleName| -> Vec<ModelRef> {
        candidates(settings, role)
            .into_iter()
            .filter(|worker| present.ready(&worker.agent))
            .collect()
    };
    match name {
        CAPTAIN => {
            let main = &settings.crew.main;
            if !present.ready(&main.agent) {
                return Readiness::Needs(format!(
                    "it needs the main agent, and {}",
                    present.why_not(&main.agent)
                ));
            }
            let theirs = provider(main);
            let reviews = ready(RoleName::Reviewer)
                .iter()
                .any(|reviewer| provider(reviewer) != theirs);
            if settings.captain.review && !reviews {
                return Readiness::Needs(
                    "it reviews each change, and no reviewer from another provider is ready"
                        .to_owned(),
                );
            }
            Readiness::Ready
        }
        ORCHESTRATOR => {
            if ready(RoleName::Orchestrator).is_empty() {
                return Readiness::Needs("nobody ready is picked to plan".to_owned());
            }
            let reviewers = if settings.crew.orchestrator_picks.reviewer {
                present.ready_agents()
            } else {
                distinct_agents(&ready(RoleName::Reviewer))
            };
            if reviewers.len() < 2 {
                return Readiness::Needs(format!(
                    "it needs two reviewers for each task, and {} ready",
                    count_words(reviewers.len())
                ));
            }
            Readiness::Ready
        }
        COUNCIL => {
            let seats = seats(settings);
            let sitting: Vec<_> = seats
                .iter()
                .filter(|(worker, _)| present.ready(&worker.agent))
                .collect();
            let all_skeptics = sitting
                .iter()
                .all(|(_, role)| *role == argonauts_core::events::council::SeatRole::Skeptic);
            if sitting.len() < 2 {
                let who = match sitting.first() {
                    Some((worker, _)) => format!("only {} can now", present.name(&worker.agent)),
                    None => "none can now".to_owned(),
                };
                return Readiness::Needs(format!("it needs two seats that can answer, and {who}"));
            }
            if all_skeptics {
                return Readiness::Needs("every seat is a skeptic, so nobody proposes".to_owned());
            }
            Readiness::Ready
        }
        _ => {
            for (role, model) in steps.unwrap_or_default() {
                let fine = role.fine_role();
                let filled = match model {
                    Some(model) => present.ready(&model.agent),
                    None => !ready(fine).is_empty(),
                };
                if !filled {
                    return Readiness::Needs(format!(
                        "nobody ready fills its {} step's role",
                        fine.as_str()
                    ));
                }
            }
            Readiness::Ready
        }
    }
}

/// Council's seats in order, each a model and its role: `[[council.seat]]`, else each of the
/// crew's workers once as a proposer.
pub(crate) fn seats(
    settings: &Settings,
) -> Vec<(ModelRef, argonauts_core::events::council::SeatRole)> {
    if !settings.council.seats.is_empty() {
        return settings
            .council
            .seats
            .iter()
            .map(|seat| (seat.model.clone(), seat.role))
            .collect();
    }
    let mut workers: Vec<ModelRef> = Vec::new();
    for worker in candidates(settings, RoleName::Worker) {
        if !workers.contains(&worker) {
            workers.push(worker);
        }
    }
    workers
        .into_iter()
        .map(|worker| (worker, Default::default()))
        .collect()
}

fn distinct_agents(workers: &[ModelRef]) -> Vec<String> {
    let mut agents: Vec<String> = Vec::new();
    for worker in workers {
        if !agents.contains(&worker.agent) {
            agents.push(worker.agent.clone());
        }
    }
    agents
}

fn count_words(count: usize) -> &'static str {
    match count {
        0 => "none is",
        _ => "only one is",
    }
}

/// Each CLI as the switch sees it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cli {
    pub id: String,
    pub name: String,
    pub now: Now,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Now {
    /// Installed, on, and not known to be logged out.
    Ready,
    /// Its login check says so; `login` is what logs it in.
    LoggedOut { login: Option<String> },
    /// `add` is what adds it, such as `/agents add agy`.
    NotInstalled { add: String },
    /// `agents.<id>.enabled = false`.
    Off,
}

/// The CLIs Doctor found, with their logins as just read. `None` until Doctor has looked, when
/// nothing counts against any agent that's on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Present {
    clis: Option<Vec<Cli>>,
    /// `agents.<id>.enabled`, for agents Doctor doesn't list, such as endpoints.
    off: Vec<String>,
}

impl Present {
    fn read(ctx: &Ctx, logins: &BTreeMap<String, LoginState>) -> Self {
        let settings = ctx.settings();
        let off = settings
            .agents
            .iter()
            .filter(|(_, agent)| !agent.enabled)
            .map(|(id, _)| id.clone())
            .collect();
        let clis = ctx
            .cli_facts
            .report()
            .map(|report| clis(&report, logins, &|id| login_hint(ctx, id)));
        Self { clis, off }
    }

    #[cfg(test)]
    pub(crate) fn of(clis: Vec<Cli>) -> Self {
        Self {
            clis: Some(clis),
            off: Vec::new(),
        }
    }

    pub(crate) fn cli(&self, agent: &str) -> Option<&Cli> {
        self.clis.as_ref()?.iter().find(|cli| cli.id == agent)
    }

    /// Whether `agent` can take a turn now, as far as anything says.
    pub(crate) fn ready(&self, agent: &str) -> bool {
        match self.cli(agent) {
            Some(cli) => cli.now == Now::Ready,
            None => !self.off.iter().any(|off| off == agent),
        }
    }

    /// Every CLI that can take a turn now, in Doctor's order.
    pub(crate) fn ready_agents(&self) -> Vec<String> {
        self.clis
            .iter()
            .flatten()
            .filter(|cli| cli.now == Now::Ready)
            .map(|cli| cli.id.clone())
            .collect()
    }

    pub(crate) fn name(&self, agent: &str) -> String {
        self.cli(agent)
            .map_or_else(|| agent.to_owned(), |cli| cli.name.clone())
    }

    /// `Codex is logged out`, for an agent that isn't ready.
    fn why_not(&self, agent: &str) -> String {
        let name = self.name(agent);
        match self.cli(agent).map(|cli| &cli.now) {
            Some(Now::LoggedOut { .. }) => format!("{name} is logged out"),
            Some(Now::NotInstalled { .. }) => format!("{name} isn't installed"),
            _ => format!("{name} is off"),
        }
    }
}

fn clis(
    report: &DoctorReport,
    logins: &BTreeMap<String, LoginState>,
    login_hint: &dyn Fn(&str) -> Option<String>,
) -> Vec<Cli> {
    report
        .agents
        .iter()
        .map(|check| {
            let now = if !check.enabled {
                Now::Off
            } else if check.status == AgentStatus::NotInstalled {
                Now::NotInstalled {
                    add: format!("/agents add {}", check.binary),
                }
            } else if logins.get(&check.id) == Some(&LoginState::LoggedOut) {
                Now::LoggedOut {
                    login: login_hint(&check.id),
                }
            } else {
                Now::Ready
            };
            Cli {
                id: check.id.clone(),
                name: check.display_name.clone(),
                now,
            }
        })
        .collect()
}

fn login_hint(ctx: &Ctx, id: &str) -> Option<String> {
    let adapters = crate::commands::adapters_here(Some(&ctx.env), ctx.settings.resolved()).ok()?;
    let adapter = adapters.iter().find(|adapter| adapter.id() == id)?;
    adapter.setup_hints().login
}

/// Every installed CLI that's on and whose adapter has a verified login check: its id, where it
/// is, and the check's arguments.
fn login_checks(ctx: &Ctx) -> Vec<(String, PathBuf, Vec<String>)> {
    let Some(report) = ctx.cli_facts.report() else {
        return Vec::new();
    };
    let Ok(adapters) = crate::commands::adapters_here(Some(&ctx.env), ctx.settings.resolved())
    else {
        return Vec::new();
    };
    report
        .agents
        .iter()
        .filter(|check| check.usable())
        .filter_map(|check| {
            let path = check.path.clone()?;
            let adapter = adapters.iter().find(|adapter| adapter.id() == check.id)?;
            let args = adapter.setup_hints().login_check?;
            Some((check.id.clone(), path, args))
        })
        .collect()
}

async fn read_logins(checks: Vec<(String, PathBuf, Vec<String>)>) -> BTreeMap<String, LoginState> {
    let asks = checks
        .into_iter()
        .map(|(id, path, args)| async move { (id, probe::login_state(&path, &args).await) });
    futures::future::join_all(asks).await.into_iter().collect()
}

#[cfg(test)]
#[path = "workflow_switch_tests.rs"]
mod tests;
