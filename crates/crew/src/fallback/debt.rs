//! Quality debt: a task carries it when a commit since a checkpoint came from a fallback
//! weaker than its strongest previous worker. Weaker by the tiers you set, else track records
//! (both at least 5 finished tasks of that kind), else `unknown_tier`. Your own commits and a
//! fallback at the same tier or above create none.

use std::cmp::Ordering;

use argonauts_core::events::task::{TaskKind, TaskSpec};
use argonauts_core::limits::TrackRecord;
use argonauts_core::names::ModelRef;
use argonauts_core::settings::fallback::UnknownTier;

use crate::engine::host::Host;

/// Finished tasks of a kind both models need before track records decide.
pub const MIN_RECORD: u32 = 5;

/// What's known about a model for the comparison.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Strength {
    pub tier: Option<u8>,
    pub record: Option<TrackRecord>,
}

/// How `fallback` compares with `original`: `Less` means weaker, so the task takes debt.
pub fn compare(fallback: Strength, original: Strength, unknown: UnknownTier) -> Ordering {
    if let (Some(fallback), Some(original)) = (fallback.tier, original.tier) {
        // Tier 5 is the strongest.
        return fallback.cmp(&original);
    }
    if let (Some(fallback), Some(original)) = (fallback.record, original.record)
        && fallback.of >= MIN_RECORD
        && original.of >= MIN_RECORD
    {
        // Pass rates compared without floats: a/b against c/d is a*d against c*b.
        let left = u64::from(fallback.passed) * u64::from(original.of);
        let right = u64::from(original.passed) * u64::from(fallback.of);
        return left.cmp(&right);
    }
    match unknown {
        UnknownTier::Weaker => Ordering::Less,
        UnknownTier::Equal => Ordering::Equal,
    }
}

/// The track-record key for a task's kind.
pub fn task_type(kind: &TaskKind) -> &'static str {
    match kind {
        TaskKind::Captain => "captain",
        TaskKind::Interface => "interface",
        TaskKind::AcceptanceTests { .. } => "acceptance_tests",
        TaskKind::Build => "build",
        TaskKind::Chore => "chore",
        TaskKind::Docs => "docs",
        TaskKind::Experiment { .. } => "experiment",
    }
}

/// A worker's tier from the registry and its track record on tasks of this kind.
pub fn strength(host: &Host, worker: &ModelRef, task: &TaskSpec) -> Strength {
    let services = host.services();
    Strength {
        tier: services.registry.get(worker).and_then(|entry| entry.tier),
        record: services.track_records.get(worker, task_type(&task.kind)),
    }
}

/// Whether `worker` is weaker than `than` for this task.
pub fn weaker(
    host: &Host,
    task: &TaskSpec,
    worker: &ModelRef,
    than: &ModelRef,
    unknown: UnknownTier,
) -> bool {
    worker != than
        && compare(
            strength(host, worker, task),
            strength(host, than, task),
            unknown,
        ) == Ordering::Less
}

/// The strongest of `workers` for this task, the earliest one on a tie.
pub fn strongest<'a>(
    host: &Host,
    task: &TaskSpec,
    workers: impl IntoIterator<Item = &'a ModelRef>,
    unknown: UnknownTier,
) -> Option<ModelRef> {
    let mut best: Option<&ModelRef> = None;
    for worker in workers {
        best = match best {
            Some(current) if !weaker(host, task, current, worker, unknown) => Some(current),
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
    fn track_records_need_five_finished_tasks_each() {
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
        // Four tasks aren't enough, so the unknown rule decides.
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
    fn task_types_name_the_task_kind() {
        assert_eq!(task_type(&TaskKind::Build), "build");
        assert_eq!(
            task_type(&TaskKind::AcceptanceTests {
                for_task: argonauts_core::TaskId::numbered(1)
            }),
            "acceptance_tests"
        );
    }
}
