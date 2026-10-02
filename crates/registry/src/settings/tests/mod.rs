//! `resolve` end to end: a home folder and a repo on disk, as the app lays them out.

mod checks;
mod presets;
mod roles;

use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use crate::{ResolvedSettings, SettingsError, SettingsSources};

/// `home/config.toml` and `repo/.argonauts/`, each with a `presets/` folder. Files are
/// written on demand, and a missing one is fine.
struct Repo {
    dir: TempDir,
}

impl Repo {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = Self { dir };
        fs::create_dir_all(repo.home().join("presets")).unwrap();
        fs::create_dir_all(repo.dot().join("presets")).unwrap();
        repo
    }

    fn outside(&self) -> PathBuf {
        self.dir.path().to_owned()
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    fn dot(&self) -> PathBuf {
        self.root().join(".argonauts")
    }

    fn global(&self, text: &str) {
        fs::write(self.home().join("config.toml"), text).unwrap();
    }

    fn project(&self, text: &str) {
        fs::write(self.dot().join("config.toml"), text).unwrap();
    }

    fn local(&self, text: &str) {
        fs::write(self.dot().join("config.local.toml"), text).unwrap();
    }

    fn global_preset(&self, name: &str, text: &str) -> PathBuf {
        let path = self.home().join(format!("presets/{name}.toml"));
        fs::write(&path, text).unwrap();
        path
    }

    fn project_preset(&self, name: &str, text: &str) -> PathBuf {
        let path = self.dot().join(format!("presets/{name}.toml"));
        fs::write(&path, text).unwrap();
        path
    }

    /// Neither project file trusted.
    fn sources(&self) -> SettingsSources {
        SettingsSources {
            global: Some(self.home().join("config.toml")),
            project: Some(self.dot().join("config.toml")),
            project_local: Some(self.dot().join("config.local.toml")),
            ..SettingsSources::default()
        }
    }

    fn trusted(&self) -> SettingsSources {
        SettingsSources {
            project_trusted: true,
            project_local_trusted: true,
            ..self.sources()
        }
    }

    fn with_flags(&self, flags: &[&str]) -> SettingsSources {
        SettingsSources {
            overrides: flags.iter().map(|flag| (*flag).to_owned()).collect(),
            ..self.trusted()
        }
    }

    fn project_file(&self) -> String {
        self.dot().join("config.toml").display().to_string()
    }

    fn global_file(&self) -> String {
        self.home().join("config.toml").display().to_string()
    }
}

/// The layer and message of an `Invalid` error.
fn invalid(result: Result<ResolvedSettings, SettingsError>) -> (String, String) {
    match result {
        Err(SettingsError::Invalid { layer, message }) => (layer, message),
        Err(other) => panic!("expected an invalid config, got {other}"),
        Ok(resolved) => panic!("expected an error, resolved with {:?}", resolved.warnings),
    }
}

fn layer_of(resolved: &ResolvedSettings, key: &str) -> Option<String> {
    resolved
        .entries
        .iter()
        .find(|entry| entry.key == key)
        .map(|entry| entry.layer.clone())
}
