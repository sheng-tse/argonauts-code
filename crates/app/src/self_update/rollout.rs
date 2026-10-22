//! Who gets a release while it rolls out. Each install keeps a random rollout id of its own,
//! never sent anywhere, and its bucket for a version comes from hashing the two, so the same
//! installs go first for a version and a different few for the next. The schedule widens with
//! the clock, so letting more installs in needs no new signature.

use semver::Version;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::RolloutStep;

/// This install's place for `version`, from 0 to 99.
pub fn bucket(rollout_id: &str, version: &Version) -> u8 {
    let digest = Sha256::digest(format!("{rollout_id} {version}").as_bytes());
    let first = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
    u8::try_from(first % 100).unwrap_or(0)
}

/// The share of installs that have the release at `now`: none before the schedule starts,
/// every one without a schedule.
pub fn share_now(schedule: &[RolloutStep], now: OffsetDateTime) -> u8 {
    if schedule.is_empty() {
        return 100;
    }
    schedule
        .iter()
        .filter(|step| step.from <= now)
        .map(|step| step.percent.min(100))
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    #[test]
    fn a_bucket_stays_put_for_a_version_and_moves_for_the_next() {
        let one = Version::new(0, 1, 1);
        let id = "5b0e7c4e-1f53-4c55-9d43-0c1d7f0f2a10";
        assert_eq!(bucket(id, &one), bucket(id, &one));
        assert!(bucket(id, &one) < 100);
        let moved = (2..40).any(|patch| bucket(id, &Version::new(0, 1, patch)) != bucket(id, &one));
        assert!(moved, "another version draws another bucket");
    }

    #[test]
    fn buckets_spread_over_the_hundred() {
        let version = Version::new(0, 1, 1);
        let mut counts = [0u32; 10];
        for n in 0..2000 {
            counts[usize::from(bucket(&format!("install-{n}"), &version) / 10)] += 1;
        }
        // Each tenth holds about 200 of the 2000.
        assert!(
            counts.iter().all(|&count| (140..=260).contains(&count)),
            "{counts:?}"
        );
    }

    #[test]
    fn the_share_widens_with_the_clock() {
        let schedule = [
            RolloutStep {
                from: datetime!(2026-11-02 16:00 UTC),
                percent: 10,
            },
            RolloutStep {
                from: datetime!(2026-11-03 16:00 UTC),
                percent: 50,
            },
            RolloutStep {
                from: datetime!(2026-11-04 16:00 UTC),
                percent: 100,
            },
        ];
        assert_eq!(share_now(&schedule, datetime!(2026-11-02 15:00 UTC)), 0);
        assert_eq!(share_now(&schedule, datetime!(2026-11-02 16:00 UTC)), 10);
        assert_eq!(share_now(&schedule, datetime!(2026-11-03 20:00 UTC)), 50);
        assert_eq!(share_now(&schedule, datetime!(2026-12-01 00:00 UTC)), 100);
        assert_eq!(share_now(&[], datetime!(2026-11-02 15:00 UTC)), 100);
    }
}
