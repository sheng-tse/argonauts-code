//! Updating Argonauts Code itself (`[updates]`). Only your own global config may change these,
//! and they are safety keys, since they decide which build of the program runs next.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct UpdateSettings {
    /// Which releases to follow: `stable`, or `beta` for each one before it's stable
    pub channel: UpdateChannel,
    /// Download a newer release in the background and start it at the next start. Off only
    /// tells you one is out
    pub auto: bool,
    /// Look once a day for a newer release. Never in CI, inside an agent's session, or without
    /// a terminal
    pub check: bool,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            channel: UpdateChannel::Stable,
            auto: true,
            check: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    #[default]
    Stable,
    Beta,
}

impl UpdateChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            UpdateChannel::Stable => "stable",
            UpdateChannel::Beta => "beta",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_updates_itself_and_checks_daily_until_you_say_otherwise() {
        let updates = UpdateSettings::default();
        assert_eq!(updates.channel, UpdateChannel::Stable);
        assert!(updates.auto);
        assert!(updates.check);
        let beta: UpdateSettings = toml::from_str("channel = \"beta\"\nauto = false\n").unwrap();
        assert_eq!(beta.channel.as_str(), "beta");
        assert!(!beta.auto);
        assert!(beta.check, "a key left out keeps its default");
        assert!(toml::from_str::<UpdateSettings>("channel = \"nightly\"").is_err());
    }
}
