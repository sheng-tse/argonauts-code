//! `/config all` lists every key with the layer that set it: the built-in default, your
//! settings everywhere, this project's, or a `--set` flag.

use crate::agents_support;
use agents_support::{IDLE, launch_ready, scenario, write_global_config, write_project_config};
use argonauts_core::app::{AppEvent, SettingRow, ViewData};
use argonauts_scenarios::WAIT;
use argonauts_testkit::{Key, Pty};

/// The settings view's line under its title.
const VIEW_TITLE: &str = "Every setting and the layer it comes from";

/// Waits until the settings view is off the screen.
fn wait_closed(pty: &Pty) -> anyhow::Result<()> {
    let deadline = argonauts_testkit::pty::deadline::after(WAIT);
    while pty
        .screen()
        .visible
        .iter()
        .any(|row| row.contains(VIEW_TITLE))
    {
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "the settings view never closed"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Ok(())
}

#[test]
fn workers_config_shows_layers() -> anyhow::Result<()> {
    let scenario = scenario("config-layers.toml")?;
    write_global_config(&scenario, "crew.main = \"codex\"\n")?;
    write_project_config(&scenario, "[review]\nper_task = 2\n")?;
    let mut pty = launch_ready(&scenario, &["--set", "review.max_change_rounds=3"], &[])?;

    // `/config all` answers with every editable row, each with the layer that set it; a bare
    // `/config` opens Claude Code's shorter list on the Config tab.
    let is_config = |data: &ViewData| matches!(data, ViewData::Settings { command: Some(command), .. } if command == "config");
    pty.send("/config all\r")?;
    let event = scenario.trace.wait_for_event(
        |event| matches!(event, AppEvent::View { data } if is_config(data)),
        WAIT,
    )?;
    let AppEvent::View { data } = event else {
        unreachable!("the wait matched a view");
    };
    let ViewData::Settings { rows: entries, .. } = *data else {
        unreachable!("the wait matched the config view");
    };
    let entry = |key: &str| -> &SettingRow {
        entries
            .iter()
            .find(|entry| entry.key == key)
            .unwrap_or_else(|| panic!("/config doesn't list {key}"))
    };
    for (key, value, layer) in [
        ("crew.main", "\"codex\"", "global"),
        ("review.per_task", "2", "project"),
        ("review.max_change_rounds", "3", "flag"),
        ("review.blind", "false", "built-in"),
    ] {
        let entry = entry(key);
        assert_eq!(entry.value, value, "{entry:?}");
        assert_eq!(entry.layer, layer, "{entry:?}");
    }
    let mut keys: Vec<&str> = entries.iter().map(|entry| entry.key.as_str()).collect();
    let listed = keys.len();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), listed, "a key is listed twice");

    // The view opens after the app answers, so Esc waits for it. What follows Esc waits for
    // the view to close and the prompt to show, or the terminal reads Esc and `/` together as
    // one Alt key. The prompt's hint is in the scrollback from before, so the view going is
    // what tells.
    pty.wait_for(VIEW_TITLE, WAIT)?;
    // On screen each layer reads in words, whatever order the list is in.
    for (key, label) in [
        ("crew.main", "everywhere"),
        ("review.per_task", "this project"),
        ("review.max_change_rounds", "--set"),
        ("review.blind", "built-in"),
    ] {
        pty.press(Key::Esc)?;
        wait_closed(&pty)?;
        pty.wait_for(IDLE, WAIT)?;
        pty.send(&format!("/config {key}\r"))?;
        pty.wait_for(
            &format!("{}.*{}", regex::escape(key), regex::escape(label)),
            WAIT,
        )?;
    }
    scenario.shot(&pty, "workers_config_shows_layers")?;
    Ok(())
}
