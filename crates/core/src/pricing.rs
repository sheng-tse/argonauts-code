//! List prices for the models the adapters test with, so a dollar budget can be tracked while a
//! CLI reports only tokens.

use crate::budget::TokenPrice;

/// Dollars per million tokens for input, output, and cached input, from OpenRouter list prices
/// on 2026-09-29, and Claude Code's changelog for the 5.5 models. Claude Code reports its own
/// cost, so its prices only shape estimates.
const PRICES: &[(&str, f64, f64, f64)] = &[
    ("haiku", 1.00, 5.00, 0.10),
    ("claude-haiku-4-5", 1.00, 5.00, 0.10),
    ("claude-opus-5-5", 4.00, 20.00, 0.20),
    ("claude-sonnet-5-5", 2.00, 10.00, 0.20),
    ("gpt-5.6-luna", 0.20, 1.20, 0.02),
    ("gpt-5.6-terra", 2.00, 12.00, 0.20),
    ("gpt-5.6-sol", 2.00, 10.00, 0.20),
    ("gpt-6-luna", 0.10, 0.50, 0.01),
    ("gpt-6-sol", 2.00, 10.00, 0.20),
    ("gpt-6-astra", 10.00, 50.00, 1.00),
    ("gemini-3.6-flash", 0.75, 3.75, 0.075),
];

/// Suffixes that name a variant of a model at the same price, such as agy's thinking levels.
const SAME_PRICE_SUFFIXES: &[&str] = &["low", "medium", "high", "minimal"];

/// Matches the exact id, a dated snapshot such as `claude-haiku-4-5-20251001`, or a thinking
/// level such as `gemini-3.6-flash-low`.
pub fn builtin_price(model: &str) -> Option<TokenPrice> {
    let model = model.trim().to_ascii_lowercase();
    PRICES
        .iter()
        .find(|(id, ..)| names_model(id, &model))
        .map(|&(_, input, output, cached)| TokenPrice {
            input_per_mtok: input,
            output_per_mtok: output,
            cached_input_per_mtok: Some(cached),
        })
}

fn names_model(id: &str, model: &str) -> bool {
    let Some(rest) = model.strip_prefix(id) else {
        return false;
    };
    let Some(suffix) = rest.strip_prefix('-') else {
        return rest.is_empty();
    };
    let dated = !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit());
    dated || SAME_PRICE_SUFFIXES.contains(&suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cheapest_models_have_prices() {
        for model in [
            "haiku",
            "gpt-5.6-luna",
            "gpt-6-luna",
            "gemini-3.6-flash-low",
        ] {
            assert!(builtin_price(model).is_some(), "{model}");
        }
    }

    #[test]
    fn snapshots_and_levels_match_but_other_models_do_not() {
        let haiku = builtin_price("claude-haiku-4-5-20251001").unwrap();
        assert_eq!(haiku.input_per_mtok, 1.0);
        assert_eq!(haiku.cached_input_per_mtok, Some(0.10));
        assert!(builtin_price("Gemini-3.6-Flash-High").is_some());
        assert!(builtin_price("gemini-3.6-flash-lite").is_none());
        assert!(builtin_price("gpt-6-luna-pro").is_none());
        assert!(builtin_price("gpt-6").is_none());
        assert!(builtin_price("").is_none());
    }

    #[test]
    fn claude_code_defaults_have_prices() {
        let opus = builtin_price("claude-opus-5-5").unwrap();
        assert_eq!((opus.input_per_mtok, opus.output_per_mtok), (4.0, 20.0));
        assert_eq!(opus.cached_input_per_mtok, Some(0.20));
        let sonnet = builtin_price("claude-sonnet-5-5").unwrap();
        assert_eq!((sonnet.input_per_mtok, sonnet.output_per_mtok), (2.0, 10.0));
        assert_eq!(sonnet.cached_input_per_mtok, Some(0.20));
        assert!(builtin_price("claude-opus-5").is_none());
    }

    #[test]
    fn prices_estimate_cost() {
        let luna = builtin_price("gpt-5.6-luna").unwrap();
        let cost = luna.cost(1_000_000, 1_000_000, 0);
        assert!((cost - 1.40).abs() < 1e-9, "{cost}");
    }
}
