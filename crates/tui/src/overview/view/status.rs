//! The Status tab on the panel: the cards above the run's, the run's card with what it
//! spent and what needs you, and the tab's keys.

use argonauts_core::api::RunState;
use argonauts_core::app::{UiCommand, ViewRequest};
use argonauts_core::input::SlashCommand;
use argonauts_core::settings::ui::ModName;
use crossterm::event::{KeyCode, KeyEvent};

use super::{Follow, OverviewView, SIDE_BY_SIDE_FROM, fill_title, halves, note, pin, run_name};
use crate::bridge::transcript::NOTHING_RUNNING;
use crate::kit::{Drawn, Form, Frame, Kit, side_by_side};
use crate::overview::cards::{UsageCard, UsageCards};
use crate::overview::shades::ModelColors;
use crate::overview::status::{Picks, RunShape, StatusCards};
use crate::overview::status_tab::StatusTabCards;
use crate::overview::subtabs::Level;
use crate::views::panes::Marks;
use crate::views::{ViewCtx, ViewOutcome};

impl OverviewView {
    /// The Status tab's framed cards, in the order Tab steps through them, each by the mod
    /// that pins it above the input.
    pub(super) fn status_cards(&self, ctx: &ViewCtx<'_>) -> Vec<ModName> {
        let Some(status) = &self.status else {
            return Vec::new();
        };
        let mut cards = vec![ModName::Run];
        if status.spend.is_some() {
            cards.push(ModName::RunLimits);
        }
        if ctx.state.usage.is_some() {
            cards.push(ModName::PlanUsage);
        }
        if !status.waiting.is_empty() {
            cards.push(ModName::Waiting);
        }
        cards
    }

    /// The Status tab's run card that has the keys, when one has.
    pub(super) fn status_focus(&self, ctx: &ViewCtx<'_>) -> Option<ModName> {
        let cards = self.status_cards(ctx);
        let at = self.focus.checked_sub(self.setup_parts())?;
        cards.get(at.min(cards.len().saturating_sub(1))).copied()
    }

    /// The Status tab's cards above the run's: your CLIs, their health, the settings, and the
    /// runs.
    pub(super) fn setup_parts(&self) -> usize {
        if self.setup.is_some() {
            SETUP_PARTS.len()
        } else {
            0
        }
    }

    /// Which of the cards above the run's has the keys.
    pub(super) fn setup_focus(&self) -> Option<SetupPart> {
        SETUP_PARTS
            .get(self.focus)
            .copied()
            .filter(|_| self.setup.is_some())
    }

    /// Version, folder, session and mode, then your CLIs, the settings in effect and this
    /// folder's runs, each card taking its place in ↑ and ↓'s round.
    fn setup_blocks(&self, kit: &Kit<'_>, ctx: &ViewCtx<'_>, width: usize) -> Vec<Drawn> {
        let Some(view) = &self.setup else {
            return Vec::new();
        };
        let mode = ctx
            .state
            .status
            .as_ref()
            .map(|status| status.approval_mode.label());
        let cards = StatusTabCards {
            view,
            names: &ctx.state.names,
            clock: match self.now {
                Some(now) => crate::format::WallClock::pinned(now),
                None => crate::format::WallClock::local(),
            },
            mode,
        };
        let mut blocks = vec![cards.facts(kit, width)];
        for (at, part) in SETUP_PARTS.iter().enumerate() {
            let frame = Frame::view(width);
            let mut card = match part {
                SetupPart::Clis => cards.clis(kit, frame),
                SetupPart::Health => {
                    let shades = ModelColors::new(self.ranked(ctx), &ctx.state.names);
                    crate::overview::health::card(&view.health, kit, frame, &cards.clock, shades)
                }
                SetupPart::Settings => cards.settings(kit, frame),
                SetupPart::Runs => cards.runs(kit, frame),
            };
            if at == self.focus {
                self.focus_block.set(blocks.len());
                if self.level() == Level::Cards {
                    fill_title(kit, &mut card);
                }
            }
            blocks.push(card);
        }
        blocks
    }

    /// The run's card, then what it spent beside plan usage where the panel is wide enough,
    /// then what needs you, and a row of what happened last.
    pub(super) fn status_blocks(
        &self,
        kit: &Kit<'_>,
        ctx: &ViewCtx<'_>,
        width: usize,
    ) -> Vec<Drawn> {
        self.focus_block.set(0);
        let setup = self.setup_blocks(kit, ctx, width);
        let first = self.setup_parts();
        let Some(summary) = &self.status else {
            if setup.is_empty() {
                return vec![note(kit, crate::overview::status_tab::NO_RUNS)];
            }
            return setup;
        };
        let title = summary.run.as_ref().and_then(|run| run_name(ctx, run));
        let cards = StatusCards {
            summary,
            names: &ctx.state.names,
            title: title.as_deref(),
        };
        if cards.is_empty() {
            if setup.is_empty() {
                return vec![note(kit, NOTHING_RUNNING)];
            }
            return setup;
        }
        // Each framed card takes the next place in ↑ and ↓'s round. The one in focus has its
        // title filled once it's drawn, never a box of its own.
        let filled = std::cell::RefCell::new(Vec::new());
        let mut place = first;
        let mut framed = |frame: Frame| {
            filled
                .borrow_mut()
                .push(place == self.focus && self.level() == Level::Cards);
            place += 1;
            frame
        };
        // The block the card in focus lands in, counted as the blocks stack up.
        let in_focus = |first: usize, cards: usize, block: usize| {
            if (first..first + cards).contains(&self.focus) {
                self.focus_block.set(block);
            }
        };
        // The arrows pick a task while the run's card has the keys.
        let tasks = summary.tasks.len();
        let picks = Picks {
            at: (self.level() == Level::Cards && self.focus == first && tasks > 0)
                .then(|| self.task.min(tasks - 1)),
            mark: Marks::for_theme(ctx.theme).selected,
        };
        let shape = RunShape {
            picks: Some(picks),
            ..RunShape::WHOLE
        };
        // The card just framed, with its title filled when it has the keys.
        let fill = |card: Option<Drawn>| {
            card.map(|mut card| {
                if filled.borrow().last() == Some(&true) {
                    fill_title(kit, &mut card);
                }
                card
            })
        };
        let mut blocks = setup;
        if self.focus == first {
            self.focus_block.set(blocks.len());
        }
        blocks.extend(fill(Some(cards.run(
            kit,
            framed(Frame::view(width)),
            shape,
        ))));
        let plan = ctx.state.usage.as_deref().map(|usage| UsageCards {
            usage,
            names: &ctx.state.names,
            now: self.wall(),
            left: self.left,
        });
        let plan_card =
            |frame| plan.map(|plan| plan.draw(kit, UsageCard::PlanUsage, Form::Compact, frame));
        let mut next = first + 1;
        if summary.spend.is_some() && plan.is_some() && width >= SIDE_BY_SIDE_FROM {
            let (left, right, at) = halves(width);
            let spend = fill(cards.spend(kit, framed(left)));
            let plan = fill(plan_card(framed(right)));
            if let (Some(spend), Some(plan)) = (spend, plan) {
                in_focus(next, 2, blocks.len());
                blocks.push(side_by_side(spend, plan, at));
            }
            next += 2;
        } else {
            if summary.spend.is_some() {
                in_focus(next, 1, blocks.len());
                next += 1;
                blocks.extend(fill(cards.spend(kit, framed(Frame::view(width)))));
            }
            if plan.is_some() {
                in_focus(next, 1, blocks.len());
                next += 1;
                blocks.extend(fill(plan_card(framed(Frame::view(width)))));
            }
        }
        if !summary.waiting.is_empty() {
            in_focus(next, 1, blocks.len());
            blocks.extend(fill(cards.needs_you(kit, framed(Frame::view(width)))));
        }
        blocks.extend(cards.recent(kit, width));
        blocks
    }

    pub(super) fn status_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let tasks = self.status.as_ref().map_or(0, |status| status.tasks.len());
        // On the run's card the arrows pick a task. On any other they scroll the tab.
        let on_run = self.status_focus(ctx) == Some(ModName::Run) && tasks > 0;
        match key.code {
            KeyCode::Char('s') => ViewOutcome::SendToConversation(UiCommand::Summary),
            // The main session, in its CLI's own screen, as the tab's row says.
            KeyCode::Char('o') => match self.setup.as_ref().and_then(|view| view.in_cli.as_ref()) {
                Some(cli) => ViewOutcome::SendToConversation(UiCommand::OpenInItsCli {
                    session: cli.session.clone(),
                    conversation: cli.conversation.clone(),
                }),
                None => ViewOutcome::Ignored,
            },
            KeyCode::Char('a') if self.lists_a_run_to_archive() => {
                ViewOutcome::SendToConversation(UiCommand::Slash {
                    command: SlashCommand::Runs { archive: None },
                })
            }
            KeyCode::Enter => match self.setup_focus() {
                // Every key with where it came from, over the panel, which Esc comes back to.
                Some(SetupPart::Settings) => {
                    ViewOutcome::Request(ViewRequest::Settings { section: None })
                }
                Some(SetupPart::Runs) => self.open_newest_run(),
                Some(SetupPart::Clis | SetupPart::Health) => ViewOutcome::Consumed,
                None => self.open_from_status(on_run, ctx),
            },
            KeyCode::Char('u') => {
                self.left = !self.left;
                ViewOutcome::Consumed
            }
            KeyCode::Char('p') => match self.status_focus(ctx) {
                Some(card) => pin(card),
                None => ViewOutcome::Ignored,
            },
            KeyCode::Up if on_run && self.task > 0 => {
                self.task = self.task.min(tasks - 1) - 1;
                ViewOutcome::Consumed
            }
            KeyCode::Down if on_run && self.task + 1 < tasks => {
                self.task += 1;
                ViewOutcome::Consumed
            }
            // On the run's card the first and last task, and from there the first and last card.
            KeyCode::Home if on_run && self.task > 0 => {
                self.task = 0;
                ViewOutcome::Consumed
            }
            KeyCode::End if on_run && self.task + 1 < tasks => {
                self.task = tasks - 1;
                ViewOutcome::Consumed
            }
            KeyCode::Home => {
                self.focus = 0;
                self.follow = Some(Follow::Focus);
                ViewOutcome::Consumed
            }
            KeyCode::End => {
                self.focus = self.card_count(ctx) - 1;
                self.follow = Some(Follow::Focus);
                ViewOutcome::Consumed
            }
            KeyCode::Up => self.step_focus(true, ctx),
            KeyCode::Down => self.step_focus(false, ctx),
            KeyCode::Char('q') => ViewOutcome::Close,
            _ => ViewOutcome::Ignored,
        }
    }

    /// Enter on the runs card: the newest run's workflow, which lists what it is doing.
    /// Whether a run of this folder was written by another build, which `a` moves out of the
    /// way.
    fn lists_a_run_to_archive(&self) -> bool {
        self.setup.as_ref().is_some_and(|view| {
            view.runs
                .iter()
                .any(|run| run.state == RunState::FromAnotherBuild)
        })
    }

    fn open_newest_run(&self) -> ViewOutcome {
        let newest = self.setup.as_ref().and_then(|view| view.runs.first());
        match newest {
            Some(run) => ViewOutcome::Request(ViewRequest::Workflow {
                run: run.run.clone(),
                workflow: None,
            }),
            None => ViewOutcome::Consumed,
        }
    }

    /// Enter on the Status tab: the picked task's agent where the screen knows one, and
    /// otherwise the run's workflow, which lists them all.
    fn open_from_status(&self, on_run: bool, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let Some(status) = &self.status else {
            return ViewOutcome::Consumed;
        };
        let Some(run) = status.run.clone() else {
            return ViewOutcome::Notice("No run to open".to_owned());
        };
        let task = status
            .tasks
            .get(self.task.min(status.tasks.len().saturating_sub(1)))
            .filter(|_| on_run);
        let agent = task.and_then(|task| {
            let sessions = ctx.state.runs.get(&run)?.sessions_in_order();
            let mut of_task: Vec<_> = sessions
                .filter(|session| session.task.as_ref() == Some(&task.task))
                .collect();
            // The one at work now, else the last that worked on it.
            of_task.sort_by_key(|session| session.is_running());
            of_task.last().map(|session| session.key.clone())
        });
        match agent {
            Some(session) => ViewOutcome::Request(ViewRequest::Agent { run, session }),
            None => ViewOutcome::Request(ViewRequest::Workflow {
                run,
                workflow: None,
            }),
        }
    }
}

/// The Status tab's cards above the run's, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SetupPart {
    Clis,
    Health,
    Settings,
    Runs,
}

const SETUP_PARTS: [SetupPart; 4] = [
    SetupPart::Clis,
    SetupPart::Health,
    SetupPart::Settings,
    SetupPart::Runs,
];
