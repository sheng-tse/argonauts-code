//! Settings from the chat: every settings command through its picker and its argument, the
//! scope question, resets, effort levels, `/vim`, preset files, and where each change lands.

mod app_support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use anyhow::Context as _;
use app_support::*;
use argonauts_core::app::{AppEvent, ChoiceKind, UiCommand, ViewData};
use argonauts_core::names::ChoiceId;
use argonauts_core::settings::SettingScope;
use argonauts_scenarios::{Scenario, WAIT};
use argonauts_testkit::{Key, Pty};

const KEPT_COMMENT: &str = "# Written by the scenario test, kept through every change.";

/// A setting given as a command's argument, saved everywhere.
struct ArgumentCase {
    line: &'static str,
    key: &'static str,
    /// TOML text.
    value: &'static str,
}

const ARGUMENT_CASES: [ArgumentCase; 12] = [
    ArgumentCase {
        line: "/model reviewer codex",
        key: "roles.reviewer.workers",
        value: "[\"codex\"]",
    },
    ArgumentCase {
        line: "/effort high",
        key: "roles.default.effort",
        value: "\"high\"",
    },
    ArgumentCase {
        line: "/model codex",
        key: "default_agent",
        value: "\"codex\"",
    },
    ArgumentCase {
        line: "/config agents.codex.effort low",
        key: "agents.codex.effort",
        value: "\"low\"",
    },
    ArgumentCase {
        line: "/preset solo",
        key: "preset",
        value: "\"solo\"",
    },
    ArgumentCase {
        line: "/approvals merge.mode shadow",
        key: "approvals.merge.mode",
        value: "\"shadow\"",
    },
    ArgumentCase {
        line: "/budget per_card_usd 5",
        key: "budget.per_card_usd",
        value: "5",
    },
    ArgumentCase {
        line: "/handoff reviewer max_handoffs 2",
        key: "roles.reviewer.handoff.max_handoffs",
        value: "2",
    },
    ArgumentCase {
        line: "/permissions sandbox.network true",
        key: "sandbox.network",
        value: "true",
    },
    ArgumentCase {
        line: "/narrator on_demand",
        key: "ui.narrator",
        value: "\"on_demand\"",
    },
    ArgumentCase {
        line: "/theme light",
        key: "ui.theme",
        value: "\"light\"",
    },
    ArgumentCase {
        line: "/config intake.include_uncommitted true",
        key: "intake.include_uncommitted",
        value: "true",
    },
];

// `/agent` became `/model`.
const SETTINGS_COMMANDS: [&str; 11] = [
    "model",
    "effort",
    "agents",
    "preset",
    "approvals",
    "budget",
    "handoff",
    "permissions",
    "narrator",
    "theme",
    "config",
];

/// Waits for the next `SettingChanged`, confirming a safety change on the way when asked.
fn next_change(
    scenario: &Scenario,
    pty: &mut Pty,
    changes_before: usize,
) -> anyhow::Result<(String, String)> {
    let mut confirmed: Vec<ChoiceId> = Vec::new();
    wait_until("the setting to change", || {
        if let Some((key, value, _)) = setting_changes(scenario)?.into_iter().nth(changes_before) {
            return Ok(Some((key, value)));
        }
        let confirm = open_choices(scenario)?
            .into_iter()
            .find(|c| c.kind == ChoiceKind::ConfirmSafetySetting && !confirmed.contains(&c.id));
        if let Some(confirm) = confirm {
            answer_card(pty, number_of(&confirm.options, 'y'))?;
            confirmed.push(confirm.id);
        }
        Ok(None)
    })
}

/// Waits until the picker has the rows it asked for and holds still.
fn settled(pty: &Pty) -> anyhow::Result<()> {
    let mut last = String::new();
    wait_until("the picker to settle", || {
        thread::sleep(Duration::from_millis(100));
        let now = pty.screen().visible_text();
        let still = now == last && !now.contains("Asking which models fit");
        last = now;
        Ok(still.then_some(()))
    })
}

/// Whether the picker shows a CLI's models, where the pointed one ends the choice. Its about
/// line reads `The model the default agent runs on Codex`.
fn on_models_level(pty: &Pty) -> bool {
    let about = regex::Regex::new(r"The model .+ runs on ").expect("a valid pattern");
    about.is_match(&pty.screen().visible_text())
}

/// A picker for `command` answered, whatever shape its rows take.
fn is_picker_for(command: &str) -> impl Fn(&AppEvent) -> bool + '_ {
    move |event| match event {
        AppEvent::View { data } => match data.as_ref() {
            ViewData::Settings {
                command: Some(c), ..
            } => c == command,
            // `/theme` previews each palette, so the screen asks for the `ui` rows itself.
            ViewData::Settings {
                command: None,
                section: Some(section),
                ..
            } => command == "theme" && section == "ui",
            ViewData::Models { .. } | ViewData::Approvals { .. } => {
                matches!(command, "model" | "agent" | "approvals")
            }
            // `/effort` opens a slider where the model's levels are known.
            ViewData::Effort { .. } => command == "effort",
            _ => false,
        },
        _ => false,
    }
}

fn scope_changes(scenario: &Scenario) -> anyhow::Result<Vec<(String, SettingScope)>> {
    Ok(events(scenario)?
        .into_iter()
        .filter_map(|event| match event {
            AppEvent::SettingChanged { key, scope, .. } => Some((key, scope)),
            _ => None,
        })
        .collect())
}

/// The line a change leaves: what `/model` and `/effort` set reads like Claude Code's
/// `⎿  Set model to Opus 5.5`, and any other key by name.
fn changed_line(key: &str) -> String {
    match key {
        "default_agent" => "⎿  Set model to".to_owned(),
        "roles.default.effort" => "⎿  Set effort to".to_owned(),
        "roles.reviewer.workers" => "⎿  Set reviewer's model to".to_owned(),
        key => format!("● Set {}", regex::escape(key)),
    }
}

fn snapshot(files: &[PathBuf]) -> Vec<Option<String>> {
    files
        .iter()
        .map(|file| std::fs::read_to_string(file).ok())
        .collect()
}

#[test]
fn app_settings_commands_picker_and_argument() -> anyhow::Result<()> {
    // Auto is the goal's default and the last of its values, so start it supervised for the
    // /approvals picker to have another value below it.
    let config = format!(
        "{}\n[approvals.goal]\nmode = \"supervised\"\n",
        run_config()
    );
    let scenario = scenario_with(&config)?;
    let global = scenario.repo.global_config();
    let mut pty = start(&scenario)?;
    let mut last: BTreeMap<String, String> = BTreeMap::new();

    for case in &ARGUMENT_CASES {
        let before = setting_changes(&scenario)?.len();
        submit(&mut pty, case.line)?;
        answer_scope(&scenario, &mut pty, 'e', None)
            .with_context(|| format!("{}:\n{}", case.line, pty.screen().text()))?;
        let (key, value) = next_change(&scenario, &mut pty, before)
            .with_context(|| format!("{}:\n{}", case.line, pty.screen().text()))?;
        assert_eq!(key, case.key, "{}", case.line);
        assert!(same_toml(&value, case.value), "{}: {value}", case.line);
        let text = read(&global)?;
        assert!(
            text.contains(KEPT_COMMENT),
            "{} lost a comment:\n{text}",
            case.line
        );
        let entry = toml_entries(&text)
            .into_iter()
            .rev()
            .find(|e| e.key == case.key)
            .unwrap_or_else(|| panic!("{} wasn't written:\n{text}", case.key));
        assert!(same_toml(&entry.value, case.value), "{text}");
        assert!(entry.commented, "{} has no comment:\n{text}", case.key);
        pty.wait_for(&changed_line(case.key), WAIT)?;
        last.insert(key, value);
    }

    for command in SETTINGS_COMMANDS {
        let before = setting_changes(&scenario)?.len();
        let commands_before = commands(&scenario)?.len();
        let pickers_before = events(&scenario)?
            .iter()
            .filter(|event| is_picker_for(command)(event))
            .count();
        submit(&mut pty, &format!("/{command}"))?;
        let pickers = wait_events(
            &scenario,
            &format!("the /{command} picker"),
            pickers_before + 1,
            is_picker_for(command),
        )
        .with_context(|| pty.screen().text())?;
        let slider = matches!(
            pickers.last(),
            Some(AppEvent::View { data }) if matches!(data.as_ref(), ViewData::Effort { .. })
        );
        // The first row, another value, then where to save it. `/model` opens a level down, on
        // the default agent's CLIs, so there it's another CLI, and `/effort`'s slider moves to
        // another level. Model lists come a moment later, so each key waits for the rows before
        // it.
        let keys: &[Key] = if command == "model" {
            &[Key::Down, Key::Enter]
        } else if slider {
            &[Key::Right, Key::Enter]
        } else {
            &[Key::Enter, Key::Down, Key::Enter]
        };
        for key in keys {
            settled(&pty)?;
            pty.press(*key)?;
        }
        settled(&pty)?;
        // From a CLI, `/model` goes on to its models, where the pointed one ends the choice.
        if on_models_level(&pty) {
            pty.press(Key::Enter)?;
            settled(&pty)?;
        }
        // Where to save it: the app asks, the picker lists the places, or nothing is asked for
        // a reset, which leaves the layer the value was set in.
        let asked = wait_until("where to save it", || {
            if setting_changes(&scenario)?.len() > before {
                return Ok(Some(None));
            }
            let by_app = open_choices(&scenario)?
                .iter()
                .any(|c| c.kind == ChoiceKind::SettingScope);
            if by_app {
                return Ok(Some(Some(true)));
            }
            let listed = pty.screen().visible_text().contains("Just this session");
            Ok(listed.then_some(Some(false)))
        })
        .with_context(|| format!("/{command}:\n{}", pty.screen().text()))?;
        match asked {
            Some(true) => answer_scope(&scenario, &mut pty, 'e', None)?,
            // Everywhere, the fourth place to save it.
            Some(false) => pty.press(Key::Char('4'))?,
            None => {}
        }
        // coordinated-crew waits for who coordinates: the first CLI, at its pointed model.
        let coordinator = wait_until("the change or who coordinates", || {
            let confirm = open_choices(&scenario)?
                .iter()
                .any(|c| c.kind == ChoiceKind::ConfirmSafetySetting);
            if confirm || setting_changes(&scenario)?.len() > before {
                return Ok(Some(false));
            }
            let asked = pty
                .screen()
                .visible_text()
                .contains("Model for coordinator");
            Ok(asked.then_some(true))
        })
        .with_context(|| format!("/{command}:\n{}", pty.screen().text()))?;
        if coordinator {
            settled(&pty)?;
            pty.press(Key::Enter)?;
            settled(&pty)?;
            if on_models_level(&pty) {
                pty.press(Key::Enter)?;
            }
        }
        let (key, value) = next_change(&scenario, &mut pty, before)
            .with_context(|| format!("/{command}:\n{}", pty.screen().text()))?;
        let sent_by_picker = commands(&scenario)?
            .into_iter()
            .skip(commands_before)
            .any(|c| matches!(c, UiCommand::SetSetting { .. } | UiCommand::Slash { .. }));
        assert!(sent_by_picker, "/{command}'s picker sent nothing");
        let text = read(&global)?;
        assert!(
            text.contains(KEPT_COMMENT),
            "/{command} lost a comment:\n{text}"
        );
        // Another value can be the picker's Reset, which reads `● Reset key` for every key.
        pty.wait_for(
            &format!("{}|● Reset {}", changed_line(&key), regex::escape(&key)),
            WAIT,
        )?;
        // A full-screen view such as /config stays open after an edit; Esc goes back to the chat.
        if !pty.screen().visible_text().contains("shift+tab to cycle") {
            pty.press(Key::Esc)?;
        }
        last.insert(key, value);
    }

    // The next run records every value.
    let run_dir = start_solo(&scenario, &mut pty, requests::SMALL_FIX)?;
    let settings = wait_until("the run's settings", || Ok(run_settings(&run_dir).ok()))?;
    for (key, value) in &last {
        let Ok(expected) = serde_json::from_str::<serde_json::Value>(value) else {
            continue;
        };
        // `settings.json` keeps the resolved settings under `settings`, beside their hash. A
        // dollar amount written as 5 resolves to 5.0.
        let recorded = json_at(&settings, &format!("settings.{key}"));
        let same = match (recorded.and_then(|v| v.as_f64()), expected.as_f64()) {
            (Some(recorded), Some(expected)) => recorded == expected,
            _ => recorded == Some(&expected),
        };
        assert!(
            same,
            "{key} in the run's settings: {recorded:?}, not {expected}"
        );
    }
    scenario.shot(&pty, "app_settings_commands_picker_and_argument")?;
    Ok(())
}

#[test]
fn app_effort_offers_only_supported_levels() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;

    submit(&mut pty, "/effort")?;
    let picker = wait_event(&scenario, "the /effort picker", is_picker_for("effort"))?;
    let AppEvent::View { data } = picker else {
        unreachable!("matched a view")
    };
    // A slider over the default agent's model's levels, or its row where those aren't known.
    let levels: Vec<String> = match *data {
        ViewData::Effort { slider } => slider.stops.into_iter().map(|stop| stop.level).collect(),
        ViewData::Settings { rows, .. } => rows
            .into_iter()
            .find(|row| row.key == "roles.default.effort")
            .expect("no row for the default agent's effort")
            .allowed
            .iter()
            .map(|level| level.trim_matches('"').to_owned())
            .collect(),
        _ => panic!("/effort answered with another view"),
    };
    assert!(!levels.is_empty());
    let mut unique = levels.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), levels.len(), "{levels:?}");
    assert!(!levels.iter().any(|level| level.contains("turbo")));
    pty.press(Key::Esc)?;
    pty.wait_for("Kept", WAIT)?;

    let changes_before = setting_changes(&scenario)?.len();
    submit(&mut pty, "/effort turbo")?;
    // The prompt refuses a level it doesn't know before the app sees it, on the mode line.
    let refusal = wait_until("the refusal", || {
        Ok(pty
            .screen()
            .visible_text()
            .lines()
            .find(|line| line.contains("turbo") && !line.trim_start().starts_with('❯'))
            .map(str::to_owned))
    })?;
    for level in &levels {
        assert!(
            refusal.contains(level.as_str()),
            "{refusal} doesn't name {level}"
        );
    }
    assert!(
        !open_choices(&scenario)?
            .iter()
            .any(|c| c.kind == ChoiceKind::SettingScope),
        "an unsupported level got the scope question"
    );
    assert_eq!(setting_changes(&scenario)?.len(), changes_before);
    scenario.shot(&pty, "app_effort_offers_only_supported_levels")?;
    Ok(())
}

/// All hands from `/effort`: Tab flips it beside the slider, Enter asks where to
/// save it, and only `triage.all_hands` is written. The input rule's label then shows it.
#[test]
fn app_effort_tab_turns_all_hands_on() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let local = project_local_config(&scenario);
    let mut pty = start(&scenario)?;

    submit(&mut pty, "/effort")?;
    pty.wait_for("All hands  off", WAIT)?;
    pty.press(Key::Tab)?;
    pty.wait_for("All hands: every task goes to the crew", WAIT)?;
    scenario.shot(&pty, "app_effort_tab_turns_all_hands_on_slider")?;
    pty.press(Key::Enter)?;
    answer_scope(&scenario, &mut pty, 'm', None)?;
    let value = wait_setting_changed(&scenario, "triage.all_hands", 1)?;
    assert!(same_toml(&value, "true"), "{value}");
    let file = read(&local)?;
    assert!(
        same_toml(
            &toml_value(&file, "triage.all_hands").unwrap_or_default(),
            "true"
        ),
        "{file}"
    );
    assert!(
        !file.contains("effort"),
        "the level it left alone stays unset:\n{file}"
    );
    pty.wait_for("Turned All hands on · just you", WAIT)?;
    pty.wait_for("─ peer-crew · all hands ─", WAIT)?;
    scenario.shot(&pty, "app_effort_tab_turns_all_hands_on")?;
    Ok(())
}

/// Haiku takes no effort setting, so `/effort` has no slider for it, yet All hands is still
/// there: Tab flips it and Enter saves it.
#[test]
fn app_effort_offers_all_hands_on_a_model_without_levels() -> anyhow::Result<()> {
    let config = GLOBAL_CONFIG.replace(
        "default_agent = \"claude-code\"",
        "default_agent = \"claude-code/haiku\"",
    );
    let scenario = scenario_with(&config)?;
    let mut pty = start(&scenario)?;

    submit(&mut pty, "/effort")?;
    pty.wait_for("Haiku.* takes no effort setting", WAIT)?;
    pty.wait_for("All hands  off · Tab to toggle", WAIT)?;
    pty.press(Key::Tab)?;
    pty.wait_for("All hands: every task goes to the crew", WAIT)?;
    scenario.shot(
        &pty,
        "app_effort_offers_all_hands_on_a_model_without_levels",
    )?;
    pty.press(Key::Enter)?;
    answer_scope(&scenario, &mut pty, 'm', None)?;
    let value = wait_setting_changed(&scenario, "triage.all_hands", 1)?;
    assert!(same_toml(&value, "true"), "{value}");
    pty.wait_for("Turned All hands on · just you", WAIT)?;
    Ok(())
}

#[test]
fn app_setting_reset_falls_back_a_layer() -> anyhow::Result<()> {
    let scenario = scenario_with(&format!(
        "{GLOBAL_CONFIG}\n[roles.default]\neffort = \"low\"\n"
    ))?;
    scenario.repo.write(
        ".argonauts/config.local.toml",
        "# keep this comment\n[roles.default]\neffort = \"high\"\n\n# and this one\n[ui]\ntheme = \"dark\"\n",
    )?;
    let local = project_local_config(&scenario);
    let mut pty = start(&scenario)?;

    submit(&mut pty, "/config roles.default.effort --reset")?;
    let now = wait_until("the first reset", || {
        Ok(events(&scenario)?
            .into_iter()
            .find_map(|event| match event {
                AppEvent::SettingChanged {
                    key,
                    value,
                    reset: true,
                    ..
                } if key == "roles.default.effort" => Some(value),
                _ => None,
            }))
    })?;
    assert!(
        same_toml(&now, "\"low\""),
        "the global value didn't show through: {now}"
    );
    let text = read(&local)?;
    assert!(
        toml_value(&text, "roles.default.effort").is_none(),
        "{text}"
    );
    assert!(
        text.contains("# keep this comment") && text.contains("# and this one"),
        "{text}"
    );
    assert!(toml_value(&text, "ui.theme").is_some(), "{text}");
    pty.wait_for(r"● Reset roles\.default\.effort · just you · now", WAIT)?;

    submit(&mut pty, "/config roles.default.effort --reset")?;
    wait_until("the second reset", || {
        let resets = events(&scenario)?
            .into_iter()
            .filter(|event| {
                matches!(event, AppEvent::SettingChanged { key, reset: true, .. }
                    if key == "roles.default.effort")
            })
            .count();
        Ok((resets >= 2).then_some(()))
    })?;
    let global = read(&scenario.repo.global_config())?;
    assert!(
        toml_value(&global, "roles.default.effort").is_none(),
        "{global}"
    );
    assert!(global.contains(KEPT_COMMENT), "{global}");
    pty.wait_for(r"● Reset roles\.default\.effort · everywhere · now", WAIT)?;
    scenario.shot(&pty, "app_setting_reset_falls_back_a_layer")?;
    Ok(())
}

#[test]
fn app_setting_applies_to_next_chat_turn() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let mut pty = start(&scenario)?;
    submit(&mut pty, requests::HI)?;
    wait_turns_finished(&scenario, 1)?;

    submit(&mut pty, "/effort high")?;
    answer_scope(&scenario, &mut pty, 's', None)?;
    wait_setting_changed(&scenario, "roles.default.effort", 1)?;
    submit(&mut pty, "tell me more")?;
    wait_turns_finished(&scenario, 2)?;

    let turns = claude_sessions(&scenario, "dontAsk")?;
    assert_eq!(turns.len(), 2, "{turns:?}");
    assert!(!has_flag(&turns[0].args, "--effort", "high"));
    assert!(
        has_flag(&turns[1].args, "--effort", "high"),
        "the next turn ran without the change: {:?}",
        turns[1].args
    );
    scenario.shot(&pty, "app_setting_applies_to_next_chat_turn")?;
    Ok(())
}

#[test]
fn app_setting_scope_session_project_global() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let global = scenario.repo.global_config();
    let project = project_config(&scenario);
    let local = project_local_config(&scenario);
    let key = "roles.default.effort";
    let mut pty = start(&scenario)?;

    // This project, just me: the local file, kept out of git.
    submit(&mut pty, "/effort medium")?;
    answer_scope(&scenario, &mut pty, 'm', None)?;
    wait_setting_changed(&scenario, key, 1)?;
    assert!(same_toml(
        &toml_value(&read(&local)?, key).unwrap_or_default(),
        "\"medium\""
    ));
    let exclude = read(&scenario.repo.root().join(".git/info/exclude"))?;
    assert!(exclude.contains("config.local.toml"), "{exclude}");

    // This project, shared.
    submit(&mut pty, "/effort low")?;
    answer_scope(&scenario, &mut pty, 'p', None)?;
    wait_setting_changed(&scenario, key, 2)?;
    assert!(same_toml(
        &toml_value(&read(&project)?, key).unwrap_or_default(),
        "\"low\""
    ));

    // Everywhere, while the project files set it: the question says so.
    submit(&mut pty, "/effort high")?;
    let question = wait_open_choice(&scenario, ChoiceKind::SettingScope)?;
    let said = format!(
        "{}\n{}\n{}",
        question.title,
        question.body.join("\n"),
        question
            .options
            .iter()
            .filter_map(|o| o.detail.clone())
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(said.contains("won't change this repo"), "{said}");
    answer_card(&mut pty, number_of(&question.options, 'e'))?;
    wait_choice_closed(&scenario, &question.id)?;
    wait_setting_changed(&scenario, key, 3)?;
    assert!(same_toml(
        &toml_value(&read(&global)?, key).unwrap_or_default(),
        "\"high\""
    ));

    // Just this session: every file stays as it is.
    let files = [global, project, local];
    let before = snapshot(&files);
    submit(&mut pty, "/effort low")?;
    answer_scope(&scenario, &mut pty, 's', None)?;
    wait_setting_changed(&scenario, key, 4)?;
    assert_eq!(snapshot(&files), before, "a session change wrote a file");
    let scopes: Vec<SettingScope> = scope_changes(&scenario)?
        .into_iter()
        .filter(|(changed, _)| changed == key)
        .map(|(_, scope)| scope)
        .collect();
    assert_eq!(
        scopes,
        [
            SettingScope::ProjectLocal,
            SettingScope::Project,
            SettingScope::Global,
            SettingScope::Session
        ]
    );
    scenario.shot(&pty, "app_setting_scope_session_project_global")?;
    Ok(())
}

#[test]
fn app_setting_keeps_comments_and_logs_line() -> anyhow::Result<()> {
    let config = format!(
        "# Mine, above everything.\n{}# Mine, at the end.\n",
        run_config()
    );
    let scenario = scenario_with(&config)?;
    let global = scenario.repo.global_config();
    let comments: Vec<String> = config
        .lines()
        .filter(|line| line.trim_start().starts_with('#'))
        .map(str::to_owned)
        .collect();
    let mut pty = start(&scenario)?;

    submit(&mut pty, "/theme light")?;
    answer_scope(&scenario, &mut pty, 'e', None)?;
    wait_setting_changed(&scenario, "ui.theme", 1)?;
    let text = read(&global)?;
    for comment in &comments {
        assert!(text.contains(comment.as_str()), "lost {comment:?}:\n{text}");
    }
    let entry = toml_entries(&text)
        .into_iter()
        .find(|e| e.key == "ui.theme")
        .expect("ui.theme wasn't written");
    assert!(entry.commented, "the new key has no description:\n{text}");
    pty.wait_for(r#"● Set ui\.theme = "?light"? · everywhere · was"#, WAIT)?;

    let run_dir = start_solo(&scenario, &mut pty, requests::SMALL_FIX)?;
    let settings = wait_until("the run's settings", || Ok(run_settings(&run_dir).ok()))?;
    assert_eq!(
        json_at(&settings, "settings.ui.theme"),
        Some(&serde_json::json!("light")),
        "the new run didn't record the value"
    );
    scenario.shot(&pty, "app_setting_keeps_comments_and_logs_line")?;
    Ok(())
}

#[test]
fn app_vim_saved_in_global_config() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let global = scenario.repo.global_config();
    let mut pty = start(&scenario)?;
    let [on, off] = toggle_vim_on_and_off(&scenario, &mut pty)?;
    assert!(same_toml(&on, "\"vim\""), "{on}");
    assert!(same_toml(&off, "\"default\""), "{off}");
    let scopes: Vec<SettingScope> = scope_changes(&scenario)?
        .into_iter()
        .filter(|(key, _)| key == "ui.keys")
        .map(|(_, scope)| scope)
        .collect();
    assert_eq!(scopes, [SettingScope::Global, SettingScope::Global]);
    let text = read(&global)?;
    assert!(same_toml(
        &toml_value(&text, "ui.keys").unwrap_or_default(),
        "\"default\""
    ));
    assert!(text.contains(KEPT_COMMENT), "{text}");
    let vim_flags: Vec<bool> = events(&scenario)?
        .into_iter()
        .filter_map(|event| match event {
            AppEvent::UiSettings { vim, .. } => Some(vim),
            _ => None,
        })
        .collect();
    assert!(vim_flags.ends_with(&[true, false]), "{vim_flags:?}");
    scenario.shot(&pty, "app_vim_saved_in_global_config")?;
    quit(&mut pty)?;

    // A project file that sets the key wins over everywhere, so the change stays in the
    // session and a notice says why.
    let scenario = scenario_with(GLOBAL_CONFIG)?;
    scenario
        .repo
        .write(".argonauts/config.toml", "[ui]\nkeys = \"default\"\n")?;
    let global_before = read(&scenario.repo.global_config())?;
    let mut pty = start(&scenario)?;
    toggle_vim_on_and_off(&scenario, &mut pty)?;
    let scopes: Vec<SettingScope> = scope_changes(&scenario)?
        .into_iter()
        .filter(|(key, _)| key == "ui.keys")
        .map(|(_, scope)| scope)
        .collect();
    assert_eq!(scopes, [SettingScope::Session, SettingScope::Session]);
    assert_eq!(read(&scenario.repo.global_config())?, global_before);
    assert!(
        notices(&scenario)?
            .iter()
            .any(|n| n.contains("ui.keys") || n.contains("this session")),
        "no notice said why the change stays in the session"
    );
    scenario.shot(&pty, "app_vim_saved_in_global_config_project")?;
    Ok(())
}

#[test]
fn app_preset_show_copy_diff() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let copy = scenario.repo.home().join("presets").join("peer-crew.toml");
    let mut pty = start(&scenario)?;

    // peer-crew sets no key, so what shows is the protocol, commented out at its defaults.
    submit(&mut pty, "/preset show peer-crew")?;
    let shown = pty.wait_for(r"\[critique\]", WAIT)?.text();
    assert!(shown.contains("# rounds = "), "{shown}");

    submit(&mut pty, "/preset copy peer-crew")?;
    wait_until("the preset file", || Ok(copy.exists().then_some(())))?;
    let mut text = read(&copy)?;
    assert!(
        text.lines().any(|l| l.trim_start().starts_with('#')),
        "{text}"
    );
    text.push_str("# my change\n");
    std::fs::write(&copy, &text)?;

    let notices_before = notices(&scenario)?.len();
    submit(&mut pty, "/preset copy peer-crew")?;
    let refusal = wait_until("the refusal", || {
        Ok(notices(&scenario)?.into_iter().nth(notices_before))
    })?;
    assert!(refusal.contains("/preset diff peer-crew"), "{refusal}");
    assert!(refusal.to_lowercase().contains("delete"), "{refusal}");
    assert_eq!(read(&copy)?, text, "copy replaced an edited file");

    submit(&mut pty, "/preset diff peer-crew")?;
    pty.wait_for("my change", WAIT)?;
    scenario.shot(&pty, "app_preset_show_copy_diff")?;
    pty.press(Key::Char('q'))?;
    pty.press(Key::Esc)?;
    check_mode_line(&pty)?;
    Ok(())
}

/// The workflow library the trace saw last: each workflow's name, and which one is in use.
fn library(scenario: &Scenario, count: usize) -> anyhow::Result<Vec<(String, bool)>> {
    let is_library = |event: &AppEvent| matches!(event, AppEvent::View { data } if matches!(**data, ViewData::WorkflowLibrary { .. }));
    let shown = wait_events(scenario, "the workflow library", count, is_library)?;
    match shown.last() {
        Some(AppEvent::View { data }) => match data.as_ref() {
            ViewData::WorkflowLibrary { workflows, .. } => Ok(workflows
                .iter()
                .map(|file| (file.name.clone(), file.in_use))
                .collect()),
            _ => unreachable!("matched the library"),
        },
        _ => anyhow::bail!("no workflow library"),
    }
}

/// Moves the library's pick from the one in use, where it opens, to `name`.
fn pick_in_library(pty: &mut Pty, rows: &[(String, bool)], name: &str) -> anyhow::Result<()> {
    let at = rows.iter().position(|(row, _)| row == name);
    let in_use = rows.iter().position(|(_, used)| *used);
    let (Some(at), Some(in_use)) = (at, in_use) else {
        anyhow::bail!("no {name}, or none in use: {rows:?}");
    };
    let key = if at > in_use { Key::Down } else { Key::Up };
    for _ in 0..at.abs_diff(in_use) {
        pty.press(key)?;
    }
    Ok(())
}

#[test]
fn app_workflows_library_shows_copies_diffs() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let copy = scenario.repo.home().join("workflows").join("solo.yaml");
    let project_copy = scenario.repo.root().join(".argonauts/workflows/solo.yaml");
    let mut pty = start(&scenario)?;

    // With no run on screen, /workflows opens the Library tab alone, on the one in use.
    submit(&mut pty, "/workflows")?;
    let rows = library(&scenario, 1)?;
    pty.wait_for("Workflow library", WAIT)?;
    pick_in_library(&mut pty, &rows, "solo")?;

    pty.press(Key::Enter)?;
    pty.wait_for("phases:", WAIT)?;
    // A key sent with the Esc would read as Alt and that key, so wait for the library.
    pty.press(Key::Esc)?;
    pty.wait_for("Workflow library", WAIT)?;

    pty.press(Key::Char('c'))?;
    wait_until("the workflow file", || Ok(copy.exists().then_some(())))?;
    library(&scenario, 2)?;
    let mut text = read(&copy)?;
    assert!(text.contains("phases:"), "{text}");
    text.push_str("# my change\n");
    std::fs::write(&copy, &text)?;

    let notices_before = notices(&scenario)?.len();
    pty.press(Key::Char('c'))?;
    let refusal = wait_until("the refusal", || {
        Ok(notices(&scenario)?.into_iter().nth(notices_before))
    })?;
    assert!(
        refusal.contains("diff solo in the Library tab of /workflows"),
        "{refusal}"
    );
    assert_eq!(read(&copy)?, text, "copy replaced an edited file");
    library(&scenario, 3)?;

    pty.press(Key::Char('d'))?;
    pty.wait_for("my change", WAIT)?;
    scenario.shot(&pty, "app_workflows_library_shows_copies_diffs")?;
    pty.press(Key::Char('q'))?;

    // A copy in the repo wins over yours from then on.
    pty.wait_for("Workflow library", WAIT)?;
    pty.press(Key::Char('C'))?;
    wait_until("the repo's workflow file", || {
        Ok(project_copy.exists().then_some(()))
    })?;
    library(&scenario, 4)?;
    let mut shared = read(&project_copy)?;
    assert!(!shared.contains("# my change"), "{shared}");
    shared.push_str("# the team's change\n");
    std::fs::write(&project_copy, &shared)?;
    pty.press(Key::Esc)?;
    check_mode_line(&pty)?;
    submit(&mut pty, "/workflows")?;
    let rows = library(&scenario, 5)?;
    pty.wait_for("Workflow library", WAIT)?;
    pick_in_library(&mut pty, &rows, "solo")?;
    pty.press(Key::Enter)?;
    // The file runs past one page, and the team's line is its last.
    pty.wait_for("solo · ", WAIT)?;
    pty.press(Key::End)?;
    pty.wait_for("# the team's change", WAIT)?;
    pty.press(Key::Esc)?;
    pty.wait_for("Workflow library", WAIT)?;
    pty.press(Key::Esc)?;
    check_mode_line(&pty)?;
    Ok(())
}
