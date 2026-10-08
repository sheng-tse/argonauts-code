//! Handoffs, vendor quota and health, and budgets.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::run::Party;
use crate::limits::{Candidate, QuotaReading, VendorHealth};
use crate::names::{VendorId, WorkerRef};
use crate::settings::budget::{BudgetAmount, BudgetLineKind};
use crate::settings::handoff::{HandoffContext, HandoffMode, HandoffStrategy};
use crate::{BudgetDimension, BudgetStatus, CardId, DiffStat, Spend};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum HandoffEvent {
    /// A handoff is needed; the work so far is committed.
    Needed(HandoffPlan),
    /// The vendor-neutral packet was built from the log, at no token cost.
    Packet {
        card: CardId,
        path: PathBuf,
        sha256: String,
    },
    HandedOff {
        card: CardId,
        to: WorkerRef,
        mode: HandoffMode,
        by: Party,
        /// What the new owner is told, which the transcript line names.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<HandoffContext>,
    },
    Waiting {
        card: CardId,
        vendor: VendorId,
        #[serde(with = "time::serde::rfc3339::option")]
        until: Option<OffsetDateTime>,
    },
}

/// Who stopped, why, and who could continue. Also the handoff checkpoint's packet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandoffPlan {
    pub card: CardId,
    pub title: String,
    pub stopped: WorkerRef,
    /// One line, such as "usage limit, resets in 2h 10m".
    pub reason: String,
    #[serde(with = "time::serde::rfc3339::option")]
    pub resets_at: Option<OffsetDateTime>,
    pub work: DiffStat,
    pub gates_ran: bool,
    /// Valid candidates, best first. Only ones the role's rules allow.
    pub candidates: Vec<Candidate>,
    pub mode: HandoffMode,
    /// For the card's strategy line, such as "Claude Code's 3 cards will be spread across
    /// Codex and agy".
    #[serde(default, skip_serializing_if = "is_first")]
    pub strategy: HandoffStrategy,
    /// Attempts at once for a race.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub race_count: u32,
    /// Other cards of the stopped vendor that the same strategy moves.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_moving: Vec<CardId>,
}

fn is_first(strategy: &HandoffStrategy) -> bool {
    *strategy == HandoffStrategy::First
}

fn one() -> u32 {
    1
}

fn is_one(n: &u32) -> bool {
    *n == 1
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum VendorEvent {
    Quota(QuotaReading),
    /// A vendor's circuit-breaker state changed.
    Health(VendorHealth),
    /// A vendor is disabled for new cards, such as after a broken CLI update.
    Disabled {
        vendor: VendorId,
        /// Why, with the suggested pin.
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum BudgetEvent {
    /// Throttled to once a second.
    Run(RunBudget),
    /// A card hit a limit; its work is committed and `limits.when_reached` decides.
    Hit {
        card: CardId,
        dimension: BudgetDimension,
        status: BudgetStatus,
    },
    /// More budget for a card, from you or from the reserve by your rules.
    Extended {
        card: CardId,
        amount: BudgetAmount,
        by: Party,
    },
    /// Cards deferred until budget frees up.
    Deferred { cards: Vec<CardId> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunBudget {
    /// Sessions and the narrator are excluded.
    pub spent: Spend,
    /// `None` when budgets are off.
    pub limit: Option<BudgetAmount>,
    pub lines: Vec<LineUse>,
    pub reserve_left: Option<BudgetAmount>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LineUse {
    pub line: BudgetLineKind,
    pub allocated: BudgetAmount,
    /// In the same unit as `allocated`.
    pub used: f64,
}
