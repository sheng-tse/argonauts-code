//! `~/.argonauts/update.json`: this install's rollout id, the highest manifest serial seen on
//! each channel, when it last looked, and what it already told you. The rollout id is its own,
//! never sent anywhere and not the usage reports' id.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use argonauts_core::settings::updates::UpdateChannel;
use semver::Version;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::manifest::ManifestError;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateState {
    #[serde(default)]
    rollout_id: String,
    /// The highest serial each channel's manifest has had.
    #[serde(default)]
    serials: BTreeMap<String, u64>,
    /// When the daily check last looked.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub checked: Option<OffsetDateTime>,
    /// The newest release the last check found and this install can't take itself, which the
    /// next start names once.
    #[serde(default)]
    pub out: Option<Version>,
    /// The release the line at start last named, so each is named once.
    #[serde(default)]
    pub told: Option<Version>,
    /// A release switched to in the background, which takes over at the next start.
    #[serde(default)]
    pub ready: Option<Version>,
}

impl UpdateState {
    /// The state at `path`, or a new one with a fresh rollout id.
    pub fn load(path: &Path) -> Self {
        let mut state: Self = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        if state.rollout_id.trim().is_empty() {
            state.rollout_id = uuid::Uuid::new_v4().to_string();
        }
        state
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            argonauts_core::paths::create_private_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        let partial = path.with_extension(format!("json.{}", std::process::id()));
        argonauts_core::paths::write_private_file(&partial, text.as_bytes())?;
        std::fs::rename(&partial, path)
    }

    pub fn rollout_id(&self) -> &str {
        &self.rollout_id
    }

    /// Takes in a manifest's serial, refusing one lower than the channel has had: an older
    /// manifest served again could hold back a fix.
    pub fn saw_serial(&mut self, channel: UpdateChannel, serial: u64) -> Result<(), ManifestError> {
        let seen = self.serials.entry(channel.as_str().to_owned()).or_default();
        if serial < *seen {
            return Err(ManifestError::Older {
                got: serial,
                seen: *seen,
            });
        }
        *seen = serial;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rollout_id_is_made_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update.json");
        let state = UpdateState::load(&path);
        assert!(!state.rollout_id().is_empty());
        state.save(&path).unwrap();
        assert_eq!(UpdateState::load(&path).rollout_id(), state.rollout_id());
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_lower_serial_than_one_seen_is_refused_on_its_channel() {
        let mut state = UpdateState::default();
        state.saw_serial(UpdateChannel::Stable, 7).unwrap();
        state.saw_serial(UpdateChannel::Stable, 7).unwrap();
        assert_eq!(
            state.saw_serial(UpdateChannel::Stable, 6),
            Err(ManifestError::Older { got: 6, seen: 7 })
        );
        // Each channel counts its own.
        state.saw_serial(UpdateChannel::Beta, 2).unwrap();
    }
}
