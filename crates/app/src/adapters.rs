use std::sync::Arc;

use argonauts_adapter_antigravity_cli::AntigravityCli;
use argonauts_adapter_claude_code::ClaudeCodeAdapter;
use argonauts_adapter_codex::CodexAdapter;
use argonauts_adapter_opencode::OpenCodeAdapter;
use argonauts_core::Adapter;

/// Claude Code, Codex, Antigravity CLI, and OpenCode, in the order the setup lists them.
pub fn builtin_adapters() -> Vec<Arc<dyn Adapter>> {
    vec![
        Arc::new(ClaudeCodeAdapter::new()),
        Arc::new(CodexAdapter::new()),
        Arc::new(AntigravityCli::new()),
        Arc::new(OpenCodeAdapter::new()),
    ]
}

#[cfg(test)]
mod tests {
    use argonauts_core::model_catalog::{CatalogPrice, ModelCatalog};
    use argonauts_core::names::ModelRef;
    use argonauts_registry::models::shipped_catalog;

    use super::builtin_adapters;

    /// What the ids of a CLI's own models start with, so each is weighed only against the
    /// others it runs.
    /// OpenCode runs any provider's models and names no cheapest of its own.
    fn id_prefix(cli: &str) -> Option<&'static str> {
        match cli {
            "claude-code" => Some("claude-"),
            "codex" => Some("gpt-"),
            "antigravity-cli" => Some("gemini-"),
            _ => None,
        }
    }

    /// The catalog's price for `model`, which a dated snapshot or a thinking level such as
    /// `gemini-3.8-flash-low` shares with the model it belongs to.
    fn listed_price<'a>(catalog: &'a ModelCatalog, model: &str) -> Option<&'a CatalogPrice> {
        catalog
            .prices
            .iter()
            .filter(|price| {
                model
                    .strip_prefix(price.model.as_str())
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('-'))
            })
            .max_by_key(|price| price.model.len())
    }

    fn per_million(price: &CatalogPrice) -> f64 {
        price.input.0 + price.output.0
    }

    /// Live tests and recordings run each CLI's cheapest model, so a cheaper one added to the
    /// catalog must be named by its adapter too.
    #[test]
    fn each_cli_names_its_cheapest_listed_model() {
        let catalog = shipped_catalog();
        let mut compared = Vec::new();
        for adapter in builtin_adapters() {
            let cli = adapter.id();
            let Some(prefix) = id_prefix(cli) else {
                continue;
            };
            let (cheapest, _) = adapter.cheapest_model();
            // An alias such as `haiku` is priced as the model it runs.
            let model = catalog
                .runs(&ModelRef::new(cli, Some(cheapest.to_owned())))
                .unwrap_or_else(|| cheapest.to_owned());
            // A cheapest model the catalog gives no price has nothing to be weighed against.
            let Some(price) = listed_price(catalog, &model) else {
                eprintln!("skipped {cli}: the catalog has no price for {cheapest}");
                continue;
            };
            let same_cli = catalog
                .prices
                .iter()
                .filter(|p| p.model.starts_with(prefix));
            for other in same_cli {
                assert!(
                    per_million(price) <= per_million(other),
                    "{cli} names {cheapest} its cheapest at ${} per million in and out, \
                     but {} is ${}",
                    per_million(price),
                    other.model,
                    per_million(other),
                );
            }
            compared.push(cli);
        }
        assert!(
            !compared.is_empty(),
            "the catalog prices no CLI's cheapest model"
        );
    }
}
