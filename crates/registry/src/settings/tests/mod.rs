//! `resolve` end to end: a home folder and a repo on disk, as the app lays them out.

mod checks;
mod passing_over;
mod roles;
mod ui;
mod workflow_files;

use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use argonauts_core::settings::Settings;

use crate::{ResolvedSettings, SettingsError, SettingsSources};

/// `home/config.toml` and `repo/.argonauts/`, each with a `workflows/` folder. Files are
/// written on demand, and a missing one is fine.
struct Repo {
    dir: TempDir,
}

impl Repo {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = Self { dir };
        fs::create_dir_all(repo.home().join("workflows")).unwrap();
        fs::create_dir_all(repo.dot().join("workflows")).unwrap();
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

    fn global_workflow(&self, name: &str, text: &str) -> PathBuf {
        let path = self.home().join(format!("workflows/{name}.toml"));
        fs::write(&path, text).unwrap();
        path
    }

    fn project_workflow(&self, name: &str, text: &str) -> PathBuf {
        let path = self.dot().join(format!("workflows/{name}.toml"));
        fs::write(&path, text).unwrap();
        path
    }

    /// Neither project file trusted. A key that can't be used is an error, so each test sees
    /// the message.
    fn sources(&self) -> SettingsSources {
        SettingsSources {
            global: Some(self.home().join("config.toml")),
            project: Some(self.dot().join("config.toml")),
            project_local: Some(self.dot().join("config.local.toml")),
            strict: true,
            ..SettingsSources::default()
        }
    }

    /// As the app reads the files when it starts.
    fn forgiving(&self) -> SettingsSources {
        SettingsSources {
            strict: false,
            ..self.trusted()
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

    /// How messages name the project file: by its path from the repo.
    fn project_file(&self) -> String {
        ".argonauts/config.toml".to_owned()
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
