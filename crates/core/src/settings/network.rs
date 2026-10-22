//! Losing the internet (`[network]`). Sessions that need a provider wait while it's gone and
//! carry on when it's back; local models keep working. A run that waits too long pauses.

use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct NetworkSettings {
    /// Minutes a run holds its sessions for the internet to come back before it pauses
    pub hold_minutes: u32,
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self { hold_minutes: 30 }
    }
}

impl NetworkSettings {
    /// How long a run holds before it pauses.
    pub fn hold(&self) -> Duration {
        Duration::from_secs(u64::from(self.hold_minutes) * 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_holds_half_an_hour_for_the_internet() {
        let network = NetworkSettings::default();
        assert_eq!(network.hold_minutes, 30);
        assert_eq!(network.hold(), Duration::from_secs(30 * 60));
        let short: NetworkSettings = toml::from_str("hold_minutes = 5").unwrap();
        assert_eq!(short.hold(), Duration::from_secs(300));
    }
}
