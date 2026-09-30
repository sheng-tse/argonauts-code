//! Claude Code repeats an API message's usage on every content block and on the partial stream
//! events, so increments are counted per message id and only growth is reported.

use std::collections::HashMap;

use argonauts_core::AgentEvent;
use serde_json::Value;

use crate::json::u64_at;

/// Token counts in Argonauts' convention, where `input` includes cached tokens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Tokens {
    input: u64,
    cached: u64,
    output: u64,
}

impl Tokens {
    /// Anthropic counts cache reads and writes apart from `input_tokens`, so both are added back.
    fn from_anthropic(uncached_input: u64, cache_read: u64, cache_write: u64, output: u64) -> Self {
        Self {
            input: uncached_input
                .saturating_add(cache_read)
                .saturating_add(cache_write),
            cached: cache_read,
            output,
        }
    }

    fn from_api_usage(usage: &Value) -> Self {
        Self::from_anthropic(
            u64_at(usage, "input_tokens"),
            u64_at(usage, "cache_read_input_tokens"),
            u64_at(usage, "cache_creation_input_tokens"),
            u64_at(usage, "output_tokens"),
        )
    }

    /// Sums a result's `modelUsage` map, which also covers subagents and compaction.
    fn from_model_usage(model_usage: &Value) -> Option<Self> {
        let models = model_usage.as_object().filter(|m| !m.is_empty())?;
        Some(models.values().fold(Self::default(), |sum, m| {
            sum.plus(Self::from_anthropic(
                u64_at(m, "inputTokens"),
                u64_at(m, "cacheReadInputTokens"),
                u64_at(m, "cacheCreationInputTokens"),
                u64_at(m, "outputTokens"),
            ))
        }))
    }

    fn plus(self, other: Self) -> Self {
        Self {
            input: self.input.saturating_add(other.input),
            cached: self.cached.saturating_add(other.cached),
            output: self.output.saturating_add(other.output),
        }
    }

    fn max(self, other: Self) -> Self {
        Self {
            input: self.input.max(other.input),
            cached: self.cached.max(other.cached),
            output: self.output.max(other.output),
        }
    }

    fn minus(self, other: Self) -> Self {
        Self {
            input: self.input.saturating_sub(other.input),
            cached: self.cached.saturating_sub(other.cached),
            output: self.output.saturating_sub(other.output),
        }
    }

    fn total(self) -> u64 {
        self.input.saturating_add(self.output)
    }

    fn into_event(self, cost_usd: Option<f64>, cumulative: bool) -> AgentEvent {
        AgentEvent::Usage {
            input_tokens: self.input,
            output_tokens: self.output,
            cached_tokens: self.cached,
            cost_usd,
            cumulative,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct UsageTracker {
    per_message: HashMap<String, Tokens>,
    counted: Tokens,
}

impl UsageTracker {
    pub(crate) fn observe(&mut self, id: &str, usage: &Value) -> Option<AgentEvent> {
        let seen = self.per_message.entry(id.to_owned()).or_default();
        let merged = seen.max(Tokens::from_api_usage(usage));
        let growth = merged.minus(*seen);
        *seen = merged;
        if growth.total() == 0 {
            return None;
        }
        self.counted = self.counted.plus(growth);
        Some(growth.into_event(None, false))
    }

    /// Skipped when the totals are below what was already counted, as after a crash that zeroes them.
    pub(crate) fn result_totals(&self, result: &Value) -> Option<AgentEvent> {
        let totals = result
            .get("modelUsage")
            .and_then(Tokens::from_model_usage)
            .or_else(|| result.get("usage").map(Tokens::from_api_usage))?;
        if totals.total() < self.counted.total() {
            return None;
        }
        let cost = result.get("total_cost_usd").and_then(Value::as_f64);
        Some(totals.into_event(cost, true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn usage(input: u64, output: u64, cached: u64, cumulative: bool) -> AgentEvent {
        AgentEvent::Usage {
            input_tokens: input,
            output_tokens: output,
            cached_tokens: cached,
            cost_usd: None,
            cumulative,
        }
    }

    #[test]
    fn cache_reads_and_writes_count_as_input() {
        let mut tracker = UsageTracker::default();
        let event = tracker.observe(
            "msg_1",
            &json!({"input_tokens": 3, "cache_creation_input_tokens": 100,
                    "cache_read_input_tokens": 1000, "output_tokens": 2}),
        );
        assert_eq!(event, Some(usage(1103, 2, 1000, false)));
    }

    #[test]
    fn repeated_usage_for_one_message_counts_once() {
        let mut tracker = UsageTracker::default();
        let start = json!({"input_tokens": 10, "output_tokens": 1});
        assert_eq!(
            tracker.observe("msg_1", &start),
            Some(usage(10, 1, 0, false))
        );
        assert_eq!(tracker.observe("msg_1", &start), None);
        assert_eq!(
            tracker.observe("msg_1", &json!({"output_tokens": 25})),
            Some(usage(0, 24, 0, false))
        );
        assert_eq!(
            tracker.observe("msg_1", &json!({"input_tokens": 10, "output_tokens": 25})),
            None
        );
        assert_eq!(
            tracker.observe("msg_2", &start),
            Some(usage(10, 1, 0, false))
        );
    }

    #[test]
    fn result_totals_prefer_model_usage_and_carry_cost() {
        let tracker = UsageTracker::default();
        let result = json!({
            "total_cost_usd": 0.0123,
            "usage": {"input_tokens": 1, "output_tokens": 1},
            "modelUsage": {
                "claude-haiku": {"inputTokens": 10, "outputTokens": 5,
                                 "cacheReadInputTokens": 100, "cacheCreationInputTokens": 20},
                "claude-sonnet": {"inputTokens": 1, "outputTokens": 1,
                                  "cacheReadInputTokens": 0, "cacheCreationInputTokens": 0}
            }
        });
        let Some(AgentEvent::Usage {
            input_tokens,
            output_tokens,
            cached_tokens,
            cost_usd,
            cumulative,
        }) = tracker.result_totals(&result)
        else {
            panic!("expected usage");
        };
        assert_eq!((input_tokens, output_tokens, cached_tokens), (131, 6, 100));
        assert_eq!(cost_usd, Some(0.0123));
        assert!(cumulative);
    }

    #[test]
    fn result_totals_fall_back_to_usage() {
        let tracker = UsageTracker::default();
        let result = json!({"usage": {"input_tokens": 4, "output_tokens": 2}, "modelUsage": {}});
        assert_eq!(tracker.result_totals(&result), Some(usage(4, 2, 0, true)));
        assert_eq!(tracker.result_totals(&json!({})), None);
    }

    #[test]
    fn zeroed_result_totals_do_not_erase_counted_usage() {
        let mut tracker = UsageTracker::default();
        tracker.observe("msg_1", &json!({"input_tokens": 50, "output_tokens": 5}));
        let zeroed = json!({"total_cost_usd": 0, "usage": {"input_tokens": 0, "output_tokens": 0}});
        assert_eq!(tracker.result_totals(&zeroed), None);
    }
}
