//! What an isolated session keeps out: your plugins and their mods, your hooks, and the skills
//! and plugins synced from your claude.ai account. `--setting-sources user` still loads all of
//! them, so the session's settings turn each one off, and its `init` line is checked.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use argonauts_core::{AgentEvent, SessionRequest};
use serde_json::Value;

use crate::json::{array_at, str_at, strings_at};

/// Built-in plugins that draw nothing and hold nothing, so an isolated session keeps them.
const KEPT_BUILTINS: &[&str] = &[
    "cc-plugin-agents-md",
    "cc-plugin-telemetry",
    "cc-plugin-plugin-authoring",
    "cc-plugin-sec-default",
];

/// The other built-ins of 2.1.289. Most stay off under `-p`, but your settings can turn any of
/// them on, and You should know starts side agents.
const OTHER_BUILTINS: &[&str] = &[
    "cc-plugin-you-should-know",
    "cc-plugin-tips",
    "cc-plugin-mermaid",
    "cc-plugin-diff",
    "cc-plugin-responsive-mode",
    "cc-plugin-claude-test",
    "cc-plugin-mods-guide",
];

const BUILTIN_MARKETPLACE: &str = "builtin";

/// The namespace Claude Code gives the skills synced from your claude.ai account.
pub const SYNCED_SKILLS: &str = "anthropic-skills";

/// Where the session's Claude Code keeps your settings: `CLAUDE_CONFIG_DIR` when the session
/// or Argonauts has one, else `~/.claude`.
pub fn config_dir(req: &SessionRequest) -> Option<PathBuf> {
    req.env_set
        .iter()
        .find(|(name, _)| name == "CLAUDE_CONFIG_DIR")
        .map(|(_, dir)| PathBuf::from(dir))
        .or_else(your_config_dir)
}

/// The folder your own Claude Code keeps your settings in.
pub fn your_config_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))
}

/// Every plugin your settings enable or you installed, and every built-in that isn't kept, by
/// id, `learned` among them: built-ins of a later version an earlier session loaded. Turning off
/// one that isn't on does nothing, so anything that might load is named.
pub fn plugins_to_turn_off(config_dir: Option<&Path>, learned: &[String]) -> Vec<String> {
    let mut ids: BTreeSet<String> = OTHER_BUILTINS
        .iter()
        .copied()
        .chain(learned.iter().map(String::as_str))
        .filter(|name| !KEPT_BUILTINS.contains(name))
        .map(|name| format!("{name}@{BUILTIN_MARKETPLACE}"))
        .collect();
    if let Some(dir) = config_dir {
        ids.extend(enabled_plugins(&dir.join("settings.json")));
        ids.extend(installed_plugins(
            &dir.join("plugins").join("installed_plugins.json"),
        ));
    }
    ids.into_iter().collect()
}

fn enabled_plugins(settings: &Path) -> Vec<String> {
    let Some(settings) = read_json(settings) else {
        return Vec::new();
    };
    let Some(enabled) = settings.get("enabledPlugins").and_then(Value::as_object) else {
        return Vec::new();
    };
    enabled
        .iter()
        .filter(|(_, on)| on.as_bool() == Some(true))
        .map(|(id, _)| id.clone())
        .collect()
}

fn installed_plugins(installed: &Path) -> Vec<String> {
    read_json(installed)
        .and_then(|installed| {
            let plugins = installed.get("plugins")?.as_object()?;
            Some(plugins.keys().cloned().collect())
        })
        .unwrap_or_default()
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

/// What `init` says the session loaded beyond the kept built-ins: plugins, by name, and synced
/// skills, and apart from them the built-ins a later version added. `None` when it loaded
/// nothing it shouldn't have.
pub fn leak(init: &Value) -> Option<AgentEvent> {
    let mut plugins = Vec::new();
    let mut builtins = Vec::new();
    for plugin in array_at(init, "plugins") {
        let name = match plugin {
            Value::String(name) => name.as_str(),
            other => str_at(other, "name").unwrap_or_default(),
        };
        if name.is_empty() {
            continue;
        }
        let builtin = builtin(plugin);
        if builtin && KEPT_BUILTINS.contains(&name) {
            continue;
        }
        if builtin && !OTHER_BUILTINS.contains(&name) {
            builtins.push(name.to_owned());
        } else {
            plugins.push(name.to_owned());
        }
    }
    let skills: Vec<String> = strings_at(init, "skills")
        .into_iter()
        .filter(|skill| is_synced_skill(skill))
        .collect();
    (!plugins.is_empty() || !skills.is_empty() || !builtins.is_empty()).then_some(
        AgentEvent::IsolationLeak {
            plugins,
            skills,
            builtins,
        },
    )
}

/// A built-in, as 2.1.289 lists one: `{"name": "cc-plugin-telemetry", "path": "builtin",
/// "source": "cc-plugin-telemetry@builtin"}`. Older versions list bare names.
fn builtin(plugin: &Value) -> bool {
    match plugin {
        Value::String(_) => true,
        other => {
            str_at(other, "path") == Some(BUILTIN_MARKETPLACE)
                || str_at(other, "source")
                    .is_some_and(|source| source.ends_with(&format!("@{BUILTIN_MARKETPLACE}")))
        }
    }
}

fn is_synced_skill(skill: &str) -> bool {
    skill
        .strip_prefix(SYNCED_SKILLS)
        .is_some_and(|rest| rest.starts_with(':'))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn every_plugin_you_enabled_or_installed_is_named_with_the_active_builtins() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path();
        write(
            &dir.join("settings.json"),
            r#"{"enabledPlugins": {"lint-helper@official": true, "off@official": false}}"#,
        );
        write(
            &dir.join("plugins/installed_plugins.json"),
            r#"{"plugins": {"memory-keeper@market": [{"scope": "user"}]}}"#,
        );
        let ids = plugins_to_turn_off(Some(dir), &[]);
        assert!(ids.contains(&"lint-helper@official".to_owned()), "{ids:?}");
        assert!(ids.contains(&"memory-keeper@market".to_owned()), "{ids:?}");
        assert!(ids.contains(&"cc-plugin-you-should-know@builtin".to_owned()));
        assert!(!ids.contains(&"off@official".to_owned()));
        for kept in KEPT_BUILTINS {
            assert!(!ids.iter().any(|id| id.starts_with(kept)), "{kept}");
        }
        let without_settings = plugins_to_turn_off(None, &[]);
        assert_eq!(without_settings.len(), OTHER_BUILTINS.len());
    }

    /// A later version's built-in, learned from an earlier session, goes off too. One of the
    /// kept never does.
    #[test]
    fn learned_builtins_go_off_and_the_kept_stay() {
        let learned = [
            "cc-plugin-new-thing".to_owned(),
            KEPT_BUILTINS[0].to_owned(),
        ];
        let ids = plugins_to_turn_off(None, &learned);
        assert!(
            ids.contains(&"cc-plugin-new-thing@builtin".to_owned()),
            "{ids:?}"
        );
        assert!(
            !ids.iter().any(|id| id.starts_with(KEPT_BUILTINS[0])),
            "{ids:?}"
        );
    }

    /// A built-in no list here names is a later version's own default, not yours: reported
    /// apart, to be learned, and no leak. A renamed kept one reads the same way.
    #[test]
    fn a_builtin_argonauts_doesnt_know_is_learned_not_a_leak() {
        let init = json!({"plugins": [
            {"name": "cc-plugin-agents-md", "path": "builtin", "source": "cc-plugin-agents-md@builtin"},
            {"name": "cc-plugin-new-thing", "path": "builtin", "source": "cc-plugin-new-thing@builtin"}
        ]});
        assert_eq!(
            leak(&init),
            Some(AgentEvent::IsolationLeak {
                plugins: Vec::new(),
                skills: Vec::new(),
                builtins: vec!["cc-plugin-new-thing".into()],
            })
        );
    }

    #[test]
    fn a_session_can_name_the_folder_its_claude_code_reads() {
        let req = SessionRequest {
            env_set: vec![("CLAUDE_CONFIG_DIR".into(), "/elsewhere/claude".into())],
            ..SessionRequest::default()
        };
        assert_eq!(config_dir(&req), Some(PathBuf::from("/elsewhere/claude")));
    }

    #[test]
    fn only_the_kept_builtins_and_your_own_skills_are_no_leak() {
        let clean = json!({
            "plugins": [
                {"name": "cc-plugin-agents-md", "path": "builtin", "source": "cc-plugin-agents-md@builtin"},
                {"name": "cc-plugin-telemetry", "path": "builtin", "source": "cc-plugin-telemetry@builtin"}
            ],
            "skills": ["debug", "verify"]
        });
        assert_eq!(leak(&clean), None);
        let leaked = json!({
            "plugins": [
                {"name": "memory-keeper", "path": "~/.claude/plugins/cache/m", "source": "memory-keeper@market"},
                {"name": "cc-plugin-you-should-know", "path": "builtin", "source": "cc-plugin-you-should-know@builtin"},
                {"name": "cc-plugin-telemetry", "path": "~/x", "source": "cc-plugin-telemetry@market"}
            ],
            "skills": ["debug", "anthropic-skills:example-a", "anthropic-skillset"]
        });
        assert_eq!(
            leak(&leaked),
            Some(AgentEvent::IsolationLeak {
                plugins: vec![
                    "memory-keeper".into(),
                    "cc-plugin-you-should-know".into(),
                    "cc-plugin-telemetry".into()
                ],
                skills: vec!["anthropic-skills:example-a".into()],
                builtins: Vec::new(),
            })
        );
    }
}
