//! The setup's last step: every answer written at once, the preset files, this folder, and the
//! closing line.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use argonauts_core::NoteLevel;
use argonauts_core::layout::project_files;
use argonauts_core::paths::{create_private_dir_all, write_private_file};
use argonauts_registry::{write_preset, write_setting};

use super::probe::FoundAgent;
use super::steps::{COORDINATED_CREW, PEER_CREW};
use crate::env::AppEnv;

/// The presets first run writes where you can read and edit them.
pub(crate) const PRESET_FILES: &[&str] = &[PEER_CREW, COORDINATED_CREW];

pub(crate) const CLOSING: &str = "Change any of this later with /model, /effort, /preset, \
                                  /agents, /approvals, /permissions, or /config.";

/// Writes the keys into a draft beside the global file and moves it into place, so the file
/// holds every answer or none, and a failed setup runs again next start.
pub(crate) fn write_config(target: &Path, keys: &[(String, String)]) -> anyhow::Result<()> {
    let dir = target
        .parent()
        .context("the global settings file has no folder")?;
    create_private_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let draft = dir.join("config.toml.setup");
    match std::fs::read(target) {
        Ok(text) => write_private_file(&draft, &text)
            .with_context(|| format!("copying {}", target.display()))?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let _ = std::fs::remove_file(&draft);
        }
        Err(err) => return Err(err).with_context(|| format!("reading {}", target.display())),
    }
    let written = keys
        .iter()
        .try_for_each(|(key, value)| write_setting(&draft, key, value))
        .map_err(anyhow::Error::from)
        .and_then(|()| {
            std::fs::rename(&draft, target).with_context(|| format!("saving {}", target.display()))
        });
    if written.is_err() {
        let _ = std::fs::remove_file(&draft);
    }
    written
}

/// Writes each preset file that's missing. A file you have is never touched. Failures come back
/// as notices, since the setup goes on without them.
pub(crate) fn write_presets(dir: &Path) -> Vec<(NoteLevel, String)> {
    PRESET_FILES
        .iter()
        .filter(|name| !dir.join(format!("{name}.toml")).exists())
        .filter_map(|name| {
            write_preset(name, dir).err().map(|err| {
                (
                    NoteLevel::Warn,
                    format!("Couldn't write the {name} preset file: {err}"),
                )
            })
        })
        .collect()
}

/// `<repo>/.argonauts/`.
pub(crate) fn project_dir(env: &AppEnv) -> PathBuf {
    env.project_config()
        .parent()
        .map_or_else(|| env.repo_root.join(".argonauts"), Path::to_owned)
}

/// The repo's settings files that exist and aren't trusted yet.
pub(crate) fn untrusted_files(env: &AppEnv, trusted: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let dir = project_dir(env);
    [project_files::CONFIG, project_files::CONFIG_LOCAL]
        .into_iter()
        .map(|name| dir.join(name))
        .filter(|file| file.exists() && !trusted(file))
        .collect()
}

/// What to install when no agent CLI is ready, one line.
pub(crate) fn nothing_ready_line(agents: &[FoundAgent]) -> String {
    let installs: Vec<String> = agents
        .iter()
        .filter_map(|agent| {
            let install = agent.hints.install.as_deref()?;
            Some(format!("{} ({install})", agent.name))
        })
        .collect();
    if installs.is_empty() {
        let names: Vec<&str> = agents.iter().map(|agent| agent.name.as_str()).collect();
        format!(
            "No agent CLI is ready yet. Install one of {}, then run /setup.",
            names.join(", ")
        )
    } else {
        format!(
            "No agent CLI is ready yet. Install one: {}. Then run /setup.",
            installs.join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_is_written_whole_with_each_key_commented() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("home").join("config.toml");
        let keys = vec![
            ("preset".to_owned(), "\"peer-crew\"".to_owned()),
            (
                "approvals.goal.mode".to_owned(),
                "\"supervised\"".to_owned(),
            ),
        ];
        write_config(&target, &keys).unwrap();
        let text = std::fs::read_to_string(&target).unwrap();
        assert!(text.contains("preset = \"peer-crew\""), "{text}");
        assert!(!dir.path().join("home").join("config.toml.setup").exists());
    }

    #[test]
    fn a_bad_key_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");
        let keys = vec![
            ("preset".to_owned(), "\"peer-crew\"".to_owned()),
            ("crew".to_owned(), "42".to_owned()),
        ];
        assert!(write_config(&target, &keys).is_err());
        assert!(!target.exists());
        assert!(!dir.path().join("config.toml.setup").exists());
    }

    #[test]
    fn a_rerun_keeps_what_the_file_had() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");
        std::fs::write(&target, "# mine\n[ui]\nbell = false\n").unwrap();
        write_config(&target, &[("preset".to_owned(), "\"solo\"".to_owned())]).unwrap();
        let text = std::fs::read_to_string(&target).unwrap();
        assert!(text.contains("# mine"), "{text}");
        assert!(text.contains("bell = false"), "{text}");
        assert!(text.contains("preset = \"solo\""), "{text}");
    }

    #[test]
    fn preset_files_you_have_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        for name in PRESET_FILES {
            std::fs::write(dir.path().join(format!("{name}.toml")), "# edited\n").unwrap();
        }
        assert!(write_presets(dir.path()).is_empty());
        for name in PRESET_FILES {
            let text = std::fs::read_to_string(dir.path().join(format!("{name}.toml"))).unwrap();
            assert_eq!(text, "# edited\n");
        }
    }
}
