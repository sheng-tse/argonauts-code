//! How full the main agent's context is. Each API request sends the whole conversation, so its
//! input plus cache reads and writes is what the context holds. Subagents have their own. What
//! it holds by category comes from `get_context_usage`, read in `context_usage`.

pub(crate) mod context_usage;

use argonauts_core::AgentEvent;
use serde_json::Value;

use crate::parser::json::u64_at;

#[derive(Debug, Default)]
pub struct ContextTracker {
    /// The main model's id from `init`, which names its `modelUsage` entry.
    model: Option<String>,
    used: Option<u64>,
    /// Learned from the first result, since nothing earlier reports it.
    window: Option<u64>,
}

impl ContextTracker {
    pub fn set_model(&mut self, model: Option<String>) {
        self.model = model;
    }

    /// Only changes are reported, since every content block repeats its message's usage.
    pub fn observe(&mut self, usage: &Value) -> Option<AgentEvent> {
        let used = u64_at(usage, "input_tokens")
            .saturating_add(u64_at(usage, "cache_read_input_tokens"))
            .saturating_add(u64_at(usage, "cache_creation_input_tokens"));
        if used == 0 || self.used == Some(used) {
            return None;
        }
        self.used = Some(used);
        Some(self.event(used))
    }

    /// Reads the window size from a result's `modelUsage`, and reports the last fill again
    /// once the size is first known.
    pub fn observe_result(&mut self, result: &Value) -> Option<AgentEvent> {
        let window = self.window_in(result.get("modelUsage")?)?;
        let first = self.window.is_none();
        self.window = Some(window);
        let used = self.used?;
        first.then(|| self.event(used))
    }

    fn window_in(&self, model_usage: &Value) -> Option<u64> {
        let models = model_usage.as_object()?;
        let entry = match (&self.model, models.len()) {
            (Some(model), _) if models.contains_key(model) => models.get(model)?,
            // With one model there is no doubt which window it is.
            (_, 1) => models.values().next()?,
            _ => return None,
        };
        entry.get("contextWindow")?.as_u64().filter(|w| *w > 0)
    }

    fn event(&self, used: u64) -> AgentEvent {
        AgentEvent::ContextFill {
            used,
            size: self.window,
            sent: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fill(used: u64, size: Option<u64>) -> Option<AgentEvent> {
        Some(AgentEvent::ContextFill {
            used,
            size,
            sent: None,
        })
    }

    #[test]
    fn input_and_cache_tokens_fill_the_context() {
        let mut tracker = ContextTracker::default();
        tracker.set_model(Some("claude-haiku-4-5".into()));
        let usage = json!({"input_tokens": 10, "cache_read_input_tokens": 4563,
                           "cache_creation_input_tokens": 1798, "output_tokens": 43});
        assert_eq!(tracker.observe(&usage), fill(6371, None));
        assert_eq!(tracker.observe(&usage), None);
        assert_eq!(tracker.observe(&json!({"output_tokens": 5})), None);

        let result = json!({"modelUsage": {
            "claude-haiku-4-5": {"contextWindow": 200000},
            "claude-sonnet-5": {"contextWindow": 1000000}
        }});
        assert_eq!(tracker.observe_result(&result), fill(6371, Some(200000)));
        assert_eq!(tracker.observe_result(&result), None);
        assert_eq!(
            tracker.observe(&json!({"input_tokens": 7000})),
            fill(7000, Some(200000))
        );
    }

    #[test]
    fn an_unknown_model_among_several_gives_no_window() {
        let mut tracker = ContextTracker::default();
        let result = json!({"modelUsage": {"a": {"contextWindow": 1}, "b": {"contextWindow": 2}}});
        assert_eq!(tracker.observe_result(&result), None);
        assert_eq!(tracker.observe(&json!({"input_tokens": 3})), fill(3, None));
        let single = json!({"modelUsage": {"a": {"contextWindow": 1000}}});
        assert_eq!(tracker.observe_result(&single), fill(3, Some(1000)));
    }
}
