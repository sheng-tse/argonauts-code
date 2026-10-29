//! `argonauts config show|schema|set` and `argonauts models`.

use std::process::ExitCode;

use argonauts_app::ModelList;
use argonauts_core::registry::ModelEntry;
use argonauts_core::sanitize::sanitize_for_terminal;
use argonauts_core::settings::SettingScope;
use argonauts_core::window_size::window_words;

use crate::args::ConfigCommand;
use crate::text::words;
use crate::usage::usage;

pub fn run(command: &ConfigCommand, set: &[String]) -> anyhow::Result<ExitCode> {
    match command {
        ConfigCommand::Show => {
            print!(
                "{}",
                argonauts_app::config_show(None, set.to_vec()).map_err(usage)?
            );
        }
        ConfigCommand::Schema => {
            println!(
                "{}",
                serde_json::to_string_pretty(&argonauts_app::config_schema())?
            );
        }
        ConfigCommand::Set {
            key,
            value,
            local,
            project,
            global,
        } => {
            let scope = scope(*local, *project, *global);
            let file = argonauts_app::config_set(None, key, value, scope)?;
            println!("Saved {key} in {}.", file.display());
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `None` saves it where your changes go.
fn scope(local: bool, project: bool, global: bool) -> Option<SettingScope> {
    if local {
        Some(SettingScope::ProjectLocal)
    } else if project {
        Some(SettingScope::Project)
    } else if global {
        Some(SettingScope::Global)
    } else {
        None
    }
}

pub async fn models(discover: bool, overrides: &[String]) -> anyhow::Result<ExitCode> {
    let listed = argonauts_app::models(None, overrides.to_vec(), discover).await?;
    print!("{}", models_text(&listed, discover));
    Ok(ExitCode::SUCCESS)
}

fn models_text(listed: &ModelList, discovered: bool) -> String {
    let ModelList { models, skipped } = listed;
    let mut lines: String = models.iter().map(model_line).collect();
    if !skipped.is_empty() {
        lines.push_str("Skipped\n");
        for reason in skipped {
            lines.push_str(&format!("  {reason}\n"));
        }
    }
    if models.is_empty() && skipped.is_empty() {
        lines.push_str(if discovered {
            "No models found.\n"
        } else {
            "No models known yet. "
        });
    }
    if !discovered {
        lines.push_str(if models.is_empty() {
            "`argonauts models --discover` asks your CLIs and local servers.\n"
        } else {
            "`argonauts models --discover` asks your CLIs and local servers for more.\n"
        });
    }
    // Discovered ids and reasons come from the CLIs and servers themselves.
    sanitize_for_terminal(&lines)
}

/// `id · provider · kind · access`, then the window a session gets when one is known, else
/// the most the model takes, as for a model a local server sizes as it loads it.
fn model_line(model: &ModelEntry) -> String {
    let window = match (model.session_window(), model.context_window) {
        (Some(window), _) => format!(" · {} window", window_words(window)),
        (None, Some(most)) => format!(" · window up to {}", window_words(most)),
        (None, None) => String::new(),
    };
    format!(
        "{} · {} · {} · {}{window}\n",
        model.id,
        model.provider,
        words(&model.kind),
        words(&model.access)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_picks_the_file_and_none_leaves_the_usual_one() {
        assert_eq!(scope(true, false, false), Some(SettingScope::ProjectLocal));
        assert_eq!(scope(false, true, false), Some(SettingScope::Project));
        assert_eq!(scope(false, false, true), Some(SettingScope::Global));
        assert_eq!(scope(false, false, false), None);
    }

    fn listed(models: Vec<ModelEntry>, skipped: &[&str]) -> ModelList {
        ModelList {
            models,
            skipped: skipped.iter().map(|reason| (*reason).to_owned()).collect(),
        }
    }

    fn entry(json: serde_json::Value) -> ModelEntry {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn no_models_says_how_to_find_some() {
        assert_eq!(
            models_text(&listed(Vec::new(), &[]), false),
            "No models known yet. `argonauts models --discover` asks your CLIs and local servers.\n"
        );
        assert_eq!(
            models_text(&listed(Vec::new(), &[]), true),
            "No models found.\n"
        );
    }

    #[test]
    fn known_models_still_say_how_to_find_more() {
        let codex = entry(serde_json::json!({
            "id": "codex", "provider": "openai", "kind": "cli", "access": "subscription",
            "cost": {"unit": "plan_usage"},
        }));
        let shown = models_text(&listed(vec![codex.clone()], &[]), false);
        assert!(
            shown.starts_with("codex · openai · CLI · subscription\n"),
            "{shown}"
        );
        assert!(shown.contains("--discover"), "{shown}");
        let discovered = models_text(&listed(vec![codex], &[]), true);
        assert_eq!(discovered, "codex · openai · CLI · subscription\n");
    }

    /// A repo that turns Claude Code and agy off and has no table for Ollama: what is found comes
    /// with its window, and what was skipped says why, where "No models found." stood.
    #[test]
    fn the_list_says_each_window_and_what_it_skipped() {
        let local = |id: &str, window: serde_json::Value, most: u64| {
            entry(serde_json::json!({
                "id": id, "provider": "alibaba", "kind": "endpoint", "access": "local",
                "cost": {"unit": "none"}, "discovered": true,
                "window": window, "context_window": most,
            }))
        };
        let codex = entry(serde_json::json!({
            "id": "codex/gpt-6-luna", "provider": "openai", "kind": "cli",
            "access": "subscription", "cost": {"unit": "plan_usage"}, "discovered": true,
            "context_window": 872_000,
        }));
        let models = vec![
            local(
                "ollama/qwen3-coder:30b-64k",
                serde_json::json!(65_536),
                262_144,
            ),
            local("ollama/qwen3-coder:30b", serde_json::Value::Null, 262_144),
            codex,
        ];
        let skipped = [
            "Claude Code, agy: off in this repo",
            "LM Studio: not running",
        ];
        assert_eq!(
            models_text(&listed(models, &skipped), true),
            "ollama/qwen3-coder:30b-64k · alibaba · endpoint · local · 64k window\n\
             ollama/qwen3-coder:30b · alibaba · endpoint · local · window up to 256k\n\
             codex/gpt-6-luna · openai · CLI · subscription · 872k window\n\
             Skipped\n\
             \x20 Claude Code, agy: off in this repo\n\
             \x20 LM Studio: not running\n"
        );
        assert_eq!(
            models_text(
                &listed(Vec::new(), &["Claude Code, Codex: off in this repo"]),
                true
            ),
            "Skipped\n  Claude Code, Codex: off in this repo\n"
        );
    }
}
