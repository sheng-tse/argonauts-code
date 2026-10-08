//! Hermes reads `config.yaml` from its home. Two of its keys depend on the session: the folders
//! it loads skills from, and the cap on its own subagents.

use std::path::Path;

use anyhow::{Context as _, bail};
use argonauts_core::SpecFile;
use argonauts_core::settings::sessions::{FanoutSwitch, NativeFanout};
use serde_json::{Map, Value, json};

/// The variable Hermes finds its home by, which marks a profile's home as Hermes'.
pub(crate) const HOME_ENV: &str = "HERMES_HOME";

const CONFIG: &str = "config.yaml";

/// Where the crew's reviewed skills live in a worktree.
const SKILLS: &str = ".agents/skills";

/// Points `skills.external_dirs` at the worktree's reviewed skills, and sets
/// `delegation.max_concurrent_children` to the role's fan-out cap. A home without `config.yaml`
/// gets one with just those keys.
pub(crate) fn tune_config(
    files: &mut Vec<SpecFile>,
    home: &Path,
    workdir: &Path,
    fanout: Option<NativeFanout>,
) -> anyhow::Result<()> {
    let path = home.join(CONFIG);
    let at = files.iter().position(|file| file.path == path);
    let mut config: Value = match at {
        Some(at) => serde_saphyr::from_str(&files[at].contents)
            .with_context(|| format!("reading the home template's {CONFIG}"))?,
        None => Value::Null,
    };
    // An empty file reads as null.
    if config.is_null() {
        config = Value::Object(Map::new());
    }
    let Some(root) = config.as_object_mut() else {
        bail!("the home template's {CONFIG} must hold a mapping");
    };
    let skills = workdir.join(SKILLS).display().to_string();
    section(root, "skills")?.insert("external_dirs".into(), json!([skills]));
    let cap = match fanout {
        Some(NativeFanout::Limit { max }) => Some(max),
        Some(NativeFanout::Switch(FanoutSwitch::Off)) => Some(0),
        // Hermes' own default decides.
        Some(NativeFanout::Switch(FanoutSwitch::On)) => {
            if let Some(delegation) = root.get_mut("delegation").and_then(Value::as_object_mut) {
                delegation.remove("max_concurrent_children");
            }
            None
        }
        None => None,
    };
    if let Some(cap) = cap {
        section(root, "delegation")?.insert("max_concurrent_children".into(), json!(cap));
    }
    // A long path stays one plain scalar instead of a folded block.
    let mut options = serde_saphyr::SerializerOptions::default();
    options.prefer_block_scalars = false;
    let contents = serde_saphyr::to_string_with_options(&config, options)
        .with_context(|| format!("writing the home's {CONFIG}"))?;
    match at {
        Some(at) => files[at].contents = contents,
        None => files.push(SpecFile { path, contents }),
    }
    Ok(())
}

/// `model.base_url` in a home template's `config.yaml`: where Hermes sends its model calls,
/// since it takes its provider only from there.
pub(crate) fn model_base_url(template: &Path) -> Option<String> {
    let text = std::fs::read_to_string(template.join(CONFIG)).ok()?;
    let config: Value = serde_saphyr::from_str(&text).ok()?;
    config
        .pointer("/model/base_url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_owned)
}

/// The mapping under `key`, made when it's missing or empty.
fn section<'a>(
    root: &'a mut Map<String, Value>,
    key: &str,
) -> anyhow::Result<&'a mut Map<String, Value>> {
    let value = root.entry(key).or_insert_with(|| Value::Object(Map::new()));
    if value.is_null() {
        *value = Value::Object(Map::new());
    }
    value
        .as_object_mut()
        .with_context(|| format!("`{key}` in the home template's {CONFIG} must be a mapping"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    const TEMPLATE: &str = "\
# Comments go, values stay.
fallback_providers: []
skills:
  write_approval: true
  external_dirs: []
delegation:
  max_concurrent_children: 1
";

    fn read(files: &[SpecFile]) -> Value {
        let file = files
            .iter()
            .find(|file| file.path == Path::new("/h/config.yaml"))
            .unwrap();
        serde_saphyr::from_str(&file.contents).unwrap()
    }

    fn template() -> Vec<SpecFile> {
        vec![SpecFile {
            path: PathBuf::from("/h/config.yaml"),
            contents: TEMPLATE.to_owned(),
        }]
    }

    #[test]
    fn skills_and_fanout_cap_follow_the_session() {
        let mut files = template();
        let capped = Some(NativeFanout::Limit { max: 4 });
        tune_config(&mut files, Path::new("/h"), Path::new("/wt"), capped).unwrap();
        let config = read(&files);
        assert_eq!(
            config["skills"]["external_dirs"],
            json!(["/wt/.agents/skills"])
        );
        assert_eq!(config["skills"]["write_approval"], json!(true));
        assert_eq!(config["delegation"]["max_concurrent_children"], json!(4));
        assert_eq!(config["fallback_providers"], json!([]));
        let long =
            Path::new("/a-worktree-path-long-enough-that-yaml-would-fold-it-into-a-block/task-001");
        let mut files = template();
        tune_config(&mut files, Path::new("/h"), long, None).unwrap();
        let line = format!("- {}/.agents/skills", long.display());
        assert!(files[0].contents.contains(&line), "{}", files[0].contents);
        assert!(files[0].contents.contains("fallback_providers: []"));

        let mut files = template();
        let off = Some(NativeFanout::Switch(FanoutSwitch::Off));
        tune_config(&mut files, Path::new("/h"), Path::new("/wt"), off).unwrap();
        assert_eq!(
            read(&files)["delegation"]["max_concurrent_children"],
            json!(0)
        );

        let mut files = template();
        let on = Some(NativeFanout::Switch(FanoutSwitch::On));
        tune_config(&mut files, Path::new("/h"), Path::new("/wt"), on).unwrap();
        assert_eq!(read(&files)["delegation"], json!({}));

        // No cap asked for leaves the template's.
        let mut files = template();
        tune_config(&mut files, Path::new("/h"), Path::new("/wt"), None).unwrap();
        assert_eq!(
            read(&files)["delegation"]["max_concurrent_children"],
            json!(1)
        );
    }

    #[test]
    fn the_model_server_comes_from_the_template() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(model_base_url(dir.path()), None);
        std::fs::write(dir.path().join(CONFIG), TEMPLATE).unwrap();
        assert_eq!(model_base_url(dir.path()), None);
        let local = format!(
            "{TEMPLATE}model:\n  provider: custom\n  base_url: http://localhost:11434/v1\n  \
             default: llama3.2\n"
        );
        std::fs::write(dir.path().join(CONFIG), local).unwrap();
        assert_eq!(
            model_base_url(dir.path()).as_deref(),
            Some("http://localhost:11434/v1")
        );
    }

    #[test]
    fn a_home_without_a_config_gets_one() {
        let mut files = vec![SpecFile {
            path: PathBuf::from("/h/skills/README.md"),
            contents: "No skills.\n".to_owned(),
        }];
        let capped = Some(NativeFanout::Limit { max: 2 });
        tune_config(&mut files, Path::new("/h"), Path::new("/wt"), capped).unwrap();
        assert_eq!(files.len(), 2);
        let config = read(&files);
        assert_eq!(
            config["skills"]["external_dirs"],
            json!(["/wt/.agents/skills"])
        );
        assert_eq!(config["delegation"]["max_concurrent_children"], json!(2));

        let mut empty = vec![SpecFile {
            path: PathBuf::from("/h/config.yaml"),
            contents: String::new(),
        }];
        tune_config(&mut empty, Path::new("/h"), Path::new("/wt"), None).unwrap();
        assert_eq!(
            read(&empty)["skills"]["external_dirs"],
            json!(["/wt/.agents/skills"])
        );
    }

    #[test]
    fn a_config_of_the_wrong_shape_is_refused() {
        for contents in ["- a list\n", "skills: [one]\n", "delegation: 3\n"] {
            let mut files = vec![SpecFile {
                path: PathBuf::from("/h/config.yaml"),
                contents: contents.to_owned(),
            }];
            let capped = Some(NativeFanout::Limit { max: 2 });
            let err = tune_config(&mut files, Path::new("/h"), Path::new("/wt"), capped);
            assert!(err.is_err(), "{contents:?}");
        }
    }
}
