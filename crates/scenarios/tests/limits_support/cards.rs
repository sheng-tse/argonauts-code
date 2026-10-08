//! Waiting for a card and answering it by key, as you would.

use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use argonauts_core::events::checkpoint::{
    AnswerAction, CheckpointKind, CheckpointOpened, CheckpointPacket, VendorDownPacket,
};
use argonauts_core::events::vendor::HandoffPlan;
use argonauts_scenarios::{WAIT, past_the_card_guard};
use argonauts_testkit::{Key, Pty};

use super::{CARD_WAIT, Limits, events::has_opened, events::opened};

impl Limits {
    /// Waits for a checkpoint of `kind` to open in the journal, then for its title on screen.
    pub fn wait_for_card(
        &self,
        pty: &Pty,
        kind: CheckpointKind,
    ) -> anyhow::Result<CheckpointOpened> {
        self.wait_for_card_within(pty, kind, CARD_WAIT)
    }

    /// As [`Limits::wait_for_card`], for a card that opens only after a vendor comes back.
    pub fn wait_for_card_within(
        &self,
        pty: &Pty,
        kind: CheckpointKind,
        timeout: Duration,
    ) -> anyhow::Result<CheckpointOpened> {
        let events = self.wait_for(&format!("{kind:?} task"), timeout, has_opened(kind))?;
        let card = opened(&events, kind)
            .cloned()
            .with_context(|| format!("the {kind:?} task went missing"))?;
        pty.wait_for(&regex::escape(&card.title), WAIT)?;
        Ok(card)
    }
}

/// Waits until a visible row passes `pick`, for text a plain pattern can't single out.
pub fn wait_for_row(
    pty: &Pty,
    what: &str,
    timeout: Duration,
    pick: impl Fn(&str) -> bool,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let screen = pty.screen();
        if screen.visible.iter().any(|row| pick(row)) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "no row with {what} in {timeout:?}; the screen was:\n{}",
                screen.visible_text()
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// The take-over choice, which asks before the screen hands the terminal over. A handoff or
/// budget card's own "Take over myself" option doesn't count.
pub fn confirm_take_over(pty: &mut Pty) -> anyhow::Result<()> {
    wait_for_row(pty, "the take-over choice", WAIT, |row| {
        row.contains("Take over") && !row.contains("Take over myself")
    })?;
    // You asked for it, so it opens with the keys, and its one option is number 1.
    pty.press(Key::Char('1'))
}

/// Focuses the card, as ↑ on an empty input does, and picks the option the event gives `key`
/// by the number the card shows beside it.
pub fn answer(pty: &mut Pty, card: &CheckpointOpened, key: char) -> anyhow::Result<()> {
    let index = card
        .options
        .iter()
        .position(|option| option.key == key)
        .with_context(|| format!("no option `{key}` on {}", card.title))?;
    let number = char::from_digit(index as u32 + 1, 10).context("more than 9 options")?;
    past_the_card_guard();
    pty.press(Key::Up)?;
    pty.press(Key::Char(number))
}

/// The option keys in the order the card shows them.
pub fn option_keys(card: &CheckpointOpened) -> Vec<char> {
    card.options.iter().map(|option| option.key).collect()
}

/// The key of the first option whose action `pick` accepts.
pub fn key_for(card: &CheckpointOpened, pick: impl Fn(&AnswerAction) -> bool) -> Option<char> {
    card.options
        .iter()
        .find(|option| pick(&option.action))
        .map(|option| option.key)
}

/// Whether `wanted` appear in `keys` in this order, others allowed between them.
pub fn in_order(keys: &[char], wanted: &[char]) -> bool {
    let mut rest = keys.iter();
    wanted.iter().all(|want| rest.any(|key| key == want))
}

/// The candidates a handoff card offers, by worker id, in order.
pub fn candidate_workers(card: &CheckpointOpened) -> Vec<String> {
    card.options
        .iter()
        .filter_map(|option| match &option.action {
            AnswerAction::Candidate { worker } => Some(worker.worker.clone()),
            _ => None,
        })
        .collect()
}

/// A handoff card's plan: who stopped, why, and the candidates.
pub fn handoff_plan(card: &CheckpointOpened) -> anyhow::Result<&HandoffPlan> {
    match &card.packet {
        CheckpointPacket::Handoff(plan) => Ok(plan),
        other => anyhow::bail!("not a fallback question: {other:?}"),
    }
}

pub fn vendor_down_packet(card: &CheckpointOpened) -> anyhow::Result<&VendorDownPacket> {
    match &card.packet {
        CheckpointPacket::VendorDown(packet) => Ok(packet),
        other => anyhow::bail!("not an agent down task: {other:?}"),
    }
}
