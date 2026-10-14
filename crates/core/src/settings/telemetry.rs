//! Telemetry (`[telemetry]`): one anonymous event a day about how Argonauts Code is used, off until
//! you say yes. Only your own global config may turn it on, so a repository you clone can't,
//! and it's a safety key, so an agent can't either.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct TelemetrySettings {
    /// Send one anonymous event a day with the day's counts; DO_NOT_TRACK=1 always wins
    pub enabled: bool,
}
