//! Taking a card over from its running session, then letting the owner continue.

mod limits_support;

use anyhow::Context as _;
use argonauts_core::events::checkpoint::{CheckpointKind, CheckpointPacket};
use argonauts_core::events::{CardEvent, RunEventKind};
use argonauts_scenarios::WAIT;
use argonauts_testkit::Key;
use limits_support::*;

#[test]
fn limits_take_over_and_return() -> anyhow::Result<()> {
    let limits = setup("take-over-and-return", "")?;
    let mut pty = limits.start()?;
    limits.wait_for_call("claude", "acceptEdits", CARD_WAIT)?;

    // A solo run is one agent, so its panel row opens the owner's session directly,
    // whose `t` asks before handing the terminal over.
    pty.press(Key::Down)?;
    pty.press(Key::Enter)?;
    pty.wait_for("t take over", WAIT)?;
    pty.press(Key::Char('t'))?;
    confirm_take_over(&mut pty)?;

    let back = limits.wait_for_card(&pty, CheckpointKind::TakeOverReturn)?;
    let CheckpointPacket::TakeOverReturn(packet) = &back.packet else {
        anyhow::bail!("not a take-over return card: {:?}", back.packet);
    };
    assert_eq!(packet.owner.worker, "claude-code");
    assert_eq!(packet.changed.files_changed, 1);
    assert!(packet.commit.is_some(), "your changes were not committed");
    assert_eq!(option_keys(&back), ['c', 't', 'd']);
    let ui = limits
        .call_with("claude", "--resume")?
        .context("the vendor UI never ran")?;
    assert!(!ui.args.iter().any(|arg| arg == "-p"), "{:?}", ui.args);
    limits.shot(&pty, "limits_take_over_and_return")?;

    // Back from the views, the agent panel keeps the focus it had, and Esc gives it back to
    // the input, where ↑ reaches the card.
    pty.press(Key::Esc)?;
    wait_for_row(&pty, "the input's mode line", WAIT, |row| {
        row.contains("shift+tab to cycle") && !row.contains("esc to go back")
    })?;
    answer(&mut pty, &back, 'c')?;
    // The owner's work is committed a moment after its session ends.
    let events = limits.wait_for("the owner to finish and commit", CARD_WAIT, |events| {
        let yours = seq_of(events, |kind| {
            matches!(kind, RunEventKind::Card(CardEvent::TakeOverEnded { .. }))
        });
        completed_by("claude-code")(events)
            && yours.is_some_and(|yours| {
                events
                    .iter()
                    .any(|event| event.seq > yours && committed(&event.kind))
            })
    })?;
    let continued = limits
        .calls_with("claude", "--resume")?
        .into_iter()
        .any(|call| call.args.iter().any(|arg| arg == "-p"));
    assert!(continued, "the owner did not resume its session");
    assert!(
        seq_of(&events, |kind| matches!(
            kind,
            RunEventKind::Card(CardEvent::TakeOverEnded { .. })
        ))
        .is_some()
    );
    let branch = card_branch(&events).context("the card has no branch")?;
    let files = limits
        .repo
        .git(&["ls-tree", "-r", "--name-only", &branch])?;
    assert!(files.lines().any(|file| file == "yours.txt"), "{files}");
    assert!(files.lines().any(|file| file == "hello.txt"), "{files}");
    Ok(())
}
