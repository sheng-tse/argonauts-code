//! `argonauts config show|schema|set` and `argonauts models`.

use std::process::ExitCode;

use argonauts_core::registry::ModelEntry;
use argonauts_core::sanitize::sanitize_for_terminal;
use argonauts_core::settings::SettingScope;

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

/// `None` leaves it to the key's usual file.
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

pub async fn models(discover: bool) -> anyhow::Result<ExitCode> {
    let models = argonauts_app::models(None, discover).await?;
    print!("{}", models_text(&models, discover));
    Ok(ExitCode::SUCCESS)
}

fn models_text(models: &[ModelEntry], discovered: bool) -> String {
    if models.is_empty() {
        return if discovered {
            "No models found.\n".to_owned()
        } else {
            "No models known yet. `argonauts models --discover` asks your CLIs and local servers.\n"
                .to_owned()
        };
    }
    let mut lines: String = models
        .iter()
        .map(|model| {
            format!(
                "{} · {} · {} · {}\n",
                model.id,
                model.provider,
                words(&model.kind),
                words(&model.access)
            )
        })
        .collect();
    if !discovered {
        lines
            .push_str("`argonauts models --discover` asks your CLIs and local servers for more.\n");
    }
    // Discovered ids come from the CLIs and servers themselves.
    sanitize_for_terminal(&lines)
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

    #[test]
    fn no_models_says_how_to_find_some() {
        assert!(models_text(&[], false).contains("--discover"));
        assert_eq!(models_text(&[], true), "No models found.\n");
    }

    #[test]
    fn known_models_still_say_how_to_find_more() {
        let entry: ModelEntry = serde_json::from_value(serde_json::json!({
            "id": "codex", "provider": "openai", "kind": "cli", "access": "subscription",
            "cost": {"unit": "plan_usage"},
        }))
        .unwrap();
        let listed = models_text(std::slice::from_ref(&entry), false);
        assert!(
            listed.starts_with("codex · openai · CLI · subscription\n"),
            "{listed}"
        );
        assert!(listed.contains("--discover"), "{listed}");
        let discovered = models_text(&[entry], true);
        assert_eq!(discovered, "codex · openai · CLI · subscription\n");
    }
}
