//! `argonauts resume <id>` with a session's id alone: the conversation that carries it, else the
//! CLI whose list has it, else, for a CLI that keeps no list, as agy, a question of whose it is.

use argonauts_core::cli_sessions::CliSessionRef;
use argonauts_core::names::ConversationId;

use crate::InteractiveOptions;
use crate::app_settings::AppSettings;
use crate::env::AppEnv;

/// Where a session's id was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoundSession {
    /// A conversation of Argonauts Code's carries it, and so carries on with it.
    Conversation(ConversationId),
    /// A CLI lists it as its own.
    CliSession(CliSessionRef),
    /// No CLI lists it, and these CLIs keep no list but could carry it on, each by its name.
    Unlisted(Vec<(String, CliSessionRef)>),
}

/// Looks for the session `id` in this repo's conversations and in every CLI's list.
pub fn find_session(options: &InteractiveOptions, id: &str) -> anyhow::Result<FoundSession> {
    let env = AppEnv::discover(options.repo.as_deref())?;
    let settings = AppSettings::load(&env, options.overrides.clone(), false)?;
    crate::controller::conversation::in_cli::find_any(&env, settings.resolved(), id)
}
