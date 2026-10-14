//! The day's event: what was used, counted from the usage and work ledgers the Overview reads,
//! with nothing in it that names you, your files or your repositories.

use std::collections::BTreeMap;

use argonauts_core::adapter::KNOWN_AGENT_IDS;
use argonauts_core::pricing::builtin_price;
use argonauts_core::session::AuthMode;
use argonauts_core::settings::Settings;
use serde::Serialize;
use time::Date;

use super::uses::{Use, UseLine};
use crate::usage_ledger::LedgerLine;
use crate::work_ledger::{Work, WorkLine};

pub(crate) const EVENT: &str = "daily_usage";

/// The CLIs whose names are sent as they are. Any other agent is an endpoint you named, so it
/// goes as `endpoint`.
const NAMED_CLIS: &[&str] = &["opencode"];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct DailyUsage {
    /// Random, made on this computer.
    pub install: String,
    pub version: String,
    pub os: String,
    /// Each installed CLI and its version.
    pub clis: BTreeMap<String, String>,
    pub models: Vec<ModelUse>,
    pub features: Features,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ModelUse {
    pub cli: String,
    pub model: String,
    pub tokens: u64,
    /// The tokens' value at list prices, in dollars, to the cent.
    pub usd: f64,
    /// `plan`, `api_key`, or `local`.
    pub billing: &'static str,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct Features {
    /// Runs by workflow. A workflow you wrote goes as `custom`, since its name is yours.
    pub workflows: BTreeMap<String, u32>,
    /// Goals set, by runs and by the conversation.
    pub goals: u32,
    /// Ultracode was used: a turn went out with it, or a run started because of it.
    pub ultracode: bool,
    /// Mod files loaded.
    pub mods: usize,
}

/// What the event counts besides the ledgers, as the app finds them when it starts.
#[derive(Debug, Clone, Default)]
pub(crate) struct Found {
    pub clis: BTreeMap<String, String>,
    pub mods: usize,
}

/// The days from `from` up to but not including `until`.
pub(crate) fn count(
    install: &str,
    usage: &[LedgerLine],
    work: &[WorkLine],
    uses: &[UseLine],
    settings: &Settings,
    found: &Found,
    (from, until): (Date, Date),
) -> DailyUsage {
    let within = |day: Date| from <= day && day < until;
    let mut models: BTreeMap<(String, String, &'static str), (u64, f64)> = BTreeMap::new();
    for line in usage.iter().filter(|line| within(line.day)) {
        let (cli, model, billing) = model_of(line, settings);
        let entry = models.entry((cli, model, billing)).or_default();
        entry.0 += line.input + line.output;
        entry.1 += line.cost;
    }
    let mut models: Vec<ModelUse> = models
        .into_iter()
        .map(|((cli, model, billing), (tokens, usd))| ModelUse {
            cli,
            model,
            tokens,
            usd: (usd * 100.0).round() / 100.0,
            billing,
        })
        .collect();
    models.sort_by_key(|model| std::cmp::Reverse(model.tokens));

    let mut workflows: BTreeMap<String, u32> = BTreeMap::new();
    let mut goals = 0;
    let mut ultracode = false;
    for line in work.iter().filter(|line| within(line.day)) {
        if let Work::Run {
            workflow,
            goals: set,
            ultracode: sent,
            ..
        } = &line.what
        {
            let name = match workflow {
                Some(name) if argonauts_registry::builtin_preset(name).is_ok() => name.clone(),
                Some(_) => "custom".to_owned(),
                None => "unknown".to_owned(),
            };
            *workflows.entry(name).or_default() += 1;
            goals += set;
            ultracode |= sent;
        }
    }
    for line in uses.iter().filter(|line| within(line.day)) {
        match line.used {
            Use::Goal => goals += 1,
            Use::Ultracode => ultracode = true,
        }
    }

    DailyUsage {
        install: install.to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        os: std::env::consts::OS.to_owned(),
        clis: found.clis.clone(),
        models,
        features: Features {
            workflows,
            goals,
            ultracode,
            mods: found.mods,
        },
    }
}

/// Which CLI and model a line is, and how it was paid. A model is named only when it is one
/// Argonauts knows a public list price for, whatever runs it, so a model you or your company
/// named, such as a cloud deployment's id, never leaves the computer.
fn model_of(line: &LedgerLine, settings: &Settings) -> (String, String, &'static str) {
    let agent = line.agent.as_str();
    let model = line
        .model
        .as_deref()
        .map_or_else(|| "default".to_owned(), shareable_model);
    if KNOWN_AGENT_IDS.contains(&agent) || NAMED_CLIS.contains(&agent) {
        let billing = match settings.agents.get(agent).map(|agent| agent.auth) {
            Some(AuthMode::ApiKey) => "api_key",
            _ => "plan",
        };
        return (agent.to_owned(), model, billing);
    }
    let local = settings
        .endpoints
        .get(agent)
        .is_some_and(|endpoint| endpoint.local);
    (
        "endpoint".to_owned(),
        model,
        if local { "local" } else { "api_key" },
    )
}

/// A built-in model id as it may be sent, read without a context suffix such as `[1m]`, or
/// "other" for any id Argonauts has no list price for.
fn shareable_model(model: &str) -> String {
    let base = model.split_once('[').map_or(model, |(base, _)| base).trim();
    if builtin_price(base).is_some() {
        base.to_ascii_lowercase()
    } else {
        "other".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(agent: &str, model: &str) -> LedgerLine {
        LedgerLine {
            day: time::macros::date!(2026 - 10 - 08),
            repo: "/work/acme".to_owned(),
            run: None,
            agent: agent.to_owned(),
            model: Some(model.to_owned()),
            vendor: agent.to_owned(),
            input: 10,
            cached: 0,
            output: 0,
            cost: 0.0,
            estimated: false,
            partial: false,
            hour: None,
        }
    }

    /// Only a model Argonauts knows by its public id is named; an id a cloud, a gateway or a
    /// provider of your own gave it goes as "other".
    #[test]
    fn only_built_in_models_are_named() {
        let settings = Settings::default();
        let model = |agent: &str, id: &str| model_of(&line(agent, id), &settings).1;
        let arn = "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/abc";
        assert_eq!(model("claude-code", arn), "other");
        assert_eq!(model("opencode", "acme-gateway/team-model"), "other");
        assert_eq!(model("codex", "acme-prod-gpt"), "other");
        assert_eq!(model("claude-code", "claude-opus-5-5"), "claude-opus-5-5");
        assert_eq!(
            model("claude-code", "claude-opus-5-5[1m]"),
            "claude-opus-5-5"
        );
        assert_eq!(
            model("claude-code", "claude-haiku-4-5-20251001"),
            "claude-haiku-4-5-20251001"
        );
        assert_eq!(model("codex", "gpt-5.6-luna"), "gpt-5.6-luna");
    }
}
