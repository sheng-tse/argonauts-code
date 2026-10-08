//! The fake vendor installed as `opencode` owns a solo card through the OpenCode adapter. The
//! owner names its model, since without one OpenCode runs its own default and gets no `--model`.

mod workers_support;

use workers_support::{
    assert_sessions_completed, attempt_workers, finish_screen, invocations_of, launch_ready,
    scenario, start_solo, wait_for_finished_card, write_global_config,
};

#[test]
fn workers_opencode_owns_card() -> anyhow::Result<()> {
    let scenario = scenario("opencode-owner.toml")?;
    write_global_config(
        &scenario,
        "workflow = \"solo\"\n\
         main_agent = \"opencode/ollama/fake-model\"\n\n\
         [agents.opencode]\nenabled = true\n",
    )?;

    let mut pty = launch_ready(&scenario, &[], &[])?;
    start_solo(&mut pty)?;
    let events = wait_for_finished_card(&scenario)?;

    let owners: Vec<String> = attempt_workers(&events)
        .iter()
        .map(|worker| worker.worker.clone())
        .collect();
    assert_eq!(owners, ["opencode"]);
    assert_sessions_completed(&events, "opencode");

    let runs = invocations_of(&scenario, "opencode")?
        .into_iter()
        .filter(|call| call.args.first().is_some_and(|arg| arg == "run"))
        .collect::<Vec<_>>();
    assert_eq!(runs.len(), 1, "{runs:?}");
    let args = &runs[0].args;
    for expected in ["--format", "json"] {
        assert!(args.iter().any(|arg| arg == expected), "{args:?}");
    }
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "--model" && pair[1] == "ollama/fake-model"),
        "{args:?}"
    );
    // A new card never picks up whatever session OpenCode ran last.
    assert!(!args.iter().any(|arg| arg == "--continue"), "{args:?}");
    assert!(
        runs[0].cwd.starts_with(scenario.repo.home()),
        "the worker ran outside its worktree: {}",
        runs[0].cwd.display()
    );

    finish_screen(&scenario, &pty, "workers_opencode_owns_card")
}
