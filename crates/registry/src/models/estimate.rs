//! What a session would cost: dollars from prices, a share of your plan's quota, or free.

use argonauts_core::TokenPrice;
use argonauts_core::builtin_price;
use argonauts_core::limits::CostEstimate;
use argonauts_core::names::WorkerRef;
use argonauts_core::registry::{CostUnit, ModelAccess, ModelEntry};

/// `Usd` from the entry's prices or the built-in price table, `Free` for a local model. A
/// subscription spends quota, but its share needs the median of similar past cards, which
/// the registry doesn't have, so it stays `Unknown` and the entry's `access` says why.
pub(crate) fn estimate(
    entry: Option<&ModelEntry>,
    worker: &WorkerRef,
    input_tokens: u64,
    output_tokens: u64,
) -> CostEstimate {
    let Some(entry) = entry else {
        return CostEstimate::Unknown;
    };
    if entry.access == ModelAccess::Local {
        return CostEstimate::Free;
    }
    match entry.cost.unit {
        CostUnit::None => CostEstimate::Free,
        CostUnit::Quota => CostEstimate::Unknown,
        CostUnit::Usd => price(entry, worker).map_or(CostEstimate::Unknown, |price| {
            CostEstimate::Usd(price.cost(input_tokens, output_tokens, 0))
        }),
    }
}

fn price(entry: &ModelEntry, worker: &WorkerRef) -> Option<TokenPrice> {
    if let (Some(input), Some(output)) = (entry.cost.input_per_mtok, entry.cost.output_per_mtok) {
        return Some(TokenPrice {
            input_per_mtok: input,
            output_per_mtok: output,
            cached_input_per_mtok: entry.cost.cached_input_per_mtok,
        });
    }
    worker
        .model
        .as_deref()
        .or(entry.id.model.as_deref())
        .and_then(builtin_price)
}

#[cfg(test)]
mod tests {
    use argonauts_core::registry::{CostModel, RegistryKind};

    use super::*;
    use crate::models::builtin::cost;

    fn entry(id: &str, access: ModelAccess, cost: CostModel) -> ModelEntry {
        let mut entry = crate::models::builtin::harness_entry(
            &argonauts_core::settings::Settings::default(),
            "codex",
            None,
        );
        entry.id = id.parse().unwrap();
        entry.kind = RegistryKind::Endpoint;
        entry.access = access;
        entry.cost = cost;
        entry
    }

    #[test]
    fn dollars_quota_and_free() {
        let worker: WorkerRef = "openrouter/z-ai/glm-5".parse().unwrap();
        let priced = entry(
            "openrouter/z-ai/glm-5",
            ModelAccess::ApiKey,
            CostModel {
                unit: CostUnit::Usd,
                input_per_mtok: Some(1.0),
                output_per_mtok: Some(4.0),
                cached_input_per_mtok: None,
            },
        );
        assert_eq!(
            estimate(Some(&priced), &worker, 1_000_000, 500_000),
            CostEstimate::Usd(3.0)
        );

        let haiku: WorkerRef = "claude-code/haiku".parse().unwrap();
        let api_key = entry(
            "claude-code/haiku",
            ModelAccess::ApiKey,
            cost(CostUnit::Usd),
        );
        assert_eq!(
            estimate(Some(&api_key), &haiku, 1_000_000, 1_000_000),
            CostEstimate::Usd(6.0)
        );
        let unpriced = entry(
            "openrouter/new-model",
            ModelAccess::ApiKey,
            cost(CostUnit::Usd),
        );
        assert_eq!(
            estimate(Some(&unpriced), &unpriced.id, 10, 10),
            CostEstimate::Unknown
        );

        let plan = entry("codex", ModelAccess::Subscription, cost(CostUnit::Quota));
        assert_eq!(
            estimate(Some(&plan), &plan.id, 10, 10),
            CostEstimate::Unknown
        );

        let local = entry("ollama/llama3.2", ModelAccess::Local, cost(CostUnit::Usd));
        assert_eq!(
            estimate(Some(&local), &local.id, 10, 10),
            CostEstimate::Free
        );
        let none = entry("vllm/x", ModelAccess::ApiKey, cost(CostUnit::None));
        assert_eq!(estimate(Some(&none), &none.id, 10, 10), CostEstimate::Free);

        assert_eq!(estimate(None, &worker, 10, 10), CostEstimate::Unknown);
    }
}
