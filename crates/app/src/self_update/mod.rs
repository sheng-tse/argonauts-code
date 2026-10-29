//! Argonauts Code updating itself. [`install`] says how this copy came to be here, and so who
//! may replace it. [`manifest`] reads the signed file that names the newest release on each
//! channel, and [`rollout`] says whether this install's turn for it has come. [`apply`]
//! downloads a release, checks it, and switches the link to it, so it takes over at the next
//! start while every open session keeps the version it started with.
//!
//! Nothing here looks for a release with `ARGONAUTS_DISABLE_UPDATES=1`, in CI, without a
//! terminal, inside an agent's session or in `argonauts mcp serve`, and nothing switches
//! versions while a run is going.

mod apply;
mod changelog;
pub(crate) mod daily;
mod install;
mod manifest;
mod own_row;
mod rollout;
mod shell;
mod state;
#[cfg(test)]
pub(crate) mod test_server;

use std::path::{Path, PathBuf};

use argonauts_core::Paths;
use argonauts_core::settings::updates::UpdateChannel;
use semver::Version;

pub use apply::{Layout, program_to_save};
pub use daily::{doctor_json, doctor_row};

/// Marks the version this process runs as in use while it runs, so an update in another
/// process never removes it from under this one.
pub fn hold_this_version() {
    if let Ok(paths) = Paths::from_env() {
        apply::hold_this_version(&Layout::for_home(&paths));
    }
}
pub(crate) use changelog::release_notes;
pub use install::{Install, RELEASES};
pub use manifest::{Download, Manifest, RolloutStep};
pub(crate) use own_row::{OwnRow, how_it_updates, own_row};
pub use shell::{Asked, behind_line, self_update};

/// Where the manifests are: `stable.json` and `beta.json`, each signed beside it as
/// `stable.json.minisig`, and `releases/<version>.json` for a version asked for by name.
pub const MANIFESTS: &str = "https://argonauts-code.github.io/updates/v1";

/// The minisign public keys a manifest's signature must come from: the one that signs now, and
/// the next, so a key can be replaced without a release that can't be checked.
pub const PUBLIC_KEYS: [&str; 2] = [
    "RWSAvBYXxtifHE0og0nVcgMAgKO6IAMpNtFcJ+dfF2bp9u5k8EsI8I0w",
    "RWRHcTUGuD6pZ1P0c5aQceGUr+rVPt8JL+9h1HUuA37KIPbsEqYXETbx",
];

/// Turns every update off, the check at start and `argonauts self-update` alike.
pub const DISABLE_ENV: &str = "ARGONAUTS_DISABLE_UPDATES";
/// Another address for the manifests, such as a server on this computer for a test. Its
/// manifests must still be signed by a key built in.
pub const BASE_ENV: &str = "ARGONAUTS_UPDATE_BASE";
/// One more public key to trust, read only by a debug build, for trying an update against a
/// test key.
pub const TEST_KEY_ENV: &str = "ARGONAUTS_UPDATE_TEST_KEY";

/// Where manifests come from and the keys that must have signed them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    base: String,
    keys: Vec<String>,
}

/// Why nothing is checked here.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Off {
    #[error("updates are off: {DISABLE_ENV} is set")]
    Disabled,
    #[error("{BASE_ENV} must be an https address, or http on this computer")]
    Insecure,
    /// Under test with no fake server set up: nothing may reach the real manifests.
    #[cfg(test)]
    #[error("updates are off in tests")]
    Testing,
}

impl Source {
    pub fn new(base: impl Into<String>, keys: Vec<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_owned(),
            keys,
        }
    }

    /// The manifests this process reads, from the environment.
    pub fn from_env() -> Result<Self, Off> {
        let var = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        if var(DISABLE_ENV).is_some_and(|value| value != "0") {
            return Err(Off::Disabled);
        }
        let base = var(BASE_ENV).unwrap_or_else(|| MANIFESTS.to_owned());
        if !safe_address(&base) {
            return Err(Off::Insecure);
        }
        let mut keys: Vec<String> = PUBLIC_KEYS.iter().map(|key| (*key).to_owned()).collect();
        if cfg!(debug_assertions) {
            keys.extend(var(TEST_KEY_ENV));
        }
        Ok(Self::new(base, keys))
    }

    /// The manifests the app reads for the Argonauts home at `paths`. A test never reaches
    /// the real ones: it reads the fake server it set up for its own home, or none.
    pub(crate) fn for_home(paths: &Paths) -> Result<Self, Off> {
        #[cfg(test)]
        return testing::source(paths.home()).ok_or(Off::Testing);
        #[cfg(not(test))]
        {
            let _ = paths;
            Self::from_env()
        }
    }

    fn channel_url(&self, channel: UpdateChannel) -> String {
        format!("{}/{}.json", self.base, channel.as_str())
    }

    fn release_url(&self, version: &Version) -> String {
        format!("{}/releases/{version}.json", self.base)
    }
}

/// An https address, or plain http only to this computer.
fn safe_address(base: &str) -> bool {
    if base.starts_with("https://") {
        return true;
    }
    let Some(rest) = base.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(['/', ':']).next().unwrap_or_default();
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

/// Everything an update needs to know about this copy of Argonauts Code.
#[derive(Debug, Clone)]
pub struct Here {
    pub source: Source,
    pub layout: Layout,
    pub install: Install,
    /// `~/.argonauts/update.json`.
    pub state_file: PathBuf,
    /// The version running now.
    pub current: Version,
    /// The release build for this computer, such as `aarch64-apple-darwin`.
    pub platform: Option<&'static str>,
}

impl Here {
    /// This process, as it was installed, reading `source`'s manifests.
    pub fn this_process(paths: &Paths, source: Source) -> Self {
        let layout = Layout::for_home(paths);
        let exe = std::env::current_exe().unwrap_or_default();
        let places = install::Places::from_env(&layout);
        let install = Install::detect(&exe, &places);
        let layout = layout.linked_where_installed(places.receipt_prefix.as_deref());
        Self {
            source,
            layout,
            install,
            state_file: state_file(paths),
            current: running_version(),
            platform: platform(),
        }
    }
}

/// The version of this build.
pub fn running_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Version::new(0, 0, 0))
}

/// `~/.argonauts/update.json`, where this install keeps its rollout id and what it has seen.
pub(crate) fn state_file(paths: &Paths) -> PathBuf {
    paths.home().join("update.json")
}

/// The release build that runs on this computer, as dist names its targets.
pub fn platform() -> Option<&'static str> {
    let musl = cfg!(target_env = "musl");
    match (std::env::consts::ARCH, std::env::consts::OS, musl) {
        ("aarch64", "macos", _) => Some("aarch64-apple-darwin"),
        ("x86_64", "macos", _) => Some("x86_64-apple-darwin"),
        ("x86_64", "linux", false) => Some("x86_64-unknown-linux-gnu"),
        ("x86_64", "linux", true) => Some("x86_64-unknown-linux-musl"),
        ("aarch64", "linux", false) => Some("aarch64-unknown-linux-gnu"),
        ("aarch64", "linux", true) => Some("aarch64-unknown-linux-musl"),
        _ => None,
    }
}

/// `path` with your home folder as `~`, as the screen names places.
fn shown(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// The fake manifest servers tests set up, one per Argonauts home, so no test reaches the
/// real manifests and tests running side by side never read each other's.
#[cfg(test)]
pub(crate) mod testing {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, PoisonError};

    use super::Source;

    static SOURCES: Mutex<BTreeMap<PathBuf, Source>> = Mutex::new(BTreeMap::new());

    /// From now on the app with its home at `home` reads its manifests from `source`.
    pub(crate) fn serve(home: &Path, source: Source) {
        SOURCES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(home.to_path_buf(), source);
    }

    pub(super) fn source(home: &Path) -> Option<Source> {
        SOURCES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(home)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifests_come_over_https_or_from_this_computer() {
        assert!(safe_address(MANIFESTS));
        assert!(safe_address("http://127.0.0.1:8123/updates/v1"));
        assert!(safe_address("http://localhost/updates"));
        assert!(!safe_address("http://example.com/updates/v1"));
        assert!(!safe_address("http://127.0.0.1.example.com/updates"));
        assert!(!safe_address("ftp://127.0.0.1/updates"));
    }

    /// A placeholder checks nothing, and the release checks refuse to ship one; any other key
    /// is a minisign public key.
    #[test]
    fn the_built_in_keys_are_minisign_keys_or_placeholders_that_check_nothing() {
        for key in PUBLIC_KEYS {
            let decoded = minisign_verify::PublicKey::from_base64(key);
            if key.starts_with("REPLACE-WITH-") {
                assert!(decoded.is_err(), "{key}");
            } else {
                assert!(decoded.is_ok(), "{key} isn't a minisign public key");
            }
        }
    }

    #[test]
    fn a_test_reads_only_the_server_it_set_up_for_its_own_home() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        assert_eq!(Source::for_home(&paths), Err(Off::Testing));
        let source = Source::new("http://127.0.0.1:1/updates/v1/", Vec::new());
        testing::serve(home.path(), source.clone());
        assert_eq!(Source::for_home(&paths), Ok(source));
        assert_eq!(
            Source::new("http://127.0.0.1:1/updates/v1/", Vec::new())
                .channel_url(UpdateChannel::Beta),
            "http://127.0.0.1:1/updates/v1/beta.json"
        );
    }
}
