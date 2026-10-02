//! Which card typed answers go to, and what a pick does: sends the answer, asks again before
//! an option that can't be undone, switches Remember this pick, or reads Show the diff out
//! while the card keeps waiting.

use argonauts_core::app::{AnswerTarget, AppEvent, UiCommand};
use argonauts_core::events::RunEventKind;
use argonauts_core::events::checkpoint::CheckpointEvent;

use crate::state::NeedsYou;
use crate::text::sanitize_line;

use super::cards::{CardList, ListTarget, Spoken};
use super::pick::{Pick, Typed};
use super::reader::{Reader, Said};

/// An option that asks again, picked once. The same option next sends it.
#[derive(Debug)]
pub struct Armed {
    target: ListTarget,
    pick: Pick,
    /// Such as `merge`.
    verb: String,
}

impl Armed {
    fn cancelled(&self) -> String {
        format!("{}: cancelled, nothing was sent.", self.verb)
    }
}

impl Reader {
    /// A line that isn't a command or a mention: an answer to the card that waits, or a
    /// message.
    pub(super) fn plain(&mut self, text: String, said: &mut Said) {
        let card = self.current_card();
        let typed = card
            .as_ref()
            .map_or(Typed::Message, |card| card.parse(&text));
        let armed = match (self.armed.take(), &card, &typed) {
            (Some(armed), Some(card), Typed::Pick(Pick::Option { index, .. }))
                if armed.target == card.target && same_option(&armed.pick, *index) =>
            {
                Some(armed)
            }
            (Some(armed), ..) => {
                said.lines.push(armed.cancelled());
                None
            }
            (None, ..) => None,
        };
        match (card, typed) {
            (Some(card), Typed::Pick(pick)) => match armed {
                Some(armed) => self.send(&card, &with_note(armed.pick, pick), said),
                None => self.pick(card, pick, said),
            },
            (_, Typed::Warning(warning)) => said.lines.push(format!("warning: {warning}")),
            _ => said.commands.push(UiCommand::Submit { text }),
        }
    }

    fn pick(&mut self, card: CardList, pick: Pick, said: &mut Said) {
        match pick {
            Pick::OwnAnswer { text: None } => {
                said.lines
                    .push("answer: type your answer and press Enter.".into());
                self.own_answer = Some(card);
            }
            Pick::View { .. } => self.show(&card, &pick, said),
            Pick::Remember => {
                said.lines.push(card.picked(&pick));
                if let ListTarget::Card(target) = &card.target {
                    if card.remember == Some(true) {
                        self.remember.retain(|on| on != target);
                    } else {
                        self.remember.push(target.clone());
                    }
                }
            }
            Pick::Option { index, .. } if card.asks_again(index) => {
                said.lines.push(card.ask_again(index));
                self.armed = Some(Armed {
                    verb: card.verb(index),
                    target: card.target,
                    pick,
                });
            }
            pick => self.send(&card, &pick, said),
        }
    }

    /// Your own answer, typed on the line after its number.
    pub(super) fn own_answer(&mut self, card: CardList, line: &str, said: &mut Said) {
        let pick = Pick::OwnAnswer {
            text: Some(line.trim().to_owned()).filter(|text| !text.is_empty()),
        };
        match card.command(&pick) {
            Some(_) => self.send(&card, &pick, said),
            None => self.own_answer = Some(card),
        }
    }

    fn send(&mut self, card: &CardList, pick: &Pick, said: &mut Said) {
        let Some(command) = card.command(pick) else {
            return;
        };
        said.lines
            .push(sanitize_line(&card.picked(pick)).into_owned());
        said.commands.push(command);
        match &card.target {
            ListTarget::Card(target) => {
                self.answered.push(target.clone());
                self.remember.retain(|on| on != target);
            }
            ListTarget::SpotCheck { run, id } => self.decisions.checked(run, id),
        }
        if let Some(next) = self.current_card() {
            said.lines.push("still waiting:".to_owned());
            said.lines.extend(next.lines());
            self.card = Some(next.target);
        }
    }

    /// Show the diff, the details, the file, or the evidence. Nothing is answered.
    fn show(&mut self, card: &CardList, pick: &Pick, said: &mut Said) {
        said.lines
            .push(sanitize_line(&card.picked(pick)).into_owned());
        match card.view() {
            Spoken::Request(view) => said.commands.push(UiCommand::Request { view }),
            Spoken::Lines(lines) => said.lines.extend(lines),
            Spoken::Warning(text) => said
                .lines
                .push(format!("warning: {}", sanitize_line(&text))),
        }
        said.lines.push(card.how_to_answer());
        self.card = Some(card.target.clone());
    }

    /// Keeps the card that numbers answer: the newest one read out, and when it closes, the
    /// oldest one still waiting, read out again. A spot check waits behind the cards.
    pub(super) fn follow_cards(&mut self, event: &AppEvent, said: &mut Said) {
        let waiting = self.waiting();
        let needs_you = self.state.needs_you();
        self.answered
            .retain(|target| needs_you.iter().any(|card| target_of(card) == *target));
        let opened = opened_card(event).map(ListTarget::Card);
        // Once the card closes or another opens, typing the same key again could answer
        // something you haven't heard ask.
        if let Some(armed) = self
            .armed
            .take_if(|armed| opened.is_some() || !waiting.contains(&armed.target))
        {
            said.lines.push(armed.cancelled());
        }
        if let Some(opened) = opened
            && !self.state.replaying()
            && waiting.contains(&opened)
        {
            self.card = Some(opened);
            return;
        }
        let still = self
            .card
            .as_ref()
            .is_some_and(|card| waiting.contains(card));
        let caught_up = matches!(event, AppEvent::ReplayDone { .. });
        if (still && !caught_up) || self.state.replaying() {
            return;
        }
        self.card = None;
        if let Some(next) = self.current_card() {
            if !queued_spot_check(event, &next.target) {
                said.lines.push("still waiting:".to_owned());
            }
            said.lines.extend(next.lines());
            self.card = Some(next.target);
        }
    }

    /// Cards waiting on you that weren't answered here, oldest first, then spot checks.
    fn waiting(&self) -> Vec<ListTarget> {
        let needs_you = self.state.needs_you();
        let cards = needs_you
            .iter()
            .map(target_of)
            .filter(|target| !self.answered.contains(target))
            .map(ListTarget::Card);
        let spot_checks =
            self.decisions
                .spot_checks()
                .iter()
                .map(|decision| ListTarget::SpotCheck {
                    run: decision.run.clone(),
                    id: decision.opened.id.clone(),
                });
        cards.chain(spot_checks).collect()
    }

    pub(super) fn current_card(&self) -> Option<CardList> {
        let waiting = self.waiting();
        let target = self
            .card
            .as_ref()
            .filter(|card| waiting.contains(card))
            .or_else(|| waiting.first())?;
        match target {
            ListTarget::Card(target) => {
                let mut card = CardList::for_target(&self.state, target)?;
                if card.remember.is_some() {
                    card.remember = Some(self.remember.contains(target));
                }
                Some(card)
            }
            ListTarget::SpotCheck { run, id } => {
                let decision = self.decisions.spot_check(run, id)?;
                let offset = self.state.clock_offset();
                Some(CardList::spot_check(decision, offset, &self.state.names))
            }
        }
    }
}

fn same_option(armed: &Pick, index: usize) -> bool {
    matches!(armed, Pick::Option { index: at, .. } if *at == index)
}

/// The armed pick, with the note typed when confirming it if there is one.
fn with_note(armed: Pick, confirm: Pick) -> Pick {
    match (armed, confirm) {
        (
            Pick::Option { index, note, cards },
            Pick::Option {
                note: confirm_note, ..
            },
        ) => Pick::Option {
            index,
            note: confirm_note.or(note),
            cards,
        },
        (armed, _) => armed,
    }
}

fn target_of(card: &NeedsYou) -> AnswerTarget {
    match card {
        NeedsYou::Checkpoint { run, id, .. } => AnswerTarget::Checkpoint {
            run: run.clone(),
            id: id.clone(),
        },
        NeedsYou::Choice { id, .. } => AnswerTarget::Choice { id: id.clone() },
    }
}

/// The card `event` read out, if it opened one that needs you.
fn opened_card(event: &AppEvent) -> Option<AnswerTarget> {
    match event {
        AppEvent::Choice { choice } => Some(AnswerTarget::Choice {
            id: choice.id.clone(),
        }),
        AppEvent::Run { event } => match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) if opened.needs_you => {
                Some(AnswerTarget::Checkpoint {
                    run: event.run_id.clone(),
                    id: opened.id.clone(),
                })
            }
            _ => None,
        },
        _ => None,
    }
}

/// Whether `event` queued the spot check `target`, so it's news rather than still waiting.
fn queued_spot_check(event: &AppEvent, target: &ListTarget) -> bool {
    let AppEvent::Run { event } = event else {
        return false;
    };
    matches!(
        (&event.kind, target),
        (
            RunEventKind::Checkpoint(CheckpointEvent::SpotCheckQueued { id }),
            ListTarget::SpotCheck { run, id: queued },
        ) if *run == event.run_id && id == queued
    )
}
