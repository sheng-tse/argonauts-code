//! A `[[harness]]` entry for the manifest adapter's JSONL plugin owns a solo card. The plugin
//! runs for real: it speaks Argonauts' JSONL protocol and writes the file it's told to.

mod workers_support;

use std::fs;
use std::os::unix::fs::PermissionsExt as _;

use anyhow::Context as _;
use workers_support::{
    assert_sessions_completed, attempt_workers, finish_screen, launch_ready, scenario, start_solo,
    toml_path, wait_for_finished_card, write_global_config, write_global_models,
};

#[test]
fn workers_manifest_harness_owns_card() -> anyhow::Result<()> {
    let scenario = scenario("manifest-harness.toml")?;
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
            "[[harness]]\n\
             id = \"jsonl-plugin\"\n\
             display_name = \"JSONL plugin\"\n\
             binary = {}\n\
             version_args = [\"--version\"]\n\
             family = \"llama\"\n\
             read_only_args = [\"--model\", \"{{model}}\"]\n\
             build_args = [\"--model\", \"{{model}}\", \"--write\", \"hello.txt\"]\n\n\
             [[model]]\n\
             id = \"jsonl-plugin/fake-local\"\n\
             family = \"llama\"\n\
             kind = \"harness\"\n\
             access = \"local\"\n\
             tools = true\n\
             cost = {{ unit = \"none\" }}\n",
            toml_path(&plugin)
        ),
    )?;
    write_global_config(
        &scenario,
        "preset = \"solo\"\ndefault_agent = \"jsonl-plugin/fake-local\"\n",
    )?;

    let mut pty = launch_ready(&scenario, &[], &[])?;
    start_solo(&mut pty)?;
    let events = wait_for_finished_card(&scenario)?;

    let owners: Vec<String> = attempt_workers(&events)
        .iter()
        .map(|worker| worker.to_string())
        .collect();
    assert_eq!(owners, ["jsonl-plugin/fake-local"]);
    assert_sessions_completed(&events, "jsonl-plugin");

    // The plugin's write reached the card's branch.
    let branches = scenario.repo.git(&["branch", "--list"])?;
    let branch = branches
        .lines()
        .map(|line| line.trim_start_matches(['*', '+', ' ']))
        .find(|name| name.starts_with("argonauts/") && !name.ends_with("/integration"))
        .with_context(|| format!("no card branch in {branches:?}"))?;
    let written = scenario
        .repo
        .git(&["show", &format!("{branch}:hello.txt")])?;
    assert_eq!(written, "written by the plugin");

    finish_screen(&scenario, &pty, "workers_manifest_harness_owns_card")
}
