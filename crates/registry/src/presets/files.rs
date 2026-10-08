//! The built-in presets written out as commented files you can read and edit, and their safe
//! upgrade: a file records the hash of the built-in it came from, so an edit is never lost.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use argonauts_core::settings::Settings;
use argonauts_core::sha256_hex;
use toml::{Table, Value};

use super::builtin::{BUILTIN, BuiltinPreset};
use super::list::preset_files;
use super::{DEFAULT_PRESET, builtin};
use crate::catalog::setting_meta;
use crate::resolve::leaves;
use crate::write::{io_error, lock_folder, real_path, write_replacing};
use crate::{PresetUpgrade, PresetUpgradeOutcome, SettingsError};

/// The keys a preset file shows, in the order the protocol runs. `phases` is a top-level key,
/// so it comes before any table.
const PROTOCOL: &[&str] = &[
    "phases",
    "solo_or_crew",
    "solo",
    "goal",
    "drafting",
    "mapping",
    "critique",
    "experiments",
    "judging",
    "divide",
    "review",
    "routing",
];

/// The header's last line. Everything above it is the header, and everything below is what the
/// hash covers, so any change below it is an edit.
const HASH_LINE: &str = "# built-in sha256: ";

/// Remembers that the app told you an upgrade kept your edits, for the built-in with this hash.
const TOLD_LINE: &str = "# kept your edits over built-in sha256: ";

/// Comment lines wrap at this many characters.
const WIDTH: usize = 96;

/// The built-in preset `name` as an editable TOML file. It sets only the keys the preset
/// changes, since the preset layer hides your settings files, shows the other protocol keys
/// commented out at their defaults, and records the built-in's hash so an upgrade can tell
/// whether you edited it.
pub fn builtin_preset(name: &str) -> Result<String, SettingsError> {
    let preset = builtin(name).ok_or_else(|| unknown(name))?;
    render(preset).map(|(text, _)| text)
}

/// The file's text and the hash it records.
fn render(preset: &BuiltinPreset) -> Result<(String, String), SettingsError> {
    let settings = body(preset)?;
    let body = match preset.steps {
        Some(steps) => with_steps(steps, &settings),
        None => settings,
    };
    let hash = body_hash(&body);
    let text = format!("{}{HASH_LINE}{hash}\n{body}", header(preset));
    Ok((text, hash))
}

/// Writes the built-in preset into `dir` and returns the file. A file there that you edited is
/// never replaced (`SettingsError::Refused`, naming `/preset diff <name>`).
pub fn write_preset(name: &str, dir: &Path) -> Result<PathBuf, SettingsError> {
    let text = builtin_preset(name)?;
    let path = dir.join(format!("{name}.toml"));
    let target = real_path(&path)?;
    let _turn = lock_folder(&target)?;
    if let Some(existing) = read_if_there(&target)?
        && PresetFile::parse(&existing).is_none_or(|file| file.edited())
    {
        return Err(SettingsError::Refused(format!(
            "{} has your edits, so it wasn't replaced. /workflow diff {name} compares it with the \
             built-in",
            path.display()
        )));
    }
    write_replacing(&target, &text)?;
    Ok(path)
}

/// Replaces each preset file in `dir` that you didn't edit with the new built-in and keeps the
/// edited ones. Only for `~/.argonauts/presets/`: a project's presets are reviewed and
/// committed by the team.
pub fn upgrade_presets(dir: &Path) -> Result<Vec<PresetUpgrade>, SettingsError> {
    let mut report = Vec::new();
    for (name, path) in preset_files(dir) {
        // Your own presets aren't ours to upgrade.
        let Some(preset) = builtin(&name) else {
            continue;
        };
        let target = real_path(&path)?;
        let _turn = lock_folder(&target)?;
        let Some(text) = read_if_there(&target)? else {
            continue;
        };
        // Nor is a file of that name that doesn't record the built-in it came from.
        let Some(file) = PresetFile::parse(&text) else {
            continue;
        };
        let (current, current_hash) = render(preset)?;
        let outcome = if !file.edited() {
            if file.recorded == current_hash {
                PresetUpgradeOutcome::Current
            } else {
                write_replacing(&target, &current)?;
                PresetUpgradeOutcome::Updated
            }
        } else if file.recorded == current_hash || file.told == Some(current_hash.as_str()) {
            // Nothing new since you edited it, or you've been told about this built-in.
            PresetUpgradeOutcome::KeptEdited { tell: false }
        } else {
            write_replacing(&target, &file.told_about(&current_hash))?;
            PresetUpgradeOutcome::KeptEdited { tell: true }
        };
        report.push(PresetUpgrade {
            name,
            path,
            outcome,
        });
    }
    Ok(report)
}

/// A written preset file split at its hash line.
struct PresetFile<'a> {
    /// The header's lines above the hash line.
    header: Vec<&'a str>,
    recorded: String,
    told: Option<&'a str>,
    body: &'a str,
}

impl<'a> PresetFile<'a> {
    /// `None` when the leading comments record no built-in, as in a preset you wrote yourself.
    fn parse(text: &'a str) -> Option<Self> {
        let mut header = Vec::new();
        let mut rest = text;
        while let Some(line) = rest.lines().next().filter(|line| line.starts_with('#')) {
            rest = rest.get(line.len()..)?;
            rest = rest
                .strip_prefix("\r\n")
                .or_else(|| rest.strip_prefix('\n'))
                .unwrap_or(rest);
            if let Some(hash) = line.strip_prefix(HASH_LINE) {
                let told = header
                    .iter()
                    .find_map(|line: &&str| line.strip_prefix(TOLD_LINE))
                    .map(str::trim);
                return Some(Self {
                    header,
                    recorded: hash.trim().to_owned(),
                    told,
                    body: rest,
                });
            }
            header.push(line);
        }
        None
    }

    fn edited(&self) -> bool {
        body_hash(self.body) != self.recorded
    }

    /// The same file, remembering that you were told about the built-in with `hash`. Only the
    /// header changes.
    fn told_about(&self, hash: &str) -> String {
        let mut text = String::new();
        for line in self
            .header
            .iter()
            .filter(|line| !line.starts_with(TOLD_LINE))
        {
            text.push_str(line);
            text.push('\n');
        }
        text.push_str(&format!(
            "{TOLD_LINE}{hash}\n{HASH_LINE}{}\n{}",
            self.recorded, self.body
        ));
        text
    }
}

/// The hash a file records: its body, with line endings and the blank lines around it evened
/// out, since an editor may change those without you editing anything.
/// The file's own keys and its steps around the settings it carries. TOML reads a key below a
/// table as that table's, so what sits at the top level comes first: the workflow's name and
/// how its steps are followed, then the top-level settings, then the steps, then the settings
/// that have tables.
fn with_steps(steps: &str, settings: &str) -> String {
    let (about, step_tables) = match steps.find("[[step]]") {
        Some(at) => steps.split_at(at),
        None => (steps, ""),
    };
    let (top_level, tables) = match settings.find("\n[") {
        Some(at) => settings.split_at(at),
        None => (settings, ""),
    };
    format!(
        "\n{}\n{}\n\n{}\n{tables}",
        about.trim(),
        top_level.trim_end(),
        step_tables.trim()
    )
}

fn body_hash(body: &str) -> String {
    sha256_hex(body.replace("\r\n", "\n").trim().as_bytes())
}

fn header(preset: &BuiltinPreset) -> String {
    let name = preset.name;
    let what = if name == DEFAULT_PRESET {
        format!(
            "The {name} workflow, the default. The built-in defaults are {name}, so this file sets \
             no key: the default workflow sits above your settings files, and a key set here would \
             hide your own. The protocol keys are below, commented out at their defaults."
        )
    } else {
        format!(
            "The {name} workflow. {DEFAULT_PRESET} is the default. This file sets only the keys \
             {name} changes. What you set in the file that picks it, or in a later one, wins over \
             it, and a key set here hides the same key in the files before that. The other \
             protocol keys are below, commented out at their defaults."
        )
    };
    let upgrades = format!(
        "Argonauts wrote this file from its built-in workflow. Until you edit it, upgrades keep it \
         the same as the built-in. Once you do, they keep your edits, and /workflow diff {name} \
         compares the two."
    );
    let mut text = comment(preset.description);
    text.push_str("#\n");
    text.push_str(&comment(&what));
    text.push_str("#\n");
    text.push_str(&comment(&upgrades));
    text
}

/// `text` as comment lines of at most [`WIDTH`] characters.
fn comment(text: &str) -> String {
    let mut out = String::new();
    let mut line = String::from("#");
    for word in text.split_whitespace() {
        if line.len() > 1 && line.chars().count() + 1 + word.chars().count() > WIDTH {
            out.push_str(&line);
            out.push('\n');
            line = String::from("#");
        }
        line.push(' ');
        line.push_str(word);
    }
    out.push_str(&line);
    out.push('\n');
    out
}

/// One key as the file shows it.
struct Shown {
    key: String,
    value: Value,
    set: bool,
}

/// The keys the preset sets, each under its description, and the other protocol keys commented
/// out at their defaults, grouped by table.
fn body(preset: &BuiltinPreset) -> Result<String, SettingsError> {
    let invalid = |message: String| SettingsError::Invalid {
        layer: format!("the built-in {} workflow", preset.name),
        message,
    };
    let set = leaves(
        &preset
            .toml
            .parse::<Table>()
            .map_err(|err| invalid(err.to_string()))?,
    );
    let defaults =
        leaves(&Table::try_from(Settings::default()).map_err(|err| invalid(err.to_string()))?);

    let mut groups: Vec<(String, Vec<Shown>)> = Vec::new();
    let mut add = |key: &str, value: &Value, is_set: bool| {
        let table = table_of(key);
        let shown = Shown {
            key: key.to_owned(),
            value: value.clone(),
            set: is_set,
        };
        match groups.iter_mut().find(|(name, _)| *name == table) {
            Some((_, keys)) => keys.push(shown),
            None => groups.push((table, vec![shown])),
        }
    };
    for section in PROTOCOL {
        let inside = |key: &str| key == *section || key.starts_with(&format!("{section}."));
        // Keys the defaults leave unset, such as `triage.force`, come first.
        for (key, value) in set.iter().filter(|(key, _)| inside(key)) {
            if !defaults.iter().any(|(default, _)| default == key) {
                add(key, value, true);
            }
        }
        for (key, default) in defaults.iter().filter(|(key, _)| inside(key)) {
            match set.iter().find(|(set_key, _)| set_key == key) {
                Some((_, value)) => add(key, value, true),
                None => add(key, default, false),
            }
        }
    }
    for (key, value) in &set {
        if !PROTOCOL
            .iter()
            .any(|section| key == section || key.starts_with(&format!("{section}.")))
        {
            add(key, value, true);
        }
    }

    let mut text = String::new();
    for (table, keys) in &groups {
        text.push('\n');
        if !table.is_empty() {
            text.push_str(&format!("[{table}]\n"));
        }
        for shown in keys {
            if let Some(meta) = setting_meta(&shown.key) {
                text.push_str(&comment(&meta.description));
            }
            let name = shown.key.rsplit('.').next().unwrap_or(&shown.key);
            let line = format!("{name} = {}", shown.value);
            if shown.set {
                text.push_str(&format!("{line}\n"));
            } else {
                text.push_str(&format!("# {line}\n"));
            }
        }
    }
    Ok(text)
}

/// The table a dotted key sits in, `""` for a top-level key.
fn table_of(key: &str) -> String {
    key.rsplit_once('.')
        .map(|(table, _)| table.to_owned())
        .unwrap_or_default()
}

fn read_if_there(path: &Path) -> Result<Option<String>, SettingsError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(io_error(path, err)),
    }
}

fn unknown(name: &str) -> SettingsError {
    let names: Vec<&str> = BUILTIN.iter().map(|preset| preset.name).collect();
    SettingsError::Invalid {
        layer: "the built-in workflows".to_owned(),
        message: format!(
            "there's no built-in workflow named {name:?}; the built-ins are {}",
            names.join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SettingsSources;

    /// `text` as though it came from an older built-in whose body was `old_body`.
    fn from_older_builtin(text: &str, old_body: &str) -> String {
        let file = PresetFile::parse(text).unwrap();
        let mut older = file.header.join("\n");
        older.push_str(&format!("\n{HASH_LINE}{}\n{old_body}", body_hash(old_body)));
        older
    }

    /// `text` recording `hash` as the built-in it came from, its body untouched.
    fn recording(text: &str, hash: &str) -> String {
        let file = PresetFile::parse(text).unwrap();
        format!(
            "{}\n{HASH_LINE}{hash}\n{}",
            file.header.join("\n"),
            file.body
        )
    }

    /// `key = value`, or the same commented out.
    fn is_key_line(line: &str) -> bool {
        let line = line.strip_prefix("# ").unwrap_or(line);
        line.split_once(" = ").is_some_and(|(key, _)| {
            !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
    }

    /// The settings a written file sets, without the keys that describe the workflow itself.
    fn settings_in(text: &str) -> Table {
        let mut table = text.parse::<Table>().unwrap();
        for key in argonauts_core::settings::workflows::WORKFLOW_FILE_KEYS {
            table.remove(key);
        }
        table
    }

    fn settings_for(home: &Path, repo_config: Option<&Path>) -> Result<Settings, SettingsError> {
        let sources = SettingsSources {
            global: Some(home.join("config.toml")),
            project: repo_config.map(Path::to_owned),
            project_trusted: true,
            ..SettingsSources::default()
        };
        crate::resolve(&sources).map(|resolved| resolved.settings)
    }

    #[test]
    fn preset_files_written_commented() {
        for preset in BUILTIN {
            let name = preset.name;
            let text = builtin_preset(name).unwrap();
            let file = PresetFile::parse(&text).unwrap();
            assert!(!file.edited(), "{name}");
            assert_eq!(file.recorded.len(), 64, "{name}");
            assert!(
                file.recorded.chars().all(|c| c.is_ascii_hexdigit()),
                "{name}"
            );
            assert_eq!(file.header[0], format!("# {}", preset.description));
            // The header wraps, so a phrase may straddle two of its comment lines.
            let header = file
                .header
                .iter()
                .map(|line| line.trim_start_matches('#').trim())
                .collect::<Vec<_>>()
                .join(" ");
            if name == DEFAULT_PRESET {
                assert!(header.contains("workflow, the default"), "{header}");
            } else {
                assert!(header.contains("council is the default"), "{header}");
            }
            assert!(
                header.contains(&format!("/workflow diff {name}")),
                "{header}"
            );

            // It sets exactly what the built-in sets, and every key sits under its description.
            // Its table headers stay, so a key you uncomment lands in the right table.
            let written = leaves(&settings_in(&text));
            let built = leaves(&preset.toml.parse::<Table>().unwrap());
            assert_eq!(written, built, "{name}");
            // The workflow's own lines are its steps file word for word, and say what they are
            // in comments of their own.
            let own: Vec<&str> = preset.steps.unwrap_or_default().lines().collect();
            if preset.steps.is_some() {
                let about = text.parse::<Table>().unwrap()["about"]
                    .as_str()
                    .map(str::to_owned);
                assert_eq!(about.as_deref(), Some(preset.description), "{name}");
            }
            for line in &own {
                assert!(file.body.contains(line), "{name}: {line}");
            }
            let lines: Vec<&str> = file.body.lines().collect();
            let mut keys = 0;
            for (at, line) in lines.iter().enumerate() {
                if own.contains(line) {
                    continue;
                }
                if is_key_line(line) {
                    keys += 1;
                    let above = lines[at - 1];
                    assert!(
                        above.starts_with("# ") && !is_key_line(above),
                        "{name}: {line} has no description above it"
                    );
                } else if line.starts_with('#') {
                    assert!(line.chars().count() <= WIDTH, "{line}");
                }
            }
            assert!(keys >= 40, "{name} shows only {keys} keys");
            for section in &PROTOCOL[1..] {
                assert!(
                    text.contains(&format!("\n[{section}]\n")),
                    "{name}: {section}"
                );
            }
        }

        let peer = builtin_preset("council").unwrap();
        assert!(leaves(&settings_in(&peer)).is_empty(), "{peer}");
        // Its steps read back as the runner's own built-in file.
        let steps = peer.parse::<Table>().unwrap()["step"]
            .as_array()
            .unwrap()
            .len();
        assert_eq!(steps, 6, "{peer}");
        assert!(peer.contains("\nname  = \"council\"\n"), "{peer}");
        assert!(peer.contains("\n# phases = \"council\"\n"), "{peer}");
        assert!(peer.contains("\n[critique]\n"), "{peer}");
        assert!(peer.contains("\n# rounds = 1\n"), "{peer}");
        let solo = builtin_preset("solo").unwrap();
        assert!(solo.contains("\n[solo_or_crew]\n# Force a mode"), "{solo}");
        assert!(solo.contains("\nforce = \"solo\"\n"), "{solo}");
        assert!(solo.starts_with("# One agent does every task\n"), "{solo}");
        let coordinated = builtin_preset("orchestrator").unwrap();
        assert!(
            coordinated.contains("\n[roles.adjudicator]\n"),
            "{coordinated}"
        );
        assert!(matches!(
            builtin_preset("nobody"),
            Err(SettingsError::Invalid { .. })
        ));

        // The written council sets nothing, so your own project setting still shows through.
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        write_preset("council", &home.join("workflows")).unwrap();
        let listed = crate::presets_in(&home.join("workflows"), None);
        assert_eq!(listed[0].description, BUILTIN[0].description);
        assert!(listed[0].source.ends_with("council.toml"));
        assert!(listed[0].keys.is_empty());
        let project = dir.path().join("repo/.argonauts/config.toml");
        fs::create_dir_all(project.parent().unwrap()).unwrap();
        fs::write(&project, "[critique]\nrounds = 0\n").unwrap();
        let settings = settings_for(&home, Some(&project)).unwrap();
        assert_eq!(settings.critique.rounds, 0);

        // Each written file resolves as its built-in does.
        for preset in BUILTIN {
            let name = preset.name;
            let with_file = dir.path().join(format!("with-{name}"));
            let without = dir.path().join(format!("without-{name}"));
            for home in [&with_file, &without] {
                fs::create_dir_all(home).unwrap();
                fs::write(home.join("config.toml"), format!("workflow = {name:?}\n")).unwrap();
            }
            write_preset(name, &with_file.join("workflows")).unwrap();
            let written = settings_for(&with_file, None);
            assert!(written.is_ok(), "{name}: {written:?}");
            assert_eq!(written, settings_for(&without, None), "{name}");
        }
    }

    #[test]
    fn write_preset_keeps_your_edits() {
        let dir = tempfile::tempdir().unwrap();
        let presets = dir.path().join("workflows");
        let name = "orchestrator";
        let built = builtin_preset(name).unwrap();
        let path = write_preset(name, &presets).unwrap();
        assert_eq!(path, presets.join("orchestrator.toml"));
        assert_eq!(fs::read_to_string(&path).unwrap(), built);

        // Unedited, whether current or from an older built-in, it's replaced.
        write_preset(name, &presets).unwrap();
        fs::write(
            &path,
            from_older_builtin(&built, "[review]\nper_task = 1\n"),
        )
        .unwrap();
        write_preset(name, &presets).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), built);

        // The header isn't yours: a changed description is still an unedited file.
        fs::write(
            &path,
            built.replacen("# One orchestrator", "# My orchestrator", 1),
        )
        .unwrap();
        write_preset(name, &presets).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), built);

        // Any change below the header is an edit, a comment included.
        for edited in [
            built.replace("per_task = 2", "per_task = 3"),
            built.replace("\n[review]\n", "\n[review]\n# two is plenty\n"),
            format!("{built}\n[critique]\nrounds = 2\n"),
            "[review]\nper_task = 3\n".to_owned(),
        ] {
            fs::write(&path, &edited).unwrap();
            let err = write_preset(name, &presets).unwrap_err();
            match &err {
                SettingsError::Refused(message) => {
                    assert!(message.contains("/workflow diff orchestrator"), "{message}");
                }
                other => panic!("{other:?}"),
            }
            assert_eq!(fs::read_to_string(&path).unwrap(), edited);
        }

        // Line endings and trailing blank lines an editor changes aren't edits.
        fs::write(&path, format!("{}\n\n", built.replace('\n', "\r\n"))).unwrap();
        write_preset(name, &presets).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), built);

        assert!(matches!(
            write_preset("nobody", &presets),
            Err(SettingsError::Invalid { .. })
        ));
        assert!(!presets.join("nobody.toml").exists());
    }

    #[test]
    fn preset_upgrade_updates_unedited_keeps_edited() {
        let dir = tempfile::tempdir().unwrap();
        let presets = dir.path().join("workflows");
        assert!(upgrade_presets(&presets).unwrap().is_empty());
        for name in ["council", "solo", "orchestrator", "best-of-n"] {
            write_preset(name, &presets).unwrap();
        }
        let file = |name: &str| presets.join(format!("{name}.toml"));
        let current = |name: &str| builtin_preset(name).unwrap();

        // solo came from an older built-in and you never touched it.
        fs::write(
            file("solo"),
            from_older_builtin(&current("solo"), "[solo_or_crew]\nforce = \"crew\"\n"),
        )
        .unwrap();
        // orchestrator came from an older built-in and you edited it.
        let edited = recording(
            &current("orchestrator").replace("per_task = 2", "per_task = 3"),
            &body_hash("an older built-in"),
        );
        fs::write(file("orchestrator"), &edited).unwrap();
        // best-of-n is edited, but the built-in hasn't changed since.
        let tuned = current("best-of-n").replace("attempts = 3", "attempts = 5");
        fs::write(file("best-of-n"), &tuned).unwrap();
        // Your own presets are never touched, even one named like a built-in.
        fs::write(file("my-team"), "[review]\nper_task = 2\n").unwrap();
        fs::write(file("draft-and-vote"), "[judging]\nmethod = \"vote\"\n").unwrap();

        let outcomes = |report: Vec<PresetUpgrade>| -> Vec<(String, PresetUpgradeOutcome)> {
            report
                .into_iter()
                .map(|upgrade| {
                    assert_eq!(upgrade.path, file(&upgrade.name));
                    (upgrade.name, upgrade.outcome)
                })
                .collect()
        };
        let kept = |tell| PresetUpgradeOutcome::KeptEdited { tell };
        assert_eq!(
            outcomes(upgrade_presets(&presets).unwrap()),
            [
                ("best-of-n".to_owned(), kept(false)),
                ("council".to_owned(), PresetUpgradeOutcome::Current),
                ("orchestrator".to_owned(), kept(true)),
                ("solo".to_owned(), PresetUpgradeOutcome::Updated),
            ]
        );
        assert_eq!(fs::read_to_string(file("solo")).unwrap(), current("solo"));
        assert_eq!(fs::read_to_string(file("best-of-n")).unwrap(), tuned);
        let kept_file = fs::read_to_string(file("orchestrator")).unwrap();
        assert_eq!(
            PresetFile::parse(&kept_file).unwrap().body,
            PresetFile::parse(&edited).unwrap().body
        );
        assert!(kept_file.contains("per_task = 3"));
        assert_eq!(
            fs::read_to_string(file("my-team")).unwrap(),
            "[review]\nper_task = 2\n"
        );

        // You're told once; the edit stays, and so does the rest of the report.
        assert_eq!(
            outcomes(upgrade_presets(&presets).unwrap()),
            [
                ("best-of-n".to_owned(), kept(false)),
                ("council".to_owned(), PresetUpgradeOutcome::Current),
                ("orchestrator".to_owned(), kept(false)),
                ("solo".to_owned(), PresetUpgradeOutcome::Current),
            ]
        );
        assert_eq!(fs::read_to_string(file("orchestrator")).unwrap(), kept_file);
        // It's remembered in the header, for this built-in only.
        let kept = PresetFile::parse(&kept_file).unwrap();
        let newest = current("orchestrator");
        let newest = PresetFile::parse(&newest).unwrap();
        assert_eq!(kept.told, Some(newest.recorded.as_str()));
        assert_eq!(kept.recorded, body_hash("an older built-in"));
    }
}
