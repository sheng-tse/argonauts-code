//! The Usage and Limits tabs on the panel: plan usage a card per CLI or one CLI's page,
//! the usage cards each stacks, the list of limits, and their keys.

use argonauts_core::app::{UiCommand, ViewRequest};
use argonauts_core::input::{ModsAction, SlashCommand, ViewAction};
use argonauts_core::runs::api::UsagePeriod;
use crossterm::event::{KeyCode, KeyEvent};

use super::{Follow, OverviewView, SIDE_BY_SIDE_FROM, Tab, fill_title, halves, note, pin};
use crate::base::draw::panes::Marks;
use crate::base::kit::{Drawn, Form, Frame, Kit, side_by_side};
use crate::frame::view::{ViewCtx, ViewOutcome};
use crate::panels::cards::{UsageCard, UsageCards};
use crate::panels::limits::{LIST_ROWS_AT_LEAST, LimitsCards, tasks_of};
use crate::panels::pinned_run::PinnedRun;
use crate::panels::status::{StatusCard, StatusCards};
use crate::panels::usage::{Stop, UsageTab};
use crate::panels::view::subtabs::Level;

impl OverviewView {
    /// The Usage tab: a card per CLI, or the picked CLI's page.
    pub(super) fn plan_usage_blocks(
        &self,
        kit: &Kit<'_>,
        ctx: &ViewCtx<'_>,
        width: usize,
    ) -> Vec<Drawn> {
        let Some(tab) = self.plan_usage_tab(ctx) else {
            self.focus_block.set(0);
            return vec![note(kit, "Reading usage…")];
        };
        // The cards leave the last column blank, as every other tab's do.
        let width = width.saturating_sub(1);
        let blocks = tab.blocks(kit, width);
        let block = tab.block_of(self.focus, width);
        self.focus_block
            .set(block.min(blocks.len().saturating_sub(1)));
        blocks
    }

    /// Plan usage is read live and holds for any period, so any numbers will do.
    pub(super) fn plan_usage_tab<'a>(&self, ctx: &'a ViewCtx<'_>) -> Option<UsageTab<'a>> {
        let now = self.wall();
        let talk = &ctx.state.talk;
        Some(UsageTab {
            usage: ctx.state.usage.as_deref()?,
            now,
            cli: self.cli,
            left: self.left,
            talked: talk
                .started
                .map(|started| ((now - started).unsigned_abs(), talk.worked)),
            changed: (talk.added, talk.removed),
            focus: (self.level() == Level::Cards && self.cli == 0).then_some(self.focus),
        })
    }

    /// The usage cards the tab stacks, the tokens and history cards as one block side by
    /// side where the panel is wide enough.
    pub(super) fn usage_blocks(
        &self,
        kit: &Kit<'_>,
        ctx: &ViewCtx<'_>,
        width: usize,
    ) -> Vec<Drawn> {
        let Some(usage) = self.usage(ctx) else {
            return vec![note(kit, "Reading usage…")];
        };
        let cards = UsageCards {
            usage,
            names: &ctx.state.names,
            now: self.wall(),
            left: self.left,
        };
        let listed = self.tab.cards();
        let focus = self.focus.min(listed.len().saturating_sub(1));
        let draw = |at: usize, frame: Frame| {
            let (card, form) = listed[at];
            let mut drawn = cards.draw(kit, card, form, frame);
            if at == focus && self.level() == Level::Cards {
                fill_title(kit, &mut drawn);
            }
            drawn
        };
        let pair = listed
            .iter()
            .position(|(card, _)| *card == UsageCard::Tokens);
        let mut stacked = Vec::new();
        // The run the screen follows counts toward a limit of its own, so it comes first.
        let followed = ctx.state.followed_run();
        let run = followed
            .filter(|_| self.tab == Tab::Limits)
            .map(PinnedRun::of);
        if let Some(run) = &run {
            let run = StatusCards {
                summary: &run.summary,
                names: &ctx.state.names,
                title: run.title.as_deref(),
            };
            if run.has(StatusCard::Spend) {
                let frame = Frame::view(width);
                stacked.push(run.pinned(kit, StatusCard::Spend, Form::Full, frame));
            }
        }
        let mut at = 0;
        while at < listed.len() {
            let beside = Some(at) == pair
                && width >= SIDE_BY_SIDE_FROM
                && listed
                    .get(at + 1)
                    .is_some_and(|(card, _)| *card == UsageCard::History);
            let cards = if beside { 2 } else { 1 };
            if (at..at + cards).contains(&focus) {
                self.focus_block.set(stacked.len());
            }
            if beside {
                let (left, right, column) = halves(width);
                stacked.push(side_by_side(draw(at, left), draw(at + 1, right), column));
            } else {
                stacked.push(draw(at, Frame::view(width)));
            }
            // The limits the tab is named for come right under what counts toward them.
            if self.tab == Tab::Limits && listed[at].0 == UsageCard::Spend {
                let tasks = followed.map(tasks_of).unwrap_or_default();
                let limits = LimitsCards {
                    tasks: &tasks,
                    task_limit: usage.task_limit,
                    limits: &usage.limits,
                };
                stacked.extend(limits.tasks(kit, Frame::view(width)));
                self.limits_block.set(stacked.len());
                stacked.push(self.limits_list(kit, ctx, &limits, &stacked, width));
            }
            at += cards;
        }
        stacked
    }

    /// The limits in the rows the panel has left under `above`, or the whole panel where
    /// they'd fit there only cut to nothing.
    fn limits_list(
        &self,
        kit: &Kit<'_>,
        ctx: &ViewCtx<'_>,
        limits: &LimitsCards<'_>,
        above: &[Drawn],
        width: usize,
    ) -> Drawn {
        let room = self.home.room();
        let taken: usize = above.iter().map(|block| block.height() + 1).sum();
        let left = room.saturating_sub(taken);
        let rows = if left >= LIST_ROWS_AT_LEAST {
            left
        } else {
            room
        };
        let mark = Marks::for_theme(ctx.theme).selected;
        limits
            .list(kit, (width, rows), self.limit, mark)
            // A controller that lists no limits still says what Enter does.
            .unwrap_or_else(|| {
                self::note(
                    kit,
                    "Enter opens the limits, which stop a run only once you set one.",
                )
            })
    }

    /// The Usage tab's own keys: in the cards Enter opens the CLI whose card has the keys, or
    /// the one that says what made up most of the usage, on its own page; Enter on the page
    /// closes it, back to its card. `None` leaves the key to the panel.
    pub(super) fn plan_usage_key(
        &mut self,
        key: KeyEvent,
        ctx: &ViewCtx<'_>,
    ) -> Option<ViewOutcome> {
        match key.code {
            KeyCode::Enter if self.level() == Level::Cards => {
                let tab = self.plan_usage_tab(ctx)?;
                if self.cli > 0 {
                    // The CLI's card is the stop after the conversation's, in the same order.
                    self.focus = self.cli;
                    self.cli = 0;
                } else {
                    let usage = tab.usage;
                    let says_most = || {
                        usage
                            .plans
                            .iter()
                            .position(|plan| {
                                plan.live
                                    .as_ref()
                                    .is_some_and(|live| live.behaviors.is_some())
                            })
                            .unwrap_or(0)
                    };
                    match tab.stop(self.focus) {
                        Stop::Conversation => return Some(ViewOutcome::Consumed),
                        Stop::Card(plan) => self.cli = plan + 1,
                        Stop::MostUsage => self.cli = says_most() + 1,
                    }
                    self.focus = 0;
                }
            }
            _ => return None,
        }
        self.scroll = 0;
        self.follow = Some(Follow::Focus);
        Some(ViewOutcome::Consumed)
    }

    fn focused(&self) -> Option<UsageCard> {
        let cards = self.tab.cards();
        cards
            .get(self.focus.min(cards.len().saturating_sub(1)))
            .map(|(card, _)| *card)
    }

    pub(super) fn usage_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        // On the Limits tab the arrows pick a limit, and Enter opens it to change.
        let limits = match (self.tab, self.usage(ctx)) {
            (Tab::Limits, Some(usage)) => usage.limits.as_slice(),
            _ => &[],
        };
        if let Some(last) = limits.len().checked_sub(1) {
            let picked = self.limit.min(last);
            let moved = match key.code {
                KeyCode::Up => Some(picked.saturating_sub(1)),
                KeyCode::Down => Some((picked + 1).min(last)),
                KeyCode::Home => Some(0),
                KeyCode::End => Some(last),
                _ => None,
            };
            if let Some(moved) = moved {
                self.limit = moved;
                self.follow = Some(Follow::Limits);
                return ViewOutcome::Consumed;
            }
            // Its picker opens under the input, so the panel gives the keys back first.
            if key.code == KeyCode::Enter {
                return ViewOutcome::SendToConversation(UiCommand::Slash {
                    command: SlashCommand::Limits {
                        key: Some(limits[picked].key.clone()),
                        value: None,
                        action: ViewAction::Open,
                    },
                });
            }
        }
        let period = |period| ViewOutcome::Request(ViewRequest::Usage { period });
        match key.code {
            KeyCode::Char(c @ ('d' | 'w' | 'm' | 'a')) if self.tab == Tab::Usage => {
                self.period = match c {
                    'd' => UsagePeriod::Today,
                    'w' => UsagePeriod::Week,
                    'm' => UsagePeriod::Month,
                    _ => UsagePeriod::All,
                };
                self.scroll = 0;
                period(self.period)
            }
            KeyCode::Char('u') => {
                self.left = !self.left;
                ViewOutcome::Consumed
            }
            KeyCode::Char('p') => match self.focused() {
                Some(card) => pin(card.name()),
                None => ViewOutcome::Ignored,
            },
            KeyCode::Char(',') => match self.focused() {
                Some(card) => ViewOutcome::Send(UiCommand::Slash {
                    command: SlashCommand::Mods {
                        action: ModsAction::Page {
                            the_mod: card.name(),
                        },
                        scope: None,
                    },
                }),
                None => ViewOutcome::Ignored,
            },
            KeyCode::Up => self.step_focus(true, ctx),
            KeyCode::Down => self.step_focus(false, ctx),
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
            // With no list to pick from, Enter opens every limit in their own picker.
            KeyCode::Enter if self.tab == Tab::Limits => {
                ViewOutcome::SendToConversation(UiCommand::Slash {
                    command: SlashCommand::Limits {
                        key: None,
                        value: None,
                        action: ViewAction::Open,
                    },
                })
            }
            KeyCode::Char('q') => ViewOutcome::Close,
            _ => ViewOutcome::Ignored,
        }
    }
}
