//! What people read for each CLI and model: "Claude Code" and "Opus 5.5" rather than the ids
//! `claude-code` and `claude-opus-5-5`. Ids stay in settings files, logs, and the detail views.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model_catalog::{ModelCatalog, display_name_from_id};
use crate::names::WorkerRef;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayNames {
    /// A CLI's or endpoint's id to its name, as its adapter gives it.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub harnesses: BTreeMap<String, String>,
    /// Model names, including what an alias such as `opus` runs.
    pub catalog: ModelCatalog,
    /// A CLI's id to the model its settings give it (`agents.<cli>.model`), which a session that
    /// names no model of its own runs.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub configured: BTreeMap<String, String>,
}

impl DisplayNames {
    /// `Claude Code` for `claude-code`. An id nothing names stays as it is.
    pub fn harness(&self, id: &str) -> String {
        let id = id.trim();
        self.harnesses
            .get(id)
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
            .or_else(|| builtin_harness_name(id))
            .unwrap_or(id)
            .to_owned()
    }

    /// `Opus 5.5` for `claude-code` with `claude-opus-5-5` or its alias `opus`, or `None` when
    /// no model is named and the CLI runs its own default.
    pub fn model(&self, harness: &str, model: Option<&str>) -> Option<String> {
        let model = model.map(str::trim).filter(|model| !model.is_empty())?;
        let id = WorkerRef::new(harness.trim(), Some(model.to_owned()));
        // An alias reads as the model it runs, so `opus` names the model rather than itself.
        let entry = self.catalog.entry(&id);
        let named = entry.and_then(|entry| entry.name.clone());
        let resolved = entry.and_then(|entry| entry.id.model.as_deref());
        Some(named.unwrap_or_else(|| display_name_from_id(resolved.unwrap_or(model))))
    }

    /// `Opus 5.5 on Claude Code`, or the CLI alone when it runs its own default. The name
    /// reads as one phrase, so it sits in sentences and in lines of `·`-joined parts alike.
    pub fn agent(&self, harness: &str, model: Option<&str>) -> String {
        let cli = self.harness(harness);
        match self.model(harness, model) {
            Some(model) => format!("{model} on {cli}"),
            None => cli,
        }
    }

    pub fn worker(&self, worker: &WorkerRef) -> String {
        self.agent(&worker.worker, worker.model.as_deref())
    }

    /// The worker with the model a session of it runs: its own, else the one its CLI's
    /// settings give, so a session reads `GPT 5.6 on Codex` from its start to its end.
    pub fn running(&self, worker: &WorkerRef) -> WorkerRef {
        if worker.model.is_some() {
            return worker.clone();
        }
        let model = self
            .configured
            .get(worker.worker.trim())
            .map(|model| model.trim())
            .filter(|model| !model.is_empty());
        match model {
            Some(model) => WorkerRef::new(worker.worker.clone(), Some(model.to_owned())),
            None => worker.clone(),
        }
    }
}

/// The names of the CLIs and endpoints Argonauts knows without asking an adapter.
pub fn builtin_harness_name(id: &str) -> Option<&'static str> {
    Some(match id {
        "claude-code" | "claude" => "Claude Code",
        "codex" => "Codex",
        "antigravity-cli" => "Antigravity CLI",
        "opencode" => "OpenCode",
        "openrouter" => "OpenRouter",
        "ollama" => "Ollama",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use crate::model_catalog::CatalogEntry;

    use super::*;

    fn id(text: &str) -> WorkerRef {
        text.parse().unwrap()
    }

    fn names() -> DisplayNames {
        let mut opus = CatalogEntry::new(id("claude-code/claude-opus-5-5"));
        opus.aliases = Some(vec!["opus".to_owned()]);
        let mut sol = CatalogEntry::new(id("codex/gpt-6.1-sol"));
        sol.name = Some("GPT-6.1 Sol".to_owned());
        DisplayNames {
            harnesses: BTreeMap::from([("my-cli".to_owned(), "My CLI".to_owned())]),
            catalog: ModelCatalog {
                models: vec![opus, sol],
                ..ModelCatalog::default()
            },
            configured: BTreeMap::from([("codex".to_owned(), "gpt-6.1-sol".to_owned())]),
        }
    }

    #[test]
    fn a_session_runs_the_model_its_settings_give() {
        let names = names();
        // Codex names no model, so it runs the one its settings give.
        assert_eq!(
            names.worker(&names.running(&id("codex"))),
            "GPT-6.1 Sol on Codex"
        );
        // A model of its own wins, and a CLI without one in its settings runs its default.
        assert_eq!(names.running(&id("codex/gpt-5.6")), id("codex/gpt-5.6"));
        assert_eq!(names.running(&id("claude-code")), id("claude-code"));
    }

    #[test]
    fn an_agent_reads_as_its_cli_and_model() {
        let names = names();
        assert_eq!(
            names.agent("claude-code", Some("claude-opus-5-5")),
            "Opus 5.5 on Claude Code"
        );
        assert_eq!(
            names.worker(&id("codex/gpt-6.1-sol")),
            "GPT-6.1 Sol on Codex"
        );
        assert_eq!(
            names.worker(&id("my-cli/some-model")),
            "Some Model on My CLI"
        );
    }

    #[test]
    fn an_alias_reads_as_the_model_it_runs() {
        assert_eq!(
            names().worker(&id("claude-code/opus")),
            "Opus 5.5 on Claude Code"
        );
        assert_eq!(
            DisplayNames::default().worker(&id("claude-code/opus")),
            "Opus on Claude Code"
        );
    }

    #[test]
    fn a_cli_default_and_an_unknown_cli_leave_out_what_isnt_known() {
        let names = DisplayNames::default();
        assert_eq!(names.worker(&id("claude-code")), "Claude Code");
        assert_eq!(names.agent("codex", Some("  ")), "Codex");
        assert_eq!(names.agent(" vllm ", None), "vllm");
        assert_eq!(
            names.worker(&id("ollama/qwen3-coder:30b")),
            "Qwen3 Coder 30B on Ollama"
        );
    }
}
