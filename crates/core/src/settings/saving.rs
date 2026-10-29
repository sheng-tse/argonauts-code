//! Where a change you make inside the app is saved, `save_changes_to`. The settings commands and
//! pickers read it when they save, so no change asks where it should go.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::SettingScope;

/// Each word is also how the line after a change says where it went. A flag on the command
/// (`--session`, `--local`, `--project`, `--global`) or ctrl+s in a picker saves one change
/// elsewhere. Only your global file may set it, so a repository you clone can't send your
/// changes into its shared file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SaveChangesTo {
    /// Your global file, `~/.argonauts/config.toml`, for every repository.
    #[default]
    Everywhere,
    /// This project, just you: `.argonauts/config.local.toml`, kept out of git.
    JustYou,
    /// This project, shared: `.argonauts/config.toml`, which a team commits.
    ThisProject,
    /// This app session only, gone when you quit.
    ThisSession,
}

impl SaveChangesTo {
    pub const ALL: [SaveChangesTo; 4] = [
        SaveChangesTo::Everywhere,
        SaveChangesTo::JustYou,
        SaveChangesTo::ThisProject,
        SaveChangesTo::ThisSession,
    ];

    /// The layer a change is written to.
    pub fn scope(self) -> SettingScope {
        match self {
            SaveChangesTo::Everywhere => SettingScope::Global,
            SaveChangesTo::JustYou => SettingScope::ProjectLocal,
            SaveChangesTo::ThisProject => SettingScope::Project,
            SaveChangesTo::ThisSession => SettingScope::Session,
        }
    }

    /// The word the setting takes, such as `just_you`.
    pub fn as_str(self) -> &'static str {
        match self {
            SaveChangesTo::Everywhere => "everywhere",
            SaveChangesTo::JustYou => "just_you",
            SaveChangesTo::ThisProject => "this_project",
            SaveChangesTo::ThisSession => "this_session",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct Saving {
        save_changes_to: SaveChangesTo,
    }

    #[test]
    fn each_word_reads_and_names_one_scope() {
        let mut scopes = Vec::new();
        for place in SaveChangesTo::ALL {
            let text = format!("save_changes_to = \"{}\"", place.as_str());
            let read: Saving = toml::from_str(&text).unwrap();
            assert_eq!(read.save_changes_to, place);
            assert_eq!(
                serde_json::to_value(place).unwrap(),
                serde_json::json!(place.as_str())
            );
            scopes.push(place.scope());
        }
        assert_eq!(
            scopes,
            [
                SettingScope::Global,
                SettingScope::ProjectLocal,
                SettingScope::Project,
                SettingScope::Session
            ]
        );
        assert_eq!(SaveChangesTo::default(), SaveChangesTo::Everywhere);
        assert!(toml::from_str::<Saving>("save_changes_to = \"global\"").is_err());
    }
}
