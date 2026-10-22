//! Usage reports (`[telemetry]`): one anonymous event a day about how Argonauts Code is used,
//! on unless you turn it off. Only your own global config may turn it on, so a repository you
//! clone can't, though its settings may turn it off; and it's a safety key, so an agent can't
//! change it either.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct TelemetrySettings {
    /// Send one anonymous event a day with the day's counts; DO_NOT_TRACK=1 always wins
    pub enabled: bool,
}

impl Default for TelemetrySettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_reports_are_on_until_you_turn_them_off() {
        assert!(TelemetrySettings::default().enabled);
        let off: TelemetrySettings = toml::from_str("enabled = false").unwrap();
        assert!(!off.enabled);
        let empty: TelemetrySettings = toml::from_str("").unwrap();
        assert!(empty.enabled);
    }
}
