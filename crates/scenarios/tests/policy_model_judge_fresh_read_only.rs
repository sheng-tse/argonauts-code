//! With merges decided by a model, the judge runs as a fresh read-only session in an empty
//! scratch directory, never in the repo or a worktree, and never resumes an earlier session.

mod policy_support;

use argonauts_core::events::run::Party;
use policy_support::{
    HELLO_CHECK, RUN_WAIT, captain_config, invocations_of, launch_ready, merge_decided, scenario,
    start_captain, trust_repo, wait_for_events, write_global_config,
};

#[test]
fn policy_model_judge_fresh_read_only() -> anyhow::Result<()> {
    let scenario = scenario("judge-model.toml")?;
    scenario.repo.write(
        ".argonauts/approval-rules.md",
        "# Approval rules\n\n- Small changes may merge.\n",
    )?;
    scenario.repo.commit_all("add approval rules")?;
    trust_repo(&scenario)?;
    // The judge decides once the hard limits pass, and passing checks are one of them. Codex
    // names no model it ran, so the judge names one. Codex's own sandbox guards it, since
    // inside Argonauts Code's sandbox Codex's is off and its flag shows nothing.
    write_global_config(
        &scenario,
        &captain_config(&format!(
            "[approvals]\ndecides = \"codex/gpt-5.6-luna\"\n\n\
             [approvals.merge]\nmode = \"auto\"\ndecide_with = \"model\"\n\n\
             [approvals.judge]\ndifferent_provider = true\n\n\
             [roles.judge.sandbox]\nmode = \"tool\"\n\n{HELLO_CHECK}",
        )),
    )?;

    let mut pty = launch_ready(&scenario, &[])?;
    start_captain(&mut pty)?;
    let events = wait_for_events(&scenario, "the judge's decision", RUN_WAIT, |events| {
        merge_decided(events).is_some()
    })?;
    let decided = merge_decided(&events).expect("waited for it");
    assert!(
        matches!(&decided.by, Party::Judge { model } if model.agent == "codex"),
        "{:?}",
        decided.by
    );

    let judge_calls = invocations_of(&scenario, "codex")?;
    assert!(!judge_calls.is_empty(), "the judge never ran");
    let root = scenario.repo.root();
    for call in &judge_calls {
        let args = &call.args;
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--sandbox" && pair[1] == "read-only"),
            "the judge could write: {args:?}"
        );
        for resumed in ["resume", "--last", "--continue"] {
            assert!(!args.iter().any(|arg| arg == resumed), "{args:?}");
        }
        assert!(
            !call.cwd.starts_with(root),
            "ran in the repo: {}",
            call.cwd.display()
        );
        assert!(
            !call.cwd.join("README.md").exists() && !call.cwd.join("hello.txt").exists(),
            "its scratch directory holds repo files: {}",
            call.cwd.display()
        );
        let prompt = call.stdin.as_deref().unwrap_or_default();
        assert!(
            prompt.contains("Small changes may merge"),
            "no rules in the prompt"
        );
    }
    // Two approvals never share a directory, since each session is fresh.
    let mut dirs: Vec<_> = judge_calls.iter().map(|call| &call.cwd).collect();
    dirs.sort();
    dirs.dedup();
    assert_eq!(dirs.len(), judge_calls.len());

    scenario.shot(&pty, "policy_model_judge_fresh_read_only")
}
