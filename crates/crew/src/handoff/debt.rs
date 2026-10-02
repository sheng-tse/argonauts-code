//! Quality debt: a card carries it when a commit since a checkpoint came from a replacement
//! weaker than its strongest previous owner. Weaker by the tiers you set, else track records
//! (both at least 5 finished cards of that kind), else `unknown_tier`. Your own commits and a
//! replacement at the same tier or above create none.

use std::cmp::Ordering;

use argonauts_core::events::card::{CardKind, CardSpec};
use argonauts_core::limits::TrackRecord;
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::handoff::UnknownTier;

use crate::engine::host::Host;

/// Finished cards of a kind both models need before track records decide.
pub const MIN_RECORD: u32 = 5;

/// What's known about a model for the comparison.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Strength {
    pub tier: Option<u8>,
    pub record: Option<TrackRecord>,
}

/// How `replacement` compares with `original`: `Less` means weaker, so the card takes debt.
pub fn compare(replacement: Strength, original: Strength, unknown: UnknownTier) -> Ordering {
    if let (Some(replacement), Some(original)) = (replacement.tier, original.tier) {
        // Tier 5 is the strongest.
        return replacement.cmp(&original);
    }
    if let (Some(replacement), Some(original)) = (replacement.record, original.record)
        && replacement.of >= MIN_RECORD
        && original.of >= MIN_RECORD
    {
        // Pass rates compared without floats: a/b against c/d is a*d against c*b.
        let left = u64::from(replacement.passed) * u64::from(original.of);
        let right = u64::from(original.passed) * u64::from(replacement.of);
        return left.cmp(&right);
    }
    match unknown {
        UnknownTier::Weaker => Ordering::Less,
        UnknownTier::Equal => Ordering::Equal,
    }
}

/// The track-record key for a card's kind of task.
pub fn task_type(kind: &CardKind) -> &'static str {
    match kind {
        CardKind::Solo => "solo",
        CardKind::Interface => "interface",
        CardKind::AcceptanceTests { .. } => "acceptance_tests",
        CardKind::Build => "build",
        CardKind::Chore => "chore",
        CardKind::Docs => "docs",
        CardKind::Spike { .. } => "spike",
    }
}

/// A worker's tier from the registry and its track record on cards of this kind.
pub fn strength(host: &Host, worker: &WorkerRef, card: &CardSpec) -> Strength {
    let services = host.services();
    Strength {
        tier: services.registry.get(worker).and_then(|entry| entry.tier),
        record: services.track_records.get(worker, task_type(&card.kind)),
    }
}

/// Whether `worker` is weaker than `than` for this card.
pub fn weaker(
    host: &Host,
    card: &CardSpec,
    worker: &WorkerRef,
    than: &WorkerRef,
    unknown: UnknownTier,
) -> bool {
    worker != than
        && compare(
            strength(host, worker, card),
            strength(host, than, card),
            unknown,
        ) == Ordering::Less
}

/// The strongest of `workers` for this card, the earliest one on a tie.
pub fn strongest<'a>(
    host: &Host,
    card: &CardSpec,
    workers: impl IntoIterator<Item = &'a WorkerRef>,
    unknown: UnknownTier,
) -> Option<WorkerRef> {
    let mut best: Option<&WorkerRef> = None;
    for worker in workers {
        best = match best {
            Some(current) if !weaker(host, card, current, worker, unknown) => Some(current),
            _ => Some(worker),
        };
    }
    best.cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tier(tier: u8) -> Strength {
        Strength {
            tier: Some(tier),
            record: None,
        }
    }

    fn record(passed: u32, of: u32) -> Strength {
        Strength {
            tier: None,
            record: Some(TrackRecord { passed, of }),
        }
    }

    const UNKNOWN: Strength = Strength {
        tier: None,
        record: None,
    };

    #[test]
    fn tiers_you_set_decide_first() {
        let weaker = UnknownTier::Weaker;
        assert_eq!(compare(tier(3), tier(5), weaker), Ordering::Less);
        assert_eq!(compare(tier(5), tier(5), weaker), Ordering::Equal);
        assert_eq!(compare(tier(5), tier(4), weaker), Ordering::Greater);

        // A tier beats a record that says otherwise.
        let both = |t, passed, of| Strength {
            tier: Some(t),
            record: Some(TrackRecord { passed, of }),
        };
        assert_eq!(
            compare(both(5, 1, 10), both(4, 10, 10), weaker),
            Ordering::Greater
        );
    }

    #[test]
    fn track_records_need_five_finished_cards_each() {
        let weaker = UnknownTier::Weaker;
        assert_eq!(
            compare(record(6, 10), record(9, 10), weaker),
            Ordering::Less
        );
        assert_eq!(
            compare(record(4, 5), record(8, 10), weaker),
            Ordering::Equal
        );
        assert_eq!(
            compare(record(9, 10), record(3, 5), weaker),
            Ordering::Greater
        );
        // Four cards aren't enough, so the unknown rule decides.
        assert_eq!(compare(record(4, 4), record(1, 10), weaker), Ordering::Less);
        assert_eq!(
            compare(record(4, 4), record(1, 10), UnknownTier::Equal),
            Ordering::Equal
        );
    }

    #[test]
    fn unknown_tiers_follow_the_setting() {
        assert_eq!(
            compare(UNKNOWN, UNKNOWN, UnknownTier::Weaker),
            Ordering::Less
        );
        assert_eq!(
            compare(UNKNOWN, UNKNOWN, UnknownTier::Equal),
            Ordering::Equal
        );
        // One tier alone doesn't decide.
        assert_eq!(
            compare(tier(5), UNKNOWN, UnknownTier::Weaker),
            Ordering::Less
        );
        assert_eq!(
            compare(UNKNOWN, tier(1), UnknownTier::Equal),
            Ordering::Equal
        );
    }

    #[test]
    fn task_types_name_the_card_kind() {
        assert_eq!(task_type(&CardKind::Build), "build");
        assert_eq!(
            task_type(&CardKind::AcceptanceTests {
                for_card: argonauts_core::CardId::numbered(1)
            }),
            "acceptance_tests"
        );
    }
}
