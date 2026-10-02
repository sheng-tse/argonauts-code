//! The budget keys that are one choice each: one run limit and one card limit, in one unit.

use std::collections::BTreeMap;

use argonauts_core::settings::budget::BudgetSettings;
use toml::{Table, Value};

use super::set_by::SetBy;
use crate::SettingsError;

const UNITS: &[(&str, &[&str])] = &[
    (
        "run",
        &["run_usd", "run_tokens", "run_quota", "run_minutes"],
    ),
    (
        "per_card",
        &[
            "per_card_usd",
            "per_card_tokens",
            "per_card_quota",
            "per_card_minutes",
        ],
    ),
];

/// The unit the latest layer picked for each limit, such as `run_tokens` for `run`, and that
/// layer.
#[derive(Debug, Default)]
pub(crate) struct BudgetUnits {
    chosen: BTreeMap<&'static str, (&'static str, &'static str)>,
}

/// A budget key the unit switch cleared, and the layer whose unit cleared it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cleared {
    pub key: String,
    pub layer: &'static str,
}

impl BudgetUnits {
    /// Records the unit `table` picked for each limit. Two units for one limit in one layer is
    /// an error, since nothing says which one you meant.
    pub fn note(
        &mut self,
        layer: &'static str,
        source: &str,
        table: &Table,
    ) -> Result<(), SettingsError> {
        let Some(Value::Table(budget)) = table.get("budget") else {
            return Ok(());
        };
        for (limit, keys) in UNITS {
            let set: Vec<&'static str> = keys
                .iter()
                .copied()
                .filter(|key| budget.contains_key(*key))
                .collect();
            match set.as_slice() {
                [] => {}
                [unit] => {
                    self.chosen.insert(limit, (unit, layer));
                }
                _ => {
                    return Err(SettingsError::Invalid {
                        layer: source.to_owned(),
                        message: format!(
                            "budget.{} are one choice; set one of them",
                            set.join(" and budget.")
                        ),
                    });
                }
            }
        }
        Ok(())
    }

    /// A layer that sets one `run_*` (or `per_card_*`) key clears the others it inherits, the
    /// built-in `run_usd` and `per_card_usd` included, since TOML can't unset a key. This runs
    /// on the settings, because `#[serde(default)]` would bring back a key removed from the
    /// table. Returns the keys that held a value, so the run's overrides can list them.
    pub fn apply(&self, budget: &mut BudgetSettings, set_by: &mut SetBy) -> Vec<Cleared> {
        let mut cleared = Vec::new();
        for (limit, keys) in UNITS {
            let Some(&(unit, layer)) = self.chosen.get(limit) else {
                continue;
            };
            for key in keys.iter().filter(|key| **key != unit) {
                let key = format!("budget.{key}");
                if clear(budget, &key) {
                    cleared.push(Cleared {
                        key: key.clone(),
                        layer,
                    });
                }
                set_by.remove(&key);
            }
        }
        cleared
    }
}

/// The other units of the limit a dotted key picks, such as `budget.run_usd` for
/// `budget.run_tokens`. A file that picks one unit drops these, as a later layer does.
pub(crate) fn other_units(key: &str) -> Vec<String> {
    let Some(unit) = key.strip_prefix("budget.") else {
        return Vec::new();
    };
    let Some((_, keys)) = UNITS.iter().find(|(_, keys)| keys.contains(&unit)) else {
        return Vec::new();
    };
    keys.iter()
        .filter(|other| **other != unit)
        .map(|other| format!("budget.{other}"))
        .collect()
}

/// Unsets one dotted budget key, and says whether it held a value.
fn clear(budget: &mut BudgetSettings, key: &str) -> bool {
    match key {
        "budget.run_usd" => budget.run_usd.take().is_some(),
        "budget.run_tokens" => budget.run_tokens.take().is_some(),
        "budget.run_quota" => budget.run_quota.take().is_some(),
        "budget.run_minutes" => budget.run_minutes.take().is_some(),
        "budget.per_card_usd" => budget.per_card_usd.take().is_some(),
        "budget.per_card_tokens" => budget.per_card_tokens.take().is_some(),
        "budget.per_card_quota" => budget.per_card_quota.take().is_some(),
        "budget.per_card_minutes" => budget.per_card_minutes.take().is_some(),
        _ => false,
    }
}
