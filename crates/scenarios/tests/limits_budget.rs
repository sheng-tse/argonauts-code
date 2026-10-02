//! A card's budget: the meter stops the session at its limit, the work is committed, and the
//! budget card asks what to do.

mod limits_support;

use argonauts_core::events::card::{AttemptCause, CardEvent, CardState};
use argonauts_core::events::checkpoint::CheckpointKind;
use argonauts_core::events::run::Party;
use argonauts_core::events::session::EndReason;
use argonauts_core::events::{BudgetEvent, RunEventKind};
use argonauts_scenarios::WAIT;
use limits_support::*;

#[test]
fn limits_budget_hit_commits_and_asks() -> anyhow::Result<()> {
    let limits = setup(
        "budget-hit-commits-and-asks",
        "[budget]\nper_card_usd = 0.05\non_hit = \"ask\"\n",
    )?;
    let mut pty = limits.start()?;
    let card = limits.wait_for_card(&pty, CheckpointKind::Budget)?;
    let events = limits.events();

    let ends = ends_of(&events, "claude-code");
    assert!(
        matches!(
            ends.first().map(|end| &end.reason),
            Some(EndReason::BudgetHit { .. })
        ),
        "{ends:?}"
    );
    let hit = seq_of(&events, |kind| {
        matches!(kind, RunEventKind::Budget(BudgetEvent::Hit { .. }))
    });
    assert!(before(hit, opened_at(&events, CheckpointKind::Budget)));
    assert!(before(
        seq_of(&events, committed),
        opened_at(&events, CheckpointKind::Budget)
    ));
    assert_eq!(option_keys(&card), ['m', 'h', 't', 'x', 'd']);
    pty.wait_for("More budget", WAIT)?;
    limits.shot(&pty, "limits_budget_hit_commits_and_asks")?;

    answer(&mut pty, &card, 'm')?;
    let events = limits.wait_for("the card to finish", CARD_WAIT, completed_by("claude-code"))?;
    let extended = find(&events, |kind| match kind {
        RunEventKind::Budget(BudgetEvent::Extended { by, .. }) => Some(by),
        _ => None,
    });
    assert!(
        extended.iter().any(|by| **by == Party::User),
        "{extended:?}"
    );
    assert!(limits.call_with("claude", "--resume")?.is_some());
    Ok(())
}

/// `on_hit = "handoff"`: the rules hand the card to another agent once, and it carries on
/// there instead of escalating.
#[test]
fn limits_budget_hit_hands_off_once_by_rule() -> anyhow::Result<()> {
    let limits = setup(
        "budget-hit-commits-and-asks",
        "[budget]\nper_card_usd = 0.05\non_hit = \"handoff\"\n",
    )?;
    let _pty = limits.start()?;
    let events = limits.wait_for(
        "an attempt by another agent",
        CARD_WAIT,
        |events: &[_]| {
            attempts(events).iter().any(|(worker, cause)| {
                worker.worker != "claude-code" && *cause == AttemptCause::Reassigned
            })
        },
    )?;
    let reassigned = find(&events, |kind| match kind {
        RunEventKind::Card(CardEvent::Reassigned { by, .. }) => Some(by),
        _ => None,
    });
    assert_eq!(reassigned, [&Party::Rules], "{reassigned:?}");
    assert!(
        !card_states(&events).contains(&CardState::Escalated),
        "{:?}",
        card_states(&events)
    );
    Ok(())
}
