//! Every setting changed from inside the app: the settings commands' pickers and arguments,
//! the scope question, the safety confirm, trust carried forward on a write, resets, the
//! toggles, and the detected-checks offer.
//!
//! A write goes through up to three questions, each a pending choice here: where to save it
//! (`SettingScope`), whether to change a safety key (`ConfirmSafetySetting`), and whether to
//! trust the project file it lands in (`TrustProject`). Then [`save`] makes it and announces it.
//!
//! At start, a project file whose safety keys you haven't trusted gets the `TrustProject`
//! question on its own, so they aren't ignored without your knowing.

mod args;
mod checks;
mod confirm;
mod labels;
pub(crate) mod memory_facts;
mod models;
mod pickers;
mod refresh;
pub(crate) mod renamed;
mod rows;
mod save;
mod scope;
mod toggles;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_live;
#[cfg(test)]
mod tests_models;
#[cfg(test)]
mod tests_opencode;

pub(crate) use args::{check_known, workflow_list};
pub(crate) use models::choices as model_choices;
pub(crate) use rows::model_keys;
pub(crate) use rows::rows as setting_rows;
pub(crate) use save::set_where_it_came_from;

use std::collections::HashMap;
use std::path::PathBuf;

use argonauts_core::app::{AppEvent, ChoiceKind, ViewData, ViewRequest};
use argonauts_core::events::approval::AnswerAction;
use argonauts_core::input::SlashCommand;
use argonauts_core::names::{ChoiceId, ModelRef, RoleName};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::ui::NarratorMode;
use argonauts_core::settings::{SettingScope, asks_where};
use argonauts_core::{KeyScheme, NoteLevel, Verbosity};
use argonauts_registry::{
    SettingsError, SettingsWarning, WarningAbout, safety_keys_in, setting_meta,
};

use super::{Ctx, Effect, ScopeRequest};
use crate::choices::{self, NOT_CHANGED, OnSetAside};
use crate::commands::refuse_safety_change;
use crate::trust::{cant_trust, hash_of};
use save::{Change, TrustAfter};

const ORCHESTRATOR: &str = "orchestrator";

pub(crate) struct SettingsState {
    /// The questions asked here that haven't been answered.
    pending: HashMap<ChoiceId, Pending>,
    /// `workflow = "orchestrator"` waiting for its orchestrator, with the scope picked for it.
    hold: Option<SettingScope>,
    /// The checks offer was answered in this app session, so it isn't made again even when the
    /// answer left `checks.run` ignored in a file you didn't trust.
    checks_answered: bool,
    /// The checks found here were named on autopilot in this app session, where they aren't
    /// offered, so the line is said once.
    checks_named: bool,
    /// The mode on screen is autopilot, as the controller last told it, so nothing here asks.
    autopilot: bool,
    /// A model list refresh is running.
    refreshing: bool,
    /// The model list last sent, for the refresh to bring up to date.
    shown: Option<refresh::ModelsShown>,
    /// Each source asked again one by one, and the watch while `/model` is open.
    sources: refresh::Asking,
}

impl SettingsState {
    pub(crate) fn new(ctx: &Ctx) -> Self {
        Self {
            pending: HashMap::new(),
            hold: None,
            checks_answered: false,
            checks_named: false,
            // Until Shift+Tab or a run says otherwise, the mode on screen is the settings'.
            autopilot: ctx.settings().approvals.mode() == ApprovalMode::Autopilot,
            refreshing: false,
            shown: None,
            sources: refresh::Asking::default(),
        }
    }
}

enum Pending {
    Scope {
        change: Change,
    },
    Confirm {
        change: Change,
        scope: SettingScope,
    },
    Trust {
        change: Change,
        scope: SettingScope,
    },
    /// Asked at start. `hash` is the file as you were asked about it.
    TrustAtStart {
        file: PathBuf,
        scope: SettingScope,
        hash: String,
    },
    Checks {
        commands: Vec<String>,
    },
}

impl Pending {
    fn kind(&self) -> ChoiceKind {
        match self {
            Self::Scope { .. } => ChoiceKind::SettingScope,
            Self::Confirm { .. } => ChoiceKind::ConfirmSafetySetting,
            Self::Trust { .. } | Self::TrustAtStart { .. } => ChoiceKind::TrustProject,
            Self::Checks { .. } => ChoiceKind::DetectedChecks,
        }
    }
}

type Handled = anyhow::Result<Vec<Effect>>;

pub(crate) fn owns_choice(settings: &SettingsState, id: &ChoiceId) -> bool {
    settings.pending.contains_key(id)
}

/// Something other than a settings command arrived, so a workflow held for its orchestrator is
/// dropped and a later orchestrator pick is a plain `/model orchestrator`.
pub(crate) fn interrupt(settings: &mut SettingsState) {
    settings.hold = None;
}

/// `/model`, `/effort`, `/agents`, `/workflow <name>`, `/workflows`, `/approvals`, `/limit`,
/// `/fallback`, `/permissions`, `/verbosity`, `/narrator`, `/theme`, and `/config`: a picker with
/// no argument, the scope question with one.
pub(crate) async fn command(
    ctx: &mut Ctx,
    settings: &mut SettingsState,
    command: SlashCommand,
) -> Handled {
    if let SlashCommand::Model {
        role: Some(RoleName::Orchestrator),
        worker: Some(worker),
        ..
    } = &command
        && let Some(scope) = settings.hold.take()
    {
        return orchestrator_picked(ctx, settings, scope, worker);
    }
    settings.hold = None;
    if let SlashCommand::Config {
        key: Some(key),
        reset: true,
        ..
    } = &command
    {
        return reset_in_effect(ctx, settings, key);
    }
    // A `/config key=value` that can't be saved says why under the command, where a saved one
    // says where it went.
    let typed = match &command {
        SlashCommand::Config {
            key: Some(key),
            value: Some(_),
            reset: false,
        } => Some(key.clone()),
        _ => None,
    };
    match (write_or_pick(ctx, settings, &command), typed) {
        (Err(err), Some(key)) => {
            let why = ctx.env.shown_in(&format!("{err:#}"));
            ctx.send(AppEvent::SettingRefused { key, why });
            Ok(Vec::new())
        }
        (handled, _) => handled,
    }
}

/// A command's setting written, or asked where to save, else its picker opened.
fn write_or_pick(ctx: &mut Ctx, settings: &mut SettingsState, command: &SlashCommand) -> Handled {
    match args::setting_write(ctx, command)? {
        // A key of yours saves at once, as Claude Code's `/config key=value` does.
        Some(write)
            if write.command == "config"
                && write.also.is_empty()
                && !asks_where(
                    &write.key,
                    setting_meta(&write.key).is_some_and(|m| m.safety),
                ) =>
        {
            set_at_once(ctx, settings, write.key, write.value)
        }
        Some(write) => {
            let recommended = rows::default_scope(write.command, &write.key);
            let mut pairs = vec![(write.key, write.value)];
            pairs.extend(write.also);
            ask_where(ctx, settings, pairs, recommended)
        }
        None => {
            if let SlashCommand::Model {
                role, worker: None, ..
            } = command
            {
                refresh::ask_each(ctx, settings);
                if role.is_some() {
                    let data = refresh::view(ctx, settings, *role);
                    ctx.send(AppEvent::View {
                        data: Box::new(data),
                    });
                    return Ok(Vec::new());
                }
            }
            for data in pickers::picker(ctx, command) {
                ctx.send(AppEvent::View {
                    data: Box::new(data),
                });
            }
            Ok(Vec::new())
        }
    }
}

/// Saves `key` to your global file at once, unless a project file, the workflow or `--set`
/// holds it, where a save there wouldn't show; then it says where the value is set instead.
fn set_at_once(ctx: &mut Ctx, settings: &mut SettingsState, key: String, value: String) -> Handled {
    let layer = ctx.settings.layer(&key);
    if let Some(holder) = held_above_your_file(&layer) {
        ctx.notice(
            NoteLevel::Info,
            format!(
                "{key} is set by {holder}, which wins over ~/.argonauts/config.toml · /config {key} \
                 changes it there"
            ),
        );
        return Ok(Vec::new());
    }
    begin(
        ctx,
        settings,
        Change::Set(vec![(key, value)]),
        SettingScope::Global,
    )
}

/// Who holds a value above your global file, in words, or `None` when your file decides.
fn held_above_your_file(layer: &str) -> Option<&'static str> {
    use argonauts_core::settings::layer;
    Some(match layer {
        layer::PROJECT => "the project's .argonauts/config.toml",
        layer::PROJECT_LOCAL => "this project's .argonauts/config.local.toml",
        layer::WORKFLOW => "the workflow",
        layer::FLAG => "--set",
        layer::SESSION => "this session",
        _ => return None,
    })
}

/// Reads the CLIs' model lists again in the background, as at start.
pub(crate) fn refresh_models(ctx: &Ctx, settings: &mut SettingsState) {
    refresh::start(ctx, settings);
}

/// A model list refresh merged.
pub(crate) fn models_refreshed(ctx: &Ctx, settings: &mut SettingsState) -> Handled {
    refresh::merged(ctx, settings)
}

/// One source's list was read again.
pub(crate) fn model_source_listed(ctx: &Ctx, settings: &mut SettingsState, cli: &str) -> Handled {
    refresh::source_listed(ctx, settings, cli)
}

/// `/model`'s picker opened or closed.
pub(crate) fn watch_models(ctx: &Ctx, settings: &mut SettingsState, watching: bool) {
    refresh::watch(ctx, settings, watching);
}

/// The local servers are due to be asked again.
pub(crate) fn models_tick(ctx: &Ctx, settings: &mut SettingsState) -> Handled {
    refresh::tick(ctx, settings)
}

/// A reading of this machine's memory came back.
pub(crate) fn memory_read(
    ctx: &Ctx,
    settings: &mut SettingsState,
    reading: Option<Box<memory_facts::Reading>>,
) -> Handled {
    refresh::memory_read(ctx, settings, reading)
}

/// `Config`, `Settings`, `Models`, and `Approvals`.
pub(crate) async fn view(
    ctx: &mut Ctx,
    settings: &mut SettingsState,
    view: ViewRequest,
) -> Handled {
    // The orchestrator's picker may ask for its rows again; anything else ends the hold.
    if !matches!(
        view,
        ViewRequest::Models {
            role: Some(RoleName::Orchestrator)
        }
    ) {
        settings.hold = None;
    }
    let data = match view {
        ViewRequest::Config => ViewData::Config {
            entries: ctx.settings.entries(),
        },
        ViewRequest::Settings { section } => ViewData::Settings {
            rows: rows::rows(&ctx.settings, None, section.as_deref()),
            command: None,
            section,
            not_installed: Vec::new(),
        },
        ViewRequest::Models { role } => refresh::view(ctx, settings, role),
        ViewRequest::Approvals => pickers::approvals(ctx),
        _ => return Ok(Vec::new()),
    };
    ctx.send(AppEvent::View {
        data: Box::new(data),
    });
    Ok(Vec::new())
}

/// A picker's pick after the TUI asked the scope.
pub(crate) async fn set(
    ctx: &mut Ctx,
    settings: &mut SettingsState,
    key: String,
    value: String,
    scope: SettingScope,
) -> Handled {
    settings.hold = None;
    let value = args::check_known(ctx, &key, &value)?;
    begin(ctx, settings, Change::Set(vec![(key, value)]), scope)
}

/// A safety key you set by answering a question that said what it saves, such as a direct turn's
/// "don't ask again": the answer was the confirm, so it's saved at once.
pub(crate) fn save_answered(
    ctx: &mut Ctx,
    key: &str,
    value: String,
    scope: SettingScope,
) -> Handled {
    let change = save::Change::Set(vec![(key.to_owned(), value)]);
    save::apply(ctx, &change, scope, save::TrustAfter::Carry)
}

pub(crate) async fn reset(
    ctx: &mut Ctx,
    settings: &mut SettingsState,
    key: String,
    scope: SettingScope,
) -> Handled {
    settings.hold = None;
    begin_reset(ctx, settings, key, scope)
}

/// The scope question for a value worked out elsewhere, such as a remembered fallback pick.
pub(crate) async fn ask_scope(
    ctx: &mut Ctx,
    settings: &mut SettingsState,
    request: ScopeRequest,
) -> Handled {
    settings.hold = None;
    let value = args::check_known(ctx, &request.key, &request.value)?;
    let recommended = rows::default_scope(&request.command, &request.key);
    ask_where(ctx, settings, vec![(request.key, value)], recommended)
}

/// Settings worked out elsewhere, saved at a scope you already picked, such as what a request
/// said to keep "from now on". A safety key among them still asks you to confirm.
pub(crate) fn save(
    ctx: &mut Ctx,
    settings: &mut SettingsState,
    pairs: Vec<(String, String)>,
    scope: SettingScope,
) -> Handled {
    settings.hold = None;
    let pairs = pairs
        .into_iter()
        .map(|(key, value)| {
            let value = args::check_known(ctx, &key, &value)?;
            Ok((key, value))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    begin(ctx, settings, Change::Set(pairs), scope)
}

/// Settings you confirmed already by picking where a setup applies on its question, so a safety
/// key among them asks no second question. A project file you haven't trusted still asks.
pub(crate) fn save_confirmed(
    ctx: &mut Ctx,
    settings: &mut SettingsState,
    pairs: Vec<(String, String)>,
    scope: SettingScope,
) -> Handled {
    settings.hold = None;
    let pairs = pairs
        .into_iter()
        .map(|(key, value)| {
            let value = args::check_known(ctx, &key, &value)?;
            Ok((key, value))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let change = Change::Set(pairs);
    refuse_unless_at_terminal(ctx, &change)?;
    if hold_for_orchestrator(ctx, settings, &change, scope) {
        return Ok(Vec::new());
    }
    trust_step(ctx, settings, change, scope)
}

/// `SettingScope`, `ConfirmSafetySetting`, `TrustProject`, and `DetectedChecks`.
pub(crate) async fn answer_choice(
    ctx: &mut Ctx,
    settings: &mut SettingsState,
    id: ChoiceId,
    action: AnswerAction,
    text: Option<String>,
) -> Handled {
    let Some(pending) = settings.pending.remove(&id) else {
        ctx.notice(NoteLevel::Info, "That question is already closed.");
        return Ok(Vec::new());
    };
    match (pending, action) {
        (Pending::Scope { change }, AnswerAction::Choose { value }) => {
            let Some(scope) = choices::scope_from_value(&value) else {
                settings.pending.insert(id, Pending::Scope { change });
                anyhow::bail!("{value:?} isn't a place to save a setting");
            };
            close(ctx, id, choices::scope_label(scope));
            begin(ctx, settings, change, scope)
        }
        (Pending::Confirm { change, scope }, AnswerAction::Approve) => {
            close(ctx, id, "Change it");
            trust_step(ctx, settings, change, scope)
        }
        (Pending::Confirm { .. }, AnswerAction::Cancel) => {
            close(ctx, id, NOT_CHANGED);
            Ok(Vec::new())
        }
        (Pending::Trust { change, scope }, AnswerAction::Approve) => {
            let shown = shown_file(ctx, scope);
            close(ctx, id, format!("Trusted {shown}."));
            save::apply(ctx, &change, scope, TrustAfter::Approved)
        }
        (Pending::Trust { change, scope }, AnswerAction::Cancel) => {
            not_now(ctx, id, &change, scope)
        }
        // "Show the file" prints it above the question, which stays open.
        (
            pending @ (Pending::Trust { .. } | Pending::TrustAtStart { .. }),
            AnswerAction::ShowDiff,
        ) => {
            let file = match &pending {
                Pending::TrustAtStart { file, .. } => Some(file.clone()),
                Pending::Trust { scope, .. } => save::file_for(ctx, *scope),
                _ => None,
            };
            if let Some(file) = file {
                super::prompt_trust::show_file(ctx, &file);
            }
            settings.pending.insert(id, pending);
            Ok(Vec::new())
        }
        (Pending::TrustAtStart { file, scope, hash }, AnswerAction::Approve) => {
            trust_at_start(ctx, id, &file, scope, &hash)
        }
        (Pending::TrustAtStart { scope, .. }, AnswerAction::Cancel) => {
            still_untrusted(ctx, id, scope);
            Ok(Vec::new())
        }
        (Pending::Checks { commands }, AnswerAction::Approve) => {
            close(ctx, id, "Use these");
            save_checks(ctx, settings, &commands)
        }
        (Pending::Checks { .. }, AnswerAction::ApproveWithEdits) => {
            let lines: Vec<String> = text
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect();
            close(ctx, id, "Edited");
            save_checks(ctx, settings, &lines)
        }
        (Pending::Checks { .. }, AnswerAction::Cancel) => {
            settings.checks_answered = true;
            checks::remember_skip(&ctx.env.state_dir)?;
            close(ctx, id, "Skipped. Not offered again in this repo.");
            Ok(Vec::new())
        }
        (pending, _) => {
            settings.pending.insert(id, pending);
            anyhow::bail!("That answer doesn't fit this question")
        }
    }
}

/// Said when autopilot comes on with the check commands' offer open.
const CHECKS_LEFT_ON_AUTOPILOT: &str = "Decided on autopilot · not saved, since check commands \
                                        are yours to confirm. Offered again for a run started \
                                        off autopilot.";

/// Follows the mode on screen. True when autopilot just came on, by Shift+Tab, a setting, or
/// a run that brings its own mode.
pub(crate) fn follow_mode(settings: &mut SettingsState, autopilot: bool) -> bool {
    let was = std::mem::replace(&mut settings.autopilot, autopilot);
    autopilot && !was
}

/// Autopilot came on with the check commands' offer open. Saving them is yours to confirm, so
/// the offer closes and waits for a run started off autopilot, as one found on autopilot does.
pub(crate) fn settle_on_autopilot(ctx: &Ctx, settings: &mut SettingsState) {
    let mut offers: Vec<ChoiceId> = settings
        .pending
        .iter()
        .filter(|(_, pending)| matches!(pending, Pending::Checks { .. }))
        .map(|(id, _)| id.clone())
        .collect();
    offers.sort();
    for id in offers {
        settings.pending.remove(&id);
        close(ctx, id, CHECKS_LEFT_ON_AUTOPILOT);
    }
}

pub(crate) fn set_aside(ctx: &mut Ctx, settings: &mut SettingsState, id: ChoiceId) -> Handled {
    let Some(pending) = settings.pending.remove(&id) else {
        return Ok(Vec::new());
    };
    match (choices::on_set_aside(pending.kind()), pending) {
        (OnSetAside::NotNow, Pending::Trust { change, scope }) => not_now(ctx, id, &change, scope),
        (OnSetAside::NotNow, Pending::TrustAtStart { scope, .. }) => {
            still_untrusted(ctx, id, scope);
            Ok(Vec::new())
        }
        (OnSetAside::NextRun, _) => {
            close(ctx, id, "Offered again when the next run starts.");
            Ok(Vec::new())
        }
        _ => {
            close(ctx, id, NOT_CHANGED);
            Ok(Vec::new())
        }
    }
}

/// Sets `ui.verbosity` everywhere, or for this session when a project file or flag sets it.
pub(crate) async fn verbosity(
    ctx: &mut Ctx,
    settings: &mut SettingsState,
    level: Verbosity,
) -> Handled {
    settings.hold = None;
    let value = toml::Value::try_from(level)?.to_string();
    toggles::save_toggle(ctx, "ui.verbosity", &value)
}

/// Saves `ui.keys` everywhere, or for this session when a project file or flag sets it.
pub(crate) async fn vim(ctx: &mut Ctx, settings: &mut SettingsState, on: bool) -> Handled {
    settings.hold = None;
    let keys = if on {
        KeyScheme::Vim
    } else {
        KeyScheme::Default
    };
    let value = toml::Value::try_from(keys)?.to_string();
    toggles::save_toggle(ctx, "ui.keys", &value)
}

pub(crate) async fn toggle_vim(ctx: &mut Ctx, settings: &mut SettingsState) -> Handled {
    let on = ctx.settings().ui.keys != KeyScheme::Vim;
    vim(ctx, settings, on).await
}

/// "Yes, and let the main agent answer" on a go-ahead question: the main agent answers the crew's
/// questions in this project from now on, just for you, as Claude Code's "don't ask again" does.
/// Picking it on the question is the confirm a safety key needs; `/approvals` undoes it.
pub(crate) fn let_the_main_agent_answer(ctx: &mut Ctx) -> Handled {
    const KEY: &str = "approvals.answers";
    refuse_safety_change(&ctx.env, &[KEY.to_owned()])?;
    let change = Change::Set(vec![(KEY.to_owned(), "\"main\"".to_owned())]);
    let effects = save::apply(
        ctx,
        &change,
        SettingScope::ProjectLocal,
        TrustAfter::Approved,
    )?;
    ctx.notice(
        NoteLevel::Info,
        "From now on the main agent answers the crew's questions in this project, just for you. \
         /approvals changes it.",
    );
    Ok(effects)
}

/// A narrator mode for this app session only.
pub(crate) async fn narrator_mode(
    ctx: &mut Ctx,
    settings: &mut SettingsState,
    mode: NarratorMode,
) -> Handled {
    settings.hold = None;
    let value = toml::Value::try_from(mode)?.to_string();
    let change = Change::Set(vec![("ui.narrator".to_owned(), value)]);
    save::apply(ctx, &change, SettingScope::Session, TrustAfter::Carry)
}

/// Said on autopilot in place of offering the check commands found in the folder.
pub(crate) const CHECKS_ON_AUTOPILOT: &str = "Decided on autopilot · this run has no check commands. Saving the ones found here is yours \
     to confirm, so they aren't used:";

/// Before a run starts: the detected checks, once per repo, when `checks.run` is empty. On
/// autopilot nothing is asked, since checks are a safety setting only you confirm: one line
/// names what was found, once, and the question waits for a run you start off autopilot.
pub(crate) async fn offer_checks(ctx: &mut Ctx, settings: &mut SettingsState) -> Handled {
    settings.hold = None;
    let open = settings
        .pending
        .values()
        .any(|pending| matches!(pending, Pending::Checks { .. }));
    if open
        || settings.checks_answered
        || ctx.env.agent_session
        || !ctx.env.interactive
        || !ctx.settings().checks.run.is_empty()
        || checks::skipped(&ctx.env.state_dir)
    {
        return Ok(Vec::new());
    }
    let commands = checks::detect(&ctx.env.repo_root);
    if commands.is_empty() {
        return Ok(Vec::new());
    }
    if settings.autopilot {
        if !std::mem::replace(&mut settings.checks_named, true) {
            let found = commands.join(", ");
            ctx.notice(
                NoteLevel::Info,
                format!("{CHECKS_ON_AUTOPILOT} {found}. /config checks.run sets them."),
            );
        }
        return Ok(Vec::new());
    }
    let id = ctx.next_choice_id();
    ctx.send(AppEvent::Choice {
        choice: checks::offer(id.clone(), &commands),
    });
    settings.pending.insert(id, Pending::Checks { commands });
    Ok(Vec::new())
}

/// Asks where to save `key = value`, and any keys that go with it, `recommended` picked.
fn ask_where(
    ctx: &mut Ctx,
    settings: &mut SettingsState,
    pairs: Vec<(String, String)>,
    recommended: SettingScope,
) -> Handled {
    let Some((key, value)) = pairs.first().cloned() else {
        return Ok(Vec::new());
    };
    let also: Vec<String> = pairs[1..]
        .iter()
        .map(|(key, value)| format!("{key} = {value}"))
        .collect();
    let change = Change::Set(pairs);
    refuse_unless_at_terminal(ctx, &change)?;
    let id = ctx.next_choice_id();
    let mut choice = scope::scope_question(ctx, id.clone(), &key, &value, recommended);
    choice.body.splice(1..1, also);
    settings.pending.insert(id, Pending::Scope { change });
    ctx.send(AppEvent::Choice { choice });
    Ok(Vec::new())
}

/// The scope is known: hold for the orchestrator, confirm a safety key, then the trust step.
fn begin(
    ctx: &mut Ctx,
    settings: &mut SettingsState,
    change: Change,
    scope: SettingScope,
) -> Handled {
    refuse_unless_at_terminal(ctx, &change)?;
    if hold_for_orchestrator(ctx, settings, &change, scope) {
        return Ok(Vec::new());
    }
    let changes: Vec<String> = match &change {
        Change::Set(pairs) => pairs
            .iter()
            .flat_map(|(key, value)| confirm::safety_changes(&ctx.settings, key, Some(value)))
            .collect(),
        Change::Reset(key) => confirm::safety_changes(&ctx.settings, key, None),
    };
    if !changes.is_empty() {
        let id = ctx.next_choice_id();
        ctx.send(AppEvent::Choice {
            choice: confirm::confirm_question(id.clone(), changes),
        });
        settings
            .pending
            .insert(id, Pending::Confirm { change, scope });
        return Ok(Vec::new());
    }
    trust_step(ctx, settings, change, scope)
}

/// A change that needs an orchestrator first opens the orchestrator's settings, and waits.
fn hold_for_orchestrator(
    ctx: &mut Ctx,
    settings: &mut SettingsState,
    change: &Change,
    scope: SettingScope,
) -> bool {
    if !needs_orchestrator(ctx, change) {
        return false;
    }
    settings.hold = Some(scope);
    let data = refresh::view(ctx, settings, Some(RoleName::Orchestrator));
    ctx.send(AppEvent::View {
        data: Box::new(data),
    });
    true
}

/// A project file you haven't trusted, which sets safety keys, asks first; anything else is
/// saved at once, trust carried forward.
fn trust_step(
    ctx: &mut Ctx,
    settings: &mut SettingsState,
    change: Change,
    scope: SettingScope,
) -> Handled {
    let unsafe_lines = project_file(ctx, scope)
        .filter(|file| file.exists() && !ctx.settings.trust().was_trusted(file))
        .map(|file| confirm::safety_lines(&file))
        .unwrap_or_default();
    if unsafe_lines.is_empty() {
        return save::apply(ctx, &change, scope, TrustAfter::Carry);
    }
    let id = ctx.next_choice_id();
    let shown = shown_file(ctx, scope);
    ctx.send(AppEvent::Choice {
        choice: confirm::trust_question(id.clone(), &shown, &unsafe_lines),
    });
    settings
        .pending
        .insert(id, Pending::Trust { change, scope });
    Ok(Vec::new())
}

/// Whether a question at start already says what `warning` would, so its line is left out: the
/// trust question lists a file's untrusted safety keys, and the rename question a file's old
/// names. `argonauts run` and /doctor still print every line.
pub(crate) fn said_by_a_question(ctx: &Ctx, warning: &SettingsWarning) -> bool {
    match &warning.about {
        WarningAbout::Untrusted { file, key } => {
            !ctx.settings.trust().is_trusted(file)
                && confirm::safety_lines(file)
                    .iter()
                    .any(|line| line.split_once(" = ").is_some_and(|(set, _)| set == key))
                && ctx.settings.check_trust(&ctx.env, file).is_ok()
        }
        WarningAbout::Renamed { file } => renamed::said_at_start(ctx, file),
        WarningAbout::Unknown { .. } | WarningAbout::Other => false,
    }
}

/// At start: one `TrustProject` question for each project file that sets safety keys you
/// haven't trusted, which stay ignored until you do.
pub(crate) fn ask_trust_at_start(ctx: &mut Ctx, settings: &mut SettingsState) {
    for scope in [SettingScope::Project, SettingScope::ProjectLocal] {
        let Some(file) = project_file(ctx, scope) else {
            continue;
        };
        if ctx.settings.trust().is_trusted(&file) {
            continue;
        }
        let unsafe_lines = confirm::safety_lines(&file);
        let Some(hash) = hash_of(&file).filter(|_| !unsafe_lines.is_empty()) else {
            continue;
        };
        let shown = shown_file(ctx, scope);
        // A yes couldn't work, so this says what to fix instead of asking.
        if let Err(err) = ctx.settings.check_trust(&ctx.env, &file) {
            ctx.notice(NoteLevel::Warn, cant_trust(&shown, &err));
            continue;
        }
        let id = ctx.next_choice_id();
        ctx.send(AppEvent::Choice {
            choice: confirm::start_trust_question(id.clone(), &shown, &unsafe_lines),
        });
        settings
            .pending
            .insert(id, Pending::TrustAtStart { file, scope, hash });
    }
}

/// Trusts the file as you were asked about it, so its safety keys apply from now on. A file
/// that changed since stays untrusted, and the question comes back at the next start.
fn trust_at_start(
    ctx: &mut Ctx,
    id: ChoiceId,
    file: &std::path::Path,
    scope: SettingScope,
    hash: &str,
) -> Handled {
    let shown = shown_file(ctx, scope);
    if hash_of(file).as_deref() != Some(hash) {
        close(
            ctx,
            id,
            format!(
                "{shown} changed since you were asked, so it stays untrusted. You're asked \
                 again at the next start."
            ),
        );
        return Ok(Vec::new());
    }
    let keys: Vec<String> = confirm::safety_lines(file)
        .iter()
        .filter_map(|line| line.split_once(" = ").map(|(key, _)| key.to_owned()))
        .collect();
    let before = ctx.settings().clone();
    let env = ctx.env.clone();
    // The settings must still read whole with the file trusted, as when you were asked: one
    // of your files that broke since leaves the trust as it was.
    if let Err(err) = ctx.settings.check_trust(&env, file) {
        close(ctx, id, NOT_CHANGED);
        ctx.notice(NoteLevel::Warn, cant_trust(&shown, &err));
        return Ok(Vec::new());
    }
    let recorded = ctx.settings.trust().recorded(file);
    let trusted = ctx
        .settings
        .trust_mut()
        .trust(file)
        .and_then(|()| ctx.settings.reload(&env));
    if let Err(err) = trusted {
        // Kept, the trust would break the next start on settings that don't resolve.
        let put_back = ctx.settings.trust_mut().put_back(file, recorded);
        close(ctx, id, NOT_CHANGED);
        return match (put_back, err.downcast_ref::<SettingsError>()) {
            (Ok(()), Some(settings_err)) => {
                ctx.notice(NoteLevel::Warn, cant_trust(&shown, settings_err));
                Ok(Vec::new())
            }
            (Ok(()), None) => Err(err.context(format!("trusting {shown}"))),
            (Err(restore), _) => Err(err.context(format!(
                "trusting {shown}, and putting its trust back failed: {restore:#}"
            ))),
        };
    }
    close(ctx, id, format!("Trusted {shown}."));
    if ctx.settings().ui != before.ui {
        save::send_ui(ctx);
    }
    let mut effects = Vec::new();
    if ctx.settings().crew.main != before.crew.main {
        effects.push(Effect::FreshConversation);
    }
    effects.push(Effect::SettingsApplied { keys });
    Ok(effects)
}

fn still_untrusted(ctx: &Ctx, id: ChoiceId, scope: SettingScope) {
    let shown = shown_file(ctx, scope);
    close(
        ctx,
        id,
        format!(
            "{shown} stays untrusted, so its safety settings stay ignored. You're asked again at \
             the next start."
        ),
    );
}

/// Not now: your change is still written, and the file stays untrusted.
fn not_now(ctx: &mut Ctx, id: ChoiceId, change: &Change, scope: SettingScope) -> Handled {
    let shown = shown_file(ctx, scope);
    close(
        ctx,
        id,
        format!("{shown} stays untrusted, so its safety settings stay ignored."),
    );
    save::apply(ctx, change, scope, TrustAfter::Declined)
}

fn begin_reset(
    ctx: &mut Ctx,
    settings: &mut SettingsState,
    key: String,
    scope: SettingScope,
) -> Handled {
    let sets = match save::file_for(ctx, scope) {
        None => ctx
            .settings
            .session_overrides()
            .iter()
            .any(|(set, _)| *set == key),
        Some(file) => save::file_sets(&file, &key),
    };
    if !sets {
        ctx.notice(
            NoteLevel::Info,
            format!("{key} isn't set for {}.", scope::scope_words(scope)),
        );
        return Ok(Vec::new());
    }
    begin(ctx, settings, Change::Reset(key), scope)
}

/// `/config <key> --reset`: from the layer that sets it now.
fn reset_in_effect(ctx: &mut Ctx, settings: &mut SettingsState, key: &str) -> Handled {
    if setting_meta(key).is_none() {
        anyhow::bail!("{key} isn't a setting; /config lists them");
    }
    let layer = ctx.settings.layer(key);
    let chosen = ctx.settings().workflow.clone();
    let explain = match layer.as_str() {
        "workflow" => Some(format!(
            "{key} comes from the {chosen} workflow, so it can't be reset here. Use /workflow copy \
             {chosen} to edit the workflow."
        )),
        "flag" => Some(format!(
            "{key} comes from a --set flag, so it can't be reset here. It holds until you quit."
        )),
        _ => None,
    };
    if let Some(explain) = explain {
        ctx.notice(NoteLevel::Info, explain);
        return Ok(Vec::new());
    }
    let Some(scope) = scope::scope_of_layer(&layer) else {
        ctx.notice(
            NoteLevel::Info,
            format!("{key} is already the built-in default."),
        );
        return Ok(Vec::new());
    };
    begin_reset(ctx, settings, key.to_owned(), scope)
}

/// The held workflow and the orchestrator you picked go together, to the scope picked for the
/// workflow, without asking it again.
fn orchestrator_picked(
    ctx: &mut Ctx,
    settings: &mut SettingsState,
    scope: SettingScope,
    worker: &ModelRef,
) -> Handled {
    if let Some(why) = models::refusal(ctx, Some(RoleName::Orchestrator), worker) {
        anyhow::bail!("{why}");
    }
    trust_step(ctx, settings, orchestrator_change(worker), scope)
}

fn orchestrator_change(worker: &ModelRef) -> Change {
    let (key, value) = args::model_key(Some(RoleName::Orchestrator), worker);
    Change::Set(vec![
        ("workflow".to_owned(), args::quoted(ORCHESTRATOR)),
        (key, value),
    ])
}

/// `workflow = "orchestrator"` alone while no orchestrator is set.
fn needs_orchestrator(ctx: &Ctx, change: &Change) -> bool {
    let Change::Set(pairs) = change else {
        return false;
    };
    let quoted = args::quoted(ORCHESTRATOR);
    let picks_crew = pairs
        .iter()
        .any(|(key, value)| key == "workflow" && *value == quoted);
    let names_orchestrator = pairs
        .iter()
        .any(|(key, _)| key == "roles.orchestrator.models");
    let has_orchestrator = ctx
        .settings()
        .roles
        .get(&RoleName::Orchestrator)
        .is_some_and(|role| !role.models.is_empty());
    picks_crew && !names_orchestrator && !has_orchestrator
}

fn save_checks(ctx: &mut Ctx, settings: &mut SettingsState, lines: &[String]) -> Handled {
    settings.checks_answered = true;
    let commands = checks::check_commands(lines);
    if commands.is_empty() {
        ctx.notice(
            NoteLevel::Info,
            "No check commands given, so none were saved.",
        );
        return Ok(Vec::new());
    }
    let change = Change::Set(vec![(
        "checks.run".to_owned(),
        checks::checks_value(&commands)?,
    )]);
    refuse_unless_at_terminal(ctx, &change)?;
    // The offer said checks are a safety setting, so answering it was the confirm.
    trust_step(ctx, settings, change, SettingScope::Project)
}

/// Without a terminal, or inside an agent session, no safety key changes, in any scope.
fn refuse_unless_at_terminal(ctx: &Ctx, change: &Change) -> anyhow::Result<()> {
    let safety: Vec<String> = match change {
        Change::Set(pairs) => pairs
            .iter()
            .flat_map(|(key, value)| safety_keys_in(key, value))
            .collect(),
        Change::Reset(key) => safety_keys_in(key, "\"\""),
    };
    refuse_safety_change(&ctx.env, &safety)
}

fn project_file(ctx: &Ctx, scope: SettingScope) -> Option<PathBuf> {
    match scope {
        SettingScope::Project | SettingScope::ProjectLocal => save::file_for(ctx, scope),
        SettingScope::Session | SettingScope::Global => None,
    }
}

fn shown_file(ctx: &Ctx, scope: SettingScope) -> String {
    save::file_for(ctx, scope).map_or_else(
        || "this session".to_owned(),
        |file| {
            file.strip_prefix(&ctx.env.repo_root)
                .map(|relative| relative.display().to_string())
                .unwrap_or_else(|_| ctx.env.display(&file))
        },
    )
}

fn close(ctx: &Ctx, id: ChoiceId, outcome: impl Into<String>) {
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: outcome.into(),
    });
}
