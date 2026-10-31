//! A CLI that must be updated is asked about before each run that would use it: one older
//! than the oldest version supported, or a version known not to work. The run waits on the
//! answer, which updates the CLI, leaves it out of this run, or runs with it anyway. On
//! autopilot, and where nobody is at the screen, one line says so and the run goes ahead.
//! Nothing here speaks once a run is under way, and nothing updates a CLI unasked or while a
//! run is going, so no run switches versions halfway.

use std::collections::BTreeSet;
use std::sync::Arc;

use argonauts_conductor::AgentCheck;
use argonauts_conductor::doctor::update::{CliUpdate, UPDATE_TIMEOUT, UpdateOutcome, update_cli};
use argonauts_conductor::doctor::{VERSION_TIMEOUT, check_agent_at};
use argonauts_core::Adapter;
use argonauts_core::app::status_tab::{CliVersionCheck, VersionStanding};
use argonauts_core::app::{AppEvent, ChoiceKind};
use argonauts_core::base::log::NoteLevel;
use argonauts_core::base::names::{ChoiceId, RoleName};
use argonauts_core::runs::api::StartRun;
use argonauts_core::runs::events::approval::{AnswerAction, AnswerOption};
use argonauts_core::runs::events::run::RunMode;
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_crew::roles::candidates;

use super::base::ctx::Internal;
use super::{Ctx, Effect};
use crate::setup::choices;

/// The `Choose` values of the question's answers.
const UPDATE: &str = "update";
const LEAVE_OUT: &str = "leave_out";
const RUN_ANYWAY: &str = "run";

#[derive(Default)]
pub(crate) struct NeededUpdateState {
    /// The question on screen, and the run that waits on its answer.
    asked: Option<(ChoiceId, Waiting)>,
    /// The run that waits while its CLI updates.
    updating: Option<Waiting>,
    /// The CLIs already answered for the run about to start, which aren't asked about twice.
    answered: BTreeSet<String>,
}

/// A run that hasn't started, and the CLI it waits on.
struct Waiting {
    request: StartRun,
    cli: Outdated,
}

/// An installed CLI whose version needs an update before runs can rely on it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Outdated {
    id: String,
    /// Such as `Codex`.
    name: String,
    version: String,
    check: CliVersionCheck,
    /// Another CLI can take its roles, since the run's main agent is on another one.
    can_leave_out: bool,
    /// A run is going now, which an update would switch versions under.
    run_going: bool,
}

/// What a run about to start does next.
pub(crate) enum Start {
    Now(Box<StartRun>),
    /// It waits on the question now on screen.
    Waits,
    /// It can't start, and why.
    Refused(String),
}

/// The update a question's answer asked for has ended.
#[derive(Debug)]
pub(crate) struct Updated {
    update: CliUpdate,
    /// The CLI as it is now.
    check: AgentCheck,
}

pub(crate) fn owns_choice(state: &NeededUpdateState, id: &ChoiceId) -> bool {
    state.asked.as_ref().is_some_and(|(asked, _)| asked == id)
}

/// Holds `request` while a CLI it would use needs an update and you haven't said what to do
/// about it for this run. Autopilot is never asked: it is told in one line and the run goes on
/// with the CLI as it is, since only you may have a CLI updated.
pub(crate) async fn before_run(
    ctx: &mut Ctx,
    state: &mut NeededUpdateState,
    request: StartRun,
    mode: ApprovalMode,
) -> Start {
    let waited_on = state
        .asked
        .as_ref()
        .map(|(_, waiting)| waiting)
        .or(state.updating.as_ref());
    if let Some(waiting) = waited_on {
        return Start::Refused(format!(
            "another run waits on the question about {}'s update, so answer that first",
            waiting.cli.name
        ));
    }
    let unanswered = |ctx: &Ctx, state: &NeededUpdateState| -> Vec<Outdated> {
        let mut found = outdated(ctx, &request);
        found.retain(|cli| !state.answered.contains(&cli.id));
        found
    };
    // What Doctor found as the app started may be old news, such as a CLI you updated in
    // another terminal since. Each is read again before anything is said of it.
    let stale = unanswered(ctx, state);
    read_again(ctx, &stale).await;
    let mut left = unanswered(ctx, state).into_iter().peekable();
    if left.peek().is_none() {
        state.answered.clear();
        return Start::Now(Box::new(request));
    }
    if mode == ApprovalMode::Autopilot || !ctx.env.interactive {
        for cli in left {
            ctx.notice(NoteLevel::Warn, cli.unasked_line());
        }
        state.answered.clear();
        return Start::Now(Box::new(request));
    }
    if let Some(cli) = left.next() {
        ask(ctx, state, Waiting { request, cli });
    }
    Start::Waits
}

pub(crate) fn answer_choice(
    ctx: &mut Ctx,
    state: &mut NeededUpdateState,
    id: ChoiceId,
    action: AnswerAction,
) -> anyhow::Result<Vec<Effect>> {
    let Some((_, mut waiting)) = state.asked.take_if(|(asked, _)| *asked == id) else {
        ctx.notice(NoteLevel::Info, "That question is already closed.");
        return Ok(Vec::new());
    };
    let picked = match &action {
        AnswerAction::Choose { value } => value.as_str(),
        _ => "",
    };
    let cli = &waiting.cli;
    let outcome = match picked {
        UPDATE => return update(ctx, state, id, waiting),
        LEAVE_OUT if cli.can_leave_out => {
            // Off at the request's own layer, so it is off for this run and no longer.
            let off = format!("agents.{}.enabled=false", cli.id);
            waiting.request.request_settings.push(off);
            format!("Leaving {} out of this run.", waiting.cli.name)
        }
        RUN_ANYWAY => format!("Running with {} {}.", cli.name, cli.version),
        _ => {
            let name = cli.name.clone();
            state.asked = Some((id, waiting));
            anyhow::bail!("{action:?} doesn't answer what to do about {name}'s update");
        }
    };
    close(ctx, id, outcome);
    state.answered.insert(waiting.cli.id);
    Ok(vec![Effect::StartRun(Box::new(waiting.request))])
}

/// Esc: the run that waited doesn't start. Returns why, for whoever asked for the run.
pub(crate) fn set_aside(
    ctx: &mut Ctx,
    state: &mut NeededUpdateState,
    id: ChoiceId,
) -> Option<String> {
    let (_, waiting) = state.asked.take_if(|(asked, _)| *asked == id)?;
    state.answered.clear();
    let cli = &waiting.cli;
    close(
        ctx,
        id,
        format!(
            "Not started · {} {} still needs an update",
            cli.name, cli.version
        ),
    );
    Some(format!(
        "{} {} needs an update, and the question about it was closed unanswered",
        cli.name, cli.version
    ))
}

/// The update ended. A CLI that no longer needs one lets its run start; one that still does is
/// asked about again, with what went wrong said first.
pub(crate) fn updated(ctx: &mut Ctx, state: &mut NeededUpdateState, done: Updated) -> Vec<Effect> {
    let Some(waiting) = state.updating.take() else {
        return Vec::new();
    };
    let now = done.check.version.clone();
    ctx.cli_facts.rechecked(done.check);
    let still = outdated_cli(ctx, &waiting);
    let Some(cli) = still else {
        let name = &waiting.cli.name;
        let line = match now {
            Some(version) => format!("{name} is now {version}."),
            None => format!("{name} was updated."),
        };
        ctx.notice(NoteLevel::Info, line);
        return vec![Effect::StartRun(Box::new(waiting.request))];
    };
    ctx.notice(NoteLevel::Warn, cli.not_updated_line(&done.update));
    let request = waiting.request;
    ask(ctx, state, Waiting { request, cli });
    Vec::new()
}

/// Runs the update off the loop, since an updater downloads, then reads the CLI again. A run
/// that started since the question was asked puts the update off, and the question comes back
/// without it.
fn update(
    ctx: &mut Ctx,
    state: &mut NeededUpdateState,
    id: ChoiceId,
    waiting: Waiting,
) -> anyhow::Result<Vec<Effect>> {
    let cli = &waiting.cli;
    let (Some(command), Some(adapter)) = (&cli.check.update_command, adapter(ctx, &cli.id)) else {
        let name = cli.name.clone();
        state.asked = Some((id, waiting));
        anyhow::bail!("Argonauts Code knows no way to update {name}");
    };
    if super::clis::cli_updates::a_run_is_going(ctx) {
        close(ctx, id, format!("{} wasn't updated.", cli.name));
        let cli = outdated_cli(ctx, &waiting).unwrap_or(waiting.cli);
        let request = waiting.request;
        ask(ctx, state, Waiting { request, cli });
        return Ok(Vec::new());
    }
    close(ctx, id, format!("Updating {} · {command}", cli.name));
    let settings = ctx.settings.shared();
    let internal = ctx.internal.clone();
    tokio::spawn(async move {
        let update = update_cli(adapter.as_ref(), &settings, UPDATE_TIMEOUT).await;
        let binary = settings
            .agents
            .get(adapter.id())
            .and_then(|agent| agent.binary.as_deref());
        let check = check_agent_at(adapter.as_ref(), binary, VERSION_TIMEOUT).await;
        let _ = internal.send(Internal::CliUpdated(Box::new(Updated { update, check })));
    });
    state.updating = Some(waiting);
    Ok(Vec::new())
}

/// Reads each of `clis` now, in place of what was last found of it.
async fn read_again(ctx: &Ctx, clis: &[Outdated]) {
    for cli in clis {
        let Some(adapter) = adapter(ctx, &cli.id) else {
            continue;
        };
        let settings = ctx.settings();
        let binary = settings
            .agents
            .get(adapter.id())
            .and_then(|agent| agent.binary.as_deref());
        let check = check_agent_at(adapter.as_ref(), binary, VERSION_TIMEOUT).await;
        ctx.cli_facts.rechecked(check);
    }
}

fn ask(ctx: &mut Ctx, state: &mut NeededUpdateState, waiting: Waiting) {
    let id = ctx.next_choice_id();
    let cli = &waiting.cli;
    ctx.send(AppEvent::Choice {
        choice: choices::app_choice(
            id.clone(),
            ChoiceKind::NeededUpdate,
            cli.title(),
            cli.body(),
            cli.options(),
        ),
    });
    state.asked = Some((id, waiting));
}

/// What the question says under its title.
const WHAT_IT_MEANS: &str = "Runs with it may misread what it does. Nothing has started yet.";

fn close(ctx: &Ctx, id: ChoiceId, outcome: String) {
    ctx.send(AppEvent::ChoiceClosed { id, outcome });
}

fn adapter(ctx: &Ctx, id: &str) -> Option<Arc<dyn Adapter>> {
    crate::commands::adapters_here(Some(&ctx.env), ctx.settings.resolved())
        .ok()?
        .into_iter()
        .find(|adapter| adapter.id() == id)
}

/// The CLIs `request`'s run would use whose version needs an update, in Doctor's order. None
/// until Doctor has checked them.
fn outdated(ctx: &Ctx, request: &StartRun) -> Vec<Outdated> {
    let Some(report) = ctx.cli_facts.report() else {
        return Vec::new();
    };
    let checks = super::panels::status_tab::version_checks(ctx, &report);
    let settings = run_settings(ctx.settings(), request);
    let used = clis_used(&settings, request.mode);
    let run_going = super::clis::cli_updates::a_run_is_going(ctx);
    report
        .agents
        .iter()
        .filter(|agent| agent.usable() && used.contains(&agent.id))
        .filter_map(|agent| {
            let check = checks.get(&agent.id)?;
            Some(Outdated {
                id: agent.id.clone(),
                name: agent.display_name.clone(),
                version: agent.version.clone()?,
                check: check.standing.needs_update().then(|| check.clone())?,
                can_leave_out: agent.id != settings.crew.main.agent,
                run_going,
            })
        })
        .collect()
}

/// The CLI a run waits on as it is now, if it still needs an update.
fn outdated_cli(ctx: &Ctx, waiting: &Waiting) -> Option<Outdated> {
    let all = outdated(ctx, &waiting.request);
    all.into_iter().find(|cli| cli.id == waiting.cli.id)
}

/// The settings `request`'s run starts with: what the request itself set, and its main agent,
/// over the settings in effect.
fn run_settings(settings: &Settings, request: &StartRun) -> Settings {
    let mut settings = super::crew_lines::with_request(settings, &request.request_settings)
        .unwrap_or_else(|| settings.clone());
    if let Some(main) = &request.main_agent {
        settings.crew.main = main.clone();
    }
    settings
}

/// The CLIs whose models may fill a role in a run of `mode`, less any the run has off. Captain
/// works alone and is reviewed; a crew run may call on every role.
fn clis_used(settings: &Settings, mode: RunMode) -> BTreeSet<String> {
    const CAPTAIN: [RoleName; 3] = [RoleName::Main, RoleName::Reviewer, RoleName::Judge];
    let roles: &[RoleName] = match mode {
        RunMode::Captain => &CAPTAIN,
        RunMode::Crew => &RoleName::ALL,
    };
    roles
        .iter()
        .flat_map(|role| candidates(settings, *role))
        .map(|model| model.agent)
        .filter(|id| settings.agents.get(id).is_none_or(|agent| agent.enabled))
        .collect()
}

impl Outdated {
    /// `Codex 0.158.0 is older than Argonauts Code supports (0.160.0)`.
    fn title(&self) -> String {
        let Self { name, version, .. } = self;
        match (self.check.standing, &self.check.oldest_supported) {
            (VersionStanding::Broken, _) => {
                format!("{name} {version} is known not to work with Argonauts Code")
            }
            (_, Some(oldest)) => {
                format!("{name} {version} is older than Argonauts Code supports ({oldest})")
            }
            (_, None) => format!("{name} {version} is older than Argonauts Code supports"),
        }
    }

    /// What the question says under its title, with why it can't be updated now where a run
    /// is in the way.
    fn body(&self) -> Vec<String> {
        let mut body = vec![WHAT_IT_MEANS.to_owned()];
        if let Some(command) = self
            .check
            .update_command
            .as_ref()
            .filter(|_| self.run_going)
        {
            body.push(format!(
                "A run is going now, so it can't be updated until that ends: {command}"
            ));
        }
        body
    }

    /// Update it, where a command for that is known and no run is going; leave it out, where
    /// another CLI can take its roles; or run with it. What each does is said beside it.
    fn options(&self) -> Vec<AnswerOption> {
        let answer = |key, label: &str, value, detail: String| AnswerOption {
            detail: Some(detail),
            ..choices::option(key, label, choices::choose(value), false)
        };
        let mut options = Vec::new();
        if let Some(command) = self
            .check
            .update_command
            .as_ref()
            .filter(|_| !self.run_going)
        {
            let then = format!("{command}, then the run starts");
            options.push(answer('u', "Update it now", UPDATE, then));
        }
        if self.can_leave_out {
            options.push(answer(
                'l',
                &format!("Leave {} out of this run", self.name),
                LEAVE_OUT,
                "its roles go to the next model in your fallback order".to_owned(),
            ));
        }
        options.push(answer(
            'r',
            "Run with it anyway",
            RUN_ANYWAY,
            "and ask again before the next run".to_owned(),
        ));
        options
    }

    /// What is said in place of the question where nobody is asked.
    fn unasked_line(&self) -> String {
        let mut line = format!("{} · running with it anyway", self.title());
        if let Some(command) = &self.check.update_command {
            line.push_str(&format!(" · {command} updates it"));
        }
        line
    }

    /// Why it still needs an update after one was tried.
    fn not_updated_line(&self, update: &CliUpdate) -> String {
        let Self { name, version, .. } = self;
        match &update.outcome {
            UpdateOutcome::Failed { why, .. } => format!("{name} couldn't be updated: {why}"),
            _ => match &self.check.update_command {
                Some(command) => format!("{name} is still {version} after {command}."),
                None => format!("{name} is still {version}."),
            },
        }
    }
}

#[cfg(test)]
mod tests;
