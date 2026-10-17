//! Claude Code repeats an API message's usage on every content block and on the partial stream
//! events, so increments are counted per message id and only growth is reported.

use std::collections::HashMap;

use argonauts_core::AgentEvent;
use argonauts_core::pricing::{RequestTokens, claude_request_cost};
use serde_json::Value;

use crate::json::u64_at;

/// Token counts in Argonauts Code's convention, where `input` includes cached tokens.
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

/// One API request as it is billed: its model, and its tokens with the cache writes kept
/// apart by how long they keep.
#[derive(Debug, Default)]
struct Request {
    model: Option<String>,
    input: u64,
    cache_read: u64,
    cache_write: u64,
    cache_write_1h: u64,
    output: u64,
}

impl Request {
    /// The largest of each count so far, since a message's usage only grows. A partial event
    /// that gives the cache writes without saying how long they keep leaves the hour-long part
    /// as the earlier events said.
    fn merge(&mut self, model: Option<&str>, usage: &Value) {
        if self.model.is_none() {
            self.model = model.map(str::to_owned);
        }
        self.input = self.input.max(u64_at(usage, "input_tokens"));
        self.cache_read = self
            .cache_read
            .max(u64_at(usage, "cache_read_input_tokens"));
        self.cache_write = self
            .cache_write
            .max(u64_at(usage, "cache_creation_input_tokens"));
        let hour = usage
            .get("cache_creation")
            .map_or(0, |split| u64_at(split, "ephemeral_1h_input_tokens"));
        self.cache_write_1h = self.cache_write_1h.max(hour).min(self.cache_write);
        self.output = self.output.max(u64_at(usage, "output_tokens"));
    }

    fn cost(&self) -> Option<f64> {
        let tokens = RequestTokens {
            input: self.input,
            cache_read: self.cache_read,
            cache_write_5m: self.cache_write - self.cache_write_1h,
            cache_write_1h: self.cache_write_1h,
            output: self.output,
        };
        claude_request_cost(self.model.as_deref()?, &tokens)
    }
}

/// About this many characters of a call's input make a token, as Claude Code's spinner counts.
const CHARS_PER_TOKEN: u64 = 4;
/// An estimate goes out once it has grown by this many tokens, so the count moves as the
/// spinner's does without a line for every piece of a call.
const ESTIMATE_STEP: u64 = 20;

#[derive(Debug, Default)]
pub(crate) struct UsageTracker {
    /// What each message has added to the count so far.
    per_message: HashMap<String, Tokens>,
    /// What each message's own usage said.
    reported: HashMap<String, Tokens>,
    /// The characters of call input each message streamed that its usage doesn't count yet.
    streaming: HashMap<String, u64>,
    /// What the messages' usage said in all, which a result's totals are checked against.
    counted: Tokens,
    /// The requests since the last result, as they are billed.
    requests: HashMap<String, Request>,
}

impl UsageTracker {
    pub(crate) fn observe(
        &mut self,
        id: &str,
        model: Option<&str>,
        usage: &Value,
    ) -> Option<AgentEvent> {
        self.requests
            .entry(id.to_owned())
            .or_default()
            .merge(model, usage);
        let reported = self.reported.entry(id.to_owned()).or_default();
        let merged = reported.max(Tokens::from_api_usage(usage));
        self.counted = self.counted.plus(merged.minus(*reported));
        *reported = merged;
        self.shown(id, 1)
    }

    /// The message `id` streamed `chars` more of a call's input, which its usage counts only
    /// once the message ends: until then the count goes up by a token for about every four.
    pub(crate) fn streamed_input(&mut self, id: &str, chars: usize) -> Option<AgentEvent> {
        let streamed = self.streaming.entry(id.to_owned()).or_default();
        *streamed = streamed.saturating_add(chars as u64);
        self.shown(id, ESTIMATE_STEP)
    }

    /// The message `id` ended, and its usage now counts all it wrote, so the estimate goes.
    /// What an estimate counted past that stays only until the next result's totals.
    pub(crate) fn settle(&mut self, id: &str) {
        self.streaming.remove(id);
    }

    /// What the message `id` adds to the count now: its usage, with what it streamed beyond
    /// that while it streams, once that comes to `at_least` tokens more than went out before.
    fn shown(&mut self, id: &str, at_least: u64) -> Option<AgentEvent> {
        let reported = self.reported.get(id).copied().unwrap_or_default();
        let estimate = self
            .streaming
            .get(id)
            .map_or(0, |chars| chars / CHARS_PER_TOKEN);
        let shown = Tokens {
            output: reported.output.saturating_add(estimate),
            ..reported
        };
        let seen = self.per_message.entry(id.to_owned()).or_default();
        let merged = seen.max(shown);
        let growth = merged.minus(*seen);
        if growth.total() < at_least.max(1) {
            return None;
        }
        *seen = merged;
        Some(growth.into_event(None, false))
    }

    /// What the requests since the last result cost as Anthropic bills them: a prompt past
    /// the model's long-prompt size at that price, and the cache written for an hour at twice
    /// the input price. `None` once any of them is a model the price list doesn't know.
    pub(crate) fn billed_since_result(&self) -> Option<f64> {
        self.requests.values().map(Request::cost).sum()
    }

    /// Skipped when the totals are below what was already counted, as after a crash that zeroes
    /// them. Totals that count are Claude Code's own figure for every request before them.
    pub(crate) fn result_totals(&mut self, result: &Value) -> Option<AgentEvent> {
        let totals = result
            .get("modelUsage")
            .and_then(Tokens::from_model_usage)
            .or_else(|| result.get("usage").map(Tokens::from_api_usage))?;
        if totals.total() < self.counted.total() {
            return None;
        }
        let cost = result.get("total_cost_usd").and_then(Value::as_f64);
        self.requests.clear();
        Some(totals.into_event(cost, true))
    }
}

/// What the process spent at least, by the line "Budget limit reached ($0.68 of $0.66); stopping
/// background agents." that Claude Code writes on stderr when its spend cap stops the session's
/// background agents between turns, after which no result need come. The spend leaves out what
/// a resumed session spent before the process. It's rounded to the cent, while the cap is what
/// was reached, so the floor is the more of the cap and the spend less half a cent.
pub(crate) fn spent_at_cap(line: &str) -> Option<f64> {
    let figures = line
        .strip_prefix("Budget limit reached ($")?
        .strip_suffix("); stopping background agents.")?;
    let (spent, cap) = figures.split_once(" of $")?;
    let (spent, cap) = (spent.parse::<f64>().ok()?, cap.parse::<f64>().ok()?);
    let at_least = (spent - 0.005).max(cap);
    (at_least.is_finite() && at_least > 0.0).then_some(at_least)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Claude Code 2.1.296 to 2.1.301 word it alike.
    #[test]
    fn the_cap_note_says_what_the_process_spent_at_least() {
        let halt = |figures: &str| {
            spent_at_cap(&format!(
                "Budget limit reached ({figures}); stopping background agents."
            ))
        };
        let at_least = halt("$0.68 of $0.66").unwrap();
        assert!((at_least - 0.675).abs() < 1e-12, "{at_least}");
        // The cap it reached, where the rounded spend may be under it.
        assert_eq!(halt("$0.66 of $0.66"), Some(0.66));
        assert_eq!(halt("$1.00 of $1"), Some(1.0));
        assert_eq!(halt("$NaN of $0.66"), Some(0.66));
        assert_eq!(halt("$0.68 of $"), None);
        // What Claude Code tells the model when it won't start another agent is not this.
        assert_eq!(
            spent_at_cap(
                "Budget limit reached ($0.68 spent of the $0.66 maximum). New agents cannot be \
                 started."
            ),
            None
        );
    }

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
            None,
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
            tracker.observe("msg_1", None, &start),
            Some(usage(10, 1, 0, false))
        );
        assert_eq!(tracker.observe("msg_1", None, &start), None);
        assert_eq!(
            tracker.observe("msg_1", None, &json!({"output_tokens": 25})),
            Some(usage(0, 24, 0, false))
        );
        assert_eq!(
            tracker.observe(
                "msg_1",
                None,
                &json!({"input_tokens": 10, "output_tokens": 25})
            ),
            None
        );
        assert_eq!(
            tracker.observe("msg_2", None, &start),
            Some(usage(10, 1, 0, false))
        );
    }

    #[test]
    fn result_totals_prefer_model_usage_and_carry_cost() {
        let mut tracker = UsageTracker::default();
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
        let mut tracker = UsageTracker::default();
        let result = json!({"usage": {"input_tokens": 4, "output_tokens": 2}, "modelUsage": {}});
        assert_eq!(tracker.result_totals(&result), Some(usage(4, 2, 0, true)));
        assert_eq!(tracker.result_totals(&json!({})), None);
    }

    /// A run session's first request as Claude Code 2.1.301 streamed it: past 100K tokens of
    /// prompt and written to the cache for an hour, it is billed ten times what a flat price
    /// says. The final usage repeats the cache writes without saying how long they keep.
    #[test]
    fn a_request_is_priced_as_it_is_billed_until_the_result() {
        let mut tracker = UsageTracker::default();
        assert_eq!(tracker.billed_since_result(), Some(0.0));
        let start = json!({"input_tokens": 2, "cache_creation_input_tokens": 361_885,
            "cache_read_input_tokens": 0, "output_tokens": 2,
            "cache_creation": {"ephemeral_5m_input_tokens": 0,
                               "ephemeral_1h_input_tokens": 361_885}});
        tracker.observe("msg_1", Some("claude-haiku-5-5"), &start);
        let billed = tracker.billed_since_result().unwrap();
        assert!((billed - 0.361891).abs() < 1e-12, "{billed}");
        let end = json!({"input_tokens": 2, "cache_creation_input_tokens": 361_885,
            "cache_read_input_tokens": 0, "output_tokens": 5_039});
        tracker.observe("msg_1", None, &end);
        let billed = tracker.billed_since_result().unwrap();
        assert!((billed - 0.3744835).abs() < 1e-12, "{billed}");
        // The result's own figure covers it from there on.
        let result = json!({"total_cost_usd": 0.3744835,
            "usage": {"input_tokens": 2, "cache_creation_input_tokens": 361_885,
                      "output_tokens": 5_039}});
        assert!(tracker.result_totals(&result).is_some());
        assert_eq!(tracker.billed_since_result(), Some(0.0));
        // A model the price list doesn't know leaves the figure to the meter.
        tracker.observe("msg_2", Some("claude-opus-9"), &start);
        assert_eq!(tracker.billed_since_result(), None);
    }

    #[test]
    fn zeroed_result_totals_do_not_erase_counted_usage() {
        let mut tracker = UsageTracker::default();
        tracker.observe(
            "msg_1",
            None,
            &json!({"input_tokens": 50, "output_tokens": 5}),
        );
        let zeroed = json!({"total_cost_usd": 0, "usage": {"input_tokens": 0, "output_tokens": 0}});
        assert_eq!(tracker.result_totals(&zeroed), None);
    }
}
