//! A `final-json` CLI entry, the fake installed as `openclaw`, owns a Captain task. Its one
//! JSON document at exit is read through the entry's pointers, and its config comes from a
//! home template copied fresh into the session.

use crate::agents_support;
use agents_support::{
    assert_sessions_completed, finish_screen, invocations_of, launch_ready, paths, scenario,
    session_end, session_home, sessions_of, start_captain, toml_path, try_workers,
    wait_for_finished_task, write_file, write_global_config, write_global_models,
};
use argonauts_core::layout::user_files;

const TEMPLATE: &str = "// From the test's openclaw-exec template.\n{ agents: { defaults: { runtime: \"embedded\" } } }\n";

#[test]
fn workers_final_json_cli_owns_task() -> anyhow::Result<()> {
    let scenario = scenario("openclaw-final-json.toml")?;
    let templates = paths(&scenario).home().join(user_files::CLIS);
    for file in ["build.json5", "read-only.json5"] {
        write_file(&templates.join("openclaw-exec").join(file), TEMPLATE)?;
    }
    let binary = scenario.clis.bin_dir().join("openclaw");
    write_global_models(
        &scenario,
        &format!(
            "[[cli]]\n\
             id = \"openclaw-exec\"\n\
             display_name = \"OpenClaw one-shot\"\n\
             binary = {}\n\
             version_args = [\"--version\"]\n\
             provider = \"from-model\"\n\
             prompt = \"stdin\"\n\
             output = \"final-json\"\n\
             read_only_args = [\"agent\", \"exec\", \"--message-file\", \"-\", \"--cwd\", \"{{workdir}}\", \
             \"--config\", \"{{home}}/read-only.json5\", \"--model\", \"{{model}}\", \"--json\"]\n\
             build_args = [\"agent\", \"exec\", \"--message-file\", \"-\", \"--cwd\", \"{{workdir}}\", \
             \"--config\", \"{{home}}/build.json5\", \"--model\", \"{{model}}\", \"--json\"]\n\
             env_set = {{ OPENCLAW_NO_AUTO_UPDATE = \"1\" }}\n\
             home = {{ env = \"OPENCLAW_HOME\", template = \"openclaw-exec\", keep = \"session\" }}\n\n\
             [cli.final_json]\n\
             text = \"/final\"\n\
             status = \"/status\"\n\
             ok_status = [\"ok\"]\n\
             session_id = \"/sessionId\"\n\
             model = \"/model\"\n\
             input_tokens = \"/usage/input\"\n\
             output_tokens = \"/usage/output\"\n\
             cost_usd = \"/costUsd\"\n\
             error_kind = \"/error/kind\"\n\
             error_message = \"/error/message\"\n\n\
             [[model]]\n\
             id = \"openclaw-exec/fake-local\"\n\
             provider = \"meta\"\n\
             kind = \"cli\"\n\
             access = \"local\"\n\
             tools = true\n\
             cost = {{ unit = \"none\" }}\n",
            toml_path(&binary)
        ),
    )?;
    write_global_config(
        &scenario,
        "workflow = \"captain\"\ncrew.main = \"openclaw-exec/fake-local\"\n",
    )?;

    let mut pty = launch_ready(&scenario, &[], &[])?;
    start_captain(&mut pty)?;
    let events = wait_for_finished_task(&scenario)?;

    let workers: Vec<String> = try_workers(&events)
        .iter()
        .map(|model| model.to_string())
        .collect();
    assert_eq!(workers, ["openclaw-exec/fake-local"]);
    assert_sessions_completed(&events, "openclaw-exec");

    // The pointers read the document printed at exit.
    let sessions = sessions_of(&events, "openclaw-exec");
    let key = sessions[0].0.expect("the worker session has a key");
    let end = session_end(&events, key).expect("the worker session ended");
    assert_eq!(end.cli_session.as_deref(), Some("oc-workers-1"));
    assert!(
        end.result_text
            .as_deref()
            .is_some_and(|text| text.contains("Created hello.txt")),
        "{end:?}"
    );

    // The run used a fresh home made from the template, inside the session's folder.
    let home = session_home(&scenario, key)?;
    assert_eq!(std::fs::read_to_string(home.join("build.json5"))?, TEMPLATE);
    let calls = invocations_of(&scenario, "openclaw")?;
    // The merger may write the commit message on this CLI too as the task ends, which is
    // timing, so the worker's call is the first.
    let call = calls.first().expect("openclaw never ran");
    let config = call
        .args
        .iter()
        .skip_while(|arg| *arg != "--config")
        .nth(1)
        .expect("a --config argument");
    assert_eq!(config, &home.join("build.json5").display().to_string());
    for name in ["OPENCLAW_HOME", "OPENCLAW_NO_AUTO_UPDATE"] {
        assert!(call.env.iter().any(|set| set == name), "{name} not set");
    }
    assert!(
        call.stdin
            .as_deref()
            .is_some_and(|prompt| prompt.contains("hello.txt")),
        "the prompt didn't arrive on stdin"
    );

    finish_screen(&scenario, &pty, "workers_final_json_cli_owns_task")
}
