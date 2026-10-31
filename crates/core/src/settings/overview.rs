//! The Overview tab (`[overview]`): how its year of days is coloured, and your own list prices
//! for its API values.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::models::pricing::ListPrice;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct OverviewSettings {
    /// Each day of the year in the colour of `cli` (the CLI that ran most that day, the
    /// default), `model` (the model that ran most, in its CLI's colour), or `one`
    /// colour for every day. Darker is always less.
    pub year_colors: YearColors,
    /// Your own list prices by model id, in dollars per million tokens, such as
    /// `[overview.prices."claude-opus-5-5"]` with `input`, `output`, `cache_read` and
    /// `cache_write`. They win over the public model list's and the built-in table's.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub prices: BTreeMap<String, ListPrice>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum YearColors {
    #[default]
    Cli,
    Model,
    One,
}

impl YearColors {
    /// The next one `c` on the tab turns to.
    pub fn next(self) -> Self {
        match self {
            YearColors::Cli => YearColors::Model,
            YearColors::Model => YearColors::One,
            YearColors::One => YearColors::Cli,
        }
    }

    /// As the setting writes it.
    pub fn word(self) -> &'static str {
        match self {
            YearColors::Cli => "cli",
            YearColors::Model => "model",
            YearColors::One => "one",
        }
    }
}
