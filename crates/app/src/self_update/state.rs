//! `~/.argonauts/update.json`: this install's rollout id, the highest manifest serial seen on
//! each channel, when it last looked and what that came to, and what it already told you. The
//! rollout id is its own, never sent anywhere and not the usage reports' id.

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
    /// What the last background look came to, which `argonauts doctor` shows. A kind of
    /// outcome a later version adds reads as none, so it never costs the rest of the file.
    #[serde(default, deserialize_with = "look_or_none")]
    pub last: Option<Look>,
}

/// One background look for a new release, and what came of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Look {
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    /// Nothing newer, or a newer release held back from everyone.
    UpToDate,
    /// Newer, and the rollout reaches this install later.
    Later { version: Version },
    /// Newer, and put in place for the next start.
    Ready { version: Version },
    /// Newer, for you or this copy's package manager to install.
    Out { version: Version },
    /// Newer, and putting it in place failed, for `why`.
    Failed { version: Version, why: String },
    /// The manifest couldn't be read, for `why`.
    Unread { why: String },
}

fn look_or_none<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Look>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
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
    fn the_last_look_is_kept_and_an_unknown_one_costs_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update.json");
        let mut state = UpdateState::load(&path);
        state.last = Some(Look {
            at: OffsetDateTime::UNIX_EPOCH,
            outcome: Outcome::Failed {
                version: Version::new(0, 2, 1),
                why: "the download stopped at 61%".to_owned(),
            },
        });
        state.save(&path).unwrap();
        assert_eq!(UpdateState::load(&path), state);

        let id = state.rollout_id().to_owned();
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\"failed\"", "\"kind_from_later\"");
        std::fs::write(&path, text).unwrap();
        let read = UpdateState::load(&path);
        assert_eq!(read.last, None);
        assert_eq!(read.rollout_id(), id, "the rest of the file still reads");
    }

    #[test]
    fn a_file_from_before_the_last_look_was_kept_still_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update.json");
        std::fs::write(
            &path,
            r#"{"rollout_id":"r-1","serials":{"stable":4},"checked":null,"out":"0.1.1"}"#,
        )
        .unwrap();
        let state = UpdateState::load(&path);
        assert_eq!(state.rollout_id(), "r-1");
        assert_eq!(state.out, Some(Version::new(0, 1, 1)));
        assert_eq!(state.last, None);
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
