//! How much context a session may hold (`sessions.context`). By default each CLI is asked for
//! the largest window it offers for the model, and a session starts fresh only near that
//! window's end (`sessions.rotate_at`). A token count here lowers it, for cheaper turns.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `"largest"`, or a token count such as `"200k"` or `200000`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ContextSize {
    Tokens(u64),
    /// `largest`, or a count with an optional `k` or `m`.
    Text(String),
}

impl Default for ContextSize {
    fn default() -> Self {
        Self::Text(LARGEST.to_owned())
    }
}

/// The word for no cap of yours.
pub const LARGEST: &str = "largest";

/// A window smaller than this couldn't hold one turn of any CLI.
pub const MIN_CONTEXT_TOKENS: u64 = 20_000;

impl ContextSize {
    /// Your cap in tokens. `None` is the largest window the CLI offers.
    pub fn cap(&self) -> Result<Option<u64>, String> {
        let count = match self {
            Self::Tokens(count) => *count,
            Self::Text(text) if text.trim().eq_ignore_ascii_case(LARGEST) => return Ok(None),
            Self::Text(text) => token_count(text).ok_or_else(|| {
                format!("context {text:?}: use \"largest\", or a count such as \"200k\" or \"1m\"")
            })?,
        };
        if count < MIN_CONTEXT_TOKENS {
            return Err(format!(
                "context is {count} tokens, too few for one turn; use \"largest\", or a count \
                 such as \"200k\""
            ));
        }
        Ok(Some(count))
    }

    /// The window a session gets on a model whose CLI offers `largest` at most: that, held
    /// to your cap. `None` when neither is known, and then the CLI's own default stands.
    pub fn window(&self, largest: Option<u64>) -> Option<u64> {
        match (largest, self.cap().ok().flatten()) {
            (Some(largest), Some(cap)) => Some(largest.min(cap)),
            (largest, cap) => largest.or(cap),
        }
    }

    /// What a session asks its CLI for, on a model whose CLI offers `largest` at most when
    /// the registry lists it.
    pub fn ask(&self, largest: Option<u64>) -> ContextAsk {
        match self.cap().ok().flatten() {
            None => ContextAsk::Largest { known: largest },
            Some(cap) => ContextAsk::Held {
                tokens: largest.map_or(cap, |largest| largest.min(cap)),
            },
        }
    }
}

/// How much context a session asks its CLI for, where the CLI has a setting for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "ask", rename_all = "snake_case")]
pub enum ContextAsk {
    /// Nothing is passed, and the CLI's own default stands.
    #[default]
    AsItIs,
    /// The largest window the CLI offers for the model. `known` is that window in tokens
    /// when the registry lists it. Without it an adapter passes what it knows of its own
    /// CLI's names, or nothing.
    Largest { known: Option<u64> },
    /// No more than this many tokens: your cap, or the model's largest when that's smaller.
    Held { tokens: u64 },
}

impl ContextAsk {
    pub fn is_as_it_is(&self) -> bool {
        *self == Self::AsItIs
    }
}

/// `200k`, `1m`, or a plain count. `None` when it isn't one, or is zero.
pub fn token_count(text: &str) -> Option<u64> {
    let text = text.trim();
    let (digits, scale) = match text.char_indices().last() {
        Some((at, 'k' | 'K')) => (&text[..at], 1_000),
        Some((at, 'm' | 'M')) => (&text[..at], 1_000_000),
        _ => (text, 1),
    };
    let count: u64 = digits.trim().parse().ok()?;
    count.checked_mul(scale).filter(|count| *count > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_the_largest_window_and_a_count_lowers_it() {
        let largest = ContextSize::default();
        assert_eq!(largest.cap(), Ok(None));
        assert_eq!(largest.window(Some(872_000)), Some(872_000));
        assert_eq!(largest.window(None), None);

        for lowered in [
            ContextSize::Text("200k".to_owned()),
            ContextSize::Tokens(200_000),
        ] {
            assert_eq!(lowered.cap(), Ok(Some(200_000)));
            assert_eq!(lowered.window(Some(1_000_000)), Some(200_000));
            // A cap above what the CLI offers changes nothing.
            assert_eq!(lowered.window(Some(128_000)), Some(128_000));
            // With no window known, your cap is where the session is held.
            assert_eq!(lowered.window(None), Some(200_000));
            assert_eq!(
                lowered.ask(Some(1_000_000)),
                ContextAsk::Held { tokens: 200_000 }
            );
            assert_eq!(
                lowered.ask(Some(128_000)),
                ContextAsk::Held { tokens: 128_000 }
            );
        }
        assert_eq!(
            largest.ask(Some(872_000)),
            ContextAsk::Largest {
                known: Some(872_000)
            }
        );
        assert_eq!(largest.ask(None), ContextAsk::Largest { known: None });
        assert_eq!(
            ContextSize::Text("1M".to_owned()).cap(),
            Ok(Some(1_000_000))
        );
    }

    #[test]
    fn a_count_that_reads_wrong_or_is_too_small_is_refused() {
        for bad in ["", "lots", "0", "12q"] {
            let err = ContextSize::Text(bad.to_owned()).cap().unwrap_err();
            assert!(err.contains("\"largest\""), "{err}");
        }
        assert!(
            ContextSize::Tokens(500)
                .cap()
                .unwrap_err()
                .contains("too few")
        );
    }

    #[test]
    fn it_reads_from_toml_as_a_word_or_a_count() {
        #[derive(Deserialize)]
        struct Holder {
            context: ContextSize,
        }
        let word: Holder = toml::from_str("context = \"largest\"").unwrap();
        assert_eq!(word.context, ContextSize::default());
        let count: Holder = toml::from_str("context = 150000").unwrap();
        assert_eq!(count.context, ContextSize::Tokens(150_000));
    }
}
