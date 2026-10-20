//! A `[[cli]]` entry for the manifest adapter's JSONL plugin owns a Captain task. The plugin
//! runs for real: it speaks Argonauts Code's JSONL protocol and writes the file it's told to.

use crate::agents_support;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;

use agents_support::{
    assert_sessions_completed, finish_screen, launch_ready, scenario, start_captain, toml_path,
    try_workers, wait_for_finished_task, write_global_config, write_global_models,
};
use anyhow::Context as _;

#[test]
fn workers_manifest_cli_owns_task() -> anyhow::Result<()> {
    let scenario = scenario("manifest-cli.toml")?;
    let fixture =
        argonauts_scenarios::scenario_file("../../adapters/manifest/fixtures/jsonl-plugin.sh");
    // A copy under the test's own folder, so its mode doesn't depend on the checkout.
    let plugin = scenario.repo.scratch().join("plugins/jsonl-plugin.sh");
    fs::create_dir_all(plugin.parent().context("the plugin has a folder")?)?;
    fs::copy(&fixture, &plugin)
        .with_context(|| format!("copying the plugin {}", fixture.display()))?;
    fs::set_permissions(&plugin, fs::Permissions::from_mode(0o755))?;

    write_global_models(
        &scenario,
        &format!(
            "[[cli]]\n\
             id = \"jsonl-plugin\"\n\
             display_name = \"JSONL plugin\"\n\
             binary = {}\n\
             version_args = [\"--version\"]\n\
             provider = \"meta\"\n\
             read_only_args = [\"--model\", \"{{model}}\"]\n\
             build_args = [\"--model\", \"{{model}}\", \"--write\", \"hello.txt\"]\n\n\
             [[model]]\n\
             id = \"jsonl-plugin/fake-local\"\n\
             provider = \"meta\"\n\
             kind = \"cli\"\n\
             access = \"local\"\n\
             tools = true\n\
             cost = {{ unit = \"none\" }}\n",
            toml_path(&plugin)
        ),
    )?;
    write_global_config(
        &scenario,
        "workflow = \"captain\"\ncrew.main = \"jsonl-plugin/fake-local\"\n",
    )?;

    let mut pty = launch_ready(&scenario, &[], &[])?;
    start_captain(&mut pty)?;
    let events = wait_for_finished_task(&scenario)?;

    let workers: Vec<String> = try_workers(&events)
        .iter()
        .map(|model| model.to_string())
        .collect();
    assert_eq!(workers, ["jsonl-plugin/fake-local"]);
    assert_sessions_completed(&events, "jsonl-plugin");

    // The plugin's write reached the task's branch.
    let branches = scenario.repo.git(&["branch", "--list"])?;
    let branch = branches
        .lines()
        .map(|line| line.trim_start_matches(['*', '+', ' ']))
        .find(|name| name.starts_with("argonauts/") && !name.ends_with("/merged"))
        .with_context(|| format!("no task branch in {branches:?}"))?;
    let written = scenario
        .repo
        .git(&["show", &format!("{branch}:hello.txt")])?;
    assert_eq!(written, "written by the plugin");

    finish_screen(&scenario, &pty, "workers_manifest_cli_owns_task")
}
