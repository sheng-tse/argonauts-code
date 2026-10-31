//! Waiting for a question and answering it by key, as you would.

use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use argonauts_core::runs::events::agent_status::FallbackPlan;
use argonauts_core::runs::events::approval::{
    AgentDownPacket, AnswerAction, ApprovalKind, ApprovalOpened, ApprovalPacket,
};
use argonauts_scenarios::{WAIT, past_the_question_guard};
use argonauts_testkit::{Key, Pty};

use super::{LONG_WAIT, Limits, events::has_opened, events::opened};

impl Limits {
    /// Waits for an approval of `kind` to open in the journal, then for its title on screen.
    pub fn wait_for_question(
        &self,
        pty: &Pty,
        kind: ApprovalKind,
    ) -> anyhow::Result<ApprovalOpened> {
        self.wait_for_question_within(pty, kind, LONG_WAIT)
    }

    /// As [`Limits::wait_for_question`], for one that opens only after an agent comes back.
    pub fn wait_for_question_within(
        &self,
        pty: &Pty,
        kind: ApprovalKind,
        timeout: Duration,
    ) -> anyhow::Result<ApprovalOpened> {
        let events = self.wait_for(&format!("{kind:?} task"), timeout, has_opened(kind))?;
        let question = opened(&events, kind)
            .cloned()
            .with_context(|| format!("the {kind:?} task went missing"))?;
        match kind {
            // This one says its title's first words on its `◆` row, and the tasks that wait on
            // rows under it.
            ApprovalKind::AgentDown => {
                let headline = question.title.split(" · ").next().unwrap_or_default();
                wait_for_row(pty, "the question's headline", WAIT, |row| {
                    row.trim().strip_prefix("◆ ") == Some(headline)
                })?;
            }
            // A long title wraps onto the rows under its own, so it is read across them.
            _ => {
                wait_for_rows(pty, "the question's title", WAIT, |rows| {
                    rows.contains(&question.title)
                })?;
            }
        }
        Ok(question)
    }
}

/// Waits until the visible rows, read as one line with each row's own spaces trimmed, pass
/// `pick`: for text that wraps.
pub fn wait_for_rows(
    pty: &Pty,
    what: &str,
    timeout: Duration,
    pick: impl Fn(&str) -> bool,
) -> anyhow::Result<()> {
    let deadline = argonauts_testkit::pty::deadline::after(timeout);
    loop {
        let screen = pty.screen();
        let read: Vec<&str> = screen.visible.iter().map(|row| row.trim()).collect();
        if pick(&read.join(" ")) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "no {what} in {timeout:?}; the screen was:\n{}",
                screen.visible_text()
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Waits until a visible row passes `pick`, for text a plain pattern can't single out.
pub fn wait_for_row(
    pty: &Pty,
    what: &str,
    timeout: Duration,
    pick: impl Fn(&str) -> bool,
) -> anyhow::Result<()> {
    let deadline = argonauts_testkit::pty::deadline::after(timeout);
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

/// The take-over choice, which asks before the screen hands the terminal over. A fallback or
/// limit question's own "Take over myself" option doesn't count.
pub fn confirm_take_over(pty: &mut Pty) -> anyhow::Result<()> {
    wait_for_row(pty, "the take-over choice", WAIT, |row| {
        row.contains("Take over") && !row.contains("Take over myself")
    })?;
    // You asked for it, so it opens with the keys, and its one option is number 1.
    pty.press(Key::Char('1'))
}

/// Focuses the question, as ↑ on an empty input does, and picks the option the event gives `key`
/// by the number the question shows beside it.
pub fn answer(pty: &mut Pty, question: &ApprovalOpened, key: char) -> anyhow::Result<()> {
    let index = question
        .options
        .iter()
        .position(|option| option.key == key)
        .with_context(|| format!("no option `{key}` on {}", question.title))?;
    let number = char::from_digit(index as u32 + 1, 10).context("more than 9 options")?;
    past_the_question_guard();
    pty.press(Key::Up)?;
    pty.press(Key::Char(number))
}

/// The option keys in the order the question shows them.
pub fn option_keys(question: &ApprovalOpened) -> Vec<char> {
    question.options.iter().map(|option| option.key).collect()
}

/// The key of the first option whose action `pick` accepts.
pub fn key_for(question: &ApprovalOpened, pick: impl Fn(&AnswerAction) -> bool) -> Option<char> {
    question
        .options
        .iter()
        .find(|option| pick(&option.action))
        .map(|option| option.key)
}

/// Whether `wanted` appear in `keys` in this order, others allowed between them.
pub fn in_order(keys: &[char], wanted: &[char]) -> bool {
    let mut rest = keys.iter();
    wanted.iter().all(|want| rest.any(|key| key == want))
}

/// The candidates a fallback question offers, by worker id, in order.
pub fn candidate_workers(question: &ApprovalOpened) -> Vec<String> {
    question
        .options
        .iter()
        .filter_map(|option| match &option.action {
            AnswerAction::Candidate { worker } => Some(worker.agent.clone()),
            _ => None,
        })
        .collect()
}

/// A fallback question's plan: who stopped, why, and the candidates.
pub fn fallback_plan(question: &ApprovalOpened) -> anyhow::Result<&FallbackPlan> {
    match &question.packet {
        ApprovalPacket::Fallback(plan) => Ok(plan),
        other => anyhow::bail!("not a fallback question: {other:?}"),
    }
}

pub fn agent_down_packet(question: &ApprovalOpened) -> anyhow::Result<&AgentDownPacket> {
    match &question.packet {
        ApprovalPacket::AgentDown(packet) => Ok(packet),
        other => anyhow::bail!("not an agent down task: {other:?}"),
    }
}
