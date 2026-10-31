//! Providers come from the model id, not the CLI, so a CLI that runs many providers' models
//! still keeps reviews and judging apart by provider.

use std::collections::BTreeSet;

use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::models::registry::provider::{UNKNOWN_PROVIDER, is_known};
use argonauts_core::models::registry::{ModelEntry, ModelRegistry};
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::ReviewAssignment;

use super::builtin::cli_default_provider;

/// A manifest's `provider` that means "ask the registry for each model".
const FROM_MODEL: &str = "from-model";

/// Model names and the provider that trained them. A prefix matches only up to a digit or a
/// separator, so `phind-codellama` isn't `phi`. Open-weight lines that run under many names
/// (code, distilled and sized variants) map to the provider that trained the line.
const MODEL_PREFIXES: &[(&str, &str)] = &[
    ("claude", "anthropic"),
    ("gpt", "openai"),
    ("gemini", "google"),
    ("gemma", "google"),
    ("codegemma", "google"),
    ("glm", "zhipu"),
    ("deepseek", "deepseek"),
    ("meta-llama", "meta"),
    ("codellama", "meta"),
    ("llama", "meta"),
    ("codeqwen", "alibaba"),
    ("qwen", "alibaba"),
    ("qwq", "alibaba"),
    ("mistral", "mistral"),
    ("mixtral", "mistral"),
    ("codestral", "mistral"),
    ("devstral", "mistral"),
    ("ministral", "mistral"),
    ("magistral", "mistral"),
    ("pixtral", "mistral"),
    ("phi", "microsoft"),
    ("granite", "ibm"),
    ("command-r", "cohere"),
    ("command-a", "cohere"),
    ("olmo", "ai2"),
    ("kimi", "moonshot"),
    ("minimax", "minimax"),
    ("nemotron", "nvidia"),
    ("starcoder", "bigcode"),
    ("falcon", "tii"),
    ("smollm", "huggingface"),
    ("internlm", "shanghai-ai-lab"),
    ("exaone", "lg"),
    ("grok", "xai"),
];

/// OpenRouter and Hugging Face publisher prefixes that name a provider whatever the model is
/// called.
const PUBLISHER_PREFIXES: &[(&str, &str)] = &[
    ("z-ai", "zhipu"),
    ("meta-llama", "meta"),
    ("mistralai", "mistral"),
    ("qwen", "alibaba"),
    ("moonshotai", "moonshot"),
    ("ibm-granite", "ibm"),
    ("x-ai", "xai"),
];

/// The provider a model id names, such as `anthropic`, `openai`, `google`, `zhipu`, or the
/// maker of an open-weight line like `meta`, `alibaba`, `mistral`, `deepseek` or `microsoft`.
/// `None` when the id doesn't say, and then the entry's own `provider`, else the adapter's
/// default, decides.
pub fn provider_from_model_id(model: &str) -> Option<&'static str> {
    let model = model.trim().to_ascii_lowercase();
    let mut segments = model.rsplit('/');
    let name = segments.next()?;
    let by_agent = segments.find_map(|agent_id| {
        PUBLISHER_PREFIXES
            .iter()
            .find(|(prefix, _)| *prefix == agent_id)
            .map(|&(_, provider)| provider)
    });
    by_agent.or_else(|| {
        MODEL_PREFIXES
            .iter()
            .find(|(prefix, _)| names_line(name, prefix))
            .map(|&(_, provider)| provider)
    })
}

/// `llama3.2`, `llama-3` and `llama` name the `llama` line; `llamafile` doesn't.
fn names_line(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix)
        .is_some_and(|rest| !rest.starts_with(|c: char| c.is_ascii_alphabetic()))
}

/// The model id first, then the entry's `provider`, then the built-in CLI's default, then a
/// manifest CLI's own `provider` unless it's `from-model`.
pub(crate) fn resolve_provider(
    model: &ModelRef,
    entry: Option<&ModelEntry>,
    manifest_provider: Option<&str>,
) -> String {
    let id = model
        .model
        .as_deref()
        .or_else(|| entry.and_then(|e| e.id.model.as_deref()));
    if let Some(provider) = id.and_then(provider_from_model_id) {
        return provider.to_owned();
    }
    if let Some(provider) = entry.map(|e| e.provider.as_str()).filter(|p| names(p)) {
        return provider.to_owned();
    }
    cli_default_provider(&model.agent)
        .or(manifest_provider.filter(|p| names(p)))
        .unwrap_or(UNKNOWN_PROVIDER)
        .to_owned()
}

fn names(provider: &str) -> bool {
    is_known(provider) && provider != FROM_MODEL
}

/// Refuse, don't guess: a model whose provider the registry can't tell (a `from-model` CLI
/// running a model with no known provider) in any role that compares providers, such as
/// reviewers, the judge, and the review ring. Each problem names the role and the model.
///
/// Reviewers and the ring compare providers only while `review.other_provider` is on, and the
/// models `approvals.decides` and `approvals.judge.second_model` name only while
/// `approvals.judge.different_provider` is.
///
/// It asks only `registry.provider`, so it agrees with every other provider comparison.
pub fn provider_problems(settings: &Settings, registry: &dyn ModelRegistry) -> Vec<String> {
    let role_workers = |role: RoleName| {
        settings
            .roles
            .get(&role)
            .map(|r| r.models.clone())
            .unwrap_or_default()
    };
    let other_provider = settings.review.other_provider;
    let judge_settings = &settings.approvals.judge;

    let mut comparing: Vec<(&str, ModelRef)> = Vec::new();
    if other_provider {
        comparing.extend(
            role_workers(RoleName::Reviewer)
                .into_iter()
                .map(|w| ("roles.reviewer", w)),
        );
    }
    comparing.extend(
        role_workers(RoleName::Judge)
            .into_iter()
            .map(|w| ("roles.judge", w)),
    );
    if judge_settings.different_provider {
        comparing.extend(
            settings
                .approvals
                .judge_model()
                .cloned()
                .map(|w| ("approvals.decides", w)),
        );
        comparing.extend(
            judge_settings
                .second_model
                .clone()
                .map(|w| ("approvals.judge.second_model", w)),
        );
    }
    if other_provider && settings.review.assign == ReviewAssignment::Ring {
        comparing.extend(
            settings
                .crew
                .members()
                .into_iter()
                .map(|w| ("the review ring", w)),
        );
    }

    let mut seen = BTreeSet::new();
    comparing
        .into_iter()
        .filter(|(role, worker)| seen.insert((*role, worker.clone())))
        .filter(|(_, worker)| !is_known(&registry.provider(worker)))
        .map(|(role, worker)| {
            format!(
                "{role} has {worker}, whose provider is unknown, and this role compares \
                 providers. Add a `provider` to its [[model]] entry in models.toml, or pick \
                 another model."
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::approvals::Decides;
    use argonauts_core::settings::roles::RoleSettings;

    use super::*;
    use crate::models::FileRegistry;

    #[test]
    fn provider_from_model_id() {
        let cases = [
            ("claude-sonnet-4-6", Some("anthropic")),
            ("claude-opus-4-6-thinking", Some("anthropic")),
            ("gpt-oss-120b-medium", Some("openai")),
            ("gpt-oss:20b", Some("openai")),
            ("openai/gpt-oss-120b", Some("openai")),
            ("gpt-6-luna", Some("openai")),
            ("gpt-5.6-terra", Some("openai")),
            ("gemini-3.6-flash-low", Some("google")),
            ("google/gemini-3.1-pro", Some("google")),
            ("z-ai/glm-5.3", Some("zhipu")),
            ("z-ai/some-future-name", Some("zhipu")),
            ("glm4:9b", Some("zhipu")),
            ("deepseek/deepseek-v4-pro", Some("deepseek")),
            ("deepseek-ai/DeepSeek-V3", Some("deepseek")),
            ("deepseek-r1:8b", Some("deepseek")),
            ("anthropic/claude-haiku-4.5", Some("anthropic")),
            ("llama3.2", Some("meta")),
            ("ollama/llama3.2:3b", Some("meta")),
            ("codellama:7b", Some("meta")),
            ("meta-llama/llama-4-maverick", Some("meta")),
            (
                "hf.co/bartowski/Meta-Llama-3.1-8B-Instruct-GGUF",
                Some("meta"),
            ),
            ("gemma3:12b", Some("google")),
            ("gemma4:e2b", Some("google")),
            ("google/gemma-3-27b-it", Some("google")),
            ("qwen2.5-coder:7b", Some("alibaba")),
            ("Qwen/Qwen3-Coder-30B", Some("alibaba")),
            ("qwq:32b", Some("alibaba")),
            ("mistral", Some("mistral")),
            ("mistral-small3.2", Some("mistral")),
            ("mixtral:8x7b", Some("mistral")),
            ("mistralai/devstral-small", Some("mistral")),
            ("phi4-mini", Some("microsoft")),
            ("microsoft/phi-4", Some("microsoft")),
            ("granite3.3:8b", Some("ibm")),
            ("command-r-plus", Some("cohere")),
            ("moonshotai/kimi-k2", Some("moonshot")),
            ("starcoder2:15b", Some("bigcode")),
            ("phind-codellama", None),
            ("llamafile", None),
            ("mystery-7b", None),
            ("opus", None),
            ("", None),
        ];
        for (model, provider) in cases {
            assert_eq!(super::provider_from_model_id(model), provider, "{model}");
        }

        let registry = FileRegistry::from_files(&Settings::default(), None, None);
        let provider = |w: &str| registry.provider(&w.parse().unwrap());
        assert_eq!(provider("claude-code/opus"), "anthropic");
        assert_eq!(provider("claude-code"), "anthropic");
        assert_eq!(provider("codex"), "openai");
        // One provider's open-weight line is still that provider.
        assert_eq!(provider("codex/gpt-oss:20b"), "openai");
        assert_eq!(provider("antigravity-cli/claude-sonnet-4-6"), "anthropic");
        assert_eq!(provider("opencode/openrouter/z-ai/glm-5"), "zhipu");
        assert_eq!(provider("openrouter/deepseek/deepseek-v4-pro"), "deepseek");
        assert_eq!(provider("opencode/ollama/llama3.2"), "meta");
        assert_eq!(provider("ollama/llama3.2"), "meta");
        assert_eq!(provider("ollama/gemma4:e2b"), "google");
        assert_eq!(provider("opencode/ollama/mystery-7b"), "unknown");
    }

    fn with_workers(workers: &[&str]) -> RoleSettings {
        RoleSettings {
            models: workers.iter().map(|w| w.parse().unwrap()).collect(),
            ..RoleSettings::default()
        }
    }

    #[test]
    fn provider_from_model_refuses_unknown_provider() {
        let mut settings = Settings::default();
        settings.roles.insert(
            RoleName::Reviewer,
            with_workers(&["claude-code/opus", "opencode/ollama/mystery-7b"]),
        );
        settings.roles.insert(
            RoleName::Judge,
            with_workers(&[
                "ollama/house-model:12b",
                "codex",
                "opencode/openrouter/z-ai/glm-5",
                "my-acp-agent/mystery-1",
            ]),
        );
        settings.approvals.decides = Decides::Model("my-agent/whatever".parse().unwrap());
        settings.review.assign = ReviewAssignment::Ring;
        settings.crew = vec![
            "claude-code".parse().unwrap(),
            "opencode/ollama/mystery-7b".parse().unwrap(),
        ]
        .into();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("models.toml");
        let cli = |id: &str, provider: &str| {
            format!(
                "[[cli]]\nid = \"{id}\"\ndisplay_name = \"{id}\"\nbinary = \"/opt/{id}\"\n\
                 read_only_args = []\nbuild_args = []\nprovider = \"{provider}\"\n"
            )
        };
        let clis = cli("my-acp-agent", FROM_MODEL) + &cli("my-agent", "mistral");
        std::fs::write(&file, &clis).unwrap();
        let registry = FileRegistry::from_files(&settings, Some(&file), None);
        assert!(registry.warnings().is_empty(), "{:?}", registry.warnings());

        // The registry and the check agree on every model.
        let provider = |w: &str| registry.provider(&w.parse().unwrap());
        assert_eq!(provider("my-agent/whatever"), "mistral");
        assert_eq!(provider("my-agent"), "mistral");
        assert_eq!(provider("my-acp-agent/mystery-1"), "unknown");
        assert_eq!(provider("my-acp-agent/claude-sonnet-4-6"), "anthropic");

        let problems = provider_problems(&settings, &registry);
        let refused: Vec<(&str, &str)> = [
            ("roles.reviewer", "opencode/ollama/mystery-7b"),
            ("roles.judge", "ollama/house-model:12b"),
            ("roles.judge", "my-acp-agent/mystery-1"),
            ("the review ring", "opencode/ollama/mystery-7b"),
        ]
        .into();
        assert_eq!(problems.len(), refused.len(), "{problems:#?}");
        for ((role, worker), problem) in refused.iter().zip(&problems) {
            assert!(
                problem.starts_with(&format!("{role} has {worker}, ")),
                "{problem}"
            );
        }

        for (role, worker) in &refused {
            assert_eq!(provider(worker), "unknown", "{role}");
        }

        // A provider from the registry entry clears it.
        std::fs::write(
            &file,
            clis + "[[model]]\nid = \"ollama/house-model:12b\"\nprovider = \"house\"\n\
                   kind = \"endpoint\"\naccess = \"local\"\ncost = { unit = \"none\" }\n",
        )
        .unwrap();
        let registry = FileRegistry::from_files(&settings, Some(&file), None);
        let problems = provider_problems(&settings, &registry);
        assert!(
            !problems.iter().any(|p| p.contains("house-model")),
            "{problems:#?}"
        );

        // With review across providers off, reviewers and the ring compare nothing.
        settings.review.other_provider = false;
        let problems = provider_problems(&settings, &registry);
        assert_eq!(problems.len(), 1, "{problems:#?}");
        assert!(problems[0].starts_with("roles.judge has my-acp-agent/mystery-1, "));
    }
}
