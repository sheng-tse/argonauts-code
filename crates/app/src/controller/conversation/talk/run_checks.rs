//! "Keep these checks": checks the main session picked for a run hold for that run alone. When
//! your own message asks to keep them and the session hands those very checks back as
//! `checks.run`, they save where a change to `checks.run` goes (this project, just for you,
//! while your changes save everywhere), with no question. Your words are the confirmation, so
//! the save asks nothing more and a file it creates is trusted.

use argonauts_core::settings::SettingScope;
use argonauts_core::settings::protocol::CheckCommand;

use super::form::{Place, Proposal};
use super::{Call, ConversationState};
use crate::controller::instructions::asks_to_keep_checks;
use crate::controller::{Ctx, Effect, settings};

const KEY: &str = "checks.run";

/// The checks a run of this conversation holds for itself, the latest run's, so asking to keep
/// them can be held against what that run had.
pub(crate) fn ran_with(conversation: &mut ConversationState, checks: &[CheckCommand]) {
    conversation.talk.run_checks = checks.to_vec();
}

/// Where the setup saves, when it is nothing but the checks a run here had for itself and your
/// message asks to keep checks. A place your words named stands; otherwise they go where a
/// change to them saves. `None` for any other setup, or one your words kept for a request or
/// this conversation, which takes the usual way.
pub(super) fn kept(
    ctx: &Ctx,
    conversation: &ConversationState,
    proposal: &Proposal,
    said: &str,
) -> Option<SettingScope> {
    let [(key, value)] = proposal.settings.as_slice() else {
        return None;
    };
    let only_settings = proposal.roles.is_empty()
        && proposal.prompts.is_empty()
        && proposal.mods.is_empty()
        && proposal.steps.is_empty();
    let ran = &conversation.talk.run_checks;
    if key != KEY || !only_settings || ran.is_empty() || checks_in(value)? != *ran {
        return None;
    }
    if !asks_to_keep_checks(said) {
        return None;
    }
    match proposal.keep {
        None => Some(settings::place(ctx, KEY).scope),
        Some(Place::Repository) => Some(SettingScope::Project),
        Some(Place::AllRepositories) => Some(SettingScope::Global),
        Some(Place::Request | Place::Conversation) => None,
    }
}

/// Keeps the run's checks in `scope`, confirmed since you asked, and tells the session where.
/// The save's own line says where under your message.
pub(super) fn keep(ctx: &Ctx, call: Call, proposal: Proposal, scope: SettingScope) -> Vec<Effect> {
    call.answer(format!(
        "The user's own words kept the run's checks as {KEY}, saved {}. The line under their \
         message says so, so say no more than that they're kept.",
        place_words(ctx, scope)
    ));
    vec![Effect::KeepSettings {
        pairs: proposal.settings,
        scope,
    }]
}

/// `checks.run`'s TOML value as checks, `None` when it doesn't read as them.
fn checks_in(value: &str) -> Option<Vec<CheckCommand>> {
    #[derive(serde::Deserialize)]
    struct Value {
        value: Vec<CheckCommand>,
    }
    let read: Value = toml::from_str(&format!("value = {value}")).ok()?;
    Some(read.value)
}

/// `everywhere (~/.argonauts/config.toml)`, as the save's line names the place.
fn place_words(ctx: &Ctx, scope: SettingScope) -> String {
    let (words, file) = match scope {
        SettingScope::Session => return "for this session".to_owned(),
        SettingScope::ProjectLocal => ("just for them", ctx.env.project_local_config()),
        SettingScope::Project => ("for this project", ctx.env.project_config()),
        SettingScope::Global => ("everywhere", ctx.env.global_config()),
    };
    format!("{words} ({})", ctx.env.shown(&file))
}
