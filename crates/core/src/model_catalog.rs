//! How the pickers name and describe models. Names derive from the model id by one rule, so new
//! models name themselves. Descriptions, the "recommended" mark, and the picker's order come from
//! a catalog merged in layers, each later one winning field by field: the catalog shipped with
//! Argonauts Code, a newer one fetched from its repository, what the CLIs say of their own models,
//! and your own entries in `models.toml`.

use std::cmp::{Ordering, Reverse};

use serde::{Deserialize, Serialize};

use crate::names::ModelRef;
use crate::pricing::ListPrice;
use crate::registry::ModelEntry;

/// The catalog format this build reads. A fetched catalog with a newer one is skipped.
pub const CATALOG_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelCatalog {
    pub version: u32,
    #[serde(rename = "model", skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<CatalogEntry>,
    /// What each effort level means, for models whose entry doesn't say.
    #[serde(rename = "effort", skip_serializing_if = "Vec::is_empty")]
    pub efforts: Vec<EffortNote>,
    /// List prices, so a model that ships after a release still gets an API value.
    #[serde(rename = "price", skip_serializing_if = "Vec::is_empty")]
    pub prices: Vec<CatalogPrice>,
}

/// One model's list price, in dollars per million tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogPrice {
    /// The model's id, such as `claude-opus-5-5`, which its dated snapshots share.
    pub model: String,
    pub input: Dollars,
    pub output: Dollars,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<Dollars>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<Dollars>,
}

impl CatalogPrice {
    pub fn list_price(&self) -> ListPrice {
        ListPrice {
            input: self.input.0,
            output: self.output.0,
            cache_read: self.cache_read.map(|usd| usd.0),
            cache_write: self.cache_write.map(|usd| usd.0),
        }
    }
}

/// An amount of dollars, compared exactly, so a catalog read twice compares equal.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Dollars(pub f64);

impl PartialEq for Dollars {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for Dollars {}

/// Every field but `id` is optional, so a later layer changes only what it sets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// `cli/model`, or the CLI alone for the model it runs when you name none.
    pub id: ModelRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended: Option<bool>,
    /// Lowest first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub efforts: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
    /// Short names the CLI resolves to this model, such as `opus`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    /// Where the picker lists it among its CLI's models, lowest first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<u32>,
    /// On a CLI's own entry: the model its default runs, when the CLI doesn't say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<String>,
    /// What a level means on this model, where it is different from the catalog's notes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effort_notes: Vec<EffortNote>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffortNote {
    pub level: String,
    /// One line.
    pub description: String,
}

/// What a picker shows for one model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelLabel {
    /// "Opus 5.5", or "Default" for the CLI's default.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub recommended: bool,
    /// No layer of the catalog knows it.
    #[serde(default)]
    pub new: bool,
}

impl CatalogEntry {
    pub fn new(id: ModelRef) -> Self {
        Self {
            id,
            name: None,
            description: None,
            recommended: None,
            efforts: None,
            default_effort: None,
            aliases: None,
            rank: None,
            runs: None,
            effort_notes: Vec::new(),
        }
    }

    /// The layer a CLI's own model list makes, where it names or describes the model.
    pub fn from_cli(entry: &ModelEntry) -> Option<Self> {
        (entry.name.is_some() || entry.description.is_some()).then(|| Self {
            name: entry.name.clone(),
            description: entry.description.clone(),
            ..Self::new(entry.id.clone())
        })
    }

    fn overlay(&mut self, later: CatalogEntry) {
        let CatalogEntry {
            id: _,
            name,
            description,
            recommended,
            efforts,
            default_effort,
            aliases,
            rank,
            runs,
            effort_notes,
        } = later;
        self.name = name.or(self.name.take());
        self.description = description.or(self.description.take());
        self.recommended = recommended.or(self.recommended);
        self.efforts = efforts.or(self.efforts.take());
        self.default_effort = default_effort.or(self.default_effort.take());
        self.aliases = aliases.or(self.aliases.take());
        self.rank = rank.or(self.rank);
        self.runs = runs.or(self.runs.take());
        overlay_notes(&mut self.effort_notes, effort_notes);
    }
}

fn overlay_notes(notes: &mut Vec<EffortNote>, later: Vec<EffortNote>) {
    for note in later {
        match notes.iter_mut().find(|known| known.level == note.level) {
            Some(known) => *known = note,
            None => notes.push(note),
        }
    }
}

impl ModelCatalog {
    /// Lays `later` over this one, field by field.
    pub fn merge(&mut self, later: ModelCatalog) {
        self.version = self.version.max(later.version);
        for entry in later.models {
            match self.models.iter_mut().find(|known| known.id == entry.id) {
                Some(known) => known.overlay(entry),
                None => self.models.push(entry),
            }
        }
        overlay_notes(&mut self.efforts, later.efforts);
        for price in later.prices {
            self.prices.retain(|known| known.model != price.model);
            self.prices.push(price);
        }
    }

    /// The entry for `id`, or for the model a CLI alias such as `claude-code/opus` names.
    pub fn entry(&self, id: &ModelRef) -> Option<&CatalogEntry> {
        self.models
            .iter()
            .find(|entry| entry.id == *id)
            .or_else(|| self.alias_target(id))
    }

    fn alias_target(&self, id: &ModelRef) -> Option<&CatalogEntry> {
        let alias = id.model.as_deref()?;
        self.models.iter().find(|entry| {
            entry.id.agent == id.agent
                && entry
                    .aliases
                    .as_ref()
                    .is_some_and(|aliases| aliases.iter().any(|known| known == alias))
        })
    }

    /// The model a session of `id` runs, spelled the one way the catalog spells it: the model
    /// an alias such as `opus` names, and for a CLI named alone the model its default runs.
    /// `None` when the catalog doesn't know it.
    pub fn runs(&self, id: &ModelRef) -> Option<String> {
        let entry = self.entry(id)?;
        match &entry.runs {
            // A default names a model, which may itself be an alias.
            Some(runs) if entry.id.model.as_ref() != Some(runs) => {
                let named = ModelRef::new(id.agent.clone(), Some(runs.clone()));
                Some(
                    self.entry(&named)
                        .and_then(|entry| entry.id.model.clone())
                        .unwrap_or_else(|| runs.clone()),
                )
            }
            _ => entry.id.model.clone(),
        }
    }

    /// Whether `id` is an alias the catalog resolves, rather than a model of its own.
    pub fn is_alias(&self, id: &ModelRef) -> bool {
        !self.models.iter().any(|entry| entry.id == *id) && self.alias_target(id).is_some()
    }

    pub fn label(&self, id: &ModelRef) -> ModelLabel {
        let entry = self.entry(id);
        let name = entry
            .and_then(|entry| entry.name.as_deref())
            .map(spelled_one_way)
            .unwrap_or_else(|| match &id.model {
                Some(model) => display_name_from_id(model),
                None => "Default".to_owned(),
            });
        ModelLabel {
            name,
            description: entry.and_then(|entry| entry.description.clone()),
            recommended: entry.and_then(|entry| entry.recommended) == Some(true),
            new: entry.is_none(),
        }
    }

    /// What `level` means on `id`: its own note, the catalog's, or Argonauts Code's own words.
    pub fn effort_note(&self, id: &ModelRef, level: &str) -> Option<String> {
        let own = self
            .entry(id)
            .and_then(|entry| entry.effort_notes.iter().find(|note| note.level == level));
        own.or_else(|| self.efforts.iter().find(|note| note.level == level))
            .map(|note| note.description.clone())
            .or_else(|| builtin_effort_note(level).map(str::to_owned))
    }

    /// Newest first: models the catalog hasn't ranked come first, by their version numbers, since
    /// they came out after it was written. Ranked ones follow in rank order.
    pub fn newest_first(&self, a: &ModelRef, b: &ModelRef) -> Ordering {
        let key = |id: &ModelRef| {
            let rank = self.entry(id).and_then(|entry| entry.rank);
            let version = id.model.as_deref().map(version_numbers).unwrap_or_default();
            (rank.is_some(), rank, Reverse(version))
        };
        key(a).cmp(&key(b)).then_with(|| a.cmp(b))
    }
}

/// Argonauts Code's own words for each effort level, when no catalog layer says more.
pub fn builtin_effort_note(level: &str) -> Option<&'static str> {
    Some(match level {
        "minimal" => "Answers right away, with almost no reasoning",
        "low" => "Quick answers with light reasoning",
        "medium" => "Balances speed and depth for everyday work",
        "high" => "Thinks longer for complex problems",
        "xhigh" => "Extra depth for hard, many-step problems",
        "max" => "The deepest reasoning. Slow and costly, so keep it for the hardest tasks",
        "ultra" => "The deepest reasoning, and it hands parts of the task to helpers",
        _ => return None,
    })
}

/// A model's name as every place in Argonauts Code writes it, with its words apart: a CLI's own
/// list may join them, as Codex's "GPT-5.6-Luna", and the same model then read two ways.
pub fn spelled_one_way(name: &str) -> String {
    name.split(|c: char| c.is_whitespace() || c == '-')
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A model's display name from its id: `claude-opus-5-5` reads "Opus 5.5",
/// `claude-haiku-4-5-20251001` reads "Haiku 4.5", and `qwen3-coder:30b` reads "Qwen3 Coder 30B".
/// A provider path, a release date, and `latest` are dropped, and a `[1m]` suffix reads
/// "(1M context)".
pub fn display_name_from_id(id: &str) -> String {
    let id = id.rsplit('/').next().unwrap_or(id).trim();
    let (base, context) = match id.strip_suffix(']').and_then(|rest| rest.rsplit_once('[')) {
        Some((base, size)) => (base, Some(size)),
        None => (id, None),
    };
    let mut tokens: Vec<&str> = base
        .split(['-', '_', ':', ' '])
        .filter(|token| !token.is_empty())
        .collect();
    drop_release_date(&mut tokens);
    if tokens
        .last()
        .is_some_and(|last| last.eq_ignore_ascii_case("latest"))
    {
        tokens.pop();
    }
    // The first word says it's Claude, unless only a version follows, as in `claude-3-5-sonnet`.
    if tokens.first() == Some(&"claude")
        && tokens
            .get(1)
            .is_some_and(|next| next.starts_with(|c: char| c.is_ascii_alphabetic()))
    {
        tokens.remove(0);
    }
    let mut words: Vec<String> = Vec::new();
    let mut pending_version: Vec<&str> = Vec::new();
    for token in tokens {
        if is_version_part(token) {
            pending_version.push(token);
            continue;
        }
        if !pending_version.is_empty() {
            words.push(pending_version.join("."));
            pending_version.clear();
        }
        words.push(word_case(token));
    }
    if !pending_version.is_empty() {
        words.push(pending_version.join("."));
    }
    if let Some(size) = context.filter(|size| !size.is_empty()) {
        words.push(format!("({} context)", size.to_ascii_uppercase()));
    }
    words.join(" ")
}

/// One or two digits, as in the `5-5` of `opus-5-5`. Longer runs are dates or sizes.
fn is_version_part(token: &str) -> bool {
    (1..=2).contains(&token.len()) && token.chars().all(|c| c.is_ascii_digit())
}

/// `20251001`, or `2025-10-01` split into three tokens, at the end.
fn drop_release_date(tokens: &mut Vec<&str>) {
    let digits =
        |token: &str, len: usize| token.len() == len && token.bytes().all(|b| b.is_ascii_digit());
    let date_tokens = match tokens.as_slice() {
        [_, .., last] if digits(last, 8) && last.starts_with("20") => 1,
        [_, .., year, month, day]
            if digits(year, 4) && year.starts_with("20") && digits(month, 2) && digits(day, 2) =>
        {
            3
        }
        _ => 0,
    };
    tokens.truncate(tokens.len() - date_tokens);
}

/// Short letter-only names without a vowel are acronyms (`gpt`, `glm`), sizes take a capital
/// unit (`30B`), and other words a capital first letter.
fn word_case(token: &str) -> String {
    const ACRONYMS: &[&str] = &["oss", "ai", "api", "moe", "vl"];
    let lower = token.to_ascii_lowercase();
    let letters_only = lower.chars().all(|c| c.is_ascii_alphabetic());
    let no_vowel = !lower.contains(['a', 'e', 'i', 'o', 'u', 'y']);
    if ACRONYMS.contains(&lower.as_str()) || (letters_only && no_vowel && lower.len() <= 4) {
        return lower.to_ascii_uppercase();
    }
    let size = lower.strip_suffix(['b', 'k', 'm']).is_some_and(|number| {
        !number.is_empty() && number.chars().all(|c| c.is_ascii_digit() || c == '.')
    });
    if size {
        return lower.to_ascii_uppercase();
    }
    let mut chars = token.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The numbers in a model id, for ordering versions: `gpt-6.1-sol` gives `[6, 1]`.
fn version_numbers(id: &str) -> Vec<u32> {
    let name = display_name_from_id(id);
    name.split(|c: char| !c.is_ascii_digit())
        .filter_map(|digits| digits.parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn id(text: &str) -> ModelRef {
        text.parse().unwrap()
    }

    #[test]
    fn names_derive_from_ids() {
        for (model, name) in [
            ("claude-opus-5-5", "Opus 5.5"),
            ("claude-haiku-4-5-20251001", "Haiku 4.5"),
            ("claude-sonnet-5", "Sonnet 5"),
            ("claude-fable-5-1", "Fable 5.1"),
            ("opus", "Opus"),
            ("claude-opus-5-5[1m]", "Opus 5.5 (1M context)"),
            ("claude-3-5-sonnet-20241022", "Claude 3.5 Sonnet"),
            ("gpt-6.1-sol", "GPT 6.1 Sol"),
            ("gpt-oss-120b", "GPT OSS 120B"),
            ("gemini-3.8-flash-high", "Gemini 3.8 Flash High"),
            ("z-ai/glm-5", "GLM 5"),
            ("qwen3-coder:30b", "Qwen3 Coder 30B"),
            ("llama3.1:latest", "Llama3.1"),
            ("gemini-2.5-pro-2025-06-05", "Gemini 2.5 Pro"),
            ("gpt-4-0613", "GPT 4 0613"),
            ("", ""),
        ] {
            assert_eq!(display_name_from_id(model), name, "{model}");
        }
    }

    fn seed() -> ModelCatalog {
        toml::from_str(
            r#"
            version = 1

            [[model]]
            id = "claude-code"
            description = "Best for everyday, complex tasks"
            recommended = true
            runs = "claude-opus-5-5"

            [[model]]
            id = "claude-code/claude-opus-5-5"
            name = "Opus 5.5"
            description = "For complex work and everyday tasks"
            efforts = ["low", "medium", "high", "xhigh", "max"]
            default_effort = "xhigh"
            aliases = ["opus"]
            rank = 1

            [[model]]
            id = "claude-code/claude-sonnet-4-6"
            description = "Efficient for routine tasks"
            default_effort = "high"
            rank = 12

            [[effort]]
            level = "max"
            description = "Deepest reasoning"
            "#,
        )
        .unwrap()
    }

    #[test]
    fn a_catalog_round_trips_through_toml_and_json() {
        let catalog = seed();
        assert_eq!(catalog.version, CATALOG_VERSION);
        assert_eq!(catalog.models.len(), 3);
        let text = toml::to_string(&catalog).unwrap();
        assert_eq!(toml::from_str::<ModelCatalog>(&text).unwrap(), catalog);
        let value = serde_json::to_value(&catalog).unwrap();
        assert_eq!(
            value["model"][0],
            json!({"id": "claude-code",
            "description": "Best for everyday, complex tasks", "recommended": true,
            "runs": "claude-opus-5-5"})
        );
        assert_eq!(
            serde_json::from_value::<ModelCatalog>(value).unwrap(),
            catalog
        );

        let label = ModelLabel {
            name: "Opus 5.5".into(),
            description: Some("For complex work".into()),
            recommended: false,
            new: false,
        };
        let value = serde_json::to_value(&label).unwrap();
        assert_eq!(serde_json::from_value::<ModelLabel>(value).unwrap(), label);
    }

    #[test]
    fn later_layers_win_field_by_field() {
        let mut catalog = seed();
        let codex = ModelEntry {
            name: Some("GPT-6.1-Sol".into()),
            description: Some("Latest workhorse model.".into()),
            ..entry("codex/gpt-6.1-sol")
        };
        let cli = ModelCatalog {
            models: CatalogEntry::from_cli(&codex).into_iter().collect(),
            ..ModelCatalog::default()
        };
        assert!(CatalogEntry::from_cli(&entry("codex/gpt-6-luna")).is_none());
        catalog.merge(cli);
        let yours: ModelCatalog = toml::from_str(
            r#"
            [[model]]
            id = "claude-code/claude-opus-5-5"
            description = "My usual"
            effort_notes = [{ level = "max", description = "Only for proofs" }]
            "#,
        )
        .unwrap();
        catalog.merge(yours);

        let opus = catalog.entry(&id("claude-code/claude-opus-5-5")).unwrap();
        assert_eq!(opus.description.as_deref(), Some("My usual"));
        assert_eq!(
            opus.name.as_deref(),
            Some("Opus 5.5"),
            "kept from the layer below"
        );
        assert_eq!(opus.rank, Some(1));
        assert_eq!(catalog.version, 1);

        let sol = catalog.label(&id("codex/gpt-6.1-sol"));
        assert_eq!(sol.name, "GPT 6.1 Sol");
        assert!(!sol.new);

        let fresh = catalog.label(&id("claude-code/claude-opus-6"));
        assert_eq!(fresh.name, "Opus 6");
        assert!(fresh.new);
        assert_eq!(fresh.description, None);

        let default = catalog.label(&id("claude-code"));
        assert_eq!(default.name, "Default");
        assert!(default.recommended);

        assert_eq!(
            catalog
                .effort_note(&id("claude-code/opus"), "max")
                .as_deref(),
            Some("Only for proofs"),
            "an alias reads its model's notes"
        );
        assert_eq!(
            catalog
                .effort_note(&id("claude-code/claude-sonnet-4-6"), "max")
                .as_deref(),
            Some("Deepest reasoning")
        );
        assert_eq!(
            catalog
                .effort_note(&id("codex/gpt-6.1-sol"), "low")
                .as_deref(),
            builtin_effort_note("low")
        );
        assert_eq!(catalog.effort_note(&id("codex/gpt-6.1-sol"), "turbo"), None);
    }

    #[test]
    fn a_model_is_spelled_one_way_however_a_session_names_it() {
        let catalog = seed();
        for named in [
            "claude-code",
            "claude-code/opus",
            "claude-code/claude-opus-5-5",
        ] {
            assert_eq!(
                catalog.runs(&id(named)).as_deref(),
                Some("claude-opus-5-5"),
                "{named}"
            );
        }
        assert_eq!(
            catalog
                .runs(&id("claude-code/claude-sonnet-4-6"))
                .as_deref(),
            Some("claude-sonnet-4-6")
        );
        assert_eq!(catalog.runs(&id("claude-code/claude-opus-9")), None);
        assert_eq!(catalog.runs(&id("codex")), None);
    }

    #[test]
    fn aliases_resolve_and_new_models_sort_first() {
        let catalog = seed();
        assert!(catalog.is_alias(&id("claude-code/opus")));
        assert!(!catalog.is_alias(&id("claude-code/claude-opus-5-5")));
        assert!(!catalog.is_alias(&id("codex/opus")));
        assert_eq!(catalog.label(&id("claude-code/opus")).name, "Opus 5.5");

        let mut ids = [
            id("claude-code/claude-sonnet-4-6"),
            id("claude-code/claude-opus-5-5"),
            id("claude-code/claude-opus-5-6"),
            id("claude-code/claude-opus-6"),
        ];
        ids.sort_by(|a, b| catalog.newest_first(a, b));
        let order: Vec<String> = ids.iter().map(ToString::to_string).collect();
        assert_eq!(
            order,
            [
                "claude-code/claude-opus-6",
                "claude-code/claude-opus-5-6",
                "claude-code/claude-opus-5-5",
                "claude-code/claude-sonnet-4-6",
            ]
        );
    }

    fn entry(worker: &str) -> ModelEntry {
        serde_json::from_value(json!({
            "id": worker, "provider": "openai", "kind": "cli", "access": "subscription",
            "cost": {"unit": "plan_usage"}
        }))
        .unwrap()
    }
}
