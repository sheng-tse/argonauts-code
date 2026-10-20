//! The built-in Hermes profile runs the fake `hermes` over ACP with a home of its own: its
//! `HERMES_HOME` is the one the task keeps for its worker (`keep = "task"`), made from the template
//! with its pinned keys, and nothing comes from or goes to your own `~/.hermes`. That the sandbox
//! denies reading it is `limits_acp_denies_forbidden_path`'s to show.

use crate::agents_support;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;

use agents_support::{
    assert_sessions_completed, files_under, finish_screen, invocations_of, launch_ready, scenario,
    session_dir, sessions_of, start_captain, task_home, toml_path, try_workers,
    wait_for_finished_task, write_file, write_global_config, write_global_models,
};
use argonauts_testkit::LogLine;

/// Stands in for the `hermes` binary: it prints the `HERMES_HOME` it was given on stderr,
/// which the session's raw stderr keeps, then runs the fake from `PATH`.
const WRAPPER: &str =
    "#!/bin/sh\nprintf 'HERMES_HOME=%s\\n' \"${HERMES_HOME-}\" >&2\nexec hermes \"$@\"\n";

const SENTINEL: &str = "sentinel-from-your-own-hermes";

#[test]
fn workers_hermes_profile_isolated_home() -> anyhow::Result<()> {
    let scenario = scenario("hermes-acp.toml")?;
    let wrapper = scenario.repo.scratch().join("wrappers/hermes");
    write_file(&wrapper, WRAPPER)?;
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755))?;

    // Your own Hermes home, which the session must never use.
    let your_hermes = scenario.repo.user_home().join(".hermes");
    write_file(
        &your_hermes.join("config.yaml"),
        &format!("model:\n  default: {SENTINEL}\nmemory:\n  memory_enabled: true\n"),
    )?;
    write_file(&your_hermes.join("memories/MEMORY.md"), SENTINEL)?;
    let before = files_under(&your_hermes)?;

    write_global_config(
        &scenario,
        &format!(
            "workflow = \"captain\"\n\
             crew.main = \"hermes/fake-local\"\n\n\
             [agents.hermes]\n\
             enabled = true\n\
             binary = {}\n",
            toml_path(&wrapper)
        ),
    )?;
    write_global_models(
        &scenario,
        "[[model]]\n\
         id = \"hermes/fake-local\"\n\
         provider = \"meta\"\n\
         kind = \"cli\"\n\
         access = \"local\"\n\
         tools = true\n\
         cost = { unit = \"none\" }\n",
    )?;

    let mut pty = launch_ready(&scenario, &[], &[])?;
    start_captain(&mut pty)?;
    let events = wait_for_finished_task(&scenario)?;

    let workers: Vec<String> = try_workers(&events)
        .iter()
        .map(|model| model.to_string())
        .collect();
    assert_eq!(workers, ["hermes/fake-local"]);
    assert_sessions_completed(&events, "hermes");

    let calls: Vec<_> = invocations_of(&scenario, "hermes")?
        .into_iter()
        .filter(|call| call.args.first().is_some_and(|arg| arg == "acp"))
        .collect();
    assert_eq!(calls.len(), 1, "{calls:?}");
    for name in ["HERMES_HOME", "HERMES_ACP_SKIP_CONFIGURED_MCP"] {
        assert!(calls[0].env.iter().any(|set| set == name), "{name} not set");
    }
    let reported = scenario.clis.log()?.into_iter().any(
        |line| matches!(line, LogLine::ReportCall { report, .. } if report["status"] == "done"),
    );
    assert!(
        reported,
        "the worker's report never reached argonauts_report"
    );

    // HERMES_HOME is the home the task keeps for its worker.
    let sessions = sessions_of(&events, "hermes");
    let key = sessions[0].0.expect("the worker session has a key");
    let home = task_home(&scenario, "task-001", "worker", "hermes")?;
    let seen = files_under(&session_dir(&scenario, key)?)?
        .into_iter()
        .flat_map(|(_, text)| {
            text.lines()
                .filter_map(|line| line.strip_prefix("HERMES_HOME="))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(seen, [home.display().to_string()]);

    // Made from the template, with its pinned keys, and nothing from your own.
    let home_text: String = files_under(&home)?
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    for key in [
        "fallback_providers: []",
        "adopt_external_logins: false",
        "memory_enabled: false",
        "user_profile_enabled: false",
        "backend: local",
    ] {
        assert!(home_text.contains(key), "the home lacks {key:?}");
    }
    assert!(
        !home_text.contains(SENTINEL),
        "the home copied your own Hermes files"
    );
    assert_eq!(
        files_under(&your_hermes)?,
        before,
        "your own ~/.hermes changed"
    );

    finish_screen(&scenario, &pty, "workers_hermes_profile_isolated_home")
}
