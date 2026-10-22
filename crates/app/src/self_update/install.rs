//! How this copy of Argonauts Code was installed, from where its program really is. Only a copy
//! the installer or a self-update put in place replaces itself; one a package manager owns is
//! that manager's to update, and a copy made any other way gets the releases' address.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::Layout;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Install {
    /// The shell installer's, or one of the versions a self-update keeps: it updates itself.
    Shell,
    Homebrew,
    Npm,
    Pnpm,
    Bun,
    /// Run through `npx`, which fetches the newest each time.
    Npx,
    Cargo,
    /// Built from source or copied by hand.
    Elsewhere,
}

/// The places that say who installed a program.
#[derive(Debug, Clone, Default)]
pub struct Places {
    /// Where self-updates keep each version.
    pub versions: PathBuf,
    /// The folder the shell installer's receipt says it installed into.
    pub receipt_prefix: Option<PathBuf>,
    /// `$CARGO_HOME/bin`.
    pub cargo_bin: Option<PathBuf>,
}

/// The shell installer's receipt, of which only the folder it installed into matters here.
#[derive(Debug, Deserialize)]
struct Receipt {
    install_prefix: PathBuf,
}

impl Places {
    pub fn from_env(layout: &Layout) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .or_else(|| home.as_ref().map(|home| home.join(".config")));
        let receipt_prefix = config.and_then(|config| {
            let text =
                std::fs::read_to_string(config.join("argonauts-code/argonauts-code-receipt.json"))
                    .ok()?;
            serde_json::from_str::<Receipt>(&text)
                .ok()
                .map(|receipt| receipt.install_prefix)
        });
        let cargo_bin = std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .or_else(|| home.map(|home| home.join(".cargo")))
            .map(|cargo| cargo.join("bin"));
        Self {
            versions: layout.versions.clone(),
            receipt_prefix,
            cargo_bin,
        }
    }
}

impl Install {
    /// Who installed the program at `exe`, by where it really is once links are followed.
    pub fn detect(exe: &Path, places: &Places) -> Self {
        let real = exe.canonicalize().unwrap_or_else(|_| exe.to_path_buf());
        let under = |dir: &Path| {
            let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
            real.starts_with(dir)
        };
        if !places.versions.as_os_str().is_empty() && under(&places.versions) {
            return Self::Shell;
        }
        let text = real.to_string_lossy();
        if text.contains("/Cellar/") {
            return Self::Homebrew;
        }
        if text.contains("/_npx/") {
            return Self::Npx;
        }
        if text.contains("/node_modules/") {
            return if text.contains("/.bun/") {
                Self::Bun
            } else if text.contains("/pnpm/") || text.contains("/.pnpm/") {
                Self::Pnpm
            } else {
                Self::Npm
            };
        }
        if places.cargo_bin.as_deref().is_some_and(under) {
            return Self::Cargo;
        }
        // dist's receipt names the folder; the program is in it or in its `bin`.
        let installed = places.receipt_prefix.as_deref().is_some_and(|prefix| {
            let prefix = prefix
                .canonicalize()
                .unwrap_or_else(|_| prefix.to_path_buf());
            real.parent()
                .is_some_and(|dir| dir == prefix || dir == prefix.join("bin"))
        });
        if installed {
            return Self::Shell;
        }
        Self::Elsewhere
    }

    /// Who updates this copy when it isn't Argonauts Code itself, such as `Homebrew`.
    pub fn manager(self) -> Option<&'static str> {
        match self {
            Self::Homebrew => Some("Homebrew"),
            Self::Npm => Some("npm"),
            Self::Pnpm => Some("pnpm"),
            Self::Bun => Some("Bun"),
            Self::Cargo => Some("cargo"),
            Self::Shell | Self::Npx | Self::Elsewhere => None,
        }
    }

    /// The command that updates a copy a package manager owns.
    pub fn command(self) -> Option<&'static str> {
        match self {
            Self::Homebrew => Some("brew upgrade argonauts-code"),
            Self::Npm => Some("npm install -g argonauts-code@latest"),
            Self::Pnpm => Some("pnpm add -g argonauts-code@latest"),
            Self::Bun => Some("bun add -g argonauts-code@latest"),
            Self::Cargo => Some("cargo install argonauts-code --locked"),
            Self::Shell | Self::Npx | Self::Elsewhere => None,
        }
    }
}

/// Where every release is, for a copy nothing here can update.
pub const RELEASES: &str = concat!(env!("CARGO_PKG_REPOSITORY"), "/releases");

#[cfg(test)]
mod tests {
    use super::*;

    fn places(root: &Path) -> Places {
        Places {
            versions: root.join("share/argonauts/versions"),
            receipt_prefix: None,
            cargo_bin: Some(root.join(".cargo/bin")),
        }
    }

    /// A program file at `path` under `root`, so links to it can be followed.
    fn program(root: &Path, path: &str) -> PathBuf {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "").unwrap();
        path
    }

    #[test]
    fn each_way_of_installing_is_found_by_the_programs_real_place() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let places = places(&root);
        let cases = [
            ("share/argonauts/versions/0.1.0/argonauts", Install::Shell),
            (
                "opt/homebrew/Cellar/argonauts-code/0.1.0/bin/argonauts",
                Install::Homebrew,
            ),
            (
                ".npm/_npx/4f2a/node_modules/argonauts-code/bin/argonauts",
                Install::Npx,
            ),
            (
                "lib/node_modules/argonauts-code/bin/argonauts",
                Install::Npm,
            ),
            (
                "Library/pnpm/global/5/node_modules/argonauts-code/bin/argonauts",
                Install::Pnpm,
            ),
            (
                ".bun/install/global/node_modules/argonauts-code/bin/argonauts",
                Install::Bun,
            ),
            (".cargo/bin/argonauts", Install::Cargo),
            ("src/argonauts/target/release/argonauts", Install::Elsewhere),
        ];
        for (path, install) in cases {
            assert_eq!(
                Install::detect(&program(&root, path), &places),
                install,
                "{path}"
            );
        }
    }

    /// `~/.local/bin/argonauts` as a link into the versions is a self-update's, and the plain
    /// file dist's installer wrote is the installer's once its receipt names the folder.
    #[test]
    fn the_link_and_the_installers_own_file_update_themselves() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut places = places(&root);
        let version = program(&root, "share/argonauts/versions/0.1.1/argonauts");
        std::fs::create_dir_all(root.join(".local/bin")).unwrap();
        let link = root.join(".local/bin/argonauts");
        std::os::unix::fs::symlink(&version, &link).unwrap();
        assert_eq!(Install::detect(&link, &places), Install::Shell);

        let installed = program(&root, "other/bin/argonauts");
        assert_eq!(Install::detect(&installed, &places), Install::Elsewhere);
        places.receipt_prefix = Some(root.join("other"));
        assert_eq!(Install::detect(&installed, &places), Install::Shell);
    }

    #[test]
    fn a_package_manager_names_its_own_command() {
        assert_eq!(
            Install::Homebrew.command(),
            Some("brew upgrade argonauts-code")
        );
        assert_eq!(Install::Homebrew.manager(), Some("Homebrew"));
        assert_eq!(Install::Npx.command(), None);
        assert_eq!(Install::Shell.command(), None);
        assert!(RELEASES.ends_with("/releases"), "{RELEASES}");
    }
}
