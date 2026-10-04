//! Handoffs after a usage limit: the card and its candidates, auto mode, reassigning at any
//! time, the ask timeout and wait mode on a parked run, and the checkpoint round trip.

mod limits_support;

use anyhow::Context as _;
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointEvent, CheckpointKind, CheckpointPacket,
};
use argonauts_core::events::run::{ParkReason, Party, ResumeCause, RunOutcome};
use argonauts_core::events::session::EndReason;
use argonauts_core::events::{
    CardEvent, HandoffEvent, RunEvent, RunEventKind, RunLifecycle, VendorEvent,
};
use argonauts_core::limits::QuotaStatus;
use argonauts_core::settings::handoff::HandoffMode;
use argonauts_scenarios::WAIT;
use argonauts_testkit::Key;
use limits_support::*;

const FALLBACK_CODEX: &str = "[handoff]\nmode = \"ask\"\nfallback = [\"codex\"]\n";

/// Parked runs exit their supervisor quickly here, so the wake path is what the tests see.
const PARK_FAST: &str = "\n[supervisor]\npark_grace = \"1s\"\n";

#[test]
fn limits_usage_limit_handoff_card() -> anyhow::Result<()> {
    let limits = setup(
        "usage-limit-handoff-card",
        "[handoff]\nmode = \"ask\"\nfallback = [\"codex\", \"antigravity-cli\"]\n",
    )?;
    let mut pty = limits.start()?;
    let card = limits.wait_for_card(&pty, CheckpointKind::Handoff)?;

    let plan = handoff_plan(&card)?;
    assert_eq!(plan.stopped.worker, "claude-code");
    assert!(plan.reason.contains("usage limit"), "{}", plan.reason);
    assert!(plan.resets_at.is_some());
    assert!(plan.work.files_changed >= 1, "notes.txt was not committed");
    assert_eq!(candidate_workers(&card), ["codex", "antigravity-cli"]);
    let keys = option_keys(&card);
    assert!(in_order(&keys, &['a', 'b', 't', 'w', 'o', 'd']), "{keys:?}");
    pty.wait_for("Agent: build:\\S+ · (?:\\S.* on )?Claude Code", WAIT)?;
    for option in &card.options {
        pty.wait_for(&regex::escape(&option.label), WAIT)?;
    }
    limits.shot(&pty, "limits_usage_limit_handoff_card")?;

    answer(&mut pty, &card, 'a')?;
    let events = limits.wait_for("Codex to finish the card", CARD_WAIT, completed_by("codex"))?;
    assert!(handed_off(&events).iter().any(|event| matches!(
        event,
        HandoffEvent::HandedOff { to, by: Party::User, .. } if to.worker == "codex"
    )));
    limits.wait_for(
        "the merge card",
        CARD_WAIT,
        has_opened(CheckpointKind::Merge),
    )?;
    Ok(())
}

#[test]
fn limits_auto_handoff_keeps_working() -> anyhow::Result<()> {
    let limits = setup(
        "auto-handoff-keeps-working",
        "[handoff]\nmode = \"auto\"\nfallback = [\"codex\"]\n",
    )?;
    let mut pty = limits.run(&[])?;
    let events = limits.wait_for("Codex to finish the card", CARD_WAIT, completed_by("codex"))?;

    assert!(
        opened(&events, CheckpointKind::Handoff).is_none(),
        "auto mode asked"
    );
    assert!(handed_off(&events).iter().any(|event| matches!(
        event,
        HandoffEvent::HandedOff { to, mode: HandoffMode::Auto, .. } if to.worker == "codex"
    )));
    assert!(before(
        seq_of(&events, committed),
        seq_of(&events, |kind| matches!(
            kind,
            RunEventKind::Handoff(HandoffEvent::HandedOff { .. })
        ))
    ));
    pty.wait_for_exit(CARD_WAIT)?;
    Ok(())
}

#[test]
fn limits_usage_limit_pauses_then_fails_over() -> anyhow::Result<()> {
    let limits = setup(
        "usage-limit-pauses-then-fails-over",
        "default_agent = \"codex\"\n\n[handoff]\nmode = \"auto\"\nfallback = [\"claude-code\"]\n",
    )?;
    let pty = limits.start()?;
    let events = limits.wait_for(
        "Claude Code to finish the card",
        CARD_WAIT,
        completed_by("claude-code"),
    )?;

    let ends = ends_of(&events, "codex");
    assert!(
        matches!(
            ends.first().map(|end| &end.reason),
            Some(EndReason::UsageLimit { .. })
        ),
        "{ends:?}"
    );
    let rejected = find(&events, |kind| match kind {
        RunEventKind::Vendor(VendorEvent::Quota(reading))
            if reading.vendor.0 == "codex" && reading.status == QuotaStatus::Rejected =>
        {
            Some(reading)
        }
        _ => None,
    });
    let reading = rejected
        .first()
        .context("no rejected Codex quota reading")?;
    assert!(
        reading.resets_at.is_some(),
        "the reset time was not read from the message"
    );
    assert!(before(
        seq_of(&events, committed),
        seq_of(&events, |kind| matches!(
            kind,
            RunEventKind::Handoff(HandoffEvent::HandedOff { .. })
        ))
    ));
    assert!(handed_off_to("claude-code")(&events));
    pty.wait_for("Codex out", WAIT)?;
    limits.shot(&pty, "limits_usage_limit_pauses_then_fails_over")
}

#[test]
fn limits_handoff_card_candidates() -> anyhow::Result<()> {
    let limits = setup(
        "handoff-card-candidates",
        "[agents.opencode]\nenabled = false\n\n\
         [handoff]\nmode = \"ask\"\nfallback = [\"codex\", \"opencode\", \"antigravity-cli\"]\n",
    )?;
    let mut pty = limits.start()?;
    let card = limits.wait_for_card(&pty, CheckpointKind::Handoff)?;

    assert_eq!(candidate_workers(&card), ["codex", "antigravity-cli"]);
    let plan = handoff_plan(&card)?;
    assert!(
        plan.candidates
            .iter()
            .all(|candidate| !candidate.quota_text.is_empty())
    );
    for option in &card.options {
        if let AnswerAction::Candidate { .. } = option.action {
            pty.wait_for(&regex::escape(&option.label), WAIT)?;
            let detail = option
                .detail
                .as_deref()
                .context("a candidate shows no quota or cost")?;
            pty.wait_for(&regex::escape(detail), WAIT)?;
        }
    }
    limits.shot(&pty, "limits_handoff_card_candidates")?;

    let agy = key_for(&card, |action| {
        matches!(action, AnswerAction::Candidate { worker } if worker.worker == "antigravity-cli")
    })
    .context("no key for agy")?;
    answer(&mut pty, &card, agy)?;
    limits.wait_for(
        "agy to finish the card",
        CARD_WAIT,
        completed_by("antigravity-cli"),
    )?;
    assert!(limits.calls_with("codex", "exec")?.is_empty());
    Ok(())
}

#[test]
fn limits_reassign_any_time() -> anyhow::Result<()> {
    reassign_while_working()?;
    reassign_while_parked()
}

fn reassign_while_working() -> anyhow::Result<()> {
    let limits = setup("reassign-any-time", FALLBACK_CODEX)?;
    let mut pty = limits.start()?;
    let events = limits.wait_for("Claude Code to start", CARD_WAIT, |events| {
        sessions_started(events)
            .iter()
            .any(|started| started.worker.worker == "claude-code")
    })?;
    limits.wait_for_call("claude", "acceptEdits", WAIT)?;
    let card = first_card(&events).context("no card")?;

    pty.send(&format!("/reassign {card} codex\r"))?;
    let events = limits.wait_for("Codex to finish the card", CARD_WAIT, completed_by("codex"))?;
    let ends = ends_of(&events, "claude-code");
    assert_eq!(
        ends.first().map(|end| &end.reason),
        Some(&EndReason::Stopped)
    );
    assert!(reassigned_by_you(&events, "codex"));
    Ok(())
}

fn reassign_while_parked() -> anyhow::Result<()> {
    let limits = setup(
        "reassign-parked",
        &format!("[handoff]\nmode = \"wait\"\n{PARK_FAST}"),
    )?;
    let mut pty = limits.start()?;
    let events = limits.wait_for("the run to park", CARD_WAIT, |events| {
        parks(events)
            .iter()
            .any(|reason| matches!(reason, ParkReason::WaitingForVendor { .. }))
    })?;
    let card = first_card(&events).context("no card")?;

    pty.send(&format!("/reassign {card} codex\r"))?;
    let events = limits.wait_for("Codex to finish the card", CARD_WAIT, completed_by("codex"))?;
    assert!(reassigned_by_you(&events, "codex"));
    let parked = seq_of(&events, |kind| {
        matches!(kind, RunEventKind::Run(RunLifecycle::Parked { .. }))
    });
    let moved = seq_of(&events, |kind| {
        matches!(kind, RunEventKind::Card(CardEvent::Reassigned { .. }))
    });
    assert!(before(parked, moved));
    limits.shot(&pty, "limits_reassign_any_time")
}

fn reassigned_by_you(events: &[RunEvent], worker: &str) -> bool {
    find(events, |kind| match kind {
        RunEventKind::Card(CardEvent::Reassigned { to, by, .. }) => Some((to, by)),
        _ => None,
    })
    .iter()
    .any(|(to, by)| to.worker == worker && **by == Party::User)
}

#[test]
fn limits_ask_timeout_while_parked_hands_off() -> anyhow::Result<()> {
    let limits = setup(
        "ask-timeout-while-parked",
        &format!(
            "[handoff]\nmode = \"ask\"\nask_timeout = \"5s\"\nfallback = [\"codex\"]\n{PARK_FAST}"
        ),
    )?;
    let pty = limits.start()?;
    let card = limits.wait_for_card(&pty, CheckpointKind::Handoff)?;
    limits.wait_for("the run to park on the card", CARD_WAIT, |events| {
        parks(events)
            .iter()
            .any(|reason| matches!(reason, ParkReason::Checkpoint))
    })?;

    // No key is pressed: the timeout wakes the run, which hands the card off by itself.
    let events = limits.wait_for("Codex to finish the card", WAKE_WAIT, completed_by("codex"))?;
    assert!(
        resumes(&events)
            .iter()
            .any(|resumed| resumed.cause == ResumeCause::AskTimeout)
    );
    let withdrawn = find(&events, |kind| match kind {
        RunEventKind::Checkpoint(CheckpointEvent::Withdrawn { id, .. }) => Some(id),
        _ => None,
    });
    assert!(
        withdrawn.contains(&&card.id),
        "the handoff card was not withdrawn"
    );
    assert!(handed_off(&events).iter().any(|event| matches!(
        event,
        HandoffEvent::HandedOff { to, mode: HandoffMode::Auto, .. } if to.worker == "codex"
    )));
    Ok(())
}

#[test]
fn limits_wait_mode_resumes_when_vendor_back() -> anyhow::Result<()> {
    // passes when: a waiting card goes back to Claude Code by resuming its session, or in a
    // fresh session whose handoff note carries the stopped session's last message.
    let limits = setup(
        "wait-mode-resumes",
        &format!("[handoff]\nmode = \"wait\"\nfallback = [\"codex\"]\n{PARK_FAST}"),
    )?;
    let pty = limits.start()?;
    let events = limits.wait_for("the run to wait for Claude Code", CARD_WAIT, |events| {
        parks(events).iter().any(|reason| {
            matches!(reason, ParkReason::WaitingForVendor { vendor, until: Some(_) } if vendor.0 == "claude-code")
        })
    })?;
    let until = parks(&events)
        .iter()
        .find_map(|reason| match reason {
            ParkReason::WaitingForVendor { until, .. } => *until,
            _ => None,
        })
        .context("no reset time")?;
    assert!(
        find(&events, |kind| match kind {
            RunEventKind::Handoff(HandoffEvent::Waiting { vendor, .. }) => Some(vendor),
            _ => None,
        })
        .iter()
        .any(|vendor| vendor.0 == "claude-code")
    );

    let events = limits.wait_for("Claude Code to finish the card", WAKE_WAIT, |events| {
        completed_by("claude-code")(events)
            && resumes(events)
                .iter()
                .any(|resumed| resumed.cause == ResumeCause::VendorBack)
    })?;
    let woke = events
        .iter()
        .find(|event| {
            matches!(&event.kind,
                RunEventKind::Run(RunLifecycle::Resumed(resumed)) if resumed.cause == ResumeCause::VendorBack)
        })
        .context("no wake")?;
    assert!(woke.ts >= until);
    assert!(limits.calls_with("codex", "exec")?.is_empty());
    limits.shot(&pty, "limits_wait_mode_resumes_when_vendor_back")
}

/// A handoff's whole round trip: a usage limit, a handoff with a checkpoint tag, the replacement's
/// commit, the original's return and review, and the merge.
#[test]
fn limits_handoff_checkpoint_roundtrip() -> anyhow::Result<()> {
    let limits = setup("handoff-checkpoint-roundtrip", FALLBACK_CODEX)?;
    let mut pty = limits.start()?;

    // 1. Claude Code hits its usage limit mid-card.
    let card = limits.wait_for_card(&pty, CheckpointKind::Handoff)?;
    let events = limits.events();
    let ends = ends_of(&events, "claude-code");
    assert!(matches!(
        ends.first().map(|end| &end.reason),
        Some(EndReason::UsageLimit { .. })
    ));

    // 2. The card picks Codex, and the tag `handoff-1` exists.
    let codex = key_for(
        &card,
        |action| matches!(action, AnswerAction::Candidate { worker } if worker.worker == "codex"),
    )
    .context("no key for Codex")?;
    answer(&mut pty, &card, codex)?;
    let events = limits.wait_for("the checkpoint tag", CARD_WAIT, |events| {
        seq_of(events, |kind| {
            matches!(
                kind,
                RunEventKind::Card(CardEvent::CheckpointTagged { n: 1, .. })
            )
        })
        .is_some()
    })?;
    let card_id = first_card(&events).context("no card")?;
    let tags = limits.repo.git(&["tag", "--list", "*/handoff-1"])?;
    assert!(tags.contains(&format!("{card_id}/handoff-1")), "{tags:?}");

    // 3. Codex commits.
    let events = limits.wait_for("Codex's commit", CARD_WAIT, |events| {
        find(events, |kind| match kind {
            RunEventKind::Card(CardEvent::Committed(info)) => info.by.as_ref(),
            _ => None,
        })
        .iter()
        .any(|by| by.worker == "codex")
    })?;
    assert!(
        find(&events, |kind| match kind {
            RunEventKind::Card(CardEvent::QualityDebt(debt)) => Some(debt),
            _ => None,
        })
        .iter()
        .any(|debt| debt.by.worker == "codex" && debt.since == 1)
    );

    // 4. Claude Code comes back, and the return card appears. You pick review.
    let back = limits.wait_for_card_within(&pty, CheckpointKind::OriginalReturned, WAKE_WAIT)?;
    let CheckpointPacket::OriginalReturned(packet) = &back.packet else {
        anyhow::bail!("not a return card: {:?}", back.packet);
    };
    assert_eq!(packet.original.worker, "claude-code");
    assert_eq!(packet.replacement.worker, "codex");
    assert_eq!(packet.since, 1);
    assert_eq!(option_keys(&back), ['r', 'o', 'u', 'y', 't', 'd']);
    limits.shot(&pty, "limits_handoff_checkpoint_roundtrip")?;
    answer(&mut pty, &back, 'r')?;

    // 5. The debt clears, and the merge card opens again without it. You merge.
    let events = limits.wait_for("the debt to clear", CARD_WAIT, |events| {
        find(events, |kind| match kind {
            RunEventKind::Card(CardEvent::DebtCleared { by }) => Some(by),
            _ => None,
        })
        .iter()
        .any(|by| by.worker == "claude-code")
    })?;
    let cleared = seq_of(&events, |kind| {
        matches!(kind, RunEventKind::Card(CardEvent::DebtCleared { .. }))
    })
    .context("no DebtCleared")?;
    let merge_after = |events: &[RunEvent]| {
        events
            .iter()
            .filter(|event| event.seq > cleared)
            .find_map(|event| match &event.kind {
                RunEventKind::Checkpoint(CheckpointEvent::Opened(opened))
                    if opened.kind == CheckpointKind::Merge =>
                {
                    Some(opened.as_ref().clone())
                }
                _ => None,
            })
    };
    let events = limits.wait_for("the merge card after the review", CARD_WAIT, |events| {
        merge_after(events).is_some()
    })?;
    let merge = merge_after(&events).context("the merge card went missing")?;
    let CheckpointPacket::Merge(packet) = &merge.packet else {
        anyhow::bail!("not a merge card: {:?}", merge.packet);
    };
    assert!(
        packet.rows.iter().all(|row| row.debt.is_empty()),
        "{packet:?}"
    );
    pty.wait_for(&regex::escape(&merge.title), WAIT)?;
    answer(&mut pty, &merge, 'y')?;
    pty.press(Key::Enter)?;
    limits.wait_for("the landing", CARD_WAIT, |events| {
        seq_of(events, |kind| {
            matches!(
                kind,
                RunEventKind::Run(RunLifecycle::Finished {
                    outcome: RunOutcome::Merged,
                    ..
                })
            )
        })
        .is_some()
    })?;
    assert!(
        limits.call_with("claude", "dontAsk")?.is_some(),
        "Claude Code never reviewed"
    );
    Ok(())
}
