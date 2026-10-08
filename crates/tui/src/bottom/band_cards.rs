//! What each mod pinned above the input draws, and what it draws from.
//!
//! | Mod | Draws from |
//! |---|---|
//! | plan usage, spend, tokens, history, workflows | the usage numbers the controller sends |
//! | run, the run's spend, needs you | the run this screen follows |
//! | context | the conversation's context as its CLI last counted it |
//!
//! A mod with nothing to draw yet, such as the run's card with no run, takes no row. A mod
//! from a file of yours takes its place here as one more kind of card.

use argonauts_core::api::{CardBrief, StatusSummary};
use argonauts_core::events::card::CardState;
use argonauts_core::settings::ui::WidgetName;
use ratatui::text::Span;

use crate::overview::stats::pinned::StatsCard;
use crate::overview::{StatusCard, StatusCards, UsageCard, UsageCards};
use crate::state::RunState;
use crate::text::truncate_spans;
use crate::widgets::{ContextCard, Drawn, Form, Frame, Kit};

/// A pinned mod this band draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BandCard {
    Usage(UsageCard),
    Status(StatusCard),
    Context,
    /// What each CLI and model did, one row per CLI.
    Stats,
}

impl BandCard {
    pub(super) fn of(name: WidgetName) -> Option<Self> {
        if name == WidgetName::Context {
            return Some(Self::Context);
        }
        if name == WidgetName::Stats {
            return Some(Self::Stats);
        }
        let usage = UsageCard::of(name).map(Self::Usage);
        usage.or_else(|| StatusCard::of(name).map(Self::Status))
    }

    /// The form it opens in before the band runs short of rows.
    pub(super) fn opens_as(self) -> Form {
        match self {
            // Its compact form already has a row for every CLI.
            Self::Usage(UsageCard::PlanUsage) => Form::Compact,
            _ => Form::Full,
        }
    }
}

/// The run the screen follows, as its cards above the input draw it.
#[derive(Debug, Clone, PartialEq)]
pub struct PinnedRun {
    pub summary: StatusSummary,
    /// The run's own name, such as `export-reports-csv`.
    pub title: Option<String>,
}

impl PinnedRun {
    /// The run as its cards draw it, from what its events said so far. Its goal and plan
    /// aren't shown above the input, so they're left out.
    pub fn of(run: &RunState) -> Self {
        let tasks = run.cards.values().map(|task| CardBrief {
            card: task.id.clone(),
            title: task.title.clone(),
            state: task.state.unwrap_or(CardState::Waiting),
            owner: task.owner.clone(),
            note: String::new(),
        });
        let waiting = run
            .checkpoints
            .iter()
            .filter(|approval| approval.opened.needs_you)
            .map(|approval| approval.opened.title.clone());
        Self {
            summary: StatusSummary {
                run: Some(run.id.clone()),
                cards: tasks.collect(),
                budget: run.budget.clone(),
                waiting: waiting.collect(),
                ..StatusSummary::default()
            },
            title: run.named.as_ref().map(|named| named.name.clone()),
        }
    }
}

/// What the cards draw from, each `None` until the screen holds it.
#[derive(Debug, Clone, Copy)]
pub(super) struct Sources<'a> {
    pub usage: Option<UsageCards<'a>>,
    pub status: Option<StatusCards<'a>>,
    pub context: Option<&'a ContextCard>,
}

impl Sources<'_> {
    /// The stats card, once the usage numbers carry the crew's stats.
    fn stats(&self) -> Option<StatsCard<'_>> {
        let usage = self.usage?;
        usage.usage.stats.as_ref()?;
        Some(StatsCard {
            usage: usage.usage,
            names: usage.names,
            now: usage.now,
        })
    }

    /// Whether `card` has anything to draw now.
    pub(super) fn has(&self, card: BandCard) -> bool {
        match card {
            BandCard::Usage(_) => self.usage.is_some(),
            BandCard::Stats => self.stats().is_some(),
            BandCard::Status(card) => self.status.is_some_and(|status| status.has(card)),
            BandCard::Context => self.context.is_some(),
        }
    }

    /// The card in its frame, in its full or compact form.
    pub(super) fn draw(&self, kit: &Kit<'_>, card: BandCard, form: Form, frame: Frame) -> Drawn {
        let drawn = match card {
            BandCard::Usage(card) => self.usage.map(|usage| usage.draw(kit, card, form, frame)),
            BandCard::Status(card) => self
                .status
                .map(|status| status.pinned(kit, card, form, frame)),
            BandCard::Context => self.context.map(|context| context.card(kit, form, frame)),
            BandCard::Stats => self.stats().map(|stats| stats.draw(kit, form, frame)),
        };
        drawn.unwrap_or_default()
    }

    /// The folded card: its mark and name, then the one number that matters.
    pub(super) fn line(&self, kit: &Kit<'_>, card: BandCard, width: usize) -> Vec<Span<'static>> {
        match card {
            BandCard::Usage(card) => self
                .usage
                .map(|usage| usage.line(kit, card, width))
                .unwrap_or_default(),
            BandCard::Status(card) => self
                .status
                .map(|status| status.line(kit, card, width))
                .unwrap_or_default(),
            BandCard::Stats => self
                .stats()
                .map(|stats| stats.line(kit, width))
                .unwrap_or_default(),
            BandCard::Context => {
                let Some(context) = self.context else {
                    return Vec::new();
                };
                let header = context.header(kit);
                let mut row = header.left(kit);
                row.push(Span::raw("  "));
                row.extend(context_percent(kit, context));
                row.extend(header.amount.map(|amount| kit.dim(format!(" · {amount}"))));
                row.extend(header.tail);
                truncate_spans(&row, width)
            }
        }
    }

    /// What a hidden band still says of the card, in its level's color.
    pub(super) fn key_number(&self, kit: &Kit<'_>, card: BandCard) -> Vec<Span<'static>> {
        match card {
            BandCard::Usage(card) => self
                .usage
                .map(|usage| usage.key_number(kit, card))
                .unwrap_or_default(),
            BandCard::Status(card) => self
                .status
                .map(|status| status.key_number(kit, card))
                .unwrap_or_default(),
            BandCard::Stats => self
                .stats()
                .map(|stats| stats.key_number(kit))
                .unwrap_or_default(),
            BandCard::Context => {
                let Some(context) = self.context else {
                    return Vec::new();
                };
                let mut words = vec![kit.dim("context ")];
                words.extend(context_percent(kit, context));
                words
            }
        }
    }
}

/// How full the context is, dim until it nears the point where it compacts.
fn context_percent(kit: &Kit<'_>, context: &ContextCard) -> Vec<Span<'static>> {
    let badge = context.header(kit).badge;
    let role = context.level().number_role();
    badge
        .map(|badge| kit.span(badge.text, role))
        .into_iter()
        .collect()
}
