//! Daily caps across every repo, in `~/.argonauts/daily.json`: auto-merges, spend, and runs
//! started by outside agents. Each check and its record happen under one file lock, so two
//! run hosts can't both take the last auto-merge.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use argonauts_core::base::clock;
use argonauts_core::base::layout::user_files;
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime, UtcOffset};

use crate::autonomy::file_lock::{self, Access};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DailyError {
    /// The cap is used up for today.
    #[error("{0}")]
    Capped(String),
    #[error("the daily ledger {path}: {message}")]
    Io { path: PathBuf, message: String },
}

pub struct DailyLedger {
    path: PathBuf,
    lock_wait: Duration,
}

/// One UTC day's counts. Older days are dropped when a new one starts.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Day {
    /// `YYYY-MM-DD`, in UTC.
    day: String,
    auto_merges: BTreeSet<String>,
    /// Each run's latest spend in USD.
    spend: BTreeMap<String, f64>,
    /// Each run's spend when yesterday ended, so a run that spans midnight counts only today's
    /// part.
    spend_before_today: BTreeMap<String, f64>,
    outside_agent_runs: u32,
}

impl Day {
    fn roll_to(&mut self, today: Date) {
        let today_name = today.to_string();
        // A clock that steps back keeps the later day's counts rather than resetting them.
        if self.day >= today_name {
            return;
        }
        let yesterday = today.previous_day().map(|day| day.to_string());
        let carried = if yesterday.as_deref() == Some(self.day.as_str()) {
            std::mem::take(&mut self.spend)
        } else {
            BTreeMap::new()
        };
        *self = Day {
            day: today_name,
            spend_before_today: carried,
            ..Day::default()
        };
    }

    fn spent_today(&self) -> f64 {
        self.spend
            .iter()
            .map(|(key, latest)| {
                let before = self.spend_before_today.get(key).copied().unwrap_or(0.0);
                (latest - before).max(0.0)
            })
            .sum()
    }

    fn auto_merges_taken(&self) -> u32 {
        u32::try_from(self.auto_merges.len()).unwrap_or(u32::MAX)
    }
}

impl DailyLedger {
    /// The ledger in `home`, which is `~/.argonauts`.
    pub fn open(home: &Path) -> Self {
        Self {
            path: home.join(user_files::DAILY),
            lock_wait: file_lock::LEDGER_WAIT,
        }
    }

    #[cfg(test)]
    fn with_lock_wait(mut self, wait: Duration) -> Self {
        self.lock_wait = wait;
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Takes one of today's auto-merges for `key` (repo, run, and approval), unless `cap` are
    /// taken. Taking the same key twice counts once. Returns today's count.
    pub fn take_auto_merge(
        &self,
        key: &str,
        cap: Option<u32>,
        now: OffsetDateTime,
    ) -> Result<u32, DailyError> {
        self.update(now, |day| {
            if !day.auto_merges.contains(key) {
                if let Some(cap) = cap
                    && day.auto_merges_taken() >= cap
                {
                    return Err(DailyError::Capped(format!(
                        "today's auto-merges are used up ({cap} a day)"
                    )));
                }
                day.auto_merges.insert(key.to_owned());
            }
            Ok(day.auto_merges_taken())
        })
    }

    /// Gives back an auto-merge your answer beat.
    pub fn release_auto_merge(&self, key: &str, now: OffsetDateTime) -> Result<(), DailyError> {
        self.update(now, |day| {
            day.auto_merges.remove(key);
            Ok(())
        })
    }

    /// Records a run's spend so far, keyed like auto-merges, and checks the day's total across
    /// every repo against `cap_usd`. Returns the total.
    pub fn record_spend(
        &self,
        key: &str,
        usd: f64,
        cap_usd: Option<f64>,
        now: OffsetDateTime,
    ) -> Result<f64, DailyError> {
        if !usd.is_finite() || usd < 0.0 {
            return Err(DailyError::Capped(format!(
                "a spend of {usd} USD can't be checked against the daily ceiling"
            )));
        }
        self.update(now, |day| {
            let latest = day.spend.entry(key.to_owned()).or_insert(0.0);
            // A run's spend only grows, so a late, smaller report can't hide what it spent.
            *latest = latest.max(usd);
            let total = day.spent_today();
            match cap_usd {
                Some(cap) if cap.is_nan() || total >= cap => Err(DailyError::Capped(format!(
                    "today's spend of ${total:.2} reached the ${cap:.2} daily ceiling"
                ))),
                _ => Ok(total),
            }
        })
    }

    /// Takes one of today's runs for outside agents, unless `cap` are taken. Returns today's count.
    pub fn take_outside_agent_run(&self, cap: u32, now: OffsetDateTime) -> Result<u32, DailyError> {
        self.update(now, |day| {
            if day.outside_agent_runs >= cap {
                return Err(DailyError::Capped(format!(
                    "today's runs from outside agents are used up ({cap} a day)"
                )));
            }
            day.outside_agent_runs += 1;
            Ok(day.outside_agent_runs)
        })
    }

    /// Gives back an outside-agent run taken at `taken` whose start failed. A run taken on an
    /// earlier day went with that day, so today's count keeps it.
    pub fn release_outside_agent_run(
        &self,
        taken: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<(), DailyError> {
        let taken_day = real_day(taken).to_string();
        self.update(now, |day| {
            if day.day == taken_day {
                day.outside_agent_runs = day.outside_agent_runs.saturating_sub(1);
            }
            Ok(())
        })
    }

    /// Reads, changes, and writes the ledger under one exclusive lock. It's written in place,
    /// since a rename would move the next writer to a new, unlocked inode. The ledger is written
    /// and synced even when `change` refuses, so spend and a new day's reset are kept.
    fn update<T>(
        &self,
        now: OffsetDateTime,
        change: impl FnOnce(&mut Day) -> Result<T, DailyError>,
    ) -> Result<T, DailyError> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&self.path)
            .map_err(|err| self.io(err))?;
        file_lock::lock_within(&file, Access::Exclusive, self.lock_wait)
            .map_err(|err| self.io(err))?;

        let mut text = String::new();
        file.read_to_string(&mut text).map_err(|err| self.io(err))?;
        let mut day: Day = if text.trim().is_empty() {
            Day::default()
        } else {
            serde_json::from_str(&text).map_err(|err| DailyError::Io {
                path: self.path.clone(),
                message: format!("it doesn't parse ({err}). Removing it resets today's counts"),
            })?
        };
        day.roll_to(real_day(now));
        let result = change(&mut day);

        let json = serde_json::to_vec_pretty(&day).map_err(|err| self.io(io::Error::other(err)))?;
        // Written before truncating, so a crash leaves a file that fails to parse rather than
        // an empty one that would reset the caps.
        file.seek(SeekFrom::Start(0))
            .and_then(|_| file.write_all(&json))
            .and_then(|()| file.set_len(json.len() as u64))
            .and_then(|()| file.sync_data())
            .map_err(|err| self.io(err))?;
        result
    }

    fn io(&self, err: io::Error) -> DailyError {
        DailyError::Io {
            path: self.path.clone(),
            message: err.to_string(),
        }
    }
}

/// The real UTC day of `at`, an instant of the app's time: a day's caps count real days even
/// when `ARGONAUTS_CLOCK` sets the app's time.
fn real_day(at: OffsetDateTime) -> Date {
    clock::real(at).to_offset(UtcOffset::UTC).date()
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;

    use time::Duration;
    use time::macros::datetime;

    use super::*;

    const NOW: OffsetDateTime = datetime!(2026-09-29 10:00 UTC);

    #[test]
    fn daily_caps_hold_under_a_lock_from_two_threads() {
        for round in 0..20 {
            let home = tempfile::tempdir().expect("temp dir");
            let ledger = DailyLedger::open(home.path());
            ledger
                .take_auto_merge("repo/run-1/approval-001", Some(3), NOW)
                .expect("first");
            ledger
                .take_auto_merge("repo/run-2/approval-001", Some(3), NOW)
                .expect("second");

            let start = Barrier::new(2);
            let results: Vec<_> = std::thread::scope(|scope| {
                let racers: Vec<_> = ["a", "b"]
                    .into_iter()
                    .map(|name| {
                        let start = &start;
                        let home = home.path();
                        scope.spawn(move || {
                            // Each thread opens the file itself, like another run host.
                            let ledger = DailyLedger::open(home);
                            start.wait();
                            ledger.take_auto_merge(
                                &format!("repo/run-{name}/approval-003"),
                                Some(3),
                                NOW,
                            )
                        })
                    })
                    .collect();
                racers
                    .into_iter()
                    .map(|racer| racer.join().expect("racer"))
                    .collect()
            });
            let won = results.iter().filter(|result| result.is_ok()).count();
            let capped = results
                .iter()
                .filter(|result| matches!(result, Err(DailyError::Capped(_))))
                .count();
            assert_eq!((won, capped), (1, 1), "round {round}: {results:?}");
        }
    }

    #[test]
    fn auto_merges_count_each_key_once_and_can_be_released() {
        let home = tempfile::tempdir().expect("temp dir");
        let ledger = DailyLedger::open(home.path());
        assert_eq!(
            ledger.take_auto_merge("r/a/approval-001", Some(1), NOW),
            Ok(1)
        );
        assert_eq!(
            ledger.take_auto_merge("r/a/approval-001", Some(1), NOW),
            Ok(1)
        );
        assert!(matches!(
            ledger.take_auto_merge("r/b/approval-001", Some(1), NOW),
            Err(DailyError::Capped(_))
        ));
        ledger
            .release_auto_merge("r/a/approval-001", NOW)
            .expect("release");
        assert_eq!(
            ledger.take_auto_merge("r/b/approval-001", Some(1), NOW),
            Ok(1)
        );
        assert_eq!(
            ledger.take_auto_merge("r/c/approval-001", Some(1), NOW + Duration::days(1)),
            Ok(1)
        );
    }

    #[test]
    fn without_a_cap_every_auto_merge_is_counted_and_none_refused() {
        let home = tempfile::tempdir().expect("temp dir");
        let ledger = DailyLedger::open(home.path());
        for n in 1..=20 {
            let key = format!("r/run-{n}/approval-001");
            assert_eq!(ledger.take_auto_merge(&key, None, NOW), Ok(n));
        }
    }

    #[test]
    fn outside_agent_runs_capped_per_day() {
        let home = tempfile::tempdir().expect("temp dir");
        let ledger = DailyLedger::open(home.path());
        assert_eq!(ledger.take_outside_agent_run(2, NOW), Ok(1));
        assert_eq!(ledger.take_outside_agent_run(2, NOW), Ok(2));
        assert!(matches!(
            ledger.take_outside_agent_run(2, NOW),
            Err(DailyError::Capped(_))
        ));

        // A clock stepping back doesn't reset today's count.
        assert!(
            ledger
                .take_outside_agent_run(2, NOW - Duration::days(1))
                .is_err()
        );

        let tomorrow = datetime!(2026-09-30 00:00:01 UTC);
        assert_eq!(ledger.take_outside_agent_run(2, tomorrow), Ok(1));
        assert_eq!(ledger.take_outside_agent_run(0, tomorrow).ok(), None);
    }

    #[test]
    fn outside_agent_runs_given_back_only_to_the_day_they_were_taken() {
        let home = tempfile::tempdir().expect("temp dir");
        let ledger = DailyLedger::open(home.path());
        assert_eq!(ledger.take_outside_agent_run(1, NOW), Ok(1));
        ledger.release_outside_agent_run(NOW, NOW).expect("release");
        assert_eq!(ledger.take_outside_agent_run(1, NOW), Ok(1));

        // Giving back more than was taken leaves the count at zero.
        ledger.release_outside_agent_run(NOW, NOW).expect("release");
        ledger.release_outside_agent_run(NOW, NOW).expect("release");
        assert_eq!(ledger.take_outside_agent_run(1, NOW), Ok(1));

        // A run taken before midnight and given back after it doesn't free one of today's.
        let tomorrow = datetime!(2026-09-30 00:00:01 UTC);
        assert_eq!(ledger.take_outside_agent_run(1, tomorrow), Ok(1));
        ledger
            .release_outside_agent_run(NOW, tomorrow)
            .expect("release");
        assert!(matches!(
            ledger.take_outside_agent_run(1, tomorrow),
            Err(DailyError::Capped(_))
        ));
    }

    #[test]
    fn spend_counts_only_todays_part_of_a_run() {
        let home = tempfile::tempdir().expect("temp dir");
        let ledger = DailyLedger::open(home.path());
        let evening = datetime!(2026-09-29 23:00 UTC);
        assert_eq!(
            ledger.record_spend("r/a", 30.0, Some(40.0), evening),
            Ok(30.0)
        );
        assert_eq!(
            ledger.record_spend("r/b", 5.0, Some(40.0), evening),
            Ok(35.0)
        );
        assert!(matches!(
            ledger.record_spend("r/c", 5.0, Some(40.0), evening),
            Err(DailyError::Capped(_))
        ));

        let morning = datetime!(2026-09-30 01:00 UTC);
        assert_eq!(
            ledger.record_spend("r/a", 32.0, Some(40.0), morning),
            Ok(2.0)
        );
        assert_eq!(ledger.record_spend("r/d", 1.0, None, morning), Ok(3.0));
        // A smaller, late report can't lower what a run spent.
        assert_eq!(
            ledger.record_spend("r/a", 31.0, Some(40.0), morning),
            Ok(3.0)
        );
        assert!(ledger.record_spend("r/a", f64::NAN, None, morning).is_err());
    }

    #[test]
    fn a_ledger_that_does_not_parse_refuses() {
        let home = tempfile::tempdir().expect("temp dir");
        let ledger = DailyLedger::open(home.path());
        std::fs::write(ledger.path(), "{ torn").expect("write");
        assert!(matches!(
            ledger.take_auto_merge("r/a/approval-001", Some(3), NOW),
            Err(DailyError::Io { .. })
        ));
        let refused = ledger
            .take_outside_agent_run(3, NOW)
            .expect_err("a torn ledger");
        let message = refused.to_string();
        assert!(message.contains(user_files::DAILY), "{message}");
        assert!(message.contains("Removing it resets"), "{message}");

        std::fs::remove_file(ledger.path()).expect("remove");
        assert_eq!(ledger.take_outside_agent_run(3, NOW), Ok(1));
        let missing = DailyLedger::open(&home.path().join("gone"));
        assert!(missing.take_outside_agent_run(3, NOW).is_err());
    }

    #[test]
    fn a_ledger_locked_elsewhere_refuses_instead_of_waiting() {
        let home = tempfile::tempdir().expect("temp dir");
        let ledger =
            DailyLedger::open(home.path()).with_lock_wait(std::time::Duration::from_millis(50));
        assert_eq!(ledger.take_outside_agent_run(3, NOW), Ok(1));
        let holder = std::fs::File::open(ledger.path()).expect("open");
        holder.lock().expect("hold");
        let Err(DailyError::Io { message, .. }) = ledger.take_outside_agent_run(3, NOW) else {
            panic!("a held ledger should refuse");
        };
        assert!(message.contains("locked"), "{message}");

        holder.unlock().expect("unlock");
        assert_eq!(ledger.take_outside_agent_run(3, NOW), Ok(2));
    }
}
