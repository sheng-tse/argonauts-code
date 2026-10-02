//! The learning commands from the shell, with the real binary over a temp repo: track records
//! rebuilt from the outcome log as of a date, and a recipe rolled back by hand.

mod app_support;

use anyhow::Context as _;
use app_support::*;
use argonauts_core::{Paths, RepoId};
use serde_json::Value;

/// One outcome line per card, as the retro writes them.
fn outcome(run: &str, card: u32, worker: &str, first_try: bool, at: &str) -> String {
    format!(
        "{{\"type\":\"outcome\",\"run\":\"{run}\",\"card\":\"card-{card:03}\",\"worker\":\"{worker}\",\
         \"task_type\":\"backend\",\"first_try\":{first_try},\"attempts\":1,\"integrated\":true,\
         \"spent_usd\":1.0,\"at\":\"{at}\",\"minutes\":10.0}}\n"
    )
}

fn state_dir(scenario: &AppScenario) -> anyhow::Result<std::path::PathBuf> {
    let id = RepoId::for_repo(&scenario.repo.root().canonicalize()?)?;
    Ok(Paths::new(scenario.repo.home()).state_dir(&id))
}

fn json(output: &std::process::Output) -> anyhow::Result<Value> {
    anyhow::ensure!(output.status.success(), "{}", output_text(output));
    serde_json::from_slice(&output.stdout).context("the command's JSON")
}

#[test]
fn learning_records_rebuild_from_the_shell() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let outcomes = state_dir(&scenario)?.join("outcomes");
    std::fs::create_dir_all(&outcomes)?;
    for n in 1..=12u32 {
        let run = format!("run-{n:02}");
        let mut lines = outcome(&run, 1, "codex", true, "2026-09-20T10:00:00Z");
        lines.push_str(&outcome(
            &run,
            2,
            "claude-code",
            n % 3 == 0,
            "2026-09-20T10:00:00Z",
        ));
        std::fs::write(outcomes.join(format!("{run}.jsonl")), lines)?;
    }
    // A run after the date doesn't count as of it.
    std::fs::write(
        outcomes.join("run-99.jsonl"),
        outcome("run-99", 1, "codex", false, "2026-10-05T10:00:00Z"),
    )?;

    let rebuilt = json(&run_cli(
        &scenario,
        &["records", "rebuild", "--as-of", "2026-09-30", "--json"],
    )?)?;
    let records = rebuilt["records"]
        .as_array()
        .context("records in the JSON")?;
    let rate = |worker: &str| {
        records
            .iter()
            .find(|record| record["worker"] == worker && record["task_type"] == "backend")
            .and_then(|record| {
                let pass = &record["pass"];
                Some(pass["alpha"].as_f64()? / (pass["alpha"].as_f64()? + pass["beta"].as_f64()?))
            })
    };
    let codex = rate("codex").context("codex's record")?;
    let claude = rate("claude-code").context("claude-code's record")?;
    assert!(codex > claude, "codex {codex}, claude-code {claude}");

    // A cache, never a source: the same log and date give the same records.
    let again = json(&run_cli(
        &scenario,
        &["records", "--as-of", "2026-09-30", "--json"],
    )?)?;
    assert_eq!(again["records"], rebuilt["records"]);

    // As text, for people.
    let text = output_text(&run_cli(&scenario, &["records", "--as-of", "2026-09-30"])?);
    assert!(text.contains("codex backend: first pass"), "{text}");
    Ok(())
}

#[test]
fn learning_recipe_rollback_by_hand() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let recipe = |version: u32, rounds: u32| {
        format!(
            "recipe: api-feature\nversion: {version}\n{}description: HTTP endpoints, with tests\n\
             mode: crew\nmatch: {{ task_types: [backend] }}\ncritique: {{ rounds: {rounds} }}\n",
            if version > 1 {
                format!("parent: {}\n", version - 1)
            } else {
                String::new()
            }
        )
    };
    scenario
        .repo
        .write(".argonauts/recipes/api-feature/v1.yaml", &recipe(1, 1))?;
    scenario
        .repo
        .write(".argonauts/recipes/api-feature/v2.yaml", &recipe(2, 2))?;
    scenario
        .repo
        .write(".argonauts/recipes/api-feature/current", "2\n")?;
    scenario.repo.commit_all("two versions of a recipe")?;
    let main_before = scenario.repo.head()?;

    let listed = json(&run_cli(&scenario, &["recipes", "list", "--json"])?)?;
    assert_eq!(listed["recipes"][0]["current"], 2, "{listed}");

    let output = run_cli(&scenario, &["recipes", "rollback", "api-feature"])?;
    let said = output_text(&output);
    assert!(output.status.success(), "{said}");
    assert!(said.contains("api-feature"), "{said}");

    // At once: the next run uses v1. The pointer change waits on a branch, never on main.
    let listed = json(&run_cli(
        &scenario,
        &["recipes", "show", "api-feature", "--json"],
    )?)?;
    assert_eq!(listed["recipe"]["version"], 1, "{listed}");
    assert_eq!(scenario.repo.head()?, main_before);
    let branches = scenario.repo.git(&[
        "for-each-ref",
        "--format=%(refname:short)",
        "refs/heads/argonauts/recipes/",
    ])?;
    assert!(
        !branches.trim().is_empty(),
        "no branch proposes the pointer"
    );
    let inbox = output_text(&run_cli(&scenario, &["inbox"])?);
    assert!(inbox.contains("argonauts/recipes/"), "{inbox}");
    Ok(())
}
