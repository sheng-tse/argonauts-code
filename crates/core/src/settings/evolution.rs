//! The evolution loop's settings. Nothing acts on them yet.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct EvolutionSettings {
    /// Settings that may evolve. Fixed-core and safety keys can't be listed.
    pub evolvable: Vec<String>,
    /// Runs between proposals; 0 means only when you ask.
    pub propose_every: u32,
    /// Pass-rate points a candidate must gain, fixed before any replay.
    pub min_gain_points: f64,
    /// Largest cost increase allowed, as a fraction: 0.10 is 10%.
    pub max_cost_increase: f64,
    pub replay: ReplaySettings,
    pub canary_runs: u32,
}

impl Default for EvolutionSettings {
    fn default() -> Self {
        Self {
            evolvable: [
                "critique.rounds",
                "critique.checklist",
                "prompts.drafting",
                "divide.max_cards",
                "budget.per_card_usd",
            ]
            .map(str::to_owned)
            .to_vec(),
            propose_every: 1,
            min_gain_points: 10.0,
            max_cost_increase: 0.10,
            replay: ReplaySettings::default(),
            canary_runs: 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ReplaySettings {
    pub tasks: u32,
    pub runs_per_task: u32,
    /// Replay only from the phase the change touches.
    pub prefix_replay: bool,
}

impl Default for ReplaySettings {
    fn default() -> Self {
        Self {
            tasks: 30,
            runs_per_task: 3,
            prefix_replay: true,
        }
    }
}
