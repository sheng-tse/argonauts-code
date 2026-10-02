//! Context: a full context window, or one past `sessions.rotate_at`, rotates the session to a
//! fresh one on the same worker. That is not a failure.

mod limits_support;

use argonauts_core::AgentEvent;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::{AttemptCause, CardState};
use argonauts_core::events::session::EndReason;
use limits_support::*;

// passes when: the handoff note of a rotation carries the stopped session's last message,
// which is how the fakes here tell the fresh session from the first.

#[test]
fn limits_context_full_rotates() -> anyhow::Result<()> {
    let limits = setup("context-full-rotates", "default_agent = \"codex\"\n")?;
    let mut pty = limits.run(&[])?;
    let events = limits.wait_for("the rotated session to finish", CARD_WAIT, |events| {
        has_attempt("codex", AttemptCause::Rotation)(events) && completed_by("codex")(events)
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
    assert!(handoff_plans(&events).is_empty());
    assert!(!card_states(&events).contains(&CardState::Failed));
    pty.wait_for_exit(CARD_WAIT)?;
    Ok(())
}

#[test]
fn limits_rotates_at_cap_with_transcript_line() -> anyhow::Result<()> {
    let limits = setup("rotates-at-cap", "[sessions]\nrotate_at = \"100k\"\n")?;
    let pty = limits.start()?;
    let events = limits.wait_for("the rotated session to finish", CARD_WAIT, |events| {
        has_attempt("claude-code", AttemptCause::Rotation)(events)
            && completed_by("claude-code")(events)
    })?;

    let fills = find(&events, |kind| match kind {
        RunEventKind::Agent(AgentEvent::ContextFill { used, .. }) => Some(*used),
        _ => None,
    });
    assert!(fills.iter().any(|used| *used > 100_000), "{fills:?}");
    assert_eq!(limits.calls_with("claude", "acceptEdits")?.len(), 2);
    assert!(!card_states(&events).contains(&CardState::Failed));
    pty.wait_for("(?i)rotat", argonauts_scenarios::WAIT)?;
    limits.shot(&pty, "limits_rotates_at_cap_with_transcript_line")
}
