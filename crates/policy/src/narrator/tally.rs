//! What one narration spent, counted from its usage events as they stream, so a call that
//! fails still counts against the cap.

use std::cmp::Ordering;

use argonauts_core::agents::session_runner::RunnerEvent;
use argonauts_core::base::names::ModelRef;
use argonauts_core::settings::Settings;
use argonauts_core::{AgentEvent, MeterLimit, Spend, SpendMeter, TokenPrice, builtin_price};

pub struct CallTally {
    meter: SpendMeter,
}

impl CallTally {
    /// `price` estimates dollars for usage that comes without a cost.
    pub fn new(price: Option<TokenPrice>) -> Self {
        Self {
            meter: SpendMeter::new(MeterLimit::None, 1.0).with_price(price),
        }
    }

    pub fn observe(&mut self, event: &RunnerEvent) {
        match event {
            RunnerEvent::Agent(usage @ AgentEvent::Usage { .. }) => {
                self.meter.observe(usage);
            }
            // A schema retry is another session whose cumulative totals start over. Counting a
            // resumed conversation twice overstates the spend, which is the safe side for a cap.
            RunnerEvent::Agent(AgentEvent::Result { .. }) => self.meter.start_new_session(),
            _ => {}
        }
    }

    pub fn spend(&self) -> Spend {
        self.meter.spend()
    }
}

/// The narrator model's price: the one configured for its agent, else the built-in one for its
/// model, else nothing for a local endpoint, which costs no dollars.
pub fn price_of(settings: &Settings, model: &ModelRef) -> Option<TokenPrice> {
    let agent = settings.agents.get(&model.agent);
    let model_id = model
        .model
        .as_deref()
        .or_else(|| agent.and_then(|agent| agent.model.as_deref()));
    agent
        .and_then(|agent| agent.price)
        .or_else(|| model_id.and_then(builtin_price))
        .or_else(|| {
            settings
                .endpoints
                .get(&model.agent)
                .filter(|endpoint| endpoint.local)
                .map(|_| TokenPrice {
                    input_per_mtok: 0.0,
                    output_per_mtok: 0.0,
                    cached_input_per_mtok: Some(0.0),
                })
        })
}

/// The larger of two counts of the same calls, such as the stream's and the session end's:
/// tokens from the one with more, and dollars from the one with more. On equal dollars, the
/// one that saw more tokens wins, then a known cost.
pub fn larger(a: Spend, b: Spend) -> Spend {
    let by_tokens = a.total_tokens().cmp(&b.total_tokens());
    let tokens = if by_tokens == Ordering::Less { b } else { a };
    let cost = match a.cost_usd.total_cmp(&b.cost_usd).then(by_tokens) {
        Ordering::Greater => a,
        Ordering::Less => b,
        Ordering::Equal if a.cost_partial => b,
        Ordering::Equal => a,
    };
    Spend {
        input_tokens: tokens.input_tokens,
        output_tokens: tokens.output_tokens,
        cached_tokens: tokens.cached_tokens,
        cost_usd: cost.cost_usd,
        cost_partial: cost.cost_partial,
        cost_estimated: cost.cost_estimated,
    }
}

/// Anything spent at all, so a failure that cost something is always recorded.
pub fn is_nothing(spend: &Spend) -> bool {
    spend.total_tokens() == 0 && spend.cost_usd <= 0.0 && !spend.cost_partial
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, output: u64, cost: Option<f64>, cumulative: bool) -> RunnerEvent {
        RunnerEvent::Agent(AgentEvent::Usage {
            input_tokens: input,
            output_tokens: output,
            cached_tokens: 0,
            cost_usd: cost,
            cumulative,
        })
    }

    fn result() -> RunnerEvent {
        RunnerEvent::Agent(AgentEvent::Result {
            text: None,
            structured: None,
            is_error: false,
        })
    }

    #[test]
    fn increments_add_and_totals_replace_within_a_session() {
        let mut tally = CallTally::new(None);
        tally.observe(&usage(100, 5, None, false));
        tally.observe(&usage(100, 5, None, false));
        assert!(tally.spend().cost_partial);
        tally.observe(&usage(300, 12, Some(0.02), true));
        let spend = tally.spend();
        assert_eq!((spend.input_tokens, spend.output_tokens), (300, 12));
        assert!((spend.cost_usd - 0.02).abs() < 1e-9);
        assert!(!spend.cost_partial);
    }

    #[test]
    fn a_retry_session_adds_to_the_first() {
        let mut tally = CallTally::new(None);
        tally.observe(&usage(300, 12, Some(0.02), true));
        tally.observe(&result());
        tally.observe(&usage(200, 10, Some(0.01), true));
        tally.observe(&result());
        let spend = tally.spend();
        assert_eq!(spend.input_tokens, 500);
        assert!((spend.cost_usd - 0.03).abs() < 1e-9);
    }

    #[test]
    fn a_price_fills_in_missing_costs() {
        let price = TokenPrice {
            input_per_mtok: 1.0,
            output_per_mtok: 5.0,
            cached_input_per_mtok: None,
        };
        let mut tally = CallTally::new(Some(price));
        tally.observe(&usage(1_000_000, 0, None, true));
        let spend = tally.spend();
        assert!((spend.cost_usd - 1.0).abs() < 1e-9);
        assert!(spend.cost_estimated && !spend.cost_partial);
    }

    #[test]
    fn prices_come_from_settings_then_the_table_then_local_endpoints() {
        let mut settings = Settings::default();
        let haiku: ModelRef = "claude-code/haiku".parse().unwrap();
        assert!(price_of(&settings, &haiku).is_some());
        let unknown: ModelRef = "openrouter/some-model".parse().unwrap();
        assert_eq!(price_of(&settings, &unknown), None);
        let local: ModelRef = "ollama/llama3.2".parse().unwrap();
        settings
            .endpoints
            .entry("ollama".to_owned())
            .or_default()
            .local = true;
        assert_eq!(price_of(&settings, &local).unwrap().output_per_mtok, 0.0);
    }

    #[test]
    fn the_larger_count_wins_and_a_known_cost_wins_a_tie() {
        let streamed = Spend {
            input_tokens: 500,
            output_tokens: 20,
            cost_usd: 0.03,
            ..Spend::default()
        };
        let ended = Spend {
            input_tokens: 300,
            output_tokens: 12,
            cost_usd: 0.02,
            ..Spend::default()
        };
        assert_eq!(larger(streamed, ended), streamed);
        assert_eq!(larger(ended, streamed), streamed);
        let partial = Spend {
            cost_partial: true,
            ..ended
        };
        assert!(!larger(partial, ended).cost_partial);
        // A stream that reported nothing doesn't hide an unknown cost.
        let unreported = Spend {
            cost_usd: 0.0,
            ..partial
        };
        assert!(larger(Spend::default(), unreported).cost_partial);
        assert!(is_nothing(&Spend::default()));
        assert!(!is_nothing(&partial));
    }
}
