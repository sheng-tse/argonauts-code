//! The sandbox, end to end: a probe set as the agent's binary tries what no session may do,
//! inside the session the real app starts, and then hands over to the fake vendor.

mod limits_support;

use std::path::PathBuf;

use limits_support::*;

fn assert_blocked(stderr: &str, what: &str, names: &[&str]) {
    let leaked = probe_lines(stderr, LEAKED, what);
    assert!(
        leaked.is_empty(),
        "the sandbox let these through: {leaked:?}"
    );
    let blocked = probe_lines(stderr, BLOCKED, what);
    for name in names {
        assert!(
            blocked.iter().any(|line| line.contains(name)),
            "no blocked {what} of {name}; the probe said:\n{stderr}"
        );
    }
}

#[test]
fn limits_sandbox_blocks_write_outside_worktree() -> anyhow::Result<()> {
    // passes when: the profile's temp allowance is the session's own folder, not all of
    // `TMPDIR`, since the test's repo and homes live under `TMPDIR`.
    let limits = setup("sandbox-write-outside", "")?;
    let targets: Vec<PathBuf> = vec![
        limits.repo.root().join("outside.txt"),
        limits.repo.home().join("outside.txt"),
        limits.repo.user_home().join("outside.txt"),
    ];
    let checks: Vec<String> = targets.iter().map(|target| try_write(target)).collect();
    let probe = write_probe(
        &limits.repo.scratch().join("probe-claude"),
        "claude",
        &checks,
    )?;
    limits.write_settings(&format!(
        "[agents.claude-code]\nbinary = {}\n",
        toml_string(&probe.display().to_string())
    ))?;

    let mut pty = limits.run(&[])?;
    limits.wait_for("the task to finish", CARD_WAIT, completed_by("claude-code"))?;
    assert_blocked(&limits.sessions_stderr()?, "write", &["outside.txt"]);
    assert_eq!(
        probe_lines(&limits.sessions_stderr()?, BLOCKED, "write").len(),
        targets.len()
    );
    for target in &targets {
        assert!(!target.exists(), "{} was written", target.display());
    }
    pty.wait_for_exit(CARD_WAIT)?;
    Ok(())
}

#[test]
fn limits_readonly_session_cannot_read_state_or_drafts() -> anyhow::Result<()> {
    let limits = setup("sandbox-readonly-reads", "")?;
    let draft = limits.plant("state/0000-planted/runs/0000-planted/drafts/draft-1.md")?;
    let home = limits.repo.home();
    let checks = [
        try_read_matching(home, "state/*/runs/*/approve.token"),
        try_read_matching(home, "state/*/runs/*/sessions/*/raw.stdout.jsonl"),
        try_read(&draft),
    ];
    let probe = write_probe(&limits.repo.scratch().join("probe-codex"), "codex", &checks)?;
    limits.write_settings(&format!(
        "[solo]\nreview = true\n\n[roles.reviewer]\nworkers = [\"codex\"]\n\n\
         [agents.codex]\nbinary = {}\n",
        toml_string(&probe.display().to_string())
    ))?;

    let _pty = limits.start()?;
    limits.wait_for("the review session to end", CARD_WAIT, |events| {
        !ends_of(events, "codex").is_empty()
    })?;
    assert_blocked(
        &limits.sessions_stderr()?,
        "read",
        &["approve.token", "raw.stdout.jsonl", "draft-1.md"],
    );
    Ok(())
}

#[test]
fn limits_build_session_cannot_read_other_worktrees() -> anyhow::Result<()> {
    let limits = setup("sandbox-other-worktrees", "")?;
    let other = limits.plant("worktrees/0000-planted/run-0/task-009/secret.txt")?;
    let checks = [try_read(&other)];
    let probe = write_probe(
        &limits.repo.scratch().join("probe-claude"),
        "claude",
        &checks,
    )?;
    limits.write_settings(&format!(
        "[agents.claude-code]\nbinary = {}\n",
        toml_string(&probe.display().to_string())
    ))?;

    let mut pty = limits.run(&[])?;
    limits.wait_for("the task to finish", CARD_WAIT, completed_by("claude-code"))?;
    assert_blocked(&limits.sessions_stderr()?, "read", &["task-009/secret.txt"]);
    pty.wait_for_exit(CARD_WAIT)?;
    Ok(())
}
