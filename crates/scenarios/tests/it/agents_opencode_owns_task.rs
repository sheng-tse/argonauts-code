//! The fake agent installed as `opencode` owns a Captain task through the OpenCode adapter. The
//! worker names its model, since without one OpenCode runs its own default and gets no `--model`.
//! Its model is on Ollama, a fake one here, which loads it before OpenCode starts.

use crate::agents_support;
use agents_support::{
    assert_sessions_completed, finish_screen, invocations_of, launch_ready, scenario,
    start_captain, toml_string, try_workers, wait_for_finished_task, write_global_config,
};
use argonauts_testkit::{FakeKind, FakeModelServer};

#[test]
fn workers_opencode_owns_task() -> anyhow::Result<()> {
    let scenario = scenario("opencode-worker.toml")?;
    let ollama = FakeModelServer::start(FakeKind::Ollama, &["fake-model"])?;
    write_global_config(
        &scenario,
        &format!(
            "workflow = \"captain\"\n\
             crew.main = \"opencode/ollama/fake-model\"\n\n\
             [agents.opencode]\nenabled = true\n\n\
             [endpoints.ollama]\nbase_url = {}\nlocal = true\n",
            toml_string(&ollama.base_url())
        ),
    )?;

    let mut pty = launch_ready(&scenario, &[], &[])?;
    start_captain(&mut pty)?;
    let events = wait_for_finished_task(&scenario)?;

    let workers: Vec<String> = try_workers(&events)
        .iter()
        .map(|model| model.agent.clone())
        .collect();
    assert_eq!(workers, ["opencode"]);
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
    // Ollama loaded the model with a request that generates nothing.
    let loaded = ollama.requests().iter().any(|request| {
        request.path == "/api/generate"
            && request.body == serde_json::json!({"model": "fake-model"})
    });
    assert!(loaded, "{:?}", ollama.requests());
    // A new task never picks up whatever session OpenCode ran last.
    assert!(!args.iter().any(|arg| arg == "--continue"), "{args:?}");
    assert!(
        runs[0].cwd.starts_with(scenario.repo.home()),
        "the worker ran outside its worktree: {}",
        runs[0].cwd.display()
    );

    finish_screen(&scenario, &pty, "workers_opencode_owns_task")
}
