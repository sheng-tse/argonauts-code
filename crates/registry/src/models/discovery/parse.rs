//! What each discovery source prints, as registry entries. None of them costs a model call.

use argonauts_core::input::EFFORT_LEVELS;
use argonauts_core::names::ModelRef;
use argonauts_core::registry::{CostModel, CostUnit, ModelAccess, ModelEntry, RegistryKind};
use argonauts_core::settings::Settings;
use serde_json::Value;

use crate::models::builtin::{cli_entry, cost};
use crate::models::provider_from_model_id;
use argonauts_core::registry::provider::UNKNOWN_PROVIDER;

/// `agy --output-format json models`: the list sits under `command.data.models`, each model
/// with the label agy's own picker shows. agy names thinking levels as separate model ids
/// (`gemini-3.8-flash-high`, "Gemini 3.8 Flash (High)"), and its `--effort` picks another of
/// those ids rather than changing how the named one thinks, so a model named for a level takes
/// that one only. A model named without one keeps `efforts` empty, and the CLI layer of the
/// catalog takes that to mean it takes none.
pub fn parse_agy_models(json: &str) -> Result<Vec<ModelEntry>, String> {
    let reply: Value =
        serde_json::from_str(json).map_err(|e| format!("agy's model list isn't JSON: {e}"))?;
    let models = reply
        .pointer("/command/data/models")
        .or_else(|| reply.get("models"))
        .and_then(Value::as_array)
        .ok_or("agy's reply has no model list under command.data.models")?;
    let settings = Settings::default();
    Ok(models
        .iter()
        .filter_map(|model| {
            let id = model["id"].as_str()?;
            let mut entry = discovered(cli_entry(&settings, "antigravity-cli", Some(id)));
            entry.name = plain(&model["label"]);
            if let Some(level) = agy_level(id, entry.name.as_deref()) {
                entry.efforts = vec![level.to_owned()];
                entry.default_effort = Some(level.to_owned());
            }
            Some(entry)
        })
        .collect())
}

/// The thinking levels agy names its models for. Other words a name may end with, such as
/// Ultra, a Gemini tier, are no level of agy's.
const AGY_LEVELS: [&str; 3] = ["low", "medium", "high"];

/// The level a model is named for: the one its label ends with, as in "Gemini 3.8 Flash
/// (High)", else the one its id ends with when it has no label.
fn agy_level(id: &str, label: Option<&str>) -> Option<&'static str> {
    let word = match label {
        Some(label) => label.strip_suffix(')')?.rsplit_once('(')?.1,
        None => id.rsplit_once('-')?.1,
    };
    let level = AGY_LEVELS
        .into_iter()
        .find(|level| level.eq_ignore_ascii_case(word.trim()))?;
    // The id names it too, so a label alone never makes a level.
    id.ends_with(&format!("-{level}")).then_some(level)
}

/// `codex debug models`, with each model's name, description, reasoning levels, and the level
/// it starts at. Models Codex hides from its own picker are left out, and so are levels
/// `/effort` doesn't know. Codex's picker lists them by `priority`, lowest first, whatever order
/// the list comes in, and runs the first one when you name none, marking it "(default)".
pub fn parse_codex_models(json: &str) -> Result<Vec<ModelEntry>, String> {
    let reply: Value =
        serde_json::from_str(json).map_err(|e| format!("Codex's model catalog isn't JSON: {e}"))?;
    let models = reply["models"]
        .as_array()
        .ok_or("Codex's catalog has no `models` list")?;
    let settings = Settings::default();
    let mut listed: Vec<&Value> = models
        .iter()
        .filter(|model| model["visibility"].as_str() != Some("hide"))
        .collect();
    listed.sort_by_key(|model| model["priority"].as_i64().unwrap_or(i64::MAX));
    let default = listed
        .iter()
        .filter_map(|model| Some((model["priority"].as_i64()?, model["slug"].as_str()?)))
        .min_by_key(|(priority, _)| *priority)
        .map(|(_, slug)| slug);
    let known = |level: &&str| EFFORT_LEVELS.contains(level);
    Ok(listed
        .into_iter()
        .filter_map(|model| {
            let slug = model["slug"].as_str()?;
            let mut entry = discovered(cli_entry(&settings, "codex", Some(slug)));
            entry.efforts = model["supported_reasoning_levels"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|level| level["effort"].as_str())
                .filter(known)
                .map(str::to_owned)
                .collect();
            entry.name = plain(&model["display_name"]);
            entry.description = plain(&model["description"]);
            // The largest window Codex offers for it, which a session asks for. Codex's own
            // default is `context_window`, smaller for most models.
            entry.context_window = model["max_context_window"]
                .as_u64()
                .or(model["context_window"].as_u64());
            entry.cli_default = default == Some(slug);
            entry.default_effort = model["default_reasoning_level"]
                .as_str()
                .filter(known)
                .map(str::to_owned);
            Some(entry)
        })
        .collect())
}

/// An OpenAI-compatible `GET /models` reply from the endpoint `endpoint`. OpenRouter's reply
/// also carries prices per token, the context length with its top provider's cap, and whether
/// a model takes tools; vLLM's the window it serves each model at (`max_model_len`);
/// llama-server's the most its model was trained for (`meta.n_ctx_train`); a plain server's
/// ids only. Ollama lists `name:latest`, so the bare name becomes an alias.
pub fn parse_openai_models(
    endpoint: &str,
    local: bool,
    json: &str,
) -> Result<Vec<ModelEntry>, String> {
    let reply: Value = serde_json::from_str(json)
        .map_err(|e| format!("{endpoint}'s model list isn't JSON: {e}"))?;
    let models = reply["data"]
        .as_array()
        .ok_or_else(|| format!("{endpoint}'s model list has no `data` array"))?;
    Ok(models
        .iter()
        .filter_map(|model| endpoint_entry(endpoint, local, model))
        .collect())
}

pub(super) fn endpoint_entry(endpoint: &str, local: bool, model: &Value) -> Option<ModelEntry> {
    let id = model["id"].as_str()?;
    // An id that can't name a model couldn't be run either.
    let worker: ModelRef = format!("{endpoint}/{id}").parse().ok()?;
    let supports = |parameter: &str| {
        model["supported_parameters"]
            .as_array()
            .is_some_and(|list| list.iter().any(|p| p.as_str() == Some(parameter)))
    };
    let aliases = id
        .strip_suffix(":latest")
        .map(|bare| vec![bare.to_owned()])
        .unwrap_or_default();
    Some(ModelEntry {
        id: worker,
        provider: provider_from_model_id(id)
            .unwrap_or(UNKNOWN_PROVIDER)
            .to_owned(),
        kind: RegistryKind::Endpoint,
        access: if local {
            ModelAccess::Local
        } else {
            ModelAccess::ApiKey
        },
        tools: supports("tools"),
        cost: if local {
            cost(CostUnit::None)
        } else {
            usd_per_mtok(&model["pricing"])
        },
        strengths: Vec::new(),
        good_for: Vec::new(),
        context_window: tokens(&model["context_length"])
            .or(tokens(&model["max_model_len"]))
            .or(tokens(&model["meta"]["n_ctx_train"])),
        efforts: Vec::new(),
        aliases,
        retired: false,
        successor: None,
        discovered: true,
        cli_default: false,
        default_effort: None,
        launch: None,
        tier: None,
        name: None,
        description: None,
        // What the server holds it to: vLLM's own cap, or OpenRouter's top provider's.
        window: tokens(&model["max_model_len"])
            .or(tokens(&model["top_provider"]["context_length"])),
        ram_bytes: None,
        thinking: None,
        variants: None,
        listed_cost: false,
    })
}

/// A count of tokens, which a server may also send as a float.
pub(super) fn tokens(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| {
            value
                .as_f64()
                .filter(|n| n.is_finite() && *n >= 1.0)
                .map(|n| n as u64)
        })
        .filter(|n| *n > 0)
}

/// OpenRouter prices are dollars per token, as strings.
fn usd_per_mtok(pricing: &Value) -> CostModel {
    let per_mtok = |key: &str| {
        let value = &pricing[key];
        value
            .as_str()
            .and_then(|s| s.trim().parse::<f64>().ok())
            .or_else(|| value.as_f64())
            .filter(|price| price.is_finite() && *price >= 0.0)
            .map(|per_token| per_token * 1_000_000.0)
    };
    CostModel {
        unit: CostUnit::Usd,
        input_per_mtok: per_mtok("prompt"),
        output_per_mtok: per_mtok("completion"),
        cached_input_per_mtok: per_mtok("input_cache_read"),
    }
}

/// A CLI's own words, without control characters, which would move the cursor or recolor the
/// screen where they're shown.
pub(super) fn plain(value: &Value) -> Option<String> {
    plain_text(value.as_str()?)
}

/// `text` without control characters or the spaces around it, `None` when nothing is left.
pub(super) fn plain_text(text: &str) -> Option<String> {
    let text: String = text.chars().filter(|c| !c.is_control()).collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn discovered(mut entry: ModelEntry) -> ModelEntry {
    entry.discovered = true;
    entry
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGY: &str = include_str!("../../../tests/fixtures/agy/models.json");
    const CODEX: &str = include_str!("../../../tests/fixtures/codex/debug-models.json");
    const OPENROUTER: &str =
        include_str!("../../../tests/fixtures/openai-models/synthetic/openrouter.json");
    const OLLAMA: &str =
        include_str!("../../../tests/fixtures/openai-models/synthetic/ollama.json");

    fn ids(entries: &[ModelEntry]) -> Vec<String> {
        entries.iter().map(|e| e.id.to_string()).collect()
    }

    /// Only agy's three levels count, named by the label and the id alike, so a Gemini tier
    /// such as Ultra, or a word such as Thinking, is no level.
    #[test]
    fn an_agy_level_is_low_medium_or_high_in_the_label_and_the_id() {
        assert_eq!(
            agy_level("gemini-3.8-flash-high", Some("Gemini 3.8 Flash (High)")),
            Some("high")
        );
        assert_eq!(agy_level("gemini-3.6-flash-low", None), Some("low"));
        for (id, label) in [
            ("gemini-3.5-ultra", Some("Gemini 3.5 Ultra")),
            ("gemini-3.5-ultra", None),
            ("gemini-4-max", Some("Gemini 4 (Max)")),
            ("gemini-4-minimal", None),
            (
                "claude-opus-4-6-thinking",
                Some("Claude Opus 4.6 (Thinking)"),
            ),
            ("claude-sonnet-4-6", Some("Claude Sonnet 4.6 (Thinking)")),
            ("gemini-3.8-flash", Some("Gemini 3.8 Flash (High)")),
            ("gemini-3.8-flash-high", Some("Gemini 3.8 Flash")),
        ] {
            assert_eq!(agy_level(id, label), None, "{id} {label:?}");
        }
    }

    #[test]
    fn discovery_parsers() {
        let agy = parse_agy_models(AGY).unwrap();
        assert_eq!(agy.len(), 14);
        assert_eq!(
            agy[0].id.to_string(),
            "antigravity-cli/gemini-3.8-flash-high"
        );
        let provider = |id: &str| {
            agy.iter()
                .find(|e| e.id.model.as_deref() == Some(id))
                .map(|e| e.provider.as_str())
        };
        assert_eq!(provider("gemini-3.6-flash-low"), Some("google"));
        assert_eq!(provider("claude-sonnet-4-6"), Some("anthropic"));
        assert_eq!(provider("claude-opus-4-6-thinking"), Some("anthropic"));
        assert_eq!(provider("gpt-oss-120b-medium"), Some("openai"));
        assert_eq!(agy[0].name.as_deref(), Some("Gemini 3.8 Flash (High)"));
        let fixed = |id: &str| {
            agy.iter()
                .find(|e| e.id.model.as_deref() == Some(id))
                .map(|e| (e.efforts.clone(), e.default_effort.clone()))
                .unwrap()
        };
        let only = |level: &str| (vec![level.to_owned()], Some(level.to_owned()));
        assert_eq!(fixed("gemini-3.8-flash-high"), only("high"));
        assert_eq!(fixed("gemini-3.6-flash-low"), only("low"));
        assert_eq!(fixed("gpt-oss-120b-medium"), only("medium"));
        assert_eq!(fixed("claude-sonnet-4-6"), (Vec::new(), None));
        assert_eq!(fixed("claude-opus-4-6-thinking"), (Vec::new(), None));
        for entry in &agy {
            assert!(entry.efforts.len() <= 1, "{}", entry.id);
            assert!(entry.discovered);
            assert_eq!(entry.kind, RegistryKind::Cli);
            assert_eq!(entry.access, ModelAccess::Subscription);
        }

        let codex = parse_codex_models(CODEX).unwrap();
        assert_eq!(
            ids(&codex),
            [
                "codex/gpt-6-astra",
                "codex/gpt-6-sol",
                "codex/gpt-6-luna",
                "codex/gpt-5.6-sol",
                "codex/gpt-5.6-terra",
                "codex/gpt-5.6-luna",
                "codex/gpt-5.5"
            ]
        );
        let all = ["low", "medium", "high", "xhigh", "max", "ultra"];
        let efforts: Vec<(&str, &[String])> = codex
            .iter()
            .map(|e| (e.id.model.as_deref().unwrap(), e.efforts.as_slice()))
            .collect();
        for (model, levels) in &efforts {
            let known = match *model {
                "gpt-5.5" => 4,
                "gpt-6-luna" | "gpt-5.6-luna" => 5,
                _ => 6,
            };
            assert_eq!(*levels, &all[..known], "{model}");
        }
        assert!(
            codex.iter().all(|e| e.provider == "openai"
                && e.discovered
                && e.cost.unit == CostUnit::PlanUsage)
        );
        // The largest window Codex offers: 872k for all but one, whose default is all it has.
        let window = |model: &str| {
            let listed = codex.iter().find(|e| e.id.model.as_deref() == Some(model));
            listed.and_then(|e| e.context_window)
        };
        assert_eq!(window("gpt-5.6-luna"), Some(872_000));
        assert_eq!(window("gpt-5.5"), Some(272_000));
        let smaller = codex
            .iter()
            .filter(|e| e.context_window != Some(872_000))
            .count();
        assert_eq!(smaller, 1, "only gpt-5.5");

        let openrouter = parse_openai_models("openrouter", false, OPENROUTER).unwrap();
        assert_eq!(
            ids(&openrouter),
            [
                "openrouter/z-ai/glm-5.3",
                "openrouter/deepseek/deepseek-v4-pro",
                "openrouter/openai/gpt-oss-120b",
                "openrouter/meta-llama/llama-4-maverick"
            ]
        );
        let providers: Vec<&str> = openrouter.iter().map(|e| e.provider.as_str()).collect();
        assert_eq!(providers, ["zhipu", "deepseek", "openai", "meta"]);
        let glm = &openrouter[0];
        assert_eq!(glm.kind, RegistryKind::Endpoint);
        assert_eq!(glm.access, ModelAccess::ApiKey);
        assert!(glm.tools);
        assert_eq!(glm.context_window, Some(202_752));
        assert_eq!(glm.cost.unit, CostUnit::Usd);
        let close = |a: Option<f64>, b: f64| a.is_some_and(|a| (a - b).abs() < 1e-9);
        assert!(close(glm.cost.input_per_mtok, 0.6), "{:?}", glm.cost);
        assert!(close(glm.cost.output_per_mtok, 2.2), "{:?}", glm.cost);
        assert!(!openrouter[2].tools);
        assert!(
            openrouter
                .iter()
                .all(|e| e.efforts.is_empty() && e.discovered)
        );

        let ollama = parse_openai_models("ollama", true, OLLAMA).unwrap();
        assert_eq!(
            ids(&ollama),
            [
                "ollama/llama3.2:latest",
                "ollama/gemma3:12b",
                "ollama/gpt-oss:20b"
            ]
        );
        assert_eq!(ollama[0].aliases, ["llama3.2"]);
        assert!(ollama[1].aliases.is_empty());
        assert_eq!(ollama[2].provider, "openai");
        assert!(ollama.iter().all(|e| e.access == ModelAccess::Local
            && e.cost.unit == CostUnit::None
            && e.efforts.is_empty()));

        assert!(parse_agy_models("{}").is_err());
        assert!(parse_codex_models("not json").is_err());
        assert!(parse_openai_models("x", false, "{\"object\":\"list\"}").is_err());
    }

    /// Codex 0.159 lists gpt-6.1-sol second, with the lowest priority, and runs it when no
    /// model is named. A hidden model never counts, however low its priority.
    #[test]
    fn codex_default_is_the_listed_model_with_the_lowest_priority() {
        let json = r#"{"models": [
            {"slug": "gpt-6-astra", "priority": 2, "default_reasoning_level": "low"},
            {"slug": "gpt-6.1-sol", "priority": 1, "default_reasoning_level": "low",
             "display_name": "GPT-6.1-Sol\u001b[2J", "description": " Latest workhorse. "},
            {"slug": "gpt-6-sol", "priority": 3, "default_reasoning_level": "medium"},
            {"slug": "gpt-reserve", "priority": 0, "visibility": "hide"},
            {"slug": "gpt-odd", "priority": 4, "default_reasoning_level": "turbo"}
        ]}"#;
        let codex = parse_codex_models(json).unwrap();
        let defaults: Vec<&str> = codex
            .iter()
            .filter(|entry| entry.cli_default)
            .filter_map(|entry| entry.id.model.as_deref())
            .collect();
        assert_eq!(defaults, ["gpt-6.1-sol"]);
        assert_eq!(
            ids(&codex),
            [
                "codex/gpt-6.1-sol",
                "codex/gpt-6-astra",
                "codex/gpt-6-sol",
                "codex/gpt-odd"
            ],
            "in the order Codex's picker lists them"
        );
        assert_eq!(codex[0].name.as_deref(), Some("GPT-6.1-Sol[2J"));
        assert_eq!(codex[0].description.as_deref(), Some("Latest workhorse."));
        assert_eq!(codex[1].name, None);
        let effort = |model: &str| {
            codex
                .iter()
                .find(|entry| entry.id.model.as_deref() == Some(model))
                .and_then(|entry| entry.default_effort.clone())
        };
        assert_eq!(effort("gpt-6-sol").as_deref(), Some("medium"));
        assert_eq!(effort("gpt-odd"), None, "a level /effort doesn't know");

        let fixture = parse_codex_models(CODEX).unwrap();
        let default = fixture.iter().find(|entry| entry.cli_default).unwrap();
        assert_eq!(default.id.to_string(), "codex/gpt-6-astra");
        assert_eq!(default.default_effort.as_deref(), Some("low"));

        let unranked = r#"{"models": [{"slug": "a"}, {"slug": "b"}]}"#;
        let unranked = parse_codex_models(unranked).unwrap();
        assert!(unranked.iter().all(|entry| !entry.cli_default));
    }
}
