//! With merges decided by a model, the approver runs as a fresh read-only session in an empty
//! scratch directory, never in the repo or a worktree, and never resumes an earlier session.

mod policy_support;

use argonauts_core::events::run::Party;
use policy_support::{
    HELLO_GATE, RUN_WAIT, invocations_of, launch_ready, merge_decided, scenario, solo_config,
    start_solo, trust_repo, wait_for_events, write_global_config,
};

#[test]
fn policy_model_approver_fresh_read_only() -> anyhow::Result<()> {
    let scenario = scenario("approver-model.toml")?;
    scenario.repo.write(
        ".argonauts/approval-rules.md",
        "# Approval rules\n\n- Small changes may merge.\n",
    )?;
    scenario.repo.commit_all("add approval rules")?;
    trust_repo(&scenario)?;
    // The approver decides once the hard limits pass, and passing gates are one of them. Codex
    // names no model it ran, so the approver names one. Codex's own sandbox guards it, since
    // inside Argonauts' sandbox Codex's is off and its flag shows nothing.
    write_global_config(
        &scenario,
        &solo_config(&format!(
            "[approvals.merge]\nmode = \"auto\"\ndecide_with = \"model\"\n\n\
             [approvals.approver]\nmodel = \"codex/gpt-5.6-luna\"\ndifferent_family = true\n\n\
             [roles.approver.sandbox]\nmode = \"tool\"\n\n{HELLO_GATE}",
        )),
    )?;

    let mut pty = launch_ready(&scenario, &[])?;
    start_solo(&mut pty)?;
    let events = wait_for_events(&scenario, "the approver's decision", RUN_WAIT, |events| {
        merge_decided(events).is_some()
    })?;
    let decided = merge_decided(&events).expect("waited for it");
    assert!(
        matches!(&decided.by, Party::Approver { model } if model.worker == "codex"),
        "{:?}",
        decided.by
    );

    let approver_calls = invocations_of(&scenario, "codex")?;
    assert!(!approver_calls.is_empty(), "the approver never ran");
    let root = scenario.repo.root();
    for call in &approver_calls {
        let args = &call.args;
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--sandbox" && pair[1] == "read-only"),
            "the approver could write: {args:?}"
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
    let mut dirs: Vec<_> = approver_calls.iter().map(|call| &call.cwd).collect();
    dirs.sort();
    dirs.dedup();
    assert_eq!(dirs.len(), approver_calls.len());

    scenario.shot(&pty, "policy_model_approver_fresh_read_only")
}
