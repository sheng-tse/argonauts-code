//! Which parked runs wake now. A run parked for `AllVendorsOut`, `WaitingForVendor`, or
//! `NoStandIn` wakes once its `until` has passed or its vendor left `Down`; a handoff checkpoint
//! wakes once its role's `ask_timeout` has passed since it opened; a merge checkpoint whose debt
//! waits for an original that was out wakes once that original is back, so its `on_return` can
//! act. The engine records the cause. A wake never approves anything.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::time::SystemTime;

use argonauts_core::RunId;
use argonauts_core::events::checkpoint::{CheckpointEvent, CheckpointOpened, CheckpointPacket};
use argonauts_core::events::run::{ParkReason, ResumeCause, RunLifecycle};
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::vendor::VendorEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::journal::{self, RunLock};
use argonauts_core::layout::run_files;
use argonauts_core::limits::{HealthBoard, HealthState, QuotaStatus};
use argonauts_core::names::{CheckpointId, RoleName, VendorId};
use argonauts_core::settings::Settings;
use argonauts_core::settings::handoff::HandoffSettings;
use time::OffsetDateTime;

use crate::client::last_event;
use crate::env::AppEnv;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WakeCause {
    VendorBack,
    AskTimeout,
}

/// What each run's journal said about its park when last read. A journal can hold hundreds of
/// thousands of events, so a check reads only the ones that changed since.
#[derive(Default)]
pub(crate) struct ParkedRuns {
    known: HashMap<RunId, Known>,
    #[cfg(test)]
    reads: usize,
}

struct Known {
    stamp: Stamp,
    /// `None` when nothing a wake can end holds the run.
    park: Option<Park>,
}

/// A journal only grows, so the same length and modified time mean the same events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl Stamp {
    fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }
}

/// What a parked run waits for, taken from its journal.
enum Park {
    Vendor {
        until: Option<OffsetDateTime>,
        down_at_park: BTreeSet<VendorId>,
    },
    Handoffs {
        open: Vec<OpenHandoff>,
        /// A run keeps the settings it started with, so its own `settings.json` wins over the
        /// app's. It's written once at start.
        own_settings: Option<Box<Settings>>,
    },
    MergeDebt {
        originals: Vec<OutOriginal>,
    },
}

/// An original the open merge card's debt waits for, as it was out when the run parked.
struct OutOriginal {
    vendor: VendorId,
    /// Its quota ran out: until when, if it said.
    quota_until: Option<Option<OffsetDateTime>>,
    down: bool,
}

impl OutOriginal {
    fn back(&self, health: &dyn HealthBoard, now: OffsetDateTime) -> bool {
        let quota_back = self
            .quota_until
            .is_none_or(|until| until.is_some_and(|until| until <= now));
        let health_back = !self.down || health.state(&self.vendor).state != HealthState::Down;
        quota_back && health_back
    }
}

struct OpenHandoff {
    opened_at: OffsetDateTime,
    opened: CheckpointOpened,
    /// The stopped card's role.
    role: RoleName,
}

impl ParkedRuns {
    /// This repo's parked runs that should wake now. This reads files, so call it off the async
    /// threads.
    pub(crate) fn due(
        &mut self,
        env: &AppEnv,
        settings: &Settings,
        health: &dyn HealthBoard,
        now: OffsetDateTime,
    ) -> Vec<(RunId, WakeCause)> {
        let Ok(entries) = std::fs::read_dir(env.runs_dir()) else {
            self.known.clear();
            return Vec::new();
        };
        let mut known = HashMap::new();
        let mut due = Vec::new();
        for entry in entries.filter_map(Result::ok) {
            let Some(run) = entry
                .file_name()
                .to_str()
                .and_then(|name| RunId::new(name).ok())
            else {
                continue;
            };
            let run_dir = entry.path();
            let Some(stamp) = Stamp::of(&run_dir.join(run_files::EVENTS)) else {
                continue;
            };
            let park = match self.known.remove(&run) {
                Some(seen) if seen.stamp == stamp => seen.park,
                _ => self.read(&run_dir),
            };
            if let Some(park) = &park
                && !RunLock::is_held(&run_dir)
                && let Some(cause) = park.wakes(settings, health, now)
            {
                due.push((run.clone(), cause));
            }
            known.insert(run, Known { stamp, park });
        }
        // Runs whose folders went away drop out here.
        self.known = known;
        due.sort_by(|a, b| a.0.cmp(&b.0));
        due
    }

    fn read(&mut self, run_dir: &Path) -> Option<Park> {
        #[cfg(test)]
        {
            self.reads += 1;
        }
        Park::read(run_dir)
    }
}

impl Park {
    fn read(run_dir: &Path) -> Option<Self> {
        let events_path = run_dir.join(run_files::EVENTS);
        // A parked run's supervisor exits right after `Parked`, so that is its last event. The
        // tail is cheap to read, so only a parked run's whole journal gets parsed.
        let last = last_event(&events_path)?;
        let RunEventKind::Run(RunLifecycle::Parked { reason, waiting_on }) = last.kind else {
            return None;
        };
        let events = || journal::read_all(&events_path).ok().map(|back| back.events);
        match reason {
            // Nobody could take a seat over: it tries again at its `until`, or once a vendor
            // that went down is back.
            ParkReason::AllVendorsOut { until } | ParkReason::NoStandIn { until, .. } => {
                Some(Self::Vendor {
                    until,
                    down_at_park: vendors_down_at_park(&events()?),
                })
            }
            ParkReason::WaitingForVendor { vendor, until } => {
                let mut down_at_park = vendors_down_at_park(&events()?);
                down_at_park.retain(|down| *down == vendor);
                Some(Self::Vendor {
                    until,
                    down_at_park,
                })
            }
            ParkReason::Checkpoint => {
                let events = events()?;
                let open = open_handoffs(&events, &waiting_on);
                if open.is_empty() {
                    let originals = merge_originals(&events, &waiting_on);
                    return (!originals.is_empty()).then_some(Self::MergeDebt { originals });
                }
                let own_settings = crate::run_files::read_settings(run_dir)
                    .map(|record| Box::new(record.settings))
                    .ok();
                Some(Self::Handoffs { open, own_settings })
            }
            ParkReason::Paused => None,
        }
    }

    fn wakes(
        &self,
        settings: &Settings,
        health: &dyn HealthBoard,
        now: OffsetDateTime,
    ) -> Option<WakeCause> {
        match self {
            Self::Vendor {
                until,
                down_at_park,
            } => woke_for_vendor(*until, down_at_park, health, now),
            Self::Handoffs { open, own_settings } => {
                let settings = own_settings.as_deref().unwrap_or(settings);
                open.iter()
                    .any(|handoff| ask_timeout_passed(settings, handoff, now))
                    .then_some(WakeCause::AskTimeout)
            }
            Self::MergeDebt { originals } => originals
                .iter()
                .any(|original| original.back(health, now))
                .then_some(WakeCause::VendorBack),
        }
    }
}

/// `until` passed, or a vendor the run saw go `Down` is out of `Down` on the board now. A
/// vendor that only ran out of quota was never `Down`, so for it only `until` counts.
fn woke_for_vendor(
    until: Option<OffsetDateTime>,
    down_at_park: &BTreeSet<VendorId>,
    health: &dyn HealthBoard,
    now: OffsetDateTime,
) -> Option<WakeCause> {
    let time_passed = until.is_some_and(|until| until <= now);
    let back = down_at_park
        .iter()
        .any(|vendor| health.state(vendor).state != HealthState::Down);
    (time_passed || back).then_some(WakeCause::VendorBack)
}

/// Vendors whose last health event in the run said `Down`.
fn vendors_down_at_park(events: &[RunEvent]) -> BTreeSet<VendorId> {
    let mut states: BTreeMap<VendorId, HealthState> = BTreeMap::new();
    for event in events {
        if let RunEventKind::Vendor(VendorEvent::Health(health)) = &event.kind {
            states.insert(health.vendor.clone(), health.state);
        }
    }
    states
        .into_iter()
        .filter(|(_, state)| *state == HealthState::Down)
        .map(|(vendor, _)| vendor)
        .collect()
}

/// The originals that an open merge card among `waiting_on` lists debt for and that were out
/// when the run parked. None once the run already woke for one since the card opened, so a
/// return the engine doesn't see as one wakes it only once.
fn merge_originals(events: &[RunEvent], waiting_on: &[CheckpointId]) -> Vec<OutOriginal> {
    let mut card: Option<(CheckpointId, BTreeSet<VendorId>)> = None;
    let mut quota_until = BTreeMap::new();
    let mut states = BTreeMap::new();
    for event in events {
        match &event.kind {
            RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) => {
                if let CheckpointPacket::Merge(packet) = &opened.packet {
                    let needs = packet.rows.iter().flat_map(|row| &row.debt);
                    let vendors = needs.map(|debt| debt.needs.vendor()).collect();
                    card = Some((opened.id.clone(), vendors));
                }
            }
            RunEventKind::Checkpoint(CheckpointEvent::Decided(decided)) => {
                card.take_if(|(id, _)| *id == decided.id);
            }
            RunEventKind::Checkpoint(CheckpointEvent::Withdrawn { id, .. }) => {
                card.take_if(|(open, _)| open == id);
            }
            RunEventKind::Run(RunLifecycle::Resumed(resumed))
                if resumed.cause == ResumeCause::VendorBack =>
            {
                card = None;
            }
            RunEventKind::Vendor(VendorEvent::Quota(reading)) => {
                let out = (reading.status == QuotaStatus::Rejected).then_some(reading.resets_at);
                quota_until.insert(reading.vendor.clone(), out);
            }
            RunEventKind::Vendor(VendorEvent::Health(health)) => {
                states.insert(health.vendor.clone(), health.state);
            }
            _ => {}
        }
    }
    let Some((id, vendors)) = card else {
        return Vec::new();
    };
    if !waiting_on.is_empty() && !waiting_on.contains(&id) {
        return Vec::new();
    }
    vendors
        .into_iter()
        .filter_map(|vendor| {
            let quota_until = quota_until.get(&vendor).copied().flatten();
            let down = states.get(&vendor) == Some(&HealthState::Down);
            (quota_until.is_some() || down).then_some(OutOriginal {
                vendor,
                quota_until,
                down,
            })
        })
        .collect()
}

/// Open handoff checkpoints among `waiting_on`.
fn open_handoffs(events: &[RunEvent], waiting_on: &[CheckpointId]) -> Vec<OpenHandoff> {
    let mut roles = BTreeMap::new();
    let mut open = BTreeMap::new();
    for event in events {
        match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started)) => {
                if let Some(card) = &event.card_id {
                    roles.insert(card.clone(), started.role);
                }
            }
            RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) => {
                if matches!(opened.packet, CheckpointPacket::Handoff(_)) {
                    open.insert(opened.id.clone(), (event.ts, (**opened).clone()));
                }
            }
            RunEventKind::Checkpoint(CheckpointEvent::Decided(decided)) => {
                open.remove(&decided.id);
            }
            RunEventKind::Checkpoint(CheckpointEvent::Withdrawn { id, .. }) => {
                open.remove(id);
            }
            _ => {}
        }
    }
    open.into_values()
        .filter(|(_, opened)| waiting_on.is_empty() || waiting_on.contains(&opened.id))
        .map(|(opened_at, opened)| {
            let role = opened
                .card
                .as_ref()
                .and_then(|card| roles.get(card).copied())
                .unwrap_or(RoleName::Owner);
            OpenHandoff {
                opened_at,
                opened,
                role,
            }
        })
        .collect()
}

fn ask_timeout_passed(settings: &Settings, handoff: &OpenHandoff, now: OffsetDateTime) -> bool {
    let CheckpointPacket::Handoff(plan) = &handoff.opened.packet else {
        return false;
    };
    let role_table = settings.roles.get(&handoff.role).map(|role| &role.handoff);
    let applied = HandoffSettings::applied(&settings.handoff, role_table, &plan.stopped.vendor());
    match applied.ask_timeout.parse() {
        Ok(Some(timeout)) => handoff.opened_at + timeout <= now,
        // "none" waits for you; a bad span was already refused when the settings loaded.
        Ok(None) | Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::client::test_support::{TestHome, event_at, opened, run_started, session_started};
    use argonauts_core::events::card::CardKind;
    use argonauts_core::events::checkpoint::{CheckpointKind, MergePacket, MergeRow};
    use argonauts_core::events::run::RunResumed;
    use argonauts_core::events::vendor::HandoffPlan;
    use argonauts_core::handoff::QualityDebt;
    use argonauts_core::limits::{HealthSignal, VendorHealth};
    use argonauts_core::limits::{QuotaReading, QuotaSource};
    use argonauts_core::names::{CheckpointId, SessionKey, WorkerRef};
    use argonauts_core::settings::Span;
    use argonauts_core::settings::handoff::{HandoffMode, HandoffStrategy};
    use argonauts_core::{CardId, DiffStat, Spend};
    use tokio::sync::broadcast;

    #[derive(Default)]
    struct Board {
        down: Mutex<BTreeSet<VendorId>>,
    }

    impl HealthBoard for Board {
        fn observe(&self, _signal: HealthSignal) {}

        fn state(&self, vendor: &VendorId) -> argonauts_core::limits::VendorHealth {
            let down = self.down.lock().unwrap().contains(vendor);
            health(
                vendor,
                if down {
                    HealthState::Down
                } else {
                    HealthState::Healthy
                },
            )
        }

        fn all(&self) -> Vec<VendorHealth> {
            Vec::new()
        }

        fn subscribe(&self) -> broadcast::Receiver<VendorEvent> {
            broadcast::channel(1).1
        }

        fn test_result(&self, _vendor: &VendorId, _ok: bool) {}
    }

    fn health(vendor: &VendorId, state: HealthState) -> VendorHealth {
        VendorHealth {
            vendor: vendor.clone(),
            state,
            since: crate::client::test_support::base_ts(),
            reason: None,
            next_test_at: None,
            disabled: None,
        }
    }

    fn parked(
        seq: u64,
        at: OffsetDateTime,
        reason: ParkReason,
        waiting_on: Vec<CheckpointId>,
    ) -> RunEvent {
        event_at(
            seq,
            at,
            RunEventKind::Run(RunLifecycle::Parked { reason, waiting_on }),
        )
    }

    fn run(suffix: &str) -> RunId {
        RunId::new(format!("run-20260930-101500-{suffix}")).unwrap()
    }

    #[test]
    fn a_run_wakes_once_its_until_passed() {
        let home = TestHome::new();
        let env = home.env(true);
        let now = OffsetDateTime::now_utc();
        let past = run("0a01");
        home.run_with(
            &env,
            &past,
            &[
                run_started(0, "out", None),
                parked(
                    1,
                    now - time::Duration::hours(3),
                    ParkReason::AllVendorsOut {
                        until: Some(now - time::Duration::minutes(1)),
                    },
                    Vec::new(),
                ),
            ],
        );
        let later = run("0a02");
        home.run_with(
            &env,
            &later,
            &[
                run_started(0, "out", None),
                parked(
                    1,
                    now,
                    ParkReason::AllVendorsOut {
                        until: Some(now + time::Duration::hours(1)),
                    },
                    Vec::new(),
                ),
            ],
        );
        let due = ParkedRuns::default().due(&env, &Settings::default(), &Board::default(), now);
        assert_eq!(due, vec![(past, WakeCause::VendorBack)]);
    }

    /// A run nobody could take a seat over in tries again at its `until`, with no vendor to come
    /// back: an auth error or a usage limit never marks one down.
    #[test]
    fn a_run_nobody_could_take_a_seat_in_wakes_at_its_retry() {
        let home = TestHome::new();
        let env = home.env(true);
        let now = OffsetDateTime::now_utc();
        let stalled = |until| ParkReason::NoStandIn {
            detail: "nobody could take over as the lead".to_owned(),
            until,
        };
        let due_now = run("0f01");
        home.run_with(
            &env,
            &due_now,
            &[
                run_started(0, "stalled", None),
                parked(
                    1,
                    now - time::Duration::minutes(6),
                    stalled(Some(now - time::Duration::minutes(1))),
                    Vec::new(),
                ),
            ],
        );
        let later = run("0f02");
        home.run_with(
            &env,
            &later,
            &[
                run_started(0, "stalled", None),
                parked(
                    1,
                    now,
                    stalled(Some(now + time::Duration::minutes(5))),
                    Vec::new(),
                ),
            ],
        );
        let given_up = run("0f03");
        home.run_with(
            &env,
            &given_up,
            &[
                run_started(0, "stalled", None),
                parked(1, now, stalled(None), Vec::new()),
            ],
        );
        let due = ParkedRuns::default().due(&env, &Settings::default(), &Board::default(), now);
        assert_eq!(due, vec![(due_now, WakeCause::VendorBack)]);
    }

    #[test]
    fn a_run_wakes_when_its_vendor_is_back() {
        let home = TestHome::new();
        let env = home.env(true);
        let now = OffsetDateTime::now_utc();
        let codex = VendorId("codex".to_owned());
        let waiting = run("0b01");
        home.run_with(
            &env,
            &waiting,
            &[
                run_started(0, "waiting", None),
                event_at(
                    1,
                    now,
                    RunEventKind::Vendor(VendorEvent::Health(health(&codex, HealthState::Down))),
                ),
                parked(
                    2,
                    now,
                    ParkReason::WaitingForVendor {
                        vendor: codex.clone(),
                        until: None,
                    },
                    Vec::new(),
                ),
            ],
        );
        let quota = run("0b02");
        home.run_with(
            &env,
            &quota,
            &[
                run_started(0, "out of quota, never down", None),
                parked(
                    1,
                    now,
                    ParkReason::WaitingForVendor {
                        vendor: codex.clone(),
                        until: None,
                    },
                    Vec::new(),
                ),
            ],
        );
        let board = Board::default();
        let mut parked_runs = ParkedRuns::default();
        board.down.lock().unwrap().insert(codex.clone());
        assert!(
            parked_runs
                .due(&env, &Settings::default(), &board, now)
                .is_empty()
        );
        board.down.lock().unwrap().clear();
        assert_eq!(
            parked_runs.due(&env, &Settings::default(), &board, now),
            vec![(waiting.clone(), WakeCause::VendorBack)]
        );
        let lock_dir = env.run_dir(&waiting);
        let _live = RunLock::try_acquire(&lock_dir).unwrap().unwrap();
        assert!(
            parked_runs
                .due(&env, &Settings::default(), &board, now)
                .is_empty(),
            "a live run needs no wake"
        );
    }

    #[test]
    fn a_handoff_checkpoint_wakes_once_ask_timeout_passed() {
        let home = TestHome::new();
        let env = home.env(true);
        let now = OffsetDateTime::now_utc();
        let card = CardId::new("card-001").unwrap();
        let checkpoint = CheckpointId::numbered(1);
        let plan = HandoffPlan {
            card: card.clone(),
            title: "Export".to_owned(),
            stopped: WorkerRef::new("claude-code", None),
            reason: "usage limit".to_owned(),
            resets_at: None,
            work: DiffStat::default(),
            gates_ran: false,
            candidates: Vec::new(),
            mode: HandoffMode::Ask,
            strategy: HandoffStrategy::First,
            race_count: 1,
            also_moving: Vec::new(),
        };
        let asked_at = now - time::Duration::hours(2);
        let asking = run("0c01");
        let mut handoff = opened(
            2,
            &checkpoint,
            CheckpointKind::Handoff,
            Some(&card),
            CheckpointPacket::Handoff(plan),
        );
        handoff.ts = asked_at;
        home.run_with(
            &env,
            &asking,
            &[
                run_started(0, "handoff", None),
                session_started(
                    1,
                    &SessionKey::numbered(1),
                    Some(&card),
                    RoleName::Owner,
                    "claude-code",
                ),
                handoff,
                parked(3, asked_at, ParkReason::Checkpoint, vec![checkpoint]),
            ],
        );

        let mut settings = Settings::default();
        let mut parked_runs = ParkedRuns::default();
        assert!(
            parked_runs
                .due(&env, &settings, &Board::default(), now)
                .is_empty(),
            "\"none\" waits for you"
        );
        settings.handoff.ask_timeout = Span::new("1h");
        for role in settings.roles.values_mut() {
            role.handoff.ask_timeout = Span::new("1h");
        }
        assert_eq!(
            parked_runs.due(&env, &settings, &Board::default(), now),
            vec![(asking, WakeCause::AskTimeout)]
        );
        settings.handoff.ask_timeout = Span::new("3h");
        for role in settings.roles.values_mut() {
            role.handoff.ask_timeout = Span::new("3h");
        }
        assert!(
            parked_runs
                .due(&env, &settings, &Board::default(), now)
                .is_empty()
        );
    }

    /// A run parked on a merge card whose debt needs Claude Code: `out` says how the run saw
    /// it go, and `then` follows the park.
    fn merge_waiting_on_claude(out: Option<RunEvent>, then: Vec<RunEvent>) -> Vec<RunEvent> {
        let checkpoint = CheckpointId::numbered(2);
        let worker = |name: &str| WorkerRef::new(name, None);
        let row = MergeRow {
            card: CardId::new("card-001").unwrap(),
            title: "Export".to_owned(),
            kind: CardKind::Build,
            stat: DiffStat::default(),
            owner: Some(worker("codex")),
            gates_passed: true,
            review: None,
            debt: vec![QualityDebt {
                since: 1,
                by: worker("codex"),
                needs: worker("claude-code"),
            }],
        };
        let packet = MergePacket {
            target: Some("main".to_owned()),
            branch: "integration".to_owned(),
            rows: vec![row],
            suite_passed_on_latest_main: true,
            gates: Vec::new(),
            combined: DiffStat::default(),
            conductor: DiffStat::default(),
            diff_path: std::path::PathBuf::new(),
            tree_sha: String::new(),
            spent: Spend::default(),
            limit: None,
            partial: false,
            in_the_way: Vec::new(),
            edited: Vec::new(),
            refused: None,
        };
        let merge = CheckpointPacket::Merge(packet);
        let parked_at = OffsetDateTime::now_utc() - time::Duration::hours(1);
        let mut events = vec![run_started(0, "merge", None)];
        events.extend(out);
        events.push(opened(2, &checkpoint, CheckpointKind::Merge, None, merge));
        events.push(parked(
            3,
            parked_at,
            ParkReason::Checkpoint,
            vec![checkpoint],
        ));
        events.extend(then);
        events
    }

    #[test]
    fn a_merge_card_wakes_once_an_original_its_debt_needs_is_back() {
        let home = TestHome::new();
        let env = home.env(true);
        let now = OffsetDateTime::now_utc();
        let claude = VendorId("claude-code".to_owned());
        let rejected = |resets_at| {
            let reading = QuotaReading {
                vendor: claude.clone(),
                status: QuotaStatus::Rejected,
                used: Some(1.0),
                resets_at: Some(resets_at),
                window: Some("five_hour".to_owned()),
                source: QuotaSource::RateLimitEvent,
                observed_at: now - time::Duration::hours(2),
                detail: None,
                wrap_up: false,
            };
            Some(event_at(
                1,
                now,
                RunEventKind::Vendor(VendorEvent::Quota(reading)),
            ))
        };
        let down = Some(event_at(
            1,
            now,
            RunEventKind::Vendor(VendorEvent::Health(health(&claude, HealthState::Down))),
        ));
        let in_an_hour = now + time::Duration::hours(1);
        let a_minute_ago = now - time::Duration::minutes(1);
        let resumed = RunResumed {
            cause: ResumeCause::VendorBack,
            settings_changed: false,
            harness_versions_changed: Vec::new(),
            main_moved_by: None,
            sessions: Vec::new(),
        };
        let woke_again = vec![
            event_at(4, now, RunEventKind::Run(RunLifecycle::Resumed(resumed))),
            parked(
                5,
                now,
                ParkReason::Checkpoint,
                vec![CheckpointId::numbered(2)],
            ),
        ];
        let journals = [
            (
                "0d01",
                merge_waiting_on_claude(rejected(in_an_hour), Vec::new()),
            ),
            (
                "0d02",
                merge_waiting_on_claude(rejected(a_minute_ago), Vec::new()),
            ),
            ("0d03", merge_waiting_on_claude(down, Vec::new())),
            // Never out as far as the run saw: the merge rule had its chance before it parked.
            ("0d04", merge_waiting_on_claude(None, Vec::new())),
            // Woken once for this card already, it isn't woken for it again.
            (
                "0d05",
                merge_waiting_on_claude(rejected(a_minute_ago), woke_again),
            ),
        ];
        for (suffix, events) in &journals {
            home.run_with(&env, &run(suffix), events);
        }

        let board = Board::default();
        board.down.lock().unwrap().insert(claude.clone());
        let due = ParkedRuns::default().due(&env, &Settings::default(), &board, now);
        assert_eq!(due, vec![(run("0d02"), WakeCause::VendorBack)]);
        let due = ParkedRuns::default().due(&env, &Settings::default(), &Board::default(), now);
        assert_eq!(
            due,
            vec![
                (run("0d02"), WakeCause::VendorBack),
                (run("0d03"), WakeCause::VendorBack)
            ]
        );
    }

    #[test]
    fn a_journal_is_read_again_only_once_it_grows() {
        let home = TestHome::new();
        let env = home.env(true);
        let now = OffsetDateTime::now_utc();
        let out_until = |seq, until| {
            parked(
                seq,
                now,
                ParkReason::AllVendorsOut { until: Some(until) },
                Vec::new(),
            )
        };
        let waiting = run("0d01");
        home.run_with(
            &env,
            &waiting,
            &[
                run_started(0, "out", None),
                out_until(1, now + time::Duration::hours(1)),
            ],
        );
        let settings = Settings::default();
        let board = Board::default();
        let mut parked_runs = ParkedRuns::default();
        assert!(parked_runs.due(&env, &settings, &board, now).is_empty());
        assert!(parked_runs.due(&env, &settings, &board, now).is_empty());
        assert_eq!(parked_runs.reads, 1, "an unchanged journal is read once");
        assert_eq!(
            parked_runs.due(&env, &settings, &board, now + time::Duration::hours(2)),
            vec![(waiting.clone(), WakeCause::VendorBack)],
            "what the journal said is checked against the time now"
        );
        assert_eq!(parked_runs.reads, 1);

        let later = run("0d02");
        home.run_with(
            &env,
            &later,
            &[
                run_started(0, "out", None),
                out_until(1, now + time::Duration::hours(1)),
            ],
        );
        home.run_with(
            &env,
            &later,
            &[out_until(2, now - time::Duration::minutes(1))],
        );
        assert_eq!(
            parked_runs.due(&env, &settings, &board, now),
            vec![(later.clone(), WakeCause::VendorBack)]
        );
        assert_eq!(parked_runs.reads, 2, "only the new journal is read");
        home.run_with(
            &env,
            &waiting,
            &[out_until(2, now - time::Duration::minutes(1))],
        );
        assert_eq!(
            parked_runs.due(&env, &settings, &board, now),
            vec![
                (waiting.clone(), WakeCause::VendorBack),
                (later.clone(), WakeCause::VendorBack)
            ]
        );
        assert_eq!(parked_runs.reads, 3, "a grown journal is read again");

        std::fs::remove_dir_all(env.run_dir(&waiting)).unwrap();
        assert_eq!(
            parked_runs.due(&env, &settings, &board, now),
            vec![(later, WakeCause::VendorBack)]
        );
        assert!(
            !parked_runs.known.contains_key(&waiting),
            "a removed run is forgotten"
        );
    }
}
