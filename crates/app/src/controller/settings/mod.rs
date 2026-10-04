//! Every setting changed from inside the app: the settings commands' pickers and arguments,
//! the scope question, the safety confirm, trust carried forward on a write, resets, the
//! toggles, and the detected-gates offer.
//!
//! A write goes through up to three questions, each a pending choice here: where to save it
//! (`SettingScope`), whether to change a safety key (`ConfirmSafetySetting`), and whether to
//! trust the project file it lands in (`TrustProject`). Then [`save`] makes it and announces it.
//!
//! At start, a project file whose safety keys you haven't trusted gets the `TrustProject`
//! question on its own, so they aren't ignored without your knowing.

mod args;
mod confirm;
mod gates;
mod labels;
mod models;
mod pickers;
mod refresh;
mod rows;
mod save;
mod scope;
mod toggles;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_models;

pub(crate) use models::choices as model_choices;
pub(crate) use rows::model_keys;

use std::collections::HashMap;
use std::path::PathBuf;

use argonauts_core::app::{AppEvent, ChoiceKind, ViewData, ViewRequest};
use argonauts_core::events::checkpoint::AnswerAction;
use argonauts_core::input::SlashCommand;
use argonauts_core::names::{ChoiceId, RoleName, WorkerRef};
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::ui::NarratorMode;
use argonauts_core::{KeyScheme, NoteLevel, Verbosity};
use argonauts_registry::{SettingsError, safety_keys_in, setting_meta};

use super::{Ctx, Effect, ScopeRequest};
use crate::choices::{self, NOT_CHANGED, OnSetAside};
use crate::commands::refuse_safety_change;
use crate::trust::{cant_trust, hash_of};
use save::{Change, TrustAfter};

const COORDINATED_CREW: &str = "coordinated-crew";

pub(crate) struct SettingsState {
    /// The questions asked here that haven't been answered.
    pending: HashMap<ChoiceId, Pending>,
    /// `preset = "coordinated-crew"` waiting for its coordinator, with the scope picked for it.
    hold: Option<SettingScope>,
    /// The gates offer was answered in this app session, so it isn't made again even when the
    /// answer left `gates.run` ignored in a file you didn't trust.
    gates_answered: bool,
    /// A model list refresh is running.
    refreshing: bool,
    /// The model list last sent, for the refresh to bring up to date.
    shown: Option<refresh::ModelsShown>,
}

impl SettingsState {
    pub(crate) fn new(ctx: &Ctx) -> Self {
        let _ = ctx;
        Self {
            pending: HashMap::new(),
            hold: None,
            gates_answered: false,
            refreshing: false,
            shown: None,
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
    Gates {
        commands: Vec<String>,
    },
}

impl Pending {
    fn kind(&self) -> ChoiceKind {
        match self {
            Self::Scope { .. } => ChoiceKind::SettingScope,
            Self::Confirm { .. } => ChoiceKind::ConfirmSafetySetting,
            Self::Trust { .. } | Self::TrustAtStart { .. } => ChoiceKind::TrustProject,
            Self::Gates { .. } => ChoiceKind::DetectedGates,
        }
    }
}

type Handled = anyhow::Result<Vec<Effect>>;

pub(crate) fn owns_choice(settings: &SettingsState, id: &ChoiceId) -> bool {
    settings.pending.contains_key(id)
}

/// Something other than a settings command arrived, so a preset held for its coordinator is
/// dropped and a later coordinator pick is a plain `/model coordinator`.
pub(crate) fn interrupt(settings: &mut SettingsState) {
    settings.hold = None;
}

/// `/model`, `/effort`, `/agents`, `/preset <name>`, `/workflows`, `/approvals`, `/budget`,
/// `/handoff`, `/permissions`, `/verbosity`, `/narrator`, `/theme`, and `/config`: a picker with
/// no argument, the scope question with one.
pub(crate) async fn command(
    ctx: &mut Ctx,
    settings: &mut SettingsState,
    command: SlashCommand,
) -> Handled {
    if let SlashCommand::Model {
        role: Some(RoleName::Coordinator),
        worker: Some(worker),
        ..
    } = &command
        && let Some(scope) = settings.hold.take()
    {
        return coordinator_picked(ctx, settings, scope, worker);
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
    match args::setting_write(ctx, &command)? {
        Some(write) => {
            let recommended = rows::default_scope(write.command, &write.key);
            let mut pairs = vec![(write.key, write.value)];
            pairs.extend(write.also);
            ask_where(ctx, settings, pairs, recommended)
        }
        None => {
            if let SlashCommand::Model {
                role, worker: None, ..
            } = &command
            {
                refresh::start(ctx, settings);
                if role.is_some() {
                    let data = refresh::view(ctx, settings, *role);
                    ctx.send(AppEvent::View {
                        data: Box::new(data),
                    });
                    return Ok(Vec::new());
                }
            }
            for data in pickers::picker(ctx, &command) {
                ctx.send(AppEvent::View {
                    data: Box::new(data),
                });
            }
            Ok(Vec::new())
        }
    }
}

/// Reads the CLIs' model lists again in the background, as at start.
pub(crate) fn refresh_models(ctx: &Ctx, settings: &mut SettingsState) {
    refresh::start(ctx, settings);
}

/// A model list refresh landed.
pub(crate) fn models_refreshed(ctx: &Ctx, settings: &mut SettingsState) -> Handled {
    refresh::landed(ctx, settings)
}

/// `Config`, `Settings`, `Models`, and `Approvals`.
pub(crate) async fn view(
    ctx: &mut Ctx,
    settings: &mut SettingsState,
    view: ViewRequest,
) -> Handled {
    // The coordinator's picker may ask for its rows again; anything else ends the hold.
    if !matches!(
        view,
        ViewRequest::Models {
            role: Some(RoleName::Coordinator)
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

/// A safety key you set by answering a card that said what it saves, such as a direct turn's
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

/// The scope question for a value worked out elsewhere, such as a remembered handoff pick.
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

/// `SettingScope`, `ConfirmSafetySetting`, `TrustProject`, and `DetectedGates`.
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
        // The TUI shows the file itself; the question stays open.
        (
            pending @ (Pending::Trust { .. } | Pending::TrustAtStart { .. }),
            AnswerAction::ShowDiff,
        ) => {
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
        (Pending::Gates { commands }, AnswerAction::Approve) => {
            close(ctx, id, "Use these");
            save_gates(ctx, settings, &commands)
        }
        (Pending::Gates { .. }, AnswerAction::ApproveWithEdits) => {
            let lines: Vec<String> = text
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect();
            close(ctx, id, "Edited");
            save_gates(ctx, settings, &lines)
        }
        (Pending::Gates { .. }, AnswerAction::Cancel) => {
            settings.gates_answered = true;
            gates::remember_skip(&ctx.env.state_dir)?;
            close(ctx, id, "Skipped. Not offered again in this repo.");
            Ok(Vec::new())
        }
        (pending, _) => {
            settings.pending.insert(id, pending);
            anyhow::bail!("That answer doesn't fit this question")
        }
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
        KeyScheme::Standard
    };
    let value = toml::Value::try_from(keys)?.to_string();
    toggles::save_toggle(ctx, "ui.keys", &value)
}

pub(crate) async fn toggle_vim(ctx: &mut Ctx, settings: &mut SettingsState) -> Handled {
    let on = ctx.settings().ui.keys != KeyScheme::Vim;
    vim(ctx, settings, on).await
}

/// "Yes, and let the lead answer" on a go-ahead card: the lead answers the crew's questions in
/// this project from now on, just for you, as Claude Code's "don't ask again" does. Picking it
/// on the card is the confirm a safety key needs; `/approvals` undoes it.
pub(crate) fn let_the_lead_answer(ctx: &mut Ctx) -> Handled {
    const KEY: &str = "approvals.answered_by";
    refuse_safety_change(&ctx.env, &[KEY.to_owned()])?;
    let change = Change::Set(vec![(KEY.to_owned(), "\"lead\"".to_owned())]);
    let effects = save::apply(
        ctx,
        &change,
        SettingScope::ProjectLocal,
        TrustAfter::Approved,
    )?;
    ctx.notice(
        NoteLevel::Info,
        "From now on the lead answers the crew's questions in this project, just for you. \
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

/// Before a run starts: the detected gates, once per repo, when `gates.run` is empty.
pub(crate) async fn offer_gates(ctx: &mut Ctx, settings: &mut SettingsState) -> Handled {
    settings.hold = None;
    let open = settings
        .pending
        .values()
        .any(|pending| matches!(pending, Pending::Gates { .. }));
    if open
        || settings.gates_answered
        || ctx.env.agent_session
        || !ctx.env.interactive
        || !ctx.settings().gates.run.is_empty()
        || gates::skipped(&ctx.env.state_dir)
    {
        return Ok(Vec::new());
    }
    let commands = gates::detect(&ctx.env.repo_root);
    if commands.is_empty() {
        return Ok(Vec::new());
    }
    let id = ctx.next_choice_id();
    ctx.send(AppEvent::Choice {
        choice: gates::offer(id.clone(), &commands),
    });
    settings.pending.insert(id, Pending::Gates { commands });
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

/// The scope is known: hold for the coordinator, confirm a safety key, then the trust step.
fn begin(
    ctx: &mut Ctx,
    settings: &mut SettingsState,
    change: Change,
    scope: SettingScope,
) -> Handled {
    refuse_unless_at_terminal(ctx, &change)?;
    if needs_coordinator(ctx, &change) {
        settings.hold = Some(scope);
        let data = refresh::view(ctx, settings, Some(RoleName::Coordinator));
        ctx.send(AppEvent::View {
            data: Box::new(data),
        });
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
    if ctx.settings().default_agent != before.default_agent {
        effects.push(Effect::FreshChat);
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
    let preset = ctx.settings().preset.clone();
    let explain = match layer.as_str() {
        "preset" => Some(format!(
            "{key} comes from the {preset} preset, so it can't be reset here. Use /preset copy \
             {preset} to edit the preset."
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

/// The held preset and the coordinator you picked go together, to the scope picked for the
/// preset, without asking it again.
fn coordinator_picked(
    ctx: &mut Ctx,
    settings: &mut SettingsState,
    scope: SettingScope,
    worker: &WorkerRef,
) -> Handled {
    if let Some(why) = models::refusal(ctx, Some(RoleName::Coordinator), worker) {
        anyhow::bail!("{why}");
    }
    trust_step(ctx, settings, coordinator_change(worker), scope)
}

fn coordinator_change(worker: &WorkerRef) -> Change {
    let (key, value) = args::model_key(Some(RoleName::Coordinator), worker);
    Change::Set(vec![
        ("preset".to_owned(), args::quoted(COORDINATED_CREW)),
        (key, value),
    ])
}

/// `preset = "coordinated-crew"` alone while no coordinator is set.
fn needs_coordinator(ctx: &Ctx, change: &Change) -> bool {
    let Change::Set(pairs) = change else {
        return false;
    };
    let quoted = args::quoted(COORDINATED_CREW);
    let picks_crew = pairs
        .iter()
        .any(|(key, value)| key == "preset" && *value == quoted);
    let names_coordinator = pairs
        .iter()
        .any(|(key, _)| key == "roles.coordinator.workers");
    let has_coordinator = ctx
        .settings()
        .roles
        .get(&RoleName::Coordinator)
        .is_some_and(|role| !role.workers.is_empty());
    picks_crew && !names_coordinator && !has_coordinator
}

fn save_gates(ctx: &mut Ctx, settings: &mut SettingsState, lines: &[String]) -> Handled {
    settings.gates_answered = true;
    let commands = gates::gate_commands(lines);
    if commands.is_empty() {
        ctx.notice(
            NoteLevel::Info,
            "No gate commands given, so none were saved.",
        );
        return Ok(Vec::new());
    }
    let change = Change::Set(vec![(
        "gates.run".to_owned(),
        gates::gates_value(&commands)?,
    )]);
    refuse_unless_at_terminal(ctx, &change)?;
    // The offer said gates are a safety setting, so answering it was the confirm.
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
