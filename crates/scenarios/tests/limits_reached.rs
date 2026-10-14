//! A task's limit: the meter stops the session at it and the work is committed. Then a
//! fallback takes the task, or the limit approval asks what to do when none can.

mod limits_support;

use argonauts_core::events::approval::ApprovalKind;
use argonauts_core::events::run::Party;
use argonauts_core::events::session::EndReason;
use argonauts_core::events::task::{TaskEvent, TaskState, TryCause};
use argonauts_core::events::{RunEventKind, SpendEvent};
use argonauts_scenarios::WAIT;
use limits_support::*;

#[test]
fn limits_hit_commits_and_asks() -> anyhow::Result<()> {
    let limits = setup(
        "limit-hit-commits-and-asks",
        // No other agent is on, so no fallback can take the task and the approval asks.
        "[limits]\nper_task_usd = 0.05\n\n[agents.codex]\nenabled = false\n\n\
         [agents.antigravity-cli]\nenabled = false\n\n[agents.opencode]\nenabled = false\n",
    )?;
    let mut pty = limits.start()?;
    let question = limits.wait_for_question(&pty, ApprovalKind::Limit)?;
    let events = limits.events();

    let ends = ends_of(&events, "claude-code");
    assert!(
        matches!(
            ends.first().map(|end| &end.reason),
            Some(EndReason::LimitHit { .. })
        ),
        "{ends:?}"
    );
    let hit = seq_of(&events, |kind| {
        matches!(kind, RunEventKind::Spend(SpendEvent::Hit { .. }))
    });
    assert!(before(hit, opened_at(&events, ApprovalKind::Limit)));
    assert!(before(
        seq_of(&events, committed),
        opened_at(&events, ApprovalKind::Limit)
    ));
    assert_eq!(option_keys(&question), ['m', 'h', 't', 'x', 'd']);
    pty.wait_for("Raise the limit", WAIT)?;
    limits.shot(&pty, "limits_hit_commits_and_asks")?;

    answer(&mut pty, &question, 'm')?;
    let events = limits.wait_for("the task to finish", LONG_WAIT, completed_by("claude-code"))?;
    let extended = find(&events, |kind| match kind {
        RunEventKind::Spend(SpendEvent::Extended { by, .. }) => Some(by),
        _ => None,
    });
    assert!(
        extended.iter().any(|by| **by == Party::User),
        "{extended:?}"
    );
    assert!(limits.call_with("claude", "--resume")?.is_some());
    Ok(())
}

/// `when_reached = "fallback"`, which it is unless you say otherwise: the rules hand the task
/// to another agent once, and it carries on there instead of asking.
#[test]
fn limits_hit_falls_back_once_by_rule() -> anyhow::Result<()> {
    let limits = setup(
        "limit-hit-commits-and-asks",
        "[limits]\nper_task_usd = 0.05\n",
    )?;
    let _pty = limits.start()?;
    let events = limits.wait_for("a try by another agent", LONG_WAIT, |events: &[_]| {
        tries(events)
            .iter()
            .any(|(worker, cause)| worker.agent != "claude-code" && *cause == TryCause::Reassigned)
    })?;
    let reassigned = find(&events, |kind| match kind {
        RunEventKind::Task(TaskEvent::Reassigned { by, .. }) => Some(by),
        _ => None,
    });
    assert_eq!(reassigned, [&Party::Rules], "{reassigned:?}");
    assert!(
        !task_states(&events).contains(&TaskState::NeedsYou),
        "{:?}",
        task_states(&events)
    );
    Ok(())
}
