//! The limit keys that are one choice each: one run limit and one task limit, in one unit.

use std::collections::BTreeMap;

use argonauts_core::settings::limit::SpendLimits;
use toml::{Table, Value};

use super::set_by::SetBy;
use crate::SettingsError;

const UNITS: &[(&str, &[&str])] = &[
    (
        "run",
        &[
            "spend_per_run",
            "run_tokens",
            "run_plan_usage",
            "run_minutes",
        ],
    ),
    (
        "per_task",
        &[
            "per_task_usd",
            "per_task_tokens",
            "per_task_plan_usage",
            "per_task_minutes",
        ],
    ),
];

/// The unit the latest layer picked for each limit, such as `run_tokens` for `run`, and that
/// layer.
#[derive(Debug, Default)]
pub(crate) struct LimitUnits {
    chosen: BTreeMap<&'static str, (&'static str, &'static str)>,
}

/// A limit key the unit switch cleared, and the layer whose unit cleared it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cleared {
    pub key: String,
    pub layer: &'static str,
}

impl LimitUnits {
    /// Records the unit `table` picked for each limit. Two units for one limit in one layer is
    /// an error, since nothing says which one you meant.
    pub fn note(
        &mut self,
        layer: &'static str,
        source: &str,
        table: &Table,
    ) -> Result<(), SettingsError> {
        let Some(Value::Table(spend)) = table.get("limits") else {
            return Ok(());
        };
        for (limit, keys) in UNITS {
            let set: Vec<&'static str> = keys
                .iter()
                .copied()
                .filter(|key| spend.contains_key(*key))
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
                            "limits.{} are one choice; set one of them",
                            set.join(" and limits.")
                        ),
                    });
                }
            }
        }
        Ok(())
    }

    /// A layer that sets one `run_*` (or `per_task_*`) key clears the others it inherits, the
    /// built-in `spend_per_run` and `per_task_usd` included, since TOML can't unset a key. This
    /// runs on the settings, because `#[serde(default)]` would bring back a key removed from the
    /// table. Returns the keys that held a value, so the run's overrides can list them.
    pub fn apply(&self, spend: &mut SpendLimits, set_by: &mut SetBy) -> Vec<Cleared> {
        let mut cleared = Vec::new();
        for (limit, keys) in UNITS {
            let Some(&(unit, layer)) = self.chosen.get(limit) else {
                continue;
            };
            for key in keys.iter().filter(|key| **key != unit) {
                let key = format!("limits.{key}");
                if clear(spend, &key) {
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

/// The other units of the limit a dotted key picks, such as `limits.spend_per_run` for
/// `limits.run_tokens`. A file that picks one unit drops these, as a later layer does.
pub(crate) fn other_units(key: &str) -> Vec<String> {
    let Some(unit) = key.strip_prefix("limits.") else {
        return Vec::new();
    };
    let Some((_, keys)) = UNITS.iter().find(|(_, keys)| keys.contains(&unit)) else {
        return Vec::new();
    };
    keys.iter()
        .filter(|other| **other != unit)
        .map(|other| format!("limits.{other}"))
        .collect()
}

/// Unsets one dotted limit key, and says whether it held a value.
fn clear(spend: &mut SpendLimits, key: &str) -> bool {
    match key {
        "limits.spend_per_run" => spend.spend_per_run.take().is_some(),
        "limits.run_tokens" => spend.run_tokens.take().is_some(),
        "limits.run_plan_usage" => spend.run_plan_usage.take().is_some(),
        "limits.run_minutes" => spend.run_minutes.take().is_some(),
        "limits.per_task_usd" => spend.per_task_usd.take().is_some(),
        "limits.per_task_tokens" => spend.per_task_tokens.take().is_some(),
        "limits.per_task_plan_usage" => spend.per_task_plan_usage.take().is_some(),
        "limits.per_task_minutes" => spend.per_task_minutes.take().is_some(),
        _ => false,
    }
}
