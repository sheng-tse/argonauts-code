//! List prices: the table built in, which also lets a dollar limit be tracked while a CLI
//! reports only tokens, and the price list that gives every model an API value, from your
//! settings first, then the public model list, then the table, then the nearest model's.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::display_names::DisplayNames;
use crate::meter::TokenPrice;
use crate::names::ModelRef;
use crate::overview::Tokens;

/// Dollars per million tokens for input, output, and cached input. Anthropic's from its own
/// price list on 2026-10-06, the others from OpenRouter list prices on 2026-09-29. Claude Code
/// reports its own cost, so its prices only shape estimates.
const PRICES: &[(&str, f64, f64, f64)] = &[
    ("haiku", 0.10, 0.50, 0.01),
    ("claude-haiku-5-5", 0.10, 0.50, 0.01),
    ("claude-haiku-4-5", 1.00, 5.00, 0.10),
    ("claude-opus-5-5", 4.00, 20.00, 0.20),
    ("claude-opus-5", 5.00, 25.00, 0.50),
    ("claude-opus-4-8", 5.00, 25.00, 0.50),
    ("claude-opus-4-7", 5.00, 25.00, 0.50),
    ("claude-opus-4-6", 5.00, 25.00, 0.50),
    ("claude-opus-4-5", 5.00, 25.00, 0.50),
    ("claude-sonnet-5-5", 2.00, 10.00, 0.20),
    ("claude-sonnet-5", 2.00, 10.00, 0.20),
    ("claude-sonnet-4-6", 3.00, 15.00, 0.30),
    ("claude-sonnet-4-5", 3.00, 15.00, 0.30),
    ("claude-fable-5-1", 10.00, 50.00, 0.25),
    ("claude-fable-5", 10.00, 50.00, 1.00),
    ("claude-mythos-5-1", 10.00, 50.00, 0.25),
    ("claude-mythos-5", 10.00, 50.00, 1.00),
    ("claude-mythos-preview", 25.00, 125.00, 2.50),
    ("gpt-5.6-luna", 0.20, 1.20, 0.02),
    ("gpt-5.6-terra", 2.00, 12.00, 0.20),
    ("gpt-5.6-sol", 2.00, 10.00, 0.20),
    ("gpt-6-luna", 0.10, 0.50, 0.01),
    ("gpt-6-sol", 2.00, 10.00, 0.20),
    // OpenAI's own list price on 2026-10-20, for prompts up to 272K tokens.
    ("gpt-6.1-sol", 2.00, 10.00, 0.10),
    ("gpt-6-astra", 10.00, 50.00, 1.00),
    ("gemini-3.6-flash", 0.75, 3.75, 0.075),
    // Its introductory price, which matches 3.6's.
    ("gemini-3.8-flash", 0.75, 3.75, 0.075),
    // Gemini 3 Pro's list price, which its 3.6 release keeps until a newer one is listed.
    ("gemini-3.6-pro", 2.00, 12.00, 0.20),
];

/// Suffixes that name a variant of a model at the same price, such as agy's thinking levels.
const SAME_PRICE_SUFFIXES: &[&str] = &["low", "medium", "high", "minimal"];

/// What Anthropic charges to write the cache for five minutes, against its input price.
const CLAUDE_CACHE_WRITE: f64 = 1.25;
/// And for an hour, which is how long Claude Code keeps what it writes.
const CLAUDE_CACHE_WRITE_HOUR: f64 = 2.0;

/// Claude models whose prompts past a size cost more: the size in tokens, then the input and
/// output prices past it. Anthropic's price list on 2026-10-06.
const LONG_PROMPT_PRICES: &[(&str, u64, f64, f64)] = &[
    ("haiku", 100_000, 0.50, 2.50),
    ("claude-haiku-5-5", 100_000, 0.50, 2.50),
];

/// One request to Anthropic's API by its tokens: new input, cache reads, cache writes by how
/// long they keep, and output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequestTokens {
    pub input: u64,
    pub cache_read: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub output: u64,
}

impl RequestTokens {
    /// Everything the request sent, which decides whether a long prompt's price applies.
    pub fn prompt(&self) -> u64 {
        self.input
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write_5m)
            .saturating_add(self.cache_write_1h)
    }
}

/// What Anthropic bills for one request to a Claude model at list price, in dollars. A prompt
/// past the model's long-prompt size has all its tokens at that price, and cache writes cost
/// 1.25 times the input price for five minutes and twice it for an hour. `None` for a model
/// that isn't Claude or that the table doesn't price.
pub fn claude_request_cost(model: &str, tokens: &RequestTokens) -> Option<f64> {
    let model = model.trim().to_ascii_lowercase();
    if !(model.contains("claude") || model == "haiku") {
        return None;
    }
    let base = builtin_price(&model)?;
    let read = base.cached_input_per_mtok.unwrap_or(base.input_per_mtok);
    let long = LONG_PROMPT_PRICES
        .iter()
        .find(|(id, ..)| names_model(id, &model))
        .filter(|&&(_, size, ..)| tokens.prompt() > size);
    let (input, output, read) = match long {
        // A cache read keeps its share of the input price.
        Some(&(_, _, input, output)) => (input, output, read * input / base.input_per_mtok),
        None => (base.input_per_mtok, base.output_per_mtok, read),
    };
    let usd = tokens.input as f64 * input
        + tokens.cache_read as f64 * read
        + tokens.cache_write_5m as f64 * input * CLAUDE_CACHE_WRITE
        + tokens.cache_write_1h as f64 * input * CLAUDE_CACHE_WRITE_HOUR
        + tokens.output as f64 * output;
    Some(usd / 1e6)
}

/// Matches the exact id, a dated snapshot such as `claude-haiku-4-5-20251001`, or a thinking
/// level such as `gemini-3.8-flash-low`.
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

/// A snapshot's date is eight digits, so `claude-opus-5-5` is never read as a snapshot of
/// `claude-opus-5`.
pub(crate) fn names_model(id: &str, model: &str) -> bool {
    let Some(rest) = model.strip_prefix(id) else {
        return false;
    };
    let Some(suffix) = rest.strip_prefix('-') else {
        return rest.is_empty();
    };
    let dated = suffix.len() == 8 && suffix.bytes().all(|b| b.is_ascii_digit());
    dated || SAME_PRICE_SUFFIXES.contains(&suffix)
}

/// A model's list price in dollars per million tokens of each kind the CLIs count.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListPrice {
    /// New input, not read from the cache.
    pub input: f64,
    pub output: f64,
    /// Input read from the cache. The input price when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    /// Input written to the cache. The input price when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
}

impl ListPrice {
    pub const FREE: ListPrice = ListPrice {
        input: 0.0,
        output: 0.0,
        cache_read: Some(0.0),
        cache_write: Some(0.0),
    };

    /// What `tokens` cost at this price, in dollars.
    pub fn value(&self, tokens: &Tokens) -> f64 {
        let read = self.cache_read.unwrap_or(self.input);
        let write = self.cache_write.unwrap_or(self.input);
        (tokens.input as f64 * self.input
            + tokens.output as f64 * self.output
            + tokens.cache_read as f64 * read
            + tokens.cache_write as f64 * write)
            / 1e6
    }

    /// A price that is a real amount of dollars, so no estimate goes negative or vanishes.
    pub fn is_valid(&self) -> bool {
        [
            Some(self.input),
            Some(self.output),
            self.cache_read,
            self.cache_write,
        ]
        .into_iter()
        .flatten()
        .all(|usd| usd.is_finite() && usd >= 0.0)
    }

    /// The built-in table's price, with a Claude model's cache writes at Anthropic's rate.
    fn builtin(model: &str) -> Option<ListPrice> {
        let price = builtin_price(model)?;
        let write = if model.contains("claude") || model == "haiku" {
            price.input_per_mtok * CLAUDE_CACHE_WRITE
        } else {
            price.input_per_mtok
        };
        Some(ListPrice {
            input: price.input_per_mtok,
            output: price.output_per_mtok,
            cache_read: price.cached_input_per_mtok,
            cache_write: Some(write),
        })
    }
}

/// Every model's list price, so each has an API value: your own prices, then those the public
/// model list gives, then the table built in, then the price of the newest model of its
/// provider, and for a model none of them knows, its CLI's default's. A model that runs on
/// this computer costs nothing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PriceList {
    yours: BTreeMap<String, ListPrice>,
    public: BTreeMap<String, ListPrice>,
    /// CLIs and endpoints that run their models on this computer.
    local: BTreeSet<String>,
    /// Which model a session that names none runs.
    names: DisplayNames,
}

impl PriceList {
    pub fn new(
        yours: impl IntoIterator<Item = (String, ListPrice)>,
        public: impl IntoIterator<Item = (String, ListPrice)>,
        local: impl IntoIterator<Item = String>,
    ) -> Self {
        fn clean(
            prices: impl IntoIterator<Item = (String, ListPrice)>,
        ) -> BTreeMap<String, ListPrice> {
            prices
                .into_iter()
                .filter(|(_, price)| price.is_valid())
                .map(|(model, price)| (bare(&model), price))
                .collect()
        }
        PriceList {
            yours: clean(yours),
            public: clean(public),
            local: local.into_iter().collect(),
            names: DisplayNames::default(),
        }
    }

    /// A session that names no model is priced as the model its CLI's own list says it runs,
    /// as `(cli, model)`.
    pub fn with_cli_defaults(
        mut self,
        defaults: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        self.names.defaults = defaults.into_iter().collect();
        self
    }

    /// A session that names no model is priced as the model `names` says it runs
    /// ([`DisplayNames::running`]): the one your settings give its CLI, else its CLI's default.
    pub fn with_names(mut self, names: DisplayNames) -> Self {
        self.names = names;
        self
    }

    /// The price of `model` as `agent` runs it, or when it names none, of the model the
    /// session is named after. A model nobody prices is priced as the newest of its maker's,
    /// else as its CLI's default.
    pub fn price(&self, agent: &str, model: Option<&str>) -> ListPrice {
        if self.local.contains(agent) {
            return ListPrice::FREE;
        }
        let named = model.map(str::trim).filter(|model| !model.is_empty());
        let runs = |model: Option<&str>| {
            self.names
                .running(&ModelRef::new(agent, model.map(str::to_owned)))
                .model
                .map(|model| bare(&model))
        };
        let model = runs(named).unwrap_or_else(|| UNNAMED.to_owned());
        let listed = |prices: &BTreeMap<String, ListPrice>, model: &str| {
            prices
                .iter()
                .find(|(id, _)| names_model(id, model))
                .map(|(_, price)| *price)
        };
        let priced = |model: &str| {
            listed(&self.yours, model)
                .or_else(|| listed(&self.public, model))
                .or_else(|| ListPrice::builtin(model))
                .or_else(|| ListPrice::builtin(nearest(model)?))
        };
        priced(&model)
            .or_else(|| priced(&runs(None)?))
            .or_else(|| ListPrice::builtin(UNNAMED))
            .unwrap_or(ListPrice::FREE)
    }
}

/// The model a session is priced as when nothing says which model it runs, nor its CLI's
/// default: the middle of the table.
const UNNAMED: &str = "claude-sonnet-5-5";

/// `claude-opus-5-5` from `anthropic/claude-opus-5-5[1m]`: the id alone, lower case.
fn bare(model: &str) -> String {
    let model = model.trim().to_ascii_lowercase();
    let id = model.rsplit('/').next().unwrap_or(&model);
    id.split('[').next().unwrap_or(id).trim().to_owned()
}

/// The newest listed model of the same provider, for a model the table doesn't name.
fn nearest(model: &str) -> Option<&'static str> {
    let has = |word: &str| model.contains(word);
    let openai = has("gpt")
        || model
            .strip_prefix('o')
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()));
    Some(if has("fable") {
        "claude-fable-5-1"
    } else if has("mythos") {
        "claude-mythos-5-1"
    } else if has("opus") {
        "claude-opus-5-5"
    } else if has("sonnet") {
        "claude-sonnet-5-5"
    } else if has("haiku") {
        "claude-haiku-5-5"
    } else if openai {
        if has("luna") || has("mini") || has("nano") {
            "gpt-5.6-luna"
        } else if has("terra") {
            "gpt-5.6-terra"
        } else if has("astra") {
            "gpt-6-astra"
        } else {
            "gpt-5.6-sol"
        }
    } else if has("gemini") {
        if has("flash") {
            "gemini-3.8-flash"
        } else {
            "gemini-3.6-pro"
        }
    } else {
        return None;
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cheapest_models_have_prices() {
        // Codex's cheapest, then the fallback its recordings may run on.
        for model in [
            "haiku",
            "gpt-6-luna",
            "gpt-5.6-luna",
            "gemini-3.8-flash-low",
        ] {
            assert!(builtin_price(model).is_some(), "{model}");
        }
    }

    /// GPT-6.1-Sol has a price of its own: Sol's input and output, half its cache reads.
    #[test]
    fn a_point_release_is_priced_apart_from_its_model() {
        let sol = builtin_price("gpt-6-sol").unwrap();
        let newer = builtin_price("gpt-6.1-sol").unwrap();
        assert_eq!(newer.input_per_mtok, 2.0);
        assert_eq!(newer.output_per_mtok, 10.0);
        assert_eq!(newer.cached_input_per_mtok, Some(0.10));
        assert_eq!(sol.cached_input_per_mtok, Some(0.20));
    }

    #[test]
    fn snapshots_and_levels_match_but_other_models_do_not() {
        let haiku = builtin_price("claude-haiku-4-5-20251001").unwrap();
        assert_eq!(haiku.input_per_mtok, 1.0);
        assert_eq!(haiku.cached_input_per_mtok, Some(0.10));
        // `haiku` runs Haiku 5.5, a tenth of Haiku 4.5's price.
        let alias = builtin_price("haiku").unwrap();
        assert_eq!(alias, builtin_price("claude-haiku-5-5").unwrap());
        assert_eq!(alias.input_per_mtok, 0.10);
        assert!(builtin_price("Gemini-3.6-Flash-High").is_some());
        assert!(builtin_price("gemini-3.6-flash-lite").is_none());
        assert!(builtin_price("gpt-6-luna-pro").is_none());
        assert!(builtin_price("gpt-6").is_none());
        assert!(builtin_price("").is_none());
    }

    #[test]
    fn claude_code_models_have_prices() {
        let opus = builtin_price("claude-opus-5-5").unwrap();
        assert_eq!((opus.input_per_mtok, opus.output_per_mtok), (4.0, 20.0));
        assert_eq!(opus.cached_input_per_mtok, Some(0.20));
        let sonnet = builtin_price("claude-sonnet-5-5").unwrap();
        assert_eq!((sonnet.input_per_mtok, sonnet.output_per_mtok), (2.0, 10.0));
        assert_eq!(sonnet.cached_input_per_mtok, Some(0.20));
        let opus_5 = builtin_price("claude-opus-5").unwrap();
        assert_eq!(opus_5.input_per_mtok, 5.0);
        assert!(builtin_price("claude-opus-5-1").is_none());
    }

    /// Claude Code's own totals for recorded sessions, priced as Anthropic bills them: past
    /// 100K tokens of prompt Haiku 5.5 costs five times as much, and Claude Code writes the cache
    /// for an hour.
    #[test]
    fn claude_requests_price_as_anthropic_bills_them() {
        let cost =
            |tokens: RequestTokens| claude_request_cost("claude-haiku-5-5", &tokens).unwrap();
        let close = |got: f64, want: f64| assert!((got - want).abs() < 1e-12, "{got} != {want}");
        // A run session's first request, at 361,887 tokens of prompt.
        let opening = RequestTokens {
            input: 2,
            cache_write_1h: 361_885,
            output: 2,
            ..RequestTokens::default()
        };
        close(cost(opening), 0.361891);
        // That session's result.
        close(
            cost(RequestTokens {
                output: 5_039,
                ..opening
            }),
            0.3744835,
        );
        // A conversation turn's requests, each under 100K tokens.
        close(
            cost(RequestTokens {
                input: 6,
                cache_read: 66_295,
                cache_write_1h: 18_330,
                output: 652,
                ..RequestTokens::default()
            }),
            0.00465555,
        );
        // Requests past 100K that read a cache another session wrote.
        close(
            cost(RequestTokens {
                input: 8,
                cache_read: 1_514_090,
                cache_write_1h: 11_364,
                output: 4_021,
                ..RequestTokens::default()
            }),
            0.097125,
        );
        // Five minutes of cache at 1.25 times the input price.
        let five = RequestTokens {
            cache_write_5m: 10_000,
            ..RequestTokens::default()
        };
        close(claude_request_cost("haiku", &five).unwrap(), 0.00125);
        assert_eq!(claude_request_cost("gpt-6-luna", &five), None);
        assert_eq!(claude_request_cost("claude-opus-9", &five), None);
    }

    #[test]
    fn prices_estimate_cost() {
        let luna = builtin_price("gpt-5.6-luna").unwrap();
        let cost = luna.cost(1_000_000, 1_000_000, 0);
        assert!((cost - 1.40).abs() < 1e-9, "{cost}");
    }

    fn million_each() -> Tokens {
        Tokens {
            input: 1_000_000,
            output: 1_000_000,
            cache_read: 1_000_000,
            cache_write: 1_000_000,
        }
    }

    #[test]
    fn every_model_gets_a_price_yours_first() {
        let yours = ListPrice {
            input: 1.0,
            output: 2.0,
            cache_read: None,
            cache_write: None,
        };
        let public = ListPrice {
            input: 3.0,
            output: 3.0,
            cache_read: Some(0.0),
            cache_write: Some(0.0),
        };
        let prices = PriceList::new(
            [("claude-opus-5-5".to_owned(), yours)],
            [
                ("claude-opus-5-5".to_owned(), public),
                ("kimi-k3".to_owned(), public),
            ],
            ["ollama".to_owned()],
        );
        let tokens = million_each();
        let value = |agent: &str, model: Option<&str>| prices.price(agent, model).value(&tokens);
        assert_eq!(
            value("claude-code", Some("claude-opus-5-5")),
            1.0 + 2.0 + 1.0 + 1.0
        );
        assert_eq!(value("opencode", Some("moonshot/kimi-k3")), 6.0);
        // The table built in, with Claude's cache writes a quarter dearer than its input.
        assert_eq!(
            value("claude-code", Some("claude-sonnet-5-5")),
            2.0 + 10.0 + 0.2 + 2.5
        );
        assert_eq!(
            value("claude-code", Some("claude-sonnet-5-5[1m]")),
            value("claude-code", Some("claude-sonnet-5-5"))
        );
        // A model no list names takes its provider's newest price, else its CLI's default's,
        // else the middle of the table's.
        assert_eq!(
            value("claude-code", Some("claude-sonnet-6-1")),
            value("claude-code", Some("claude-sonnet-5-5"))
        );
        assert_eq!(
            value("codex", Some("gpt-5.6")),
            value("codex", Some("gpt-5.6-sol"))
        );
        assert_eq!(
            value("antigravity-cli", Some("gemini-3.6-flash-thinking")),
            value("antigravity-cli", Some("gemini-3.6-flash"))
        );
        let middle = value("claude-code", Some("claude-sonnet-5-5"));
        assert_eq!(value("codex", Some("unnamed")), middle);
        assert_eq!(value("codex", None), middle);
        assert_eq!(value("antigravity-cli", None), middle);
        assert_eq!(value("someone-else", Some("unnamed")), middle);
        assert_eq!(value("ollama", Some("llama3.2")), 0.0);
    }

    /// The model each CLI's recorded list runs when nothing names one: Codex's first by its
    /// priority, agy's first line.
    fn recorded_defaults() -> [(String, String); 2] {
        let fixtures =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../registry/tests/fixtures");
        let read = |file: &str| std::fs::read_to_string(fixtures.join(file)).unwrap();
        let codex: serde_json::Value =
            serde_json::from_str(&read("codex/debug-models.json")).unwrap();
        let first = codex["models"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|model| model["visibility"] == "list")
            .min_by_key(|model| model["priority"].as_i64())
            .and_then(|model| model["slug"].as_str())
            .unwrap()
            .to_owned();
        let agy: serde_json::Value = serde_json::from_str(&read("agy/models.json")).unwrap();
        let line = agy["response"].as_str().unwrap().lines().next().unwrap();
        let agy_first = line.split('\t').next().unwrap().to_owned();
        [
            ("codex".to_owned(), first),
            ("antigravity-cli".to_owned(), agy_first),
        ]
    }

    /// A session that names no model is priced as the model it is named after: the one your
    /// settings give its CLI, else the one its CLI's list runs, as Codex's and agy's recorded
    /// lists give them, never another maker's model.
    #[test]
    fn a_session_naming_no_model_is_priced_as_the_model_it_is_named_after() {
        let tokens = million_each();
        let table = PriceList::default();
        let defaults = recorded_defaults();
        assert_eq!(defaults[0].1, "gpt-6-astra");
        assert_eq!(defaults[1].1, "gemini-3.8-flash-high");
        let listed = PriceList::default().with_cli_defaults(defaults.clone());
        for (cli, model) in &defaults {
            let names = DisplayNames {
                defaults: defaults.iter().cloned().collect(),
                ..DisplayNames::default()
            };
            let named = names.running(&ModelRef::new(cli.as_str(), None)).model;
            assert_eq!(named.as_deref(), Some(model.as_str()));
            assert_eq!(
                listed.price(cli, None).value(&tokens),
                table.price(cli, Some(model)).value(&tokens),
                "{cli}"
            );
        }
        assert_ne!(
            listed.price("antigravity-cli", None).value(&tokens),
            table.price("antigravity-cli", None).value(&tokens)
        );
        assert_eq!(
            listed
                .price("antigravity-cli", Some("gemini-3.6-pro"))
                .value(&tokens),
            table
                .price("antigravity-cli", Some("gemini-3.6-pro"))
                .value(&tokens)
        );

        let configured = PriceList::default().with_names(DisplayNames {
            configured: [("codex".to_owned(), "gpt-6-luna".to_owned())].into(),
            defaults: defaults.iter().cloned().collect(),
            ..DisplayNames::default()
        });
        assert_eq!(
            configured.price("codex", None).value(&tokens),
            table.price("codex", Some("gpt-6-luna")).value(&tokens)
        );
    }

    #[test]
    fn a_price_that_isnt_real_dollars_is_left_out() {
        let broken = ListPrice {
            input: f64::NAN,
            output: -1.0,
            cache_read: None,
            cache_write: None,
        };
        let prices = PriceList::new([("claude-opus-5-5".to_owned(), broken)], [], []);
        let price = prices.price("claude-code", Some("claude-opus-5-5"));
        assert_eq!(price.input, 4.0);
    }
}
