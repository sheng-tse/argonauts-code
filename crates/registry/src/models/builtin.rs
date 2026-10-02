//! Built-in entries. No model id is invented: Claude Code's aliases, each harness's default
//! model, and the endpoint presets are the only concrete names.

use argonauts_core::AuthMode;
use argonauts_core::input::EFFORT_LEVELS;
use argonauts_core::names::WorkerRef;
use argonauts_core::registry::{CostModel, CostUnit, ModelAccess, ModelEntry, RegistryKind};
use argonauts_core::settings::Settings;
pub use argonauts_core::settings::endpoints::{builtin_endpoint, endpoint_settings};

/// `claude --help` names these as aliases for the latest model of each family.
pub const CLAUDE_CODE_ALIASES: &[&str] = &["opus", "sonnet", "fable", "haiku"];

/// The harnesses whose default model has a known family, as (adapter id, family). OpenCode
/// isn't here because it runs many providers, so its family always comes from the model.
const HARNESS_FAMILIES: &[(&str, &str)] = &[
    ("claude-code", "claude"),
    ("codex", "gpt"),
    ("antigravity-cli", "gemini"),
];

/// The family of a built-in harness's models when neither the model id nor an entry says.
pub(crate) fn harness_default_family(adapter: &str) -> Option<&'static str> {
    HARNESS_FAMILIES
        .iter()
        .find(|(id, _)| *id == adapter)
        .map(|&(_, family)| family)
}

/// Claude Code's aliases `opus`, `sonnet`, `fable`, and `haiku`, then the full Claude Code
/// model ids your settings name, and Claude Code's, Codex's, and agy's default model as
/// `claude-code`, `codex`, and `antigravity-cli` with no model, since none of the CLIs documents
/// its default's id. Access follows `agents.<id>.auth`. No entry lists effort levels, because no
/// CLI documents them per model.
pub fn builtin_models(settings: &Settings) -> Vec<ModelEntry> {
    let aliases = CLAUDE_CODE_ALIASES
        .iter()
        .map(|alias| harness_entry(settings, "claude-code", Some(alias)));
    let yours = claude_code_ids(settings)
        .into_iter()
        .map(|id| harness_entry(settings, "claude-code", Some(&id)));
    let defaults = HARNESS_FAMILIES
        .iter()
        .map(|(adapter, _)| harness_entry(settings, adapter, None));
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
    let workers = std::iter::once(&settings.default_agent)
        .chain(&settings.crew)
        .chain(settings.roles.values().flat_map(|role| &role.workers))
        .chain(&settings.narrator.model)
        .chain(&settings.approvals.approver.model)
        .filter(|worker| worker.worker == "claude-code")
        .filter_map(|worker| worker.model.clone());
    let mut ids: Vec<String> = Vec::new();
    for id in named.into_iter().chain(workers) {
        if !CLAUDE_CODE_ALIASES.contains(&id.as_str()) && !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// The effort levels a harness usually takes, for a model whose own levels aren't known:
/// `claude --effort`, Codex's common `model_reasoning_effort` values, and `agy --effort` as
/// their help lists them. Any other harness or an endpoint gets every level.
pub fn usual_efforts(harness: &str) -> &'static [&'static str] {
    match harness {
        "claude-code" | "codex" => &["low", "medium", "high", "xhigh", "max"],
        "antigravity-cli" => &["low", "medium", "high", "max"],
        _ => EFFORT_LEVELS,
    }
}

/// A harness model reached through `adapter`: your plan's quota, or dollars when
/// `agents.<adapter>.auth = "api-key"`, priced by `agents.<adapter>.price` when you set one.
pub(crate) fn harness_entry(settings: &Settings, adapter: &str, model: Option<&str>) -> ModelEntry {
    let family = model
        .and_then(super::family_from_model_id)
        .or_else(|| harness_default_family(adapter))
        .unwrap_or(super::family::UNKNOWN_FAMILY);
    let (access, cost) = harness_access(settings, adapter);
    ModelEntry {
        id: WorkerRef::new(adapter, model.map(str::to_owned)),
        family: family.to_owned(),
        kind: RegistryKind::Harness,
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
    }
}

pub(crate) fn harness_access(settings: &Settings, adapter: &str) -> (ModelAccess, CostModel) {
    let agent = settings.agents.get(adapter);
    match agent.map(|a| a.auth).unwrap_or_default() {
        AuthMode::Subscription => (ModelAccess::Subscription, cost(CostUnit::Quota)),
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
        let families: Vec<&str> = entries.iter().map(|e| e.family.as_str()).collect();
        assert_eq!(
            families,
            [
                "claude", "claude", "claude", "claude", "claude", "gpt", "gemini"
            ]
        );
        assert!(entries.iter().all(|e| e.access == ModelAccess::Subscription
            && e.cost.unit == CostUnit::Quota
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
        ];
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
    fn each_harness_has_its_usual_effort_levels() {
        assert_eq!(
            usual_efforts("claude-code"),
            ["low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(
            usual_efforts("antigravity-cli"),
            ["low", "medium", "high", "max"]
        );
        assert_eq!(usual_efforts("ollama"), EFFORT_LEVELS);
        for harness in ["claude-code", "codex", "antigravity-cli"] {
            assert!(
                usual_efforts(harness)
                    .iter()
                    .all(|l| EFFORT_LEVELS.contains(l))
            );
        }
    }
}
