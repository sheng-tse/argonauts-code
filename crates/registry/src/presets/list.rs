//! What the `/preset` picker lists.

use std::path::{Path, PathBuf};

use super::builtin::BUILTIN;
use super::name::is_preset_name;
use crate::PresetInfo;
use crate::resolve::leaves;

/// The built-in presets, in the order the picker shows them.
pub fn presets() -> Vec<PresetInfo> {
    BUILTIN
        .iter()
        .map(|preset| PresetInfo {
            name: preset.name.to_owned(),
            source: "built-in".to_owned(),
            description: preset.description.to_owned(),
            keys: keys_of(preset.toml),
        })
        .collect()
}

/// Every preset you can pick: the built-ins, then the `*.toml` files in `global`, then in
/// `project`. A file shadows an earlier preset of the same name. Its description is the first
/// line of its leading comment, or the shadowed preset's when it has none. A file whose name
/// isn't a preset name, such as `my team.toml`, can't be picked, so it isn't listed.
pub fn presets_in(global: &Path, project: Option<&Path>) -> Vec<PresetInfo> {
    let mut all = presets();
    for dir in std::iter::once(global).chain(project) {
        for (name, path) in preset_files(dir) {
            // A file that can't be read lists with no keys; resolving it reports the error.
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let description = description_of(&text);
            let source = path.display().to_string();
            let keys = keys_of(&text);
            match all.iter_mut().find(|known| known.name == name) {
                Some(known) => {
                    known.source = source;
                    known.keys = keys;
                    if let Some(description) = description {
                        known.description = description;
                    }
                }
                None => all.push(PresetInfo {
                    name,
                    source,
                    description: description.unwrap_or_default(),
                    keys,
                }),
            }
        }
    }
    all
}

/// The first non-empty line of the comment a file starts with.
fn description_of(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .skip_while(|line| line.is_empty())
        .take_while(|line| line.starts_with('#'))
        .map(|line| line.trim_start_matches('#').trim())
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

/// The dotted keys a preset's TOML sets. Text that doesn't parse sets none.
pub(crate) fn keys_of(text: &str) -> Vec<String> {
    text.parse::<toml::Table>()
        .map(|table| leaves(&table).into_iter().map(|(key, _)| key).collect())
        .unwrap_or_default()
}

/// The `*.toml` files in `dir` named as presets, by name, sorted. A missing folder has none.
pub(crate) fn preset_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "toml"))
        .filter_map(|path| {
            let name = path.file_stem()?.to_str()?.to_owned();
            is_preset_name(&name).then_some((name, path))
        })
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_preset_comes_first_and_every_contract_preset_is_listed() {
        let names: Vec<String> = presets().into_iter().map(|p| p.name).collect();
        assert_eq!(names[0], "peer-crew");
        assert_eq!(names.len(), 12);
        assert!(names.iter().any(|n| n == "coordinated-crew"));
    }

    #[test]
    fn your_own_presets_are_listed_and_files_shadow_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("home/presets");
        let project = dir.path().join("repo/.argonauts/presets");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(global.join("peer-crew.toml"), "[critique]\nrounds = 2\n").unwrap();
        std::fs::write(
            global.join("my-team.toml"),
            "\n# Two reviewers on every card\n#\n# Ours.\n[review]\nper_card = 2\n",
        )
        .unwrap();
        std::fs::write(
            global.join("solo.toml"),
            "# Solo, but mine\n[solo]\nattempts = 2\n",
        )
        .unwrap();
        std::fs::write(project.join("my-team.toml"), "preset_note = 1\n").unwrap();
        std::fs::write(project.join("notes.md"), "not a preset").unwrap();
        // Names the lookup refuses aren't listed, since picking one would break every resolve.
        std::fs::write(project.join("my team.toml"), "[solo]\nattempts = 2\n").unwrap();
        std::fs::write(project.join(".hidden.toml"), "[solo]\nattempts = 2\n").unwrap();
        let all = presets_in(&global, Some(&project));
        assert_eq!(all.len(), presets().len() + 1);
        let find = |name: &str| all.iter().find(|preset| preset.name == name).unwrap();
        let peer = find("peer-crew");
        assert!(
            peer.source.ends_with("home/presets/peer-crew.toml"),
            "{peer:?}"
        );
        assert_eq!(peer.keys, ["critique.rounds"]);
        assert!(!peer.description.is_empty());
        let builtin_peer = presets()
            .into_iter()
            .find(|p| p.name == "peer-crew")
            .unwrap();
        assert_eq!(peer.description, builtin_peer.description);
        let mine = find("my-team");
        assert!(
            mine.source
                .ends_with("repo/.argonauts/presets/my-team.toml")
        );
        // The project's file has no header, so the global file's description stays.
        assert_eq!(mine.description, "Two reviewers on every card");
        assert_eq!(mine.keys, ["preset_note"]);
        let solo = find("solo");
        assert_eq!(solo.description, "Solo, but mine");
        assert!(solo.source.ends_with("home/presets/solo.toml"));
        assert_eq!(find("best-of-n").source, "built-in");
        assert_eq!(presets_in(&dir.path().join("none"), None), presets());
    }
}
