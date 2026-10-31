//! Context: a full context window, or one past `sessions.rotate_at`, rotates the session to a
//! fresh one on the same worker. That is not a failure.

use crate::limits_support;
use argonauts_core::AgentEvent;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::session::EndReason;
use argonauts_core::runs::events::task::{TaskState, TryCause};
use limits_support::*;

// passes when: the fallback note of a rotation carries the stopped session's last message,
// which is how the fakes here tell the fresh session from the first.

#[test]
fn limits_context_full_rotates() -> anyhow::Result<()> {
    let limits = setup("context-full-rotates", "crew.main = \"codex\"\n")?;
    let mut pty = limits.run(&[])?;
    let events = limits.wait_for("the rotated session to finish", LONG_WAIT, |events| {
        has_try("codex", TryCause::Rotation)(events) && completed_by("codex")(events)
    })?;

    let ends = ends_of(&events, "codex");
    assert_eq!(
        ends.first().map(|end| &end.reason),
        Some(&EndReason::ContextFull)
    );
    let calls = limits.calls_with("codex", "exec")?;
    assert_eq!(calls.len(), 2);
    assert!(
        calls
            .iter()
            .all(|call| !call.args.iter().any(|arg| arg == "resume")),
        "a rotation starts a fresh session"
    );
    assert!(fallback_plans(&events).is_empty());
    assert!(!task_states(&events).contains(&TaskState::Failed));
    pty.wait_for_exit(LONG_WAIT)?;
    Ok(())
}

#[test]
fn limits_rotates_at_cap_with_transcript_line() -> anyhow::Result<()> {
    let limits = setup("rotates-at-cap", "[sessions]\nrotate_at = \"100k\"\n")?;
    let pty = limits.start()?;
    let events = limits.wait_for("the rotated session to finish", LONG_WAIT, |events| {
        has_try("claude-code", TryCause::Rotation)(events) && completed_by("claude-code")(events)
    })?;

    let fills = find(&events, |kind| match kind {
        RunEventKind::Agent(AgentEvent::ContextFill { used, .. }) => Some(*used),
        _ => None,
    });
    assert!(fills.iter().any(|used| *used > 100_000), "{fills:?}");
    assert_eq!(limits.calls_with("claude", "acceptEdits")?.len(), 2);
    assert!(!task_states(&events).contains(&TaskState::Failed));
    pty.wait_for("(?i)rotat", argonauts_scenarios::WAIT)?;
    limits.shot(&pty, "limits_rotates_at_cap_with_transcript_line")
}
