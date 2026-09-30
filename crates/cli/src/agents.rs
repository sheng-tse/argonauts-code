use std::sync::Arc;

use anyhow::anyhow;
use argonauts_adapter_antigravity_cli::AntigravityCli;
use argonauts_adapter_claude_code::ClaudeCodeAdapter;
use argonauts_adapter_codex::CodexAdapter;
use argonauts_core::{Adapter, KNOWN_AGENT_IDS};

/// Every adapter, in `KNOWN_AGENT_IDS` order.
pub fn all() -> Vec<Arc<dyn Adapter>> {
    vec![
        Arc::new(ClaudeCodeAdapter::new()),
        Arc::new(CodexAdapter::new()),
        Arc::new(AntigravityCli::new()),
    ]
}

pub fn by_id(id: &str) -> anyhow::Result<Arc<dyn Adapter>> {
    all()
        .into_iter()
        .find(|adapter| adapter.id() == id)
        .ok_or_else(|| {
            anyhow!(
                "unknown agent `{id}`; choose one of {}",
                KNOWN_AGENT_IDS.join(", ")
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_known_id_has_an_adapter_in_order() {
        let ids: Vec<&str> = all().iter().map(|a| a.id()).collect();
        assert_eq!(ids, KNOWN_AGENT_IDS);
        for id in KNOWN_AGENT_IDS {
            assert_eq!(by_id(id).unwrap().id(), *id);
        }
    }

    #[test]
    fn unknown_ids_name_the_choices() {
        let err = by_id("gemini").err().unwrap().to_string();
        assert!(err.contains("claude-code, codex, antigravity-cli"), "{err}");
    }

    #[test]
    fn cheapest_models_follow_the_live_call_policy() {
        let cheapest: Vec<_> = all().iter().map(|a| a.cheapest_model()).collect();
        assert_eq!(
            cheapest,
            vec![
                ("haiku", Some("low")),
                ("gpt-5.6-luna", Some("low")),
                ("gemini-3.6-flash-low", Some("low")),
            ]
        );
    }
}
