//! ◆ cards: a run's questions and approvals and the app's own choices, drawn in the live
//! region in place of the input like Claude Code's permission prompt. Each takes the keys the
//! moment it shows. Esc on one a run waits on hands it on, so whoever asked decides and goes
//! on; Esc on a choice you set off yourself closes it. Every answer is bound to
//! the evidence hash you saw.

mod about;
mod checkpoint;
mod choice;
mod confirm;
mod item;
mod options;
mod plan_workflow;
mod records;
mod spot;
mod table;
mod views;
mod words;

use std::collections::{BTreeMap, VecDeque};

use argonauts_core::app::{AnswerTarget, AppChoice, AppEvent, ChoiceKind, UiCommand};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::checkpoint::{
    AnswerAction, AnswerOption, CheckpointDecided, CheckpointEvent, CheckpointKind,
    CheckpointOpened, PolicyCheck,
};
use argonauts_core::events::run::Party;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::CheckpointId;
use argonauts_core::policy::Verdict;
use argonauts_core::{CardId, RunId};
use time::{OffsetDateTime, UtcOffset};

use crate::app::UiAction;
use crate::bottom::{AnsweredPick, CardPrompt};
use crate::bridge::Cx;
use crate::cell::Cell;
use crate::state::DecisionRecord;
use crate::text::sanitize_line;
use crate::views::ViewOutcome;

pub(crate) use about::details;
pub use about::{about_cells, about_cells_local, about_item, with_run_owners};
pub(crate) use checkpoint::checklist_cards;
pub use checkpoint::{REMEMBER_LABEL, checkpoint_card, checkpoint_card_local};
pub use choice::choice_card;
pub use confirm::confirm_take_over;
pub use item::{AboutCell, Block};
pub use records::record_cell;
pub use spot::spot_check_card;
pub(crate) use spot::spot_check_options;
pub use table::{Column, Table, TableCell};
pub(crate) use views::{Shown, checkpoint_view, choice_view};
pub(crate) use words::joined;

/// Auto decisions kept for a spot check that may follow them.
const RECENT_DECISIONS: usize = 32;

#[derive(Debug, Default)]
pub struct Cards {
    /// Every card on screen, waiting behind it, or handed on, by the kit's card id.
    pub open: BTreeMap<u64, OpenCard>,
    next_id: u64,
    /// What each open card was built from, and its prompt while another card is on screen.
    held: BTreeMap<u64, Held>,
    /// Checkpoints opened and not yet decided or withdrawn, for their record lines.
    checkpoints: BTreeMap<(RunId, CheckpointId), Box<CheckpointOpened>>,
    /// Recent decisions, newest last, for the spot checks that follow auto decisions.
    decisions: VecDeque<Decision>,
}

/// What an answer to a card needs to become a `UiCommand`.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenCard {
    pub target: CardTarget,
    pub options: Vec<AnswerOption>,
    /// From `CheckpointOpened`, sent back unchanged with the answer.
    pub evidence_sha256: Option<String>,
    /// The handoff card's Remember this pick toggle.
    pub remember: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CardTarget {
    Controller(AnswerTarget),
    /// A spot check of an auto decision, answered with `UiCommand::SpotCheck`.
    SpotCheck {
        run: RunId,
        id: CheckpointId,
    },
    /// Built here and answered here, such as the confirm before a take-over or a stop.
    Local(LocalConfirm),
}

#[derive(Debug, Clone, PartialEq)]
pub enum LocalConfirm {
    TakeOver {
        run: argonauts_core::RunId,
        card: argonauts_core::CardId,
        /// The handoff card whose "Take over myself" this confirms. It waits under the confirm,
        /// and the engine answers it with the take-over.
        answering: Option<u64>,
    },
}

#[derive(Debug)]
struct Held {
    source: Source,
    /// The card's prompt while another card is on screen.
    stashed: Option<CardPrompt>,
    /// The merge rows Request changes ticks, in the checklist's order.
    checklist: Vec<CardId>,
    /// The option index of the handoff card's remember toggle.
    toggle: Option<usize>,
}

#[derive(Debug)]
enum Source {
    Checkpoint(Box<CheckpointOpened>),
    SpotCheck(Box<Decision>),
    Choice(Box<AppChoice>),
    Local,
}

#[derive(Debug, Clone)]
struct Decision {
    run: RunId,
    opened: Box<CheckpointOpened>,
    decided: Option<CheckpointDecided>,
    check: Option<PolicyCheck>,
}

impl Cards {
    /// `Checkpoint::Opened`, `Checked`, `SpotCheckQueued`, `Decided`, and `Withdrawn`; `Choice`
    /// and `ChoiceClosed`. Rings the bell for a card that needs you; the bridge keeps replay
    /// quiet.
    pub fn on_event(&mut self, event: &AppEvent, cx: &mut Cx<'_>) {
        match event {
            AppEvent::Run { event } => {
                if let RunEventKind::Checkpoint(checkpoint) = &event.kind {
                    self.on_checkpoint(event, checkpoint, cx);
                }
            }
            AppEvent::Choice { choice } => self.open_choice(choice, cx),
            AppEvent::ChoiceClosed { id, outcome } => {
                let target = CardTarget::Controller(AnswerTarget::Choice { id: id.clone() });
                let title = self
                    .close_target(&target, cx)
                    .and_then(|held| match held.source {
                        // A preview's outcome names what it closes; its plan stays above.
                        Source::Choice(choice) if choice.kind == ChoiceKind::TryWorkflow => None,
                        Source::Choice(choice) => Some(choice.title),
                        _ => None,
                    });
                if !cx.replaying() {
                    cx.model
                        .history
                        .push(records::choice_closed_cell(title.as_deref(), outcome));
                }
                self.show(cx);
            }
            AppEvent::Detached { run } => {
                let bottom = &mut cx.model.bottom;
                if bottom
                    .answered_pick
                    .as_ref()
                    .is_some_and(|pick| pick.run == *run)
                {
                    bottom.answered_pick = None;
                }
                let gone: Vec<u64> = self
                    .open
                    .iter()
                    .filter(|(_, card)| card.target.run() == Some(run))
                    .map(|(id, _)| *id)
                    .collect();
                for id in gone {
                    self.close(id, cx);
                }
                self.checkpoints.retain(|(owner, _), _| owner != run);
                self.show(cx);
            }
            _ => {}
        }
    }

    /// Takes `Answer`, `OpenView`, `Dismiss`, `AnswerCard`, and `ConfirmTakeOver`, and sees a
    /// spot check marked in `/approvals` go out.
    pub fn on_action(&mut self, action: &UiAction, cx: &mut Cx<'_>) -> bool {
        match action {
            UiAction::Answer {
                card,
                option,
                note,
                checked,
            } => self.answer(*card, *option, note.clone(), checked, cx),
            UiAction::OpenView { card, option } => self.open_view(*card, *option, cx),
            UiAction::Dismiss { card } => self.dismiss(*card, cx),
            UiAction::AnswerCard(target) => {
                let wanted = CardTarget::Controller(target.clone());
                let Some(id) = self.find(&wanted) else {
                    return false;
                };
                self.display(id, true, cx);
                cx.out.close_views = true;
                true
            }
            UiAction::Command(UiCommand::SpotCheck { run, id, .. }) => {
                // Marked in `/approvals`, so its card has nothing left to ask. The command
                // still goes out.
                let target = CardTarget::SpotCheck {
                    run: run.clone(),
                    id: id.clone(),
                };
                if self.close_target(&target, cx).is_some() {
                    self.show(cx);
                }
                false
            }
            UiAction::ConfirmTakeOver { run, card } => {
                let agent = owner_name(cx, run, card);
                let id = self.next_card_id();
                let prompt = confirm_take_over(id, run, card, &agent);
                let target = LocalConfirm::TakeOver {
                    run: run.clone(),
                    card: card.clone(),
                    answering: None,
                };
                self.raise_local(id, prompt, target, confirm::take_over_options(), cx);
                true
            }
            _ => false,
        }
    }

    pub fn next_card_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn on_checkpoint(&mut self, event: &RunEvent, checkpoint: &CheckpointEvent, cx: &mut Cx<'_>) {
        let run = &event.run_id;
        match checkpoint {
            CheckpointEvent::Opened(opened) => self.open_checkpoint(run, opened, cx),
            CheckpointEvent::Checked(check) => self.checked(run, check, cx),
            CheckpointEvent::Decided(decided) => self.decided(run, decided, event.ts, cx),
            CheckpointEvent::Withdrawn { id, reason } => {
                let opened = self.checkpoints.remove(&(run.clone(), id.clone()));
                let target = checkpoint_target(run, id);
                let held = self.close_target(&target, cx);
                let title = opened
                    .map(|opened| opened.title)
                    .or_else(|| held.map(|held| held.source.title()));
                if let Some(title) = title
                    && !cx.replaying()
                {
                    cx.model
                        .history
                        .push(records::withdrawn_cell(&title, reason));
                }
                self.show(cx);
            }
            CheckpointEvent::SpotCheckQueued { id } => self.spot_check(run, id, cx),
        }
    }

    fn open_checkpoint(&mut self, run: &RunId, opened: &CheckpointOpened, cx: &mut Cx<'_>) {
        let opened = &with_run_owners(opened, cx.state.runs.get(run), &cx.state.names);
        // A checkpoint opened again replaces its card, such as after the evidence changed.
        let target = checkpoint_target(run, &opened.id);
        self.close_target(&target, cx);
        if !cx.replaying() {
            let run_state = cx.state.runs.get(run);
            for cell in about_cells_local(opened, run_state, clock_offset(cx), &cx.state.names) {
                cx.model.history.push(cell);
            }
        }
        let id = self.next_card_id();
        let run_state = cx.state.runs.get(run);
        let mut prompt =
            checkpoint_card_local(id, opened, run_state, clock_offset(cx), &cx.state.names);
        if let Some(agent) = &mut prompt.agent {
            agent.color = cx.model.agent_colors.get(&agent.worker);
        }
        self.open.insert(
            id,
            OpenCard {
                target,
                options: opened.options.clone(),
                evidence_sha256: Some(opened.evidence_sha256.clone()),
                remember: false,
            },
        );
        self.held.insert(
            id,
            Held {
                source: Source::Checkpoint(Box::new(opened.clone())),
                stashed: Some(prompt),
                checklist: checkpoint::checklist_cards(opened),
                toggle: checkpoint::remember_toggle(opened),
            },
        );
        self.checkpoints
            .insert((run.clone(), opened.id.clone()), Box::new(opened.clone()));
        if opened.needs_you {
            cx.out.ring();
        }
        self.show(cx);
    }

    /// A checkpoint your rules couldn't settle now waits for you. In auto its card says why.
    fn checked(&mut self, run: &RunId, check: &PolicyCheck, cx: &mut Cx<'_>) {
        let key = (run.clone(), check.id.clone());
        let why = auto_left_it(check);
        // A check with no verdict is the policy leaving it to you, with or without a reason.
        let waits = !check.shadow
            && (!check.violations.is_empty()
                || why.is_some()
                || matches!(
                    check.verdict,
                    None | Some(Verdict::Reject | Verdict::Escalate)
                ));
        if let Some(why) = why {
            self.add_detail(&checkpoint_target(run, &check.id), why, cx);
        }
        let was_quiet = self
            .checkpoints
            .get(&key)
            .is_some_and(|opened| !opened.needs_you);
        if waits && was_quiet {
            cx.out.ring();
            if let Some(id) = self.find(&checkpoint_target(run, &check.id)) {
                self.needs_you(id, cx);
                self.show(cx);
            }
        }
        if let Some(decision) = self
            .decisions
            .iter_mut()
            .rev()
            .find(|decision| decision.run == *run && decision.opened.id == check.id)
        {
            decision.check = Some(check.clone());
        } else if let Some(opened) = self.checkpoints.get(&key) {
            self.remember_decision(Decision {
                run: run.clone(),
                opened: opened.clone(),
                decided: None,
                check: Some(check.clone()),
            });
        }
    }

    fn decided(
        &mut self,
        run: &RunId,
        decided: &CheckpointDecided,
        at: OffsetDateTime,
        cx: &mut Cx<'_>,
    ) {
        let opened = self.checkpoints.remove(&(run.clone(), decided.id.clone()));
        let target = checkpoint_target(run, &decided.id);
        self.close_target(&target, cx);
        if let Some(opened) = opened {
            if !cx.replaying() {
                // The screen shows one run at a time, so the record says "the run", and its
                // cards go by their labels, as the rest of the transcript names them.
                let shown = cx.state.runs.get(run);
                let label = |card: &CardId| {
                    shown.map_or_else(|| card.to_string(), |shown| shown.card_label(card))
                };
                let record = records::record(
                    &opened,
                    decided,
                    None,
                    Some(at),
                    clock_offset(cx),
                    &cx.state.names,
                    &label,
                );
                cx.model.history.push(Cell::Notice(record));
            }
            let check = self
                .decisions
                .iter()
                .rev()
                .find(|decision| decision.run == *run && decision.opened.id == decided.id)
                .and_then(|decision| decision.check.clone());
            self.decisions
                .retain(|decision| !(decision.run == *run && decision.opened.id == decided.id));
            self.remember_decision(Decision {
                run: run.clone(),
                opened,
                decided: Some(decided.clone()),
                check,
            });
        }
        self.show(cx);
    }

    fn remember_decision(&mut self, decision: Decision) {
        self.decisions.push_back(decision);
        while self.decisions.len() > RECENT_DECISIONS {
            self.decisions.pop_front();
        }
    }

    /// No bell: a spot check is yours to do when you like.
    fn spot_check(&mut self, run: &RunId, id: &CheckpointId, cx: &mut Cx<'_>) {
        let target = CardTarget::SpotCheck {
            run: run.clone(),
            id: id.clone(),
        };
        if self.find(&target).is_some() {
            return;
        }
        let Some(decision) = self
            .decisions
            .iter()
            .rev()
            .find(|decision| decision.run == *run && decision.opened.id == *id)
            .cloned()
        else {
            // Decided before this screen attached: the spot check waits in `/approvals`.
            return;
        };
        let card = self.next_card_id();
        let prompt = spot_check_card(
            card,
            &decision.opened,
            decision.decided.as_ref(),
            Some(run),
            &cx.state.names,
        );
        self.open.insert(
            card,
            OpenCard {
                target,
                options: spot::spot_check_options(),
                evidence_sha256: Some(decision.opened.evidence_sha256.clone()),
                remember: false,
            },
        );
        self.held.insert(
            card,
            Held {
                source: Source::SpotCheck(Box::new(decision)),
                stashed: Some(prompt),
                checklist: Vec::new(),
                toggle: None,
            },
        );
        self.show(cx);
    }

    fn open_choice(&mut self, choice: &AppChoice, cx: &mut Cx<'_>) {
        let target = CardTarget::Controller(AnswerTarget::Choice {
            id: choice.id.clone(),
        });
        self.close_target(&target, cx);
        let id = self.next_card_id();
        self.open.insert(
            id,
            OpenCard {
                target,
                options: choice.options.clone(),
                evidence_sha256: None,
                remember: false,
            },
        );
        self.held.insert(
            id,
            Held {
                source: Source::Choice(Box::new(choice.clone())),
                stashed: Some(choice_card(id, choice)),
                checklist: Vec::new(),
                toggle: None,
            },
        );
        if choice.bell {
            cx.out.ring();
        }
        // A change you made in a view, such as /approvals, asks here; the view would hide it.
        if matches!(
            choice.kind,
            ChoiceKind::ConfirmSafetySetting | ChoiceKind::SettingScope
        ) {
            cx.out.close_views = true;
        }
        self.show(cx);
    }

    fn raise_local(
        &mut self,
        id: u64,
        prompt: CardPrompt,
        confirm: LocalConfirm,
        options: Vec<AnswerOption>,
        cx: &mut Cx<'_>,
    ) {
        self.open.insert(
            id,
            OpenCard {
                target: CardTarget::Local(confirm),
                options,
                evidence_sha256: None,
                remember: false,
            },
        );
        self.held.insert(
            id,
            Held {
                source: Source::Local,
                stashed: Some(prompt),
                checklist: Vec::new(),
                toggle: None,
            },
        );
        // You asked for it with a key, so it takes the keys at once, over any open view.
        self.display(id, true, cx);
        cx.out.close_views = true;
    }

    fn answer(
        &mut self,
        card: u64,
        option: usize,
        note: Option<String>,
        checked: &[usize],
        cx: &mut Cx<'_>,
    ) -> bool {
        if let Some((run, taken)) = self.hands_over(card, option) {
            let id = self.next_card_id();
            let agent = owner_name(cx, &run, &taken);
            let prompt = confirm_take_over(id, &run, &taken, &agent);
            let target = LocalConfirm::TakeOver {
                run,
                card: taken,
                answering: Some(card),
            };
            self.raise_local(id, prompt, target, confirm::take_over_options(), cx);
            return true;
        }
        let (Some(open), Some(held)) = (self.open.remove(&card), self.held.remove(&card)) else {
            return false;
        };
        if cx
            .model
            .bottom
            .card
            .as_ref()
            .is_some_and(|shown| shown.id == card)
        {
            cx.model.bottom.card = None;
        }
        let picked = open
            .options
            .get(option)
            .filter(|_| held.toggle != Some(option));
        cx.model.bottom.answered_pick = picked
            .filter(|_| !open.remember)
            .and_then(|picked| answered_pick(&open.target, &held.source, picked, &cx.state.names));
        match (open.target, picked) {
            (_, None) => {}
            (_, Some(picked)) if picked.action == AnswerAction::ShowDiff => {}
            (CardTarget::Controller(to), Some(picked)) => {
                let cards = if picked.action == AnswerAction::RequestChanges {
                    checked
                        .iter()
                        .filter_map(|index| held.checklist.get(*index).cloned())
                        .collect()
                } else {
                    Vec::new()
                };
                let remember =
                    open.remember && matches!(picked.action, AnswerAction::Candidate { .. });
                cx.out.send(UiCommand::Answer {
                    to,
                    action: picked.action.clone(),
                    text: note,
                    evidence_sha256: open.evidence_sha256,
                    cards,
                    remember,
                });
            }
            (CardTarget::SpotCheck { run, id }, Some(picked)) => {
                cx.out.send(UiCommand::SpotCheck {
                    run,
                    id,
                    correct: picked.action == AnswerAction::Approve,
                });
            }
            (
                CardTarget::Local(LocalConfirm::TakeOver {
                    run,
                    card,
                    answering,
                }),
                Some(_),
            ) => {
                if let Some(answered) = answering {
                    self.close(answered, cx);
                }
                cx.out.send(UiCommand::TakeOver { run, card });
            }
        }
        self.show(cx);
        true
    }

    /// "Take over myself" on a handoff card hands the terminal to the agent's own UI, so it asks
    /// first, as a take-over from a session view does.
    fn hands_over(&self, card: u64, option: usize) -> Option<(RunId, CardId)> {
        let (open, held) = (self.open.get(&card)?, self.held.get(&card)?);
        let picked = open.options.get(option)?;
        let Source::Checkpoint(opened) = &held.source else {
            return None;
        };
        let CardTarget::Controller(AnswerTarget::Checkpoint { run, .. }) = &open.target else {
            return None;
        };
        let taken = opened.card.clone()?;
        (picked.action == AnswerAction::TakeOver
            && opened.kind == CheckpointKind::Handoff
            && held.toggle != Some(option))
        .then(|| (run.clone(), taken))
    }

    fn open_view(&mut self, card: u64, option: usize, cx: &mut Cx<'_>) -> bool {
        let (Some(open), Some(held)) = (self.open.get_mut(&card), self.held.get(&card)) else {
            return false;
        };
        if held.toggle == Some(option) {
            // The card flipped its own toggle; its state is what the answer carries.
            let shown = cx
                .model
                .bottom
                .card
                .as_ref()
                .filter(|shown| shown.id == card)
                .and_then(|shown| shown.options.get(option))
                .map(|toggle| toggle.on);
            open.remember = shown.unwrap_or(!open.remember);
            return true;
        }
        let shown = match (&open.target, &held.source) {
            (
                CardTarget::Controller(AnswerTarget::Checkpoint { run, .. }),
                Source::Checkpoint(opened),
            ) => views::checkpoint_view(run, opened, clock_offset(cx), &cx.state.names),
            (_, Source::SpotCheck(decision)) => views::evidence_view(
                &decision.run,
                &decision.opened,
                decision.decided.as_ref(),
                decision.check.as_ref(),
                clock_offset(cx),
                &cx.state.names,
            ),
            // What a repository's files say is the app's to read: it prints each one above the
            // question, which stays open.
            (CardTarget::Controller(to @ AnswerTarget::Choice { .. }), Source::Choice(choice))
                if choice.kind == ChoiceKind::TrustProject =>
            {
                cx.out.send(UiCommand::Answer {
                    to: to.clone(),
                    action: AnswerAction::ShowDiff,
                    text: None,
                    evidence_sha256: None,
                    cards: Vec::new(),
                    remember: false,
                });
                return true;
            }
            (_, Source::Choice(choice)) => views::choice_view(choice),
            _ => return true,
        };
        match shown {
            views::Shown::Request(view) => cx.out.send(UiCommand::Request { view }),
            views::Shown::Pager(pager) => cx.out.open(pager),
            // Why the key did nothing, on the notice line, never in the transcript.
            views::Shown::Notice(text) => crate::bridge::keys::notice(&text, cx),
        }
        true
    }

    /// Esc took the card off the screen with nothing picked. A choice closes as the controller
    /// says for its kind, and a confirm of this screen's own just closes. A run's question or
    /// approval is handed on: the engine has whoever asked decide and go on, and until it
    /// settles the card waits out of sight, where the list of what needs you still opens it.
    /// Nothing waits on a spot check, so it closes and stays in `/approvals`.
    fn dismiss(&mut self, card: u64, cx: &mut Cx<'_>) -> bool {
        let Some(open) = self.open.get(&card) else {
            return false;
        };
        match &open.target {
            CardTarget::Controller(to @ AnswerTarget::Choice { .. }) => {
                cx.out.send(UiCommand::SetAside { to: to.clone() });
            }
            // Esc on a run's question or approval means "carry on without me":
            // the engine takes an option in your place and its decision closes this, so
            // nothing sits under "needs you".
            CardTarget::Controller(to @ AnswerTarget::Checkpoint { .. }) => {
                cx.out.send(UiCommand::SetAside { to: to.clone() });
                if let Some(Source::Checkpoint(opened)) =
                    self.held.get(&card).map(|held| &held.source)
                    && !cx.replaying()
                {
                    let at = OffsetDateTime::now_utc();
                    let cell = records::handed_on_cell(opened.kind, at, clock_offset(cx));
                    cx.model.history.push(cell);
                }
            }
            CardTarget::SpotCheck { .. } | CardTarget::Local(_) => {
                self.close(card, cx);
            }
        }
        self.show(cx);
        true
    }

    /// A line under a waiting card's details, once, wherever the card is.
    fn add_detail(&mut self, target: &CardTarget, line: String, cx: &mut Cx<'_>) {
        let Some(id) = self.find(target) else {
            return;
        };
        let card = match cx.model.bottom.card.as_mut() {
            Some(card) if card.id == id => Some(card),
            _ => self
                .held
                .get_mut(&id)
                .and_then(|held| held.stashed.as_mut()),
        };
        if let Some(card) = card
            && !card.detail.contains(&line)
        {
            card.detail.push(line);
        }
    }

    fn find(&self, target: &CardTarget) -> Option<u64> {
        self.open
            .iter()
            .find(|(_, card)| card.target == *target)
            .map(|(id, _)| *id)
    }

    /// Marks card `id` as one work waits on, on screen or set back.
    fn needs_you(&mut self, id: u64, cx: &mut Cx<'_>) {
        let shown = cx.model.bottom.card.as_mut().filter(|card| card.id == id);
        let stashed = self
            .held
            .get_mut(&id)
            .and_then(|held| held.stashed.as_mut());
        if let Some(card) = shown.or(stashed) {
            card.needs_you = true;
        }
    }

    fn close_target(&mut self, target: &CardTarget, cx: &mut Cx<'_>) -> Option<Held> {
        let id = self.find(target)?;
        self.close(id, cx)
    }

    fn close(&mut self, id: u64, cx: &mut Cx<'_>) -> Option<Held> {
        self.open.remove(&id);
        if cx
            .model
            .bottom
            .card
            .as_ref()
            .is_some_and(|shown| shown.id == id)
        {
            cx.model.bottom.card = None;
        }
        self.held.remove(&id)
    }

    /// Several cards can wait at once. One you set off yourself comes first, since you're
    /// waiting on it. Then the one on screen stays while it holds the keys, then the oldest
    /// card work waits on, then the spot check on screen or the oldest one. A card that asks
    /// nothing, as one your rules still decide or one you handed on, stays off the screen, so
    /// a card on screen always holds the keys.
    fn show(&mut self, cx: &mut Cx<'_>) {
        let shown = cx.model.bottom.card.as_ref();
        let prompt = |id: &u64| match shown.filter(|card| card.id == *id) {
            Some(card) => Some(card),
            None => self.held.get(id).and_then(|held| held.stashed.as_ref()),
        };
        let yours = |id: &u64| prompt(id).is_some_and(|card| card.takes_focus && !card.dismissed);
        let asks = |id: &u64| prompt(id).is_some_and(CardPrompt::claims_keys);
        // A spot check asks too, but nothing waits on it, so it holds its place only while
        // you work it.
        let spot_check = |id: &u64| {
            self.held
                .get(id)
                .is_some_and(|held| matches!(held.source, Source::SpotCheck(_)))
        };
        let current = shown
            .map(|card| card.id)
            .filter(|id| self.open.contains_key(id) && asks(id));
        let holding = current.filter(|id| {
            shown.is_some_and(|card| {
                if spot_check(id) {
                    card.engaged
                } else {
                    card.focused || card.needs_you
                }
            })
        });
        let wanted = current
            .filter(|id| yours(id))
            .or_else(|| self.open.keys().find(|id| yours(id)).copied())
            .or(holding)
            .or_else(|| {
                let waits = |id: &&u64| asks(id) && !spot_check(id);
                self.open.keys().find(waits).copied()
            })
            .or(current)
            .or_else(|| self.open.keys().find(|id| asks(id)).copied());
        match wanted {
            Some(id) => self.display(id, false, cx),
            None => self.take_off_screen(cx),
        }
    }

    /// Clears the card's place on screen, keeping the prompt of one that's still open.
    fn take_off_screen(&mut self, cx: &mut Cx<'_>) {
        if let Some(mut card) = cx.model.bottom.card.take()
            && let Some(held) = self.held.get_mut(&card.id)
        {
            // A run's card set back waits again, so it never comes back holding the keys.
            card.focused &= card.takes_focus;
            held.stashed = Some(card);
        }
    }

    /// Puts card `id` on screen, keeping the one it replaces for later. A card you set off
    /// takes the keys whenever it shows, and one work waits on until you hand it on, from
    /// wherever they were; any other card only when `focus` asks.
    fn display(&mut self, id: u64, focus: bool, cx: &mut Cx<'_>) {
        let on_screen = cx.model.bottom.card.as_ref().map(|card| card.id);
        if on_screen != Some(id) {
            self.take_off_screen(cx);
            cx.model.bottom.card = self.held.get_mut(&id).and_then(|held| held.stashed.take());
        }
        if focus {
            if let Some(card) = cx.model.bottom.card.as_mut() {
                card.focus();
            }
        } else if cx.model.bottom.hand_keys_to_card(cx.now) {
            // It took the keys from wherever they were, and a view on top would keep them.
            cx.out.close_views = true;
        }
    }
}

/// Why auto left a card to you, as its line: the engine gives the reason only on the check
/// that sends a card opened in auto to you.
fn auto_left_it(check: &PolicyCheck) -> Option<String> {
    if check.shadow || check.verdict.is_some() {
        return None;
    }
    let why = check.reasons.first()?;
    let why = sanitize_line(why);
    let why = why.trim();
    (!why.is_empty()).then(|| format!("Auto couldn't decide: {why}"))
}

/// The resume picker says the same.
impl CardTarget {
    fn run(&self) -> Option<&RunId> {
        match self {
            CardTarget::Controller(AnswerTarget::Checkpoint { run, .. })
            | CardTarget::SpotCheck { run, .. }
            | CardTarget::Local(LocalConfirm::TakeOver { run, .. }) => Some(run),
            CardTarget::Controller(AnswerTarget::Choice { .. }) => None,
        }
    }
}

impl Source {
    fn title(&self) -> String {
        match self {
            Source::Checkpoint(opened) => opened.title.clone(),
            Source::SpotCheck(decision) => decision.opened.title.clone(),
            Source::Choice(choice) => choice.title.clone(),
            Source::Local => String::new(),
        }
    }
}

/// The call a spot check asks about, as its card shows it, once this screen saw it decided.
pub fn spot_check_line(
    run: &RunId,
    record: &DecisionRecord,
    names: &DisplayNames,
) -> Option<String> {
    let decided = record.decided.as_ref()?;
    Some(format!(
        "{} · decided by {}",
        records::outcome(&record.opened, decided, Some(run), names, &records::by_id),
        words::party(&decided.by, names)
    ))
}

/// What `d` opens for a spot check listed in `/approvals`, the same as on its card, with clock
/// times at `offset`.
pub fn spot_check_evidence(
    run: &RunId,
    record: &DecisionRecord,
    offset: UtcOffset,
    names: &DisplayNames,
) -> ViewOutcome {
    let shown = views::evidence_view(
        run,
        &record.opened,
        record.decided.as_ref(),
        record.check.as_ref(),
        offset,
        names,
    );
    match shown {
        views::Shown::Request(view) => ViewOutcome::Request(view),
        views::Shown::Pager(pager) => ViewOutcome::Open(Box::new(pager)),
        views::Shown::Notice(text) => ViewOutcome::Notice(text),
    }
}

/// A handoff card answered with an agent, offered as a rule until you send something else.
fn answered_pick(
    target: &CardTarget,
    source: &Source,
    picked: &AnswerOption,
    names: &DisplayNames,
) -> Option<AnsweredPick> {
    let CardTarget::Controller(AnswerTarget::Checkpoint { run, id }) = target else {
        return None;
    };
    let Source::Checkpoint(opened) = source else {
        return None;
    };
    if opened.kind != CheckpointKind::Handoff
        || !matches!(picked.action, AnswerAction::Candidate { .. })
    {
        return None;
    }
    let decided = CheckpointDecided {
        id: id.clone(),
        action: picked.action.clone(),
        text: None,
        by: Party::User,
        evidence_sha256: opened.evidence_sha256.clone(),
        shadow: None,
        cards: Vec::new(),
    };
    let outcome = records::outcome(opened, &decided, Some(run), names, &records::by_id);
    Some(AnsweredPick::new(run.clone(), id.clone(), outcome))
}

fn checkpoint_target(run: &RunId, id: &CheckpointId) -> CardTarget {
    CardTarget::Controller(AnswerTarget::Checkpoint {
        run: run.clone(),
        id: id.clone(),
    })
}

fn owner_name(cx: &Cx<'_>, run: &RunId, card: &CardId) -> String {
    let state = cx.state.runs.get(run);
    let owner = state
        .and_then(|run| run.cards.get(card))
        .and_then(|row| row.owner.clone())
        .or_else(|| {
            state?
                .sessions_in_order()
                .filter(|session| session.card.as_ref() == Some(card))
                .last()
                .map(|session| session.worker.clone())
        });
    owner.map_or_else(
        || "its agent".to_owned(),
        |owner| cx.state.names.worker(&owner),
    )
}

/// The zone card times are shown in, read at startup like the status line's. UTC when unknown.
fn clock_offset(cx: &Cx<'_>) -> UtcOffset {
    cx.state.clock_offset()
}

#[cfg(test)]
mod tests;
