//! Who is weaker: the registry's `tier` decides, then track records on the task's kind of task,
//! and a model with neither counts by `fallback.unknown_tier`. A weaker fallback leaves the task
//! with quality debt until a model at least as strong as the original reviews its work.

use std::cmp::Ordering;

use argonauts_core::limits::TrackRecord;
use argonauts_core::registry::ModelEntry;
use argonauts_core::settings::fallback::UnknownTier;

/// Finished tasks of a kind each side needs before track records decide.
pub const MIN_RECORD: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strength {
    Weaker,
    Equal,
    Stronger,
}

impl Strength {
    /// Work by a weaker fallback is quality debt.
    pub fn creates_debt(self) -> bool {
        self == Self::Weaker
    }

    fn from_ordering(ordering: Ordering) -> Self {
        match ordering {
            Ordering::Less => Self::Weaker,
            Ordering::Equal => Self::Equal,
            Ordering::Greater => Self::Stronger,
        }
    }
}

/// One side of a comparison.
#[derive(Debug, Clone, Copy)]
pub struct Contender<'a> {
    pub entry: Option<&'a ModelEntry>,
    pub record: Option<TrackRecord>,
}

impl Contender<'_> {
    fn tier(&self) -> Option<u8> {
        self.entry.and_then(|entry| entry.tier)
    }

    fn long_record(&self) -> Option<TrackRecord> {
        self.record.filter(|record| record.of >= MIN_RECORD)
    }
}

/// How `fallback` compares with `original`. Tiers decide only when both sides have one, and
/// track records only when both have at least [`MIN_RECORD`] tasks of the kind.
pub fn compare(fallback: Contender<'_>, original: Contender<'_>, unknown: UnknownTier) -> Strength {
    if let (Some(new), Some(old)) = (fallback.tier(), original.tier()) {
        return Strength::from_ordering(new.cmp(&old));
    }
    if let (Some(new), Some(old)) = (fallback.long_record(), original.long_record()) {
        // Pass rates compared by cross-multiplying, so no float rounding decides a tie.
        let new_rate = u64::from(new.passed) * u64::from(old.of);
        let old_rate = u64::from(old.passed) * u64::from(new.of);
        return Strength::from_ordering(new_rate.cmp(&old_rate));
    }
    match unknown {
        UnknownTier::Weaker => Strength::Weaker,
        UnknownTier::Equal => Strength::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fallback::testing::model;

    fn with_tier(tier: Option<u8>) -> ModelEntry {
        let mut entry = model("codex", "gpt-5", "openai");
        entry.tier = tier;
        entry
    }

    fn record(passed: u32, of: u32) -> Option<TrackRecord> {
        Some(TrackRecord { passed, of })
    }

    #[test]
    fn tier_comparison_unknown_tiers() {
        let (four, three, none) = (with_tier(Some(4)), with_tier(Some(3)), with_tier(None));
        fn side(entry: &ModelEntry, record: Option<TrackRecord>) -> Contender<'_> {
            Contender {
                entry: Some(entry),
                record,
            }
        }
        let weaker = UnknownTier::Weaker;

        // Tiers decide first, whatever the records say.
        assert_eq!(
            compare(
                side(&three, record(9, 9)),
                side(&four, record(1, 9)),
                weaker
            ),
            Strength::Weaker
        );
        assert_eq!(
            compare(side(&four, None), side(&four, None), weaker),
            Strength::Equal
        );
        assert_eq!(
            compare(side(&four, None), side(&three, None), weaker),
            Strength::Stronger
        );

        // One tier alone falls through to the records.
        assert_eq!(
            compare(
                side(&none, record(9, 10)),
                side(&four, record(5, 10)),
                weaker
            ),
            Strength::Stronger
        );
        assert_eq!(
            compare(
                side(&none, record(4, 8)),
                side(&none, record(8, 16)),
                weaker
            ),
            Strength::Equal
        );

        // Records shorter than five tasks don't count, so `unknown_tier` decides.
        assert_eq!(
            compare(side(&none, record(4, 4)), side(&none, record(1, 9)), weaker),
            Strength::Weaker
        );
        assert_eq!(
            compare(
                side(&none, record(4, 4)),
                side(&none, record(1, 9)),
                UnknownTier::Equal
            ),
            Strength::Equal
        );

        // A model missing from the registry, with no record, is unknown too.
        let unknown = Contender {
            entry: None,
            record: None,
        };
        assert_eq!(
            compare(unknown, side(&four, None), weaker),
            Strength::Weaker
        );
        assert!(compare(unknown, side(&four, None), weaker).creates_debt());
        assert!(!compare(unknown, side(&four, None), UnknownTier::Equal).creates_debt());
    }
}
