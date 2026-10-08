//! A role's `native_fanout` cap reaches each vendor its own way: an environment variable for
//! Claude Code, a config flag for Codex, and the conductor counting agy's subagents.

mod limits_support;

use argonauts_core::events::session::EndReason;
use limits_support::*;

#[test]
fn limits_native_fanout_capped_per_role() -> anyhow::Result<()> {
    let limits = setup(
        "native-fanout-capped",
        "[roles.worker]\nnative_fanout = { max = 2 }\n",
    )?;
    for agent in ["claude-code", "codex"] {
        let mut pty = limits.run(&["--agent", agent])?;
        pty.wait_for_exit(CARD_WAIT)?;
    }

    let claude = limits
        .call_with("claude", "acceptEdits")?
        .ok_or_else(|| anyhow::anyhow!("Claude Code never ran"))?;
    assert!(
        claude
            .env
            .iter()
            .any(|name| name == "CLAUDE_CODE_MAX_CONCURRENT_SUBAGENTS"),
        "{:?}",
        claude.env
    );
    let codex = limits
        .call_with("codex", "exec")?
        .ok_or_else(|| anyhow::anyhow!("Codex never ran"))?;
    let capped = codex
        .args
        .windows(2)
        .any(|pair| pair[0] == "-c" && pair[1] == "agents.max_concurrent_threads_per_session=2");
    assert!(capped, "{:?}", codex.args);

    // passes when: the end reason of a session interrupted past its fan-out cap is settled;
    // here it only must not complete, since the fake would run until stopped.
    let pty = limits.run(&["--agent", "antigravity-cli"])?;
    let events = limits.wait_for("agy to be interrupted", CARD_WAIT, |events| {
        !ends_of(events, "antigravity-cli").is_empty()
    })?;
    let ends = ends_of(&events, "antigravity-cli");
    assert!(
        ends.iter().all(|end| end.reason != EndReason::Completed),
        "{ends:?}"
    );
    limits.shot(&pty, "limits_native_fanout_capped_per_role")
}
