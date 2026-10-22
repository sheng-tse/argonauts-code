//! The update manifest: one signed JSON file per channel naming its newest release, as the
//! release script writes it and you sign it with minisign:
//!
//! ```json
//! {
//!   "serial": 7,
//!   "version": "0.1.1",
//!   "notes": "https://github.com/sheng-tse/argonauts-code/releases/tag/v0.1.1",
//!   "min_version": "0.1.0",
//!   "rollout": [
//!     { "from": "2026-11-02T16:00:00Z", "percent": 10 },
//!     { "from": "2026-11-03T16:00:00Z", "percent": 50 },
//!     { "from": "2026-11-04T16:00:00Z", "percent": 100 }
//!   ],
//!   "halted": false,
//!   "yanked": [],
//!   "platforms": {
//!     "aarch64-apple-darwin": {
//!       "url": "https://github.com/sheng-tse/argonauts-code/releases/download/v0.1.1/argonauts-code-aarch64-apple-darwin.tar.xz",
//!       "sha256": "9f2c…",
//!       "size": 23400000
//!     }
//!   }
//! }
//! ```
//!
//! `serial` only grows, so an older manifest served again is refused. `min_version` is the
//! oldest version still supported: an install older than it, or on a version in `yanked`,
//! updates at once whatever the schedule. `halted` holds the release back from everyone, even
//! a self-update you run yourself.

use std::collections::BTreeMap;
use std::time::Duration;

use argonauts_core::settings::updates::UpdateChannel;
use minisign_verify::{PublicKey, Signature};
use semver::Version;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::rollout;
use super::state::UpdateState;
use super::{Here, Source};

/// A manifest is a few hundred bytes; anything past this isn't one.
const MANIFEST_MOST: usize = 1 << 20;
/// How long fetching a manifest may take.
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// The channel it was signed for. A signed beta served in stable's place is refused, as
    /// whoever serves the files could otherwise move stable installs onto betas.
    pub channel: UpdateChannel,
    pub serial: u64,
    pub version: Version,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub min_version: Option<Version>,
    #[serde(default)]
    pub rollout: Vec<RolloutStep>,
    #[serde(default)]
    pub halted: bool,
    #[serde(default)]
    pub yanked: Vec<Version>,
    pub platforms: BTreeMap<String, Download>,
}

/// From `from` on, `percent` of installs get the release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutStep {
    #[serde(with = "time::serde::rfc3339")]
    pub from: OffsetDateTime,
    pub percent: u8,
}

/// One platform's release archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Download {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

impl Download {
    /// The archive's own name, such as `argonauts-code-aarch64-apple-darwin.tar.xz`.
    pub fn file_name(&self) -> &str {
        self.url
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.url)
    }
}

/// A release this install may take, with its archive for this computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub download: Download,
    /// Updating is due now whatever the schedule: this version is too old or was yanked.
    pub required: bool,
}

/// What a channel's manifest says for this install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    UpToDate,
    /// Newer, and this install's turn has come.
    Newer(Release),
    /// Newer, and the rollout hasn't reached this install yet.
    NotYet(Version),
    /// Newer, and held back from everyone.
    Halted(Version),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("couldn't fetch {url}: {why}")]
    Fetch { url: String, why: String },
    #[error("its signature isn't from a key Argonauts Code trusts")]
    Signature,
    #[error("it can't be read: {0}")]
    Unreadable(String),
    #[error("it is older than one already seen (serial {got}, and {seen} was seen)")]
    Older { got: u64, seen: u64 },
    #[error("it names {got}, not {asked}")]
    OtherVersion { asked: Version, got: Version },
    #[error("it was signed for {}, not {}", got.as_str(), asked.as_str())]
    OtherChannel {
        asked: UpdateChannel,
        got: UpdateChannel,
    },
    #[error("{0} is on hold")]
    Halted(Version),
    #[error("{0} was withdrawn")]
    Yanked(Version),
    #[error("{version} has no build for {platform}")]
    NoBuild { version: Version, platform: String },
}

/// The manifest in `json`, once `signature` is found to come from one of `keys`. A key that
/// isn't a minisign public key checks nothing.
pub fn verify(json: &[u8], signature: &str, keys: &[String]) -> Result<Manifest, ManifestError> {
    let signature = Signature::decode(signature).map_err(|_| ManifestError::Signature)?;
    let signed = keys
        .iter()
        .filter_map(|key| PublicKey::from_base64(key.trim()).ok())
        .any(|key| key.verify(json, &signature, false).is_ok());
    if !signed {
        return Err(ManifestError::Signature);
    }
    serde_json::from_slice(json).map_err(|err| ManifestError::Unreadable(err.to_string()))
}

/// Fetches the manifest at `url` and its signature beside it, and checks it.
async fn fetch_verified(source: &Source, url: &str) -> Result<Manifest, ManifestError> {
    let client = client().map_err(|why| ManifestError::Fetch {
        url: url.to_owned(),
        why,
    })?;
    let json = fetch(&client, url).await?;
    let signature = fetch(&client, &format!("{url}.minisig")).await?;
    let signature = String::from_utf8_lossy(&signature);
    verify(&json, &signature, &source.keys)
}

pub(super) fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .user_agent(concat!("argonauts/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|err| err.to_string())
}

async fn fetch(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, ManifestError> {
    let failed = |why: String| ManifestError::Fetch {
        url: url.to_owned(),
        why,
    };
    let response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|err| failed(without_url(err)))?;
    if response
        .content_length()
        .is_some_and(|length| length > MANIFEST_MOST as u64)
    {
        return Err(failed("it is too large to be a manifest".to_owned()));
    }
    let body = response
        .bytes()
        .await
        .map_err(|err| failed(without_url(err)))?;
    if body.len() > MANIFEST_MOST {
        return Err(failed("it is too large to be a manifest".to_owned()));
    }
    Ok(body.to_vec())
}

/// reqwest's own words, without the address it adds, which the message names already.
fn without_url(err: reqwest::Error) -> String {
    match err.status() {
        Some(status) => format!("the server answered {status}"),
        None => err.without_url().to_string(),
    }
}

/// Reads `channel`'s manifest and says what it means for this install. `manual` is a
/// self-update you asked for, which skips the rollout's schedule but never a halt.
pub async fn check(
    here: &Here,
    channel: UpdateChannel,
    manual: bool,
    now: OffsetDateTime,
) -> Result<Found, ManifestError> {
    let manifest = fetch_verified(&here.source, &here.source.channel_url(channel)).await?;
    // Before its serial is kept, so another channel's never raises this one's.
    if manifest.channel != channel {
        return Err(ManifestError::OtherChannel {
            asked: channel,
            got: manifest.channel,
        });
    }
    let mut state = UpdateState::load(&here.state_file);
    state.saw_serial(channel, manifest.serial)?;
    let bucket = rollout::bucket(state.rollout_id(), &manifest.version);
    if let Err(err) = state.save(&here.state_file) {
        tracing::debug!("couldn't keep what the update check saw: {err}");
    }
    found(&manifest, &here.current, here.platform, bucket, manual, now)
}

/// The release `version`, from its own manifest, for a self-update to a version you name.
pub async fn named(here: &Here, version: &Version) -> Result<Release, ManifestError> {
    let manifest = fetch_verified(&here.source, &here.source.release_url(version)).await?;
    if manifest.version != *version {
        return Err(ManifestError::OtherVersion {
            asked: version.clone(),
            got: manifest.version,
        });
    }
    if manifest.halted {
        return Err(ManifestError::Halted(manifest.version));
    }
    if manifest.yanked.contains(&manifest.version) {
        return Err(ManifestError::Yanked(manifest.version));
    }
    let download = build_for(&manifest, here.platform)?;
    Ok(Release {
        version: manifest.version,
        download,
        required: false,
    })
}

/// What `manifest` means for an install on `current` whose rollout bucket for it is `bucket`.
pub fn found(
    manifest: &Manifest,
    current: &Version,
    platform: Option<&str>,
    bucket: u8,
    manual: bool,
    now: OffsetDateTime,
) -> Result<Found, ManifestError> {
    // No automatic downgrade: a fix goes out as a newer release.
    if manifest.version <= *current {
        return Ok(Found::UpToDate);
    }
    if manifest.halted {
        return Ok(Found::Halted(manifest.version.clone()));
    }
    let required = manifest.yanked.contains(current)
        || manifest
            .min_version
            .as_ref()
            .is_some_and(|oldest| current < oldest);
    if !(manual || required || bucket < rollout::share_now(&manifest.rollout, now)) {
        return Ok(Found::NotYet(manifest.version.clone()));
    }
    Ok(Found::Newer(Release {
        version: manifest.version.clone(),
        download: build_for(manifest, platform)?,
        required,
    }))
}

fn build_for(manifest: &Manifest, platform: Option<&str>) -> Result<Download, ManifestError> {
    let no_build = || ManifestError::NoBuild {
        version: manifest.version.clone(),
        platform: platform.unwrap_or("this computer").to_owned(),
    };
    let platform = platform.ok_or_else(no_build)?;
    manifest
        .platforms
        .get(platform)
        .cloned()
        .ok_or_else(no_build)
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;
    use crate::self_update::test_server::{FakeServer, Signer, manifest};

    const NOW: OffsetDateTime = datetime!(2026-11-03 18:00 UTC);

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn a_manifest_signed_by_a_trusted_key_is_read() {
        let signer = Signer::new();
        let json = serde_json::to_vec(&manifest("0.1.1", 7)).unwrap();
        let read = verify(&json, &signer.sign(&json), &[signer.public()]).unwrap();
        assert_eq!(read.version, v("0.1.1"));
        // The next key alone checks it as well, so a key can be replaced.
        let next = Signer::new();
        assert!(verify(&json, &next.sign(&json), &[signer.public(), next.public()]).is_ok());
    }

    #[test]
    fn a_wrong_or_changed_signature_is_refused() {
        let signer = Signer::new();
        let other = Signer::new();
        let json = serde_json::to_vec(&manifest("0.1.1", 7)).unwrap();
        assert_eq!(
            verify(&json, &other.sign(&json), &[signer.public()]),
            Err(ManifestError::Signature)
        );
        let mut changed = json.clone();
        changed[3] ^= 1;
        assert_eq!(
            verify(&changed, &signer.sign(&json), &[signer.public()]),
            Err(ManifestError::Signature)
        );
        assert_eq!(
            verify(&json, "not a signature", &[signer.public()]),
            Err(ManifestError::Signature)
        );
        // The placeholders built in check nothing.
        let keys: Vec<String> = crate::self_update::PUBLIC_KEYS
            .iter()
            .map(|key| (*key).to_owned())
            .collect();
        assert_eq!(
            verify(&json, &signer.sign(&json), &keys),
            Err(ManifestError::Signature)
        );
    }

    #[test]
    fn only_a_newer_release_is_offered_and_a_halt_holds_it_from_everyone() {
        let mut manifest = manifest("0.1.1", 7);
        let on = |manifest: &Manifest, current: &str, manual: bool| {
            found(
                manifest,
                &v(current),
                Some("aarch64-apple-darwin"),
                99,
                manual,
                NOW,
            )
            .unwrap()
        };
        assert_eq!(on(&manifest, "0.1.1", true), Found::UpToDate);
        assert_eq!(on(&manifest, "0.1.2", true), Found::UpToDate);
        assert!(matches!(on(&manifest, "0.1.0", true), Found::Newer(_)));
        manifest.halted = true;
        assert_eq!(on(&manifest, "0.1.0", true), Found::Halted(v("0.1.1")));
    }

    /// Bucket 60 waits while 50% have it, and comes in once the schedule says 100%. A manual
    /// update, a yanked version and one older than `min_version` skip the wait.
    #[test]
    fn the_schedule_lets_installs_in_by_bucket_unless_the_update_is_due() {
        let mut manifest = manifest("0.1.1", 7);
        manifest.rollout = vec![
            RolloutStep {
                from: datetime!(2026-11-02 16:00 UTC),
                percent: 10,
            },
            RolloutStep {
                from: datetime!(2026-11-03 16:00 UTC),
                percent: 50,
            },
            RolloutStep {
                from: datetime!(2026-11-04 16:00 UTC),
                percent: 100,
            },
        ];
        let at = |manifest: &Manifest, current: &str, bucket: u8, manual: bool, now| {
            found(
                manifest,
                &v(current),
                Some("aarch64-apple-darwin"),
                bucket,
                manual,
                now,
            )
            .unwrap()
        };
        assert!(matches!(
            at(&manifest, "0.1.0", 40, false, NOW),
            Found::Newer(_)
        ));
        assert_eq!(
            at(&manifest, "0.1.0", 60, false, NOW),
            Found::NotYet(v("0.1.1"))
        );
        assert!(matches!(
            at(
                &manifest,
                "0.1.0",
                60,
                false,
                datetime!(2026-11-04 17:00 UTC)
            ),
            Found::Newer(_)
        ));
        assert!(matches!(
            at(&manifest, "0.1.0", 60, true, NOW),
            Found::Newer(_)
        ));
        manifest.yanked = vec![v("0.1.0")];
        let Found::Newer(release) = at(&manifest, "0.1.0", 60, false, NOW) else {
            panic!("a yanked version moves at once");
        };
        assert!(release.required);
        manifest.yanked.clear();
        manifest.min_version = Some(v("0.1.0"));
        assert_eq!(
            at(&manifest, "0.1.0", 60, false, NOW),
            Found::NotYet(v("0.1.1"))
        );
        assert!(matches!(
            at(&manifest, "0.0.9", 60, false, NOW),
            Found::Newer(Release { required: true, .. })
        ));
    }

    fn here(server: &FakeServer, keys: Vec<String>, dir: &std::path::Path) -> Here {
        Here {
            source: server.source(keys),
            layout: crate::self_update::Layout::under(&dir.join("home"), None, None),
            install: crate::self_update::Install::Shell,
            state_file: dir.join("update.json"),
            current: v("0.1.0"),
            platform: Some("aarch64-apple-darwin"),
        }
    }

    /// The manifests as a server serves them: each is fetched with its signature beside it
    /// and checked, and the serial each channel has had is kept.
    #[tokio::test]
    async fn a_channels_manifest_is_fetched_checked_and_kept_from_going_back() {
        let server = FakeServer::start().await;
        let (signer, stranger) = (Signer::new(), Signer::new());
        let dir = tempfile::tempdir().unwrap();
        let here = here(&server, vec![signer.public()], dir.path());
        let stable = UpdateChannel::Stable;

        server.publish("stable", &manifest("0.1.1", 7), &signer);
        let Found::Newer(release) = check(&here, stable, false, NOW).await.unwrap() else {
            panic!("0.1.1 is newer than 0.1.0");
        };
        assert_eq!(release.version, v("0.1.1"));
        assert!(
            server
                .asked()
                .contains(&"/updates/v1/stable.json.minisig".to_owned())
        );

        server.publish("stable", &manifest("0.1.2", 8), &stranger);
        assert_eq!(
            check(&here, stable, false, NOW).await,
            Err(ManifestError::Signature)
        );
        server.publish("stable", &manifest("0.1.0", 6), &signer);
        assert_eq!(
            check(&here, stable, false, NOW).await,
            Err(ManifestError::Older { got: 6, seen: 7 })
        );
        let mut halted = manifest("0.1.2", 9);
        halted.halted = true;
        server.publish("stable", &halted, &signer);
        assert_eq!(
            check(&here, stable, true, NOW).await,
            Ok(Found::Halted(v("0.1.2")))
        );
        // Nobody is in yet, so only a self-update you run gets it.
        let mut waiting = manifest("0.1.2", 10);
        waiting.rollout = vec![RolloutStep {
            from: NOW + time::Duration::days(1),
            percent: 10,
        }];
        server.publish("stable", &waiting, &signer);
        assert_eq!(
            check(&here, stable, false, NOW).await,
            Ok(Found::NotYet(v("0.1.2")))
        );
        assert!(matches!(
            check(&here, stable, true, NOW).await,
            Ok(Found::Newer(_))
        ));
        // Beta's manifest served in stable's place is refused, and its serial isn't kept.
        let beta = Manifest {
            channel: UpdateChannel::Beta,
            ..manifest("0.2.0-beta.1", 50)
        };
        server.publish("stable", &beta, &signer);
        assert_eq!(
            check(&here, stable, false, NOW).await,
            Err(ManifestError::OtherChannel {
                asked: UpdateChannel::Stable,
                got: UpdateChannel::Beta,
            })
        );
        server.publish("stable", &waiting, &signer);
        assert!(matches!(
            check(&here, stable, true, NOW).await,
            Ok(Found::Newer(_))
        ));
        // Beta keeps a serial of its own, and a missing manifest says where it looked.
        assert!(matches!(
            check(&here, UpdateChannel::Beta, false, NOW).await,
            Err(ManifestError::Fetch { url, .. }) if url.ends_with("/updates/v1/beta.json")
        ));
    }

    #[tokio::test]
    async fn a_version_asked_for_by_name_comes_from_its_own_manifest() {
        let server = FakeServer::start().await;
        let signer = Signer::new();
        let dir = tempfile::tempdir().unwrap();
        let here = here(&server, vec![signer.public()], dir.path());
        server.publish("releases/0.1.2", &manifest("0.1.2", 3), &signer);
        assert_eq!(named(&here, &v("0.1.2")).await.unwrap().version, v("0.1.2"));
        server.publish("releases/0.1.3", &manifest("0.1.2", 3), &signer);
        assert!(matches!(
            named(&here, &v("0.1.3")).await,
            Err(ManifestError::OtherVersion { .. })
        ));
        let mut yanked = manifest("0.1.4", 4);
        yanked.yanked = vec![v("0.1.4")];
        server.publish("releases/0.1.4", &yanked, &signer);
        assert_eq!(
            named(&here, &v("0.1.4")).await,
            Err(ManifestError::Yanked(v("0.1.4")))
        );
    }

    #[test]
    fn a_release_without_a_build_for_this_computer_says_so() {
        let manifest = manifest("0.1.1", 7);
        assert_eq!(
            found(
                &manifest,
                &v("0.1.0"),
                Some("riscv64gc-unknown-linux-gnu"),
                0,
                true,
                NOW
            ),
            Err(ManifestError::NoBuild {
                version: v("0.1.1"),
                platform: "riscv64gc-unknown-linux-gnu".to_owned()
            })
        );
    }

    #[test]
    fn the_archive_is_named_by_its_address() {
        let download = Download {
            url: "https://example.com/v0.1.1/argonauts-aarch64-apple-darwin.tar.xz".to_owned(),
            sha256: String::new(),
            size: 0,
        };
        assert_eq!(
            download.file_name(),
            "argonauts-aarch64-apple-darwin.tar.xz"
        );
    }
}
