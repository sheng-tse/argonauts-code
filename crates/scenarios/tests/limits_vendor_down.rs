//! Vendor outages: the breaker marks the vendor down, its cards pause with their work
//! committed, and the role's handoff mode decides what happens next.

mod limits_support;

use argonauts_core::events::checkpoint::{AnswerAction, CheckpointEvent, CheckpointKind};
use argonauts_core::events::session::EndReason;
use argonauts_core::events::{HandoffEvent, RunEvent, RunEventKind, VendorEvent};
use argonauts_core::limits::HealthState;
use argonauts_core::settings::handoff::HandoffMode;
use argonauts_scenarios::WAIT;
use limits_support::*;

/// One overload ending one session marks a vendor down, so a single card shows the outage.
const DOWN_AT_ONCE: &str = "[health]\ndown_after = 1\ndown_sessions = 1\n";

fn ask_with_codex() -> String {
    format!("{DOWN_AT_ONCE}\n[fallback]\nmode = \"ask\"\norder = [\"codex\"]\n")
}

fn went_down(events: &[RunEvent], vendor: &str) -> bool {
    !find(events, |kind| match kind {
        RunEventKind::Vendor(VendorEvent::Health(health))
            if health.vendor.0 == vendor && health.state == HealthState::Down =>
        {
            Some(())
        }
        _ => None,
    })
    .is_empty()
}

#[test]
fn limits_vendor_down_pauses_cards() -> anyhow::Result<()> {
    let limits = setup("vendor-down-pauses-cards", &ask_with_codex())?;
    let pty = limits.start()?;
    let card = limits.wait_for_card(&pty, CheckpointKind::VendorDown)?;
    let events = limits.events();

    assert!(went_down(&events, "claude-code"));
    let ends = ends_of(&events, "claude-code");
    assert!(
        matches!(
            ends.first().map(|end| &end.reason),
            Some(EndReason::VendorError { .. })
        ),
        "{ends:?}"
    );
    let packet = vendor_down_packet(&card)?;
    assert_eq!(packet.vendor.0, "claude-code");
    let paused: Vec<_> = packet
        .paused
        .iter()
        .map(|paused| paused.card.clone())
        .collect();
    assert_eq!(paused, first_card(&events).into_iter().collect::<Vec<_>>());
    assert!(before(
        seq_of(&events, committed),
        opened_at(&events, CheckpointKind::VendorDown)
    ));
    assert_eq!(option_keys(&card), ['a', 'e', 'w']);
    assert!(limits.calls_with("codex", "exec")?.is_empty());
    pty.wait_for("Claude down", WAIT)?;
    limits.shot(&pty, "limits_vendor_down_pauses_cards")
}

#[test]
fn limits_vendor_down_one_agent_for_all() -> anyhow::Result<()> {
    // passes when: it's settled whether "one agent for every paused card" takes the role's
    // first valid candidate, as here, or asks which agent with a picker.
    let limits = setup("vendor-down-one-agent-for-all", &ask_with_codex())?;
    let mut pty = limits.start()?;
    let card = limits.wait_for_card(&pty, CheckpointKind::VendorDown)?;
    limits.shot(&pty, "limits_vendor_down_one_agent_for_all")?;

    answer(&mut pty, &card, 'a')?;
    let events = limits.wait_for("Codex to finish the task", CARD_WAIT, completed_by("codex"))?;
    let decided = find(&events, |kind| match kind {
        RunEventKind::Checkpoint(CheckpointEvent::Decided(decided)) if decided.id == card.id => {
            Some(&decided.action)
        }
        _ => None,
    });
    assert_eq!(decided, [&AnswerAction::OneForAll]);
    let paused = &vendor_down_packet(&card)?.paused;
    for paused in paused {
        assert!(
            handed_off(&events).iter().any(|event| matches!(
                event,
                HandoffEvent::HandedOff { card, to, .. } if *card == paused.card && to.worker == "codex"
            )),
            "{} was not handed to Codex",
            paused.card
        );
    }
    Ok(())
}

#[test]
fn limits_vendor_down_follows_handoff_mode() -> anyhow::Result<()> {
    let limits = setup(
        "vendor-down-follows-handoff-mode",
        &format!(
            "main_agent = \"antigravity-cli\"\n\n{DOWN_AT_ONCE}\n\
             [fallback]\nmode = \"ask\"\n\n\
             [fallback.workers]\nmode = \"auto\"\norder = [\"codex\"]\n"
        ),
    )?;
    let mut pty = limits.run(&[])?;
    let events = limits.wait_for("Codex to finish the task", CARD_WAIT, completed_by("codex"))?;

    assert!(went_down(&events, "antigravity-cli"));
    assert!(
        opened(&events, CheckpointKind::VendorDown).is_none(),
        "the worker role hands off on its own, so nothing should ask"
    );
    assert!(handed_off(&events).iter().any(|event| matches!(
        event,
        HandoffEvent::HandedOff { to, mode: HandoffMode::Auto, .. } if to.worker == "codex"
    )));
    pty.wait_for_exit(CARD_WAIT)?;
    Ok(())
}
