//! Families come from the model id, not the harness, so a harness that runs many providers
//! still keeps reviews and judging across families.

use std::collections::BTreeSet;

use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::registry::{ModelEntry, ModelRegistry};
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::ReviewAssignment;

use super::builtin::harness_default_family;

pub(crate) const UNKNOWN_FAMILY: &str = "unknown";

/// A manifest's `family` that means "ask the registry for each model".
const FROM_MODEL: &str = "from-model";

/// Checked in order, so `gpt-oss` wins over `gpt`. A prefix matches only up to a digit or a
/// separator, so `phind-codellama` isn't `phi`. Open-weight lines that run under many names
/// (code, distilled and sized variants) map to the family they were trained from.
const MODEL_PREFIXES: &[(&str, &str)] = &[
    ("claude", "claude"),
    ("gpt-oss", "gpt-oss"),
    ("gpt", "gpt"),
    ("gemini", "gemini"),
    ("glm", "glm"),
    ("deepseek", "deepseek"),
    ("meta-llama", "llama"),
    ("codellama", "llama"),
    ("llama", "llama"),
    ("codegemma", "gemma"),
    ("gemma", "gemma"),
    ("codeqwen", "qwen"),
    ("qwen", "qwen"),
    ("qwq", "qwen"),
    ("mistral", "mistral"),
    ("mixtral", "mistral"),
    ("codestral", "mistral"),
    ("devstral", "mistral"),
    ("ministral", "mistral"),
    ("magistral", "mistral"),
    ("pixtral", "mistral"),
    ("phi", "phi"),
    ("granite", "granite"),
    ("command-r", "command"),
    ("command-a", "command"),
    ("olmo", "olmo"),
    ("kimi", "kimi"),
    ("minimax", "minimax"),
    ("nemotron", "nemotron"),
    ("starcoder", "starcoder"),
    ("falcon", "falcon"),
    ("smollm", "smollm"),
    ("internlm", "internlm"),
    ("exaone", "exaone"),
    ("grok", "grok"),
];

/// OpenRouter and Hugging Face vendor prefixes that name a family whatever the model is called.
const VENDOR_FAMILIES: &[(&str, &str)] = &[
    ("z-ai", "glm"),
    ("meta-llama", "llama"),
    ("mistralai", "mistral"),
    ("qwen", "qwen"),
    ("moonshotai", "kimi"),
    ("ibm-granite", "granite"),
    ("x-ai", "grok"),
];

/// The family a model id names, such as `claude`, `gpt`, `gpt-oss`, `gemini`, `glm`, or an
/// open-weight line like `llama`, `gemma`, `qwen`, `mistral`, `deepseek` or `phi`. `None`
/// when the id doesn't say, and then the entry's own `family`, else the adapter's default,
/// decides.
pub fn family_from_model_id(model: &str) -> Option<&'static str> {
    let model = model.trim().to_ascii_lowercase();
    let mut segments = model.rsplit('/');
    let name = segments.next()?;
    let by_vendor = segments.find_map(|vendor| {
        VENDOR_FAMILIES
            .iter()
            .find(|(prefix, _)| *prefix == vendor)
            .map(|&(_, family)| family)
    });
    by_vendor.or_else(|| {
        MODEL_PREFIXES
            .iter()
            .find(|(prefix, _)| names_line(name, prefix))
            .map(|&(_, family)| family)
    })
}

/// `llama3.2`, `llama-3` and `llama` name the `llama` line; `llamafile` doesn't.
fn names_line(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix)
        .is_some_and(|rest| !rest.starts_with(|c: char| c.is_ascii_alphabetic()))
}

/// The model id first, then the entry's `family`, then the built-in harness's default, then a
/// manifest harness's own `family` unless it's `from-model`.
pub(crate) fn resolve_family(
    worker: &WorkerRef,
    entry: Option<&ModelEntry>,
    manifest_family: Option<&str>,
) -> String {
    let model = worker
        .model
        .as_deref()
        .or_else(|| entry.and_then(|e| e.id.model.as_deref()));
    if let Some(family) = model.and_then(family_from_model_id) {
        return family.to_owned();
    }
    if let Some(family) = entry.map(|e| e.family.as_str()).filter(|f| is_known(f)) {
        return family.to_owned();
    }
    harness_default_family(&worker.worker)
        .or(manifest_family.filter(|f| is_known(f)))
        .unwrap_or(UNKNOWN_FAMILY)
        .to_owned()
}

fn is_known(family: &str) -> bool {
    !family.is_empty() && family != UNKNOWN_FAMILY && family != FROM_MODEL
}

/// Refuse, don't guess: a worker whose family the registry can't tell (a `from-model` harness
/// running a model with no known family) in any role that compares families, such as
/// reviewers, judges, the adjudicator, the approver, and the review ring. Each problem names
/// the role and the worker.
///
/// Reviewers and the ring compare families only while `review.other_provider` is on, and the
/// approver only while `approvals.approver.different_provider` is.
///
/// It asks only `registry.family`, so it agrees with every other family comparison.
pub fn family_problems(settings: &Settings, registry: &dyn ModelRegistry) -> Vec<String> {
    let role_workers = |role: RoleName| {
        settings
            .roles
            .get(&role)
            .map(|r| r.workers.clone())
            .unwrap_or_default()
    };
    let other_provider = settings.review.other_provider;
    let approver = &settings.approvals.approver;

    let mut seats: Vec<(&str, WorkerRef)> = Vec::new();
    if other_provider {
        seats.extend(
            role_workers(RoleName::Reviewer)
                .into_iter()
                .map(|w| ("roles.reviewer", w)),
        );
    }
    seats.extend(
        role_workers(RoleName::Judge)
            .into_iter()
            .map(|w| ("roles.judge", w)),
    );
    seats.extend(
        role_workers(RoleName::Adjudicator)
            .into_iter()
            .map(|w| ("roles.adjudicator", w)),
    );
    if approver.different_provider {
        seats.extend(
            role_workers(RoleName::Approver)
                .into_iter()
                .map(|w| ("roles.approver", w)),
        );
        seats.extend(
            settings
                .approvals
                .approver_model()
                .cloned()
                .map(|w| ("approvals.decides", w)),
        );
        seats.extend(
            approver
                .second_model
                .clone()
                .map(|w| ("approvals.approver.second_model", w)),
        );
    }
    if other_provider && settings.review.assign == ReviewAssignment::Ring {
        seats.extend(
            settings
                .crew
                .members()
                .into_iter()
                .map(|w| ("the review ring", w)),
        );
    }

    let mut seen = BTreeSet::new();
    seats
        .into_iter()
        .filter(|(role, worker)| seen.insert((*role, worker.clone())))
        .filter(|(_, worker)| !is_known(&registry.family(worker)))
        .map(|(role, worker)| {
            format!(
                "{role} has {worker}, whose provider is unknown, and this role compares \
                 providers. Add a `family` to its [[model]] entry in models.toml, or pick \
                 another worker."
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
    fn family_from_model_id() {
        let cases = [
            ("claude-sonnet-4-6", Some("claude")),
            ("claude-opus-4-6-thinking", Some("claude")),
            ("gpt-oss-120b-medium", Some("gpt-oss")),
            ("gpt-oss:20b", Some("gpt-oss")),
            ("openai/gpt-oss-120b", Some("gpt-oss")),
            ("gpt-6-luna", Some("gpt")),
            ("gpt-5.6-terra", Some("gpt")),
            ("gemini-3.6-flash-low", Some("gemini")),
            ("google/gemini-3.1-pro", Some("gemini")),
            ("z-ai/glm-5.3", Some("glm")),
            ("z-ai/some-future-name", Some("glm")),
            ("glm4:9b", Some("glm")),
            ("deepseek/deepseek-v4-pro", Some("deepseek")),
            ("deepseek-ai/DeepSeek-V3", Some("deepseek")),
            ("deepseek-r1:8b", Some("deepseek")),
            ("anthropic/claude-haiku-4.5", Some("claude")),
            ("llama3.2", Some("llama")),
            ("ollama/llama3.2:3b", Some("llama")),
            ("codellama:7b", Some("llama")),
            ("meta-llama/llama-4-maverick", Some("llama")),
            (
                "hf.co/bartowski/Meta-Llama-3.1-8B-Instruct-GGUF",
                Some("llama"),
            ),
            ("gemma3:12b", Some("gemma")),
            ("gemma4:e2b", Some("gemma")),
            ("google/gemma-3-27b-it", Some("gemma")),
            ("qwen2.5-coder:7b", Some("qwen")),
            ("Qwen/Qwen3-Coder-30B", Some("qwen")),
            ("qwq:32b", Some("qwen")),
            ("mistral", Some("mistral")),
            ("mistral-small3.2", Some("mistral")),
            ("mixtral:8x7b", Some("mistral")),
            ("mistralai/devstral-small", Some("mistral")),
            ("phi4-mini", Some("phi")),
            ("microsoft/phi-4", Some("phi")),
            ("granite3.3:8b", Some("granite")),
            ("command-r-plus", Some("command")),
            ("moonshotai/kimi-k2", Some("kimi")),
            ("starcoder2:15b", Some("starcoder")),
            ("phind-codellama", None),
            ("llamafile", None),
            ("mystery-7b", None),
            ("opus", None),
            ("", None),
        ];
        for (model, family) in cases {
            assert_eq!(super::family_from_model_id(model), family, "{model}");
        }

        let registry = FileRegistry::from_files(&Settings::default(), None, None);
        let family = |w: &str| registry.family(&w.parse().unwrap());
        assert_eq!(family("claude-code/opus"), "claude");
        assert_eq!(family("claude-code"), "claude");
        assert_eq!(family("codex"), "gpt");
        assert_eq!(family("codex/gpt-oss:20b"), "gpt-oss");
        assert_eq!(family("antigravity-cli/claude-sonnet-4-6"), "claude");
        assert_eq!(family("opencode/openrouter/z-ai/glm-5"), "glm");
        assert_eq!(family("openrouter/deepseek/deepseek-v4-pro"), "deepseek");
        assert_eq!(family("opencode/ollama/llama3.2"), "llama");
        assert_eq!(family("ollama/llama3.2"), "llama");
        assert_eq!(family("ollama/gemma4:e2b"), "gemma");
        assert_eq!(family("opencode/ollama/mystery-7b"), "unknown");
    }

    fn with_workers(workers: &[&str]) -> RoleSettings {
        RoleSettings {
            workers: workers.iter().map(|w| w.parse().unwrap()).collect(),
            ..RoleSettings::default()
        }
    }

    #[test]
    fn family_from_model_refuses_unknown_family() {
        let mut settings = Settings::default();
        settings.roles.insert(
            RoleName::Reviewer,
            with_workers(&["claude-code/opus", "opencode/ollama/mystery-7b"]),
        );
        settings.roles.insert(
            RoleName::Judge,
            with_workers(&["ollama/house-model:12b", "codex"]),
        );
        settings.roles.insert(
            RoleName::Adjudicator,
            with_workers(&["opencode/openrouter/z-ai/glm-5", "my-acp-agent/mystery-1"]),
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
        let harness = |id: &str, family: &str| {
            format!(
                "[[harness]]\nid = \"{id}\"\ndisplay_name = \"{id}\"\nbinary = \"/opt/{id}\"\n\
                 read_only_args = []\nbuild_args = []\nfamily = \"{family}\"\n"
            )
        };
        let harnesses = harness("my-acp-agent", FROM_MODEL) + &harness("my-agent", "mistral");
        std::fs::write(&file, &harnesses).unwrap();
        let registry = FileRegistry::from_files(&settings, Some(&file), None);
        assert!(registry.warnings().is_empty(), "{:?}", registry.warnings());

        // The registry and the check agree on every seat.
        let family = |w: &str| registry.family(&w.parse().unwrap());
        assert_eq!(family("my-agent/whatever"), "mistral");
        assert_eq!(family("my-agent"), "mistral");
        assert_eq!(family("my-acp-agent/mystery-1"), "unknown");
        assert_eq!(family("my-acp-agent/claude-sonnet-4-6"), "claude");

        let problems = family_problems(&settings, &registry);
        let refused: Vec<(&str, &str)> = [
            ("roles.reviewer", "opencode/ollama/mystery-7b"),
            ("roles.judge", "ollama/house-model:12b"),
            ("roles.adjudicator", "my-acp-agent/mystery-1"),
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
            assert_eq!(family(worker), "unknown", "{role}");
        }

        // A family from the registry entry clears it.
        std::fs::write(
            &file,
            harnesses
                + "[[model]]\nid = \"ollama/house-model:12b\"\nfamily = \"house\"\n\
                   kind = \"endpoint\"\naccess = \"local\"\ncost = { unit = \"none\" }\n",
        )
        .unwrap();
        let registry = FileRegistry::from_files(&settings, Some(&file), None);
        let problems = family_problems(&settings, &registry);
        assert!(
            !problems.iter().any(|p| p.contains("house-model")),
            "{problems:#?}"
        );

        // With cross-family review off, reviewers and the ring compare nothing.
        settings.review.other_provider = false;
        let problems = family_problems(&settings, &registry);
        assert_eq!(problems.len(), 1, "{problems:#?}");
        assert!(problems[0].starts_with("roles.adjudicator has my-acp-agent/mystery-1, "));
    }
}
