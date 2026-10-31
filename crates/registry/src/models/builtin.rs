//! Built-in entries. No model id is invented: Claude Code's aliases, each CLI's default
//! model, and the known endpoints are the only concrete names.

use argonauts_core::AuthMode;
use argonauts_core::base::names::ModelRef;
use argonauts_core::input::REASONING_LEVELS;
use argonauts_core::models::registry::{
    CostModel, CostUnit, ModelAccess, ModelEntry, RegistryKind,
};
use argonauts_core::settings::Settings;
pub use argonauts_core::settings::endpoints::{builtin_endpoint, endpoint_settings};

/// `claude --help` names these as aliases for the latest model of each line.
pub const CLAUDE_CODE_ALIASES: &[&str] = &["opus", "sonnet", "fable", "haiku"];

/// The CLIs whose default model has a known provider, as (adapter id, provider). OpenCode
/// isn't here because it runs many providers, so its provider always comes from the model.
const CLI_PROVIDERS: &[(&str, &str)] = &[
    ("claude-code", "anthropic"),
    ("codex", "openai"),
    ("antigravity-cli", "google"),
];

/// The provider of a built-in CLI's models when neither the model id nor an entry says.
pub(crate) fn cli_default_provider(adapter: &str) -> Option<&'static str> {
    CLI_PROVIDERS
        .iter()
        .find(|(id, _)| *id == adapter)
        .map(|&(_, provider)| provider)
}

/// Claude Code's aliases `opus`, `sonnet`, `fable`, and `haiku`, then the full Claude Code
/// model ids your settings name, and Claude Code's, Codex's, and agy's default model as
/// `claude-code`, `codex`, and `antigravity-cli` with no model, since none of the CLIs documents
/// its default's id. Access follows `agents.<id>.auth`. No entry lists effort levels, because no
/// CLI documents them per model.
pub fn builtin_models(settings: &Settings) -> Vec<ModelEntry> {
    let aliases = CLAUDE_CODE_ALIASES
        .iter()
        .map(|alias| cli_entry(settings, "claude-code", Some(alias)));
    let yours = claude_code_ids(settings)
        .into_iter()
        .map(|id| cli_entry(settings, "claude-code", Some(&id)));
    let defaults = CLI_PROVIDERS
        .iter()
        .map(|(adapter, _)| cli_entry(settings, adapter, None));
    aliases.chain(yours).chain(defaults).collect()
}

/// Claude Code can't list its models, so the full ids you named, such as
/// `agents.claude-code.model = "claude-opus-5-5"` or `claude-code/claude-sonnet-5` in a role,
/// join its aliases. In the order the settings name them, each once.
fn claude_code_ids(settings: &Settings) -> Vec<String> {
    let named = settings
        .agents
        .get("claude-code")
        .and_then(|agent| agent.model.clone());
    let members = settings.crew.members();
    let workers = std::iter::once(&settings.crew.main)
        .chain(&members)
        .chain(settings.roles.values().flat_map(|role| &role.models))
        .chain(&settings.narrator.model)
        .chain(settings.approvals.judge_model())
        .filter(|worker| worker.agent == "claude-code")
        .filter_map(|worker| worker.model.clone());
    let mut ids: Vec<String> = Vec::new();
    for id in named.into_iter().chain(workers) {
        if !CLAUDE_CODE_ALIASES.contains(&id.as_str()) && !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// The effort levels a CLI usually takes, for a model whose own levels aren't known:
/// `claude --effort`, Codex's common `model_reasoning_effort` values, and `agy --effort` as
/// their help lists them. Any other CLI or an endpoint gets every level.
pub fn usual_efforts(cli: &str) -> &'static [&'static str] {
    match cli {
        "claude-code" | "codex" => &["low", "medium", "high", "xhigh", "max"],
        "antigravity-cli" => &["low", "medium", "high", "max"],
        _ => REASONING_LEVELS,
    }
}

/// A CLI model reached through `adapter`: your plan's usage, or dollars when
/// `agents.<adapter>.auth = "api-key"`, priced by `agents.<adapter>.price` when you set one.
pub fn cli_entry(settings: &Settings, adapter: &str, model: Option<&str>) -> ModelEntry {
    let provider = model
        .and_then(super::provider_from_model_id)
        .or_else(|| cli_default_provider(adapter))
        .unwrap_or(argonauts_core::models::registry::provider::UNKNOWN_PROVIDER);
    let (access, cost) = cli_access(settings, adapter);
    ModelEntry {
        id: ModelRef::new(adapter, model.map(str::to_owned)),
        provider: provider.to_owned(),
        kind: RegistryKind::Cli,
        access,
        tools: true,
        cost,
        strengths: Vec::new(),
        good_for: Vec::new(),
        context_window: None,
        efforts: Vec::new(),
        aliases: Vec::new(),
        retired: false,
        successor: None,
        discovered: false,
        cli_default: false,
        default_effort: None,
        launch: None,
        tier: None,
        name: None,
        description: None,
        window: None,
        ram_bytes: None,
        thinking: None,
        variants: None,
        listed_cost: false,
    }
}

pub(crate) fn cli_access(settings: &Settings, adapter: &str) -> (ModelAccess, CostModel) {
    let agent = settings.agents.get(adapter);
    match agent.map(|a| a.auth).unwrap_or_default() {
        AuthMode::Subscription => (ModelAccess::Subscription, cost(CostUnit::PlanUsage)),
        AuthMode::ApiKey => {
            let price = agent.and_then(|a| a.price.as_ref());
            let cost = CostModel {
                unit: CostUnit::Usd,
                input_per_mtok: price.map(|p| p.input_per_mtok),
                output_per_mtok: price.map(|p| p.output_per_mtok),
                cached_input_per_mtok: price.and_then(|p| p.cached_input_per_mtok),
            };
            (ModelAccess::ApiKey, cost)
        }
    }
}

pub(crate) fn cost(unit: CostUnit) -> CostModel {
    CostModel {
        unit,
        input_per_mtok: None,
        output_per_mtok: None,
        cached_input_per_mtok: None,
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::TokenPrice;
    use argonauts_core::settings::agents::AgentSettings;

    use super::*;

    #[test]
    fn claude_aliases_follow_the_auth_setting() {
        let entries = builtin_models(&Settings::default());
        let ids: Vec<String> = entries.iter().map(|e| e.id.to_string()).collect();
        assert_eq!(
            ids,
            [
                "claude-code/opus",
                "claude-code/sonnet",
                "claude-code/fable",
                "claude-code/haiku",
                "claude-code",
                "codex",
                "antigravity-cli"
            ]
        );
        let providers: Vec<&str> = entries.iter().map(|e| e.provider.as_str()).collect();
        assert_eq!(
            providers,
            [
                "anthropic",
                "anthropic",
                "anthropic",
                "anthropic",
                "anthropic",
                "openai",
                "google"
            ]
        );
        assert!(entries.iter().all(|e| e.access == ModelAccess::Subscription
            && e.cost.unit == CostUnit::PlanUsage
            && e.efforts.is_empty()
            && !e.discovered));

        let mut settings = Settings::default();
        settings.agents.insert(
            "claude-code".to_owned(),
            AgentSettings {
                auth: AuthMode::ApiKey,
                price: Some(TokenPrice {
                    input_per_mtok: 3.0,
                    output_per_mtok: 15.0,
                    cached_input_per_mtok: None,
                }),
                ..AgentSettings::default()
            },
        );
        let opus = builtin_models(&settings).remove(0);
        assert_eq!(opus.access, ModelAccess::ApiKey);
        assert_eq!(opus.cost.unit, CostUnit::Usd);
        assert_eq!(opus.cost.output_per_mtok, Some(15.0));
    }

    #[test]
    fn claude_code_lists_the_full_ids_your_settings_name() {
        let mut settings = Settings::default();
        settings.agents.get_mut("claude-code").unwrap().model = Some("claude-opus-5-5".into());
        settings.crew = vec![
            "claude-code/claude-sonnet-5".parse().unwrap(),
            "claude-code/opus".parse().unwrap(),
            "codex/gpt-6-luna".parse().unwrap(),
        ]
        .into();
        settings.narrator.model = Some("claude-code/claude-opus-5-5".parse().unwrap());
        let ids: Vec<String> = builtin_models(&settings)
            .iter()
            .map(|e| e.id.to_string())
            .collect();
        assert_eq!(
            ids,
            [
                "claude-code/opus",
                "claude-code/sonnet",
                "claude-code/fable",
                "claude-code/haiku",
                "claude-code/claude-opus-5-5",
                "claude-code/claude-sonnet-5",
                "claude-code",
                "codex",
                "antigravity-cli"
            ]
        );
    }

    #[test]
    fn each_cli_has_its_usual_effort_levels() {
        assert_eq!(
            usual_efforts("claude-code"),
            ["low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(
            usual_efforts("antigravity-cli"),
            ["low", "medium", "high", "max"]
        );
        assert_eq!(usual_efforts("ollama"), REASONING_LEVELS);
        for cli in ["claude-code", "codex", "antigravity-cli"] {
            assert!(
                usual_efforts(cli)
                    .iter()
                    .all(|l| argonauts_core::input::EFFORT_LEVELS.contains(l))
            );
        }
    }
}
