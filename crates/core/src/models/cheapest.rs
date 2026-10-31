//! The cheapest model a CLI offers: of the models its own list names, the one the catalog prices
//! lowest. A CLI's list changes as models ship and retire, so the pick follows it instead of a
//! name built into the adapter, which only stands in while no list is known.

use std::cmp::Ordering;

use crate::base::names::ModelRef;
use crate::input::EFFORT_LEVELS;
use crate::models::model_catalog::{CatalogPrice, ModelCatalog};
use crate::models::pricing::names_model;
use crate::models::registry::{ModelRegistry, known_efforts};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cheapest {
    /// As the CLI's list names it, such as `haiku` or `gpt-6-luna`.
    pub model: String,
    /// Its lowest level, or `None` for a model that takes none or whose levels nobody lists.
    pub effort: Option<String>,
}

/// The cheapest model `cli` lists, as the registry knows its list. `None` when the list names
/// no model the catalog prices, as before discovery first runs, and then the adapter's own
/// choice stands.
pub fn cheapest_listed(registry: &dyn ModelRegistry, cli: &str) -> Option<Cheapest> {
    let catalog = registry.catalog();
    let mut listed: Vec<ModelRef> = registry
        .list()
        .into_iter()
        .filter(|entry| entry.id.agent == cli && !entry.retired)
        .map(|entry| entry.id)
        .collect();
    // The catalog keeps the lists a CLI shows only in its own picker, as Claude Code's.
    listed.extend(
        catalog
            .models
            .iter()
            .filter(|entry| entry.id.agent == cli)
            .map(|entry| entry.id.clone()),
    );
    cheapest_of(&catalog, &listed, |id| {
        known_efforts(registry, id).and_then(|levels| levels.into_iter().next())
    })
}

/// Of `listed`, the model with the lowest price for a million tokens in and a million out. At
/// one price the lowest level wins, then the newest model, then the alias the CLI spells it by,
/// then the list's order. Models the catalog doesn't price are left out, since nothing says
/// what they cost.
pub fn cheapest_of(
    catalog: &ModelCatalog,
    listed: &[ModelRef],
    lowest_effort: impl Fn(&ModelRef) -> Option<String>,
) -> Option<Cheapest> {
    struct Candidate<'a> {
        id: &'a ModelRef,
        runs: ModelRef,
        price: f64,
        effort: Option<String>,
    }
    let level = |effort: &Option<String>| {
        effort
            .as_deref()
            .and_then(|effort| EFFORT_LEVELS.iter().position(|known| *known == effort))
            .unwrap_or(0)
    };
    let mut candidates: Vec<Candidate<'_>> = Vec::new();
    for id in listed {
        let Some(model) = id.model.as_deref() else {
            continue;
        };
        if candidates.iter().any(|known| known.id == id) {
            continue;
        }
        let runs = ModelRef::new(
            id.agent.clone(),
            Some(catalog.runs(id).unwrap_or_else(|| model.to_owned())),
        );
        let Some(price) = runs
            .model
            .as_deref()
            .and_then(|model| price_of(catalog, model))
        else {
            continue;
        };
        candidates.push(Candidate {
            id,
            price: price.input.0 + price.output.0,
            effort: lowest_effort(id),
            runs,
        });
    }
    candidates
        .iter()
        .min_by(|a, b| {
            a.price
                .total_cmp(&b.price)
                .then_with(|| level(&a.effort).cmp(&level(&b.effort)))
                .then_with(|| {
                    // An alias and the model it names are one model.
                    if a.runs == b.runs {
                        Ordering::Equal
                    } else {
                        catalog.newest_first(&a.runs, &b.runs)
                    }
                })
                .then_with(|| catalog.is_alias(b.id).cmp(&catalog.is_alias(a.id)))
        })
        .map(|best| Cheapest {
            model: best.id.model.clone().unwrap_or_default(),
            effort: best.effort.clone(),
        })
}

/// The catalog's price for `model`, which a dated snapshot or a level such as
/// `gemini-3.8-flash-low` shares with the model it belongs to.
pub fn price_of<'a>(catalog: &'a ModelCatalog, model: &str) -> Option<&'a CatalogPrice> {
    let model = model.trim().to_ascii_lowercase();
    let id = model.rsplit('/').next().unwrap_or(&model);
    let id = id.split('[').next().unwrap_or(id).trim();
    catalog
        .prices
        .iter()
        .find(|price| names_model(&price.model.to_ascii_lowercase(), id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::model_catalog::{CatalogEntry, Dollars};

    fn price(model: &str, input: f64, output: f64) -> CatalogPrice {
        CatalogPrice {
            model: model.to_owned(),
            input: Dollars(input),
            output: Dollars(output),
            cache_read: None,
            cache_write: None,
        }
    }

    fn ids(cli: &str, models: &[&str]) -> Vec<ModelRef> {
        models
            .iter()
            .map(|model| ModelRef::new(cli, Some((*model).to_owned())))
            .collect()
    }

    /// The level a model id ends with, as agy names its thinking levels.
    fn named_level(id: &ModelRef) -> Option<String> {
        let level = id.model.as_deref()?.rsplit('-').next()?;
        EFFORT_LEVELS.contains(&level).then(|| level.to_owned())
    }

    #[test]
    fn the_lowest_price_wins_and_unpriced_models_are_left_out() {
        let catalog = ModelCatalog {
            prices: vec![price("gpt-6-sol", 2.0, 10.0), price("gpt-6-luna", 0.1, 0.5)],
            ..ModelCatalog::default()
        };
        let listed = ids("codex", &["gpt-6-sol", "gpt-7-nano", "gpt-6-luna"]);
        let cheapest = cheapest_of(&catalog, &listed, |_| Some("low".to_owned())).unwrap();
        assert_eq!(cheapest.model, "gpt-6-luna");
        assert_eq!(cheapest.effort.as_deref(), Some("low"));
        assert_eq!(
            cheapest_of(&catalog, &ids("codex", &["gpt-7-nano"]), |_| None),
            None
        );
    }

    #[test]
    fn at_one_price_the_lowest_level_then_the_newest_model_wins() {
        let catalog = ModelCatalog {
            prices: vec![
                price("gemini-3.6-flash", 0.75, 3.75),
                price("gemini-3.8-flash", 0.75, 3.75),
            ],
            ..ModelCatalog::default()
        };
        let listed = ids(
            "antigravity-cli",
            &[
                "gemini-3.6-flash-low",
                "gemini-3.8-flash-high",
                "gemini-3.8-flash-low",
                "gemini-3.8-flash-medium",
            ],
        );
        let cheapest = cheapest_of(&catalog, &listed, named_level).unwrap();
        assert_eq!(cheapest.model, "gemini-3.8-flash-low");
        assert_eq!(cheapest.effort.as_deref(), Some("low"));
    }

    #[test]
    fn an_alias_is_priced_as_its_model_and_spelled_as_the_cli_spells_it() {
        let mut haiku = CatalogEntry::new(ModelRef::new(
            "claude-code",
            Some("claude-haiku-5-5".into()),
        ));
        haiku.aliases = Some(vec!["haiku".to_owned()]);
        let catalog = ModelCatalog {
            models: vec![haiku],
            prices: vec![
                price("claude-haiku-5-5", 0.1, 0.5),
                price("claude-sonnet-5-5", 2.0, 10.0),
            ],
            ..ModelCatalog::default()
        };
        let listed = ids("claude-code", &["sonnet", "claude-haiku-5-5", "haiku"]);
        let cheapest = cheapest_of(&catalog, &listed, |_| None).unwrap();
        assert_eq!(cheapest.model, "haiku");
    }

    #[test]
    fn a_snapshot_or_a_level_shares_its_models_price() {
        let catalog = ModelCatalog {
            prices: vec![
                price("claude-haiku-4-5", 1.0, 5.0),
                price("gemini-3.8-flash", 0.75, 3.75),
            ],
            ..ModelCatalog::default()
        };
        assert!(price_of(&catalog, "claude-haiku-4-5-20251001").is_some());
        assert!(price_of(&catalog, "Gemini-3.8-Flash-Low").is_some());
        assert!(price_of(&catalog, "anthropic/claude-haiku-4-5[1m]").is_some());
        assert!(price_of(&catalog, "gemini-3.8-flash-lite").is_none());
    }
}
