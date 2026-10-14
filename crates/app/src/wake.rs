//! Which waiting runs wake now. A run waiting for `AllAgentsOut`, `WaitingForAgent`, or
//! `NoFallback` wakes once its `until` has passed or its agent left `Down`; a fallback approval
//! wakes once its role's `ask_timeout` has passed since it opened; a merge approval whose debt
//! waits for an original that was out wakes once that original is back, so its `on_return` can
//! act. The engine records the cause. A wake never approves anything.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::time::SystemTime;

use argonauts_core::RunId;
use argonauts_core::events::agent_status::AgentStatusEvent;
use argonauts_core::events::approval::{ApprovalEvent, ApprovalOpened, ApprovalPacket};
use argonauts_core::events::run::{ResumeCause, RunLifecycle, WaitReason};
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::journal::{self, RunLock};
use argonauts_core::layout::run_files;
use argonauts_core::limits::{HealthBoard, HealthState, PlanUsageStatus};
use argonauts_core::names::{AgentId, ApprovalId, RoleName};
use argonauts_core::settings::Settings;
use time::OffsetDateTime;

use crate::client::last_event;
use crate::env::AppEnv;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WakeCause {
    AgentBack,
    AskTimeout,
}

/// What each run's journal said about its wait when last read. A journal can hold hundreds of
/// thousands of events, so a check reads only the ones that changed since.
#[derive(Default)]
pub(crate) struct WaitingRuns {
    known: HashMap<RunId, Known>,
    #[cfg(test)]
    reads: usize,
}

struct Known {
    stamp: Stamp,
    /// `None` when nothing a wake can end holds the run.
    wait: Option<Wait>,
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

/// What a waiting run waits for, taken from its journal.
enum Wait {
    Cli {
        until: Option<OffsetDateTime>,
        down_at_wait: BTreeSet<AgentId>,
    },
    Fallbacks {
        open: Vec<OpenFallback>,
        /// A run keeps the settings it started with, so its own `settings.json` wins over the
        /// app's. It's written once at start.
        own_settings: Option<Box<Settings>>,
    },
    MergeDebt {
        originals: Vec<OutOriginal>,
    },
}

/// An original the open merge question's debt waits for, as it was out when the run began to wait.
struct OutOriginal {
    agent_id: AgentId,
    /// Its plan usage ran out: until when, if it said.
    plan_usage_until: Option<Option<OffsetDateTime>>,
    down: bool,
}

impl OutOriginal {
    fn back(&self, health: &dyn HealthBoard, now: OffsetDateTime) -> bool {
        let plan_usage_back = self
            .plan_usage_until
            .is_none_or(|until| until.is_some_and(|until| until <= now));
        let health_back = !self.down || health.state(&self.agent_id).state != HealthState::Down;
        plan_usage_back && health_back
    }
}

struct OpenFallback {
    opened_at: OffsetDateTime,
    opened: ApprovalOpened,
    /// The stopped task's role.
    role: RoleName,
}

impl WaitingRuns {
    /// This repo's waiting runs that should wake now. This reads files, so call it off the async
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
            let wait = match self.known.remove(&run) {
                Some(seen) if seen.stamp == stamp => seen.wait,
                _ => self.read(&run_dir),
            };
            if let Some(wait) = &wait
                && !RunLock::is_held(&run_dir)
                && let Some(cause) = wait.wakes(settings, health, now)
            {
                due.push((run.clone(), cause));
            }
            known.insert(run, Known { stamp, wait });
        }
        // Runs whose folders went away drop out here.
        self.known = known;
        due.sort_by(|a, b| a.0.cmp(&b.0));
        due
    }

    fn read(&mut self, run_dir: &Path) -> Option<Wait> {
        #[cfg(test)]
        {
            self.reads += 1;
        }
        Wait::read(run_dir)
    }
}

impl Wait {
    fn read(run_dir: &Path) -> Option<Self> {
        let events_path = run_dir.join(run_files::EVENTS);
        // A waiting run's run host exits right after `Waiting`, so that is its last event. The
        // tail is cheap to read, so only a waiting run's whole journal gets parsed.
        let last = last_event(&events_path)?;
        let RunEventKind::Run(RunLifecycle::Waiting { reason, waiting_on }) = last.kind else {
            return None;
        };
        let events = || journal::read_all(&events_path).ok().map(|back| back.events);
        match reason {
            // No fallback could take a job: it tries again at its `until`, or once an agent
            // that went down is back.
            WaitReason::AllAgentsOut { until } | WaitReason::NoFallback { until, .. } => {
                Some(Self::Cli {
                    until,
                    down_at_wait: agents_down_at_wait(&events()?),
                })
            }
            WaitReason::WaitingForAgent { agent_id, until } => {
                let mut down_at_wait = agents_down_at_wait(&events()?);
                down_at_wait.retain(|down| *down == agent_id);
                Some(Self::Cli {
                    until,
                    down_at_wait,
                })
            }
            WaitReason::Approval => {
                let events = events()?;
                let open = open_fallbacks(&events, &waiting_on);
                if open.is_empty() {
                    let originals = merge_originals(&events, &waiting_on);
                    return (!originals.is_empty()).then_some(Self::MergeDebt { originals });
                }
                let own_settings = crate::run_files::read_settings(run_dir)
                    .map(|record| Box::new(record.settings))
                    .ok();
                Some(Self::Fallbacks { open, own_settings })
            }
            WaitReason::Paused => None,
        }
    }

    fn wakes(
        &self,
        settings: &Settings,
        health: &dyn HealthBoard,
        now: OffsetDateTime,
    ) -> Option<WakeCause> {
        match self {
            Self::Cli {
                until,
                down_at_wait,
            } => woke_for_agent(*until, down_at_wait, health, now),
            Self::Fallbacks { open, own_settings } => {
                let settings = own_settings.as_deref().unwrap_or(settings);
                open.iter()
                    .any(|fallback| ask_timeout_passed(settings, fallback, now))
                    .then_some(WakeCause::AskTimeout)
            }
            Self::MergeDebt { originals } => originals
                .iter()
                .any(|original| original.back(health, now))
                .then_some(WakeCause::AgentBack),
        }
    }
}

/// `until` passed, or an agent the run saw go `Down` is out of `Down` on the board now. A
/// agent that only ran out of plan usage was never `Down`, so for it only `until` counts.
fn woke_for_agent(
    until: Option<OffsetDateTime>,
    down_at_wait: &BTreeSet<AgentId>,
    health: &dyn HealthBoard,
    now: OffsetDateTime,
) -> Option<WakeCause> {
    let time_passed = until.is_some_and(|until| until <= now);
    let back = down_at_wait
        .iter()
        .any(|agent_id| health.state(agent_id).state != HealthState::Down);
    (time_passed || back).then_some(WakeCause::AgentBack)
}

/// Agents whose last health event in the run said `Down`.
fn agents_down_at_wait(events: &[RunEvent]) -> BTreeSet<AgentId> {
    let mut states: BTreeMap<AgentId, HealthState> = BTreeMap::new();
    for event in events {
        if let RunEventKind::AgentStatus(AgentStatusEvent::Health(health)) = &event.kind {
            states.insert(health.agent_id.clone(), health.state);
        }
    }
    states
        .into_iter()
        .filter(|(_, state)| *state == HealthState::Down)
        .map(|(agent_id, _)| agent_id)
        .collect()
}

/// The originals that an open merge question among `waiting_on` lists debt for and that were out
/// when the run began to wait. None once the run already woke for one since the question
/// opened, so a
/// return the engine doesn't see as one wakes it only once.
fn merge_originals(events: &[RunEvent], waiting_on: &[ApprovalId]) -> Vec<OutOriginal> {
    let mut task: Option<(ApprovalId, BTreeSet<AgentId>)> = None;
    let mut plan_usage_until = BTreeMap::new();
    let mut states = BTreeMap::new();
    for event in events {
        match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) => {
                if let ApprovalPacket::Merge(packet) = &opened.packet {
                    let needs = packet.rows.iter().flat_map(|row| &row.debt);
                    let agent_ids = needs.map(|debt| debt.needs.agent_id()).collect();
                    task = Some((opened.id.clone(), agent_ids));
                }
            }
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) => {
                task.take_if(|(id, _)| *id == decided.id);
            }
            RunEventKind::Approval(ApprovalEvent::Withdrawn { id, .. }) => {
                task.take_if(|(open, _)| open == id);
            }
            RunEventKind::Run(RunLifecycle::Resumed(resumed))
                if resumed.cause == ResumeCause::AgentBack =>
            {
                task = None;
            }
            RunEventKind::AgentStatus(AgentStatusEvent::PlanUsage(reading)) => {
                let out =
                    (reading.status == PlanUsageStatus::Rejected).then_some(reading.resets_at);
                plan_usage_until.insert(reading.agent_id.clone(), out);
            }
            RunEventKind::AgentStatus(AgentStatusEvent::Health(health)) => {
                states.insert(health.agent_id.clone(), health.state);
            }
            _ => {}
        }
    }
    let Some((id, agent_ids)) = task else {
        return Vec::new();
    };
    if !waiting_on.is_empty() && !waiting_on.contains(&id) {
        return Vec::new();
    }
    agent_ids
        .into_iter()
        .filter_map(|agent_id| {
            let plan_usage_until = plan_usage_until.get(&agent_id).copied().flatten();
            let down = states.get(&agent_id) == Some(&HealthState::Down);
            (plan_usage_until.is_some() || down).then_some(OutOriginal {
                agent_id,
                plan_usage_until,
                down,
            })
        })
        .collect()
}

/// Open fallback approvals among `waiting_on`.
fn open_fallbacks(events: &[RunEvent], waiting_on: &[ApprovalId]) -> Vec<OpenFallback> {
    let mut roles = BTreeMap::new();
    let mut open = BTreeMap::new();
    for event in events {
        match &event.kind {
            RunEventKind::Session(SessionEvent::Started(started)) => {
                if let Some(task) = &event.task_id {
                    roles.insert(task.clone(), started.role);
                }
            }
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) => {
                if matches!(opened.packet, ApprovalPacket::Fallback(_)) {
                    open.insert(opened.id.clone(), (event.ts, (**opened).clone()));
                }
            }
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) => {
                open.remove(&decided.id);
            }
            RunEventKind::Approval(ApprovalEvent::Withdrawn { id, .. }) => {
                open.remove(id);
            }
            _ => {}
        }
    }
    open.into_values()
        .filter(|(_, opened)| waiting_on.is_empty() || waiting_on.contains(&opened.id))
        .map(|(opened_at, opened)| {
            let role = opened
                .task
                .as_ref()
                .and_then(|task| roles.get(task).copied())
                .unwrap_or(RoleName::Worker);
            OpenFallback {
                opened_at,
                opened,
                role,
            }
        })
        .collect()
}

fn ask_timeout_passed(settings: &Settings, fallback: &OpenFallback, now: OffsetDateTime) -> bool {
    let ApprovalPacket::Fallback(plan) = &fallback.opened.packet else {
        return false;
    };
    let applied = settings
        .fallback
        .applied_to(fallback.role, &plan.stopped.agent_id());
    match applied.ask_timeout.parse() {
        Ok(Some(timeout)) => fallback.opened_at + timeout <= now,
        // "none" waits for you; a bad span was already refused when the settings loaded.
        Ok(None) | Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::client::test_support::{TestHome, event_at, opened, run_started, session_started};
    use argonauts_core::events::agent_status::FallbackPlan;
    use argonauts_core::events::approval::{ApprovalKind, MergePacket, MergeRow};
    use argonauts_core::events::run::RunResumed;
    use argonauts_core::events::task::TaskKind;
    use argonauts_core::fallback::QualityDebt;
    use argonauts_core::limits::{AgentHealth, HealthSignal};
    use argonauts_core::limits::{PlanUsageReading, PlanUsageSource};
    use argonauts_core::names::{ApprovalId, ModelRef, SessionKey};
    use argonauts_core::settings::Span;
    use argonauts_core::settings::fallback::{FallbackMode, FallbackStrategy};
    use argonauts_core::{DiffStat, Spend, TaskId};
    use tokio::sync::broadcast;

    #[derive(Default)]
    struct Board {
        down: Mutex<BTreeSet<AgentId>>,
    }

    impl HealthBoard for Board {
        fn observe(&self, _signal: HealthSignal) {}

        fn state(&self, agent_id: &AgentId) -> argonauts_core::limits::AgentHealth {
            let down = self.down.lock().unwrap().contains(agent_id);
            health(
                agent_id,
                if down {
                    HealthState::Down
                } else {
                    HealthState::Healthy
                },
            )
        }

        fn all(&self) -> Vec<AgentHealth> {
            Vec::new()
        }

        fn subscribe(&self) -> broadcast::Receiver<AgentStatusEvent> {
            broadcast::channel(1).1
        }

        fn test_result(&self, _agent: &AgentId, _ok: bool) {}
    }

    fn health(agent_id: &AgentId, state: HealthState) -> AgentHealth {
        AgentHealth {
            agent_id: agent_id.clone(),
            state,
            since: crate::client::test_support::base_ts(),
            reason: None,
            next_test_at: None,
            disabled: None,
        }
    }

    fn waiting_run(
        seq: u64,
        at: OffsetDateTime,
        reason: WaitReason,
        waiting_on: Vec<ApprovalId>,
    ) -> RunEvent {
        event_at(
            seq,
            at,
            RunEventKind::Run(RunLifecycle::Waiting { reason, waiting_on }),
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
                waiting_run(
                    1,
                    now - time::Duration::hours(3),
                    WaitReason::AllAgentsOut {
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
                waiting_run(
                    1,
                    now,
                    WaitReason::AllAgentsOut {
                        until: Some(now + time::Duration::hours(1)),
                    },
                    Vec::new(),
                ),
            ],
        );
        let due = WaitingRuns::default().due(&env, &Settings::default(), &Board::default(), now);
        assert_eq!(due, vec![(past, WakeCause::AgentBack)]);
    }

    /// A run with no fallback for its jobs tries again at its `until`, with no agent to come
    /// back: an auth error or a usage limit never marks one down.
    #[test]
    fn a_run_with_no_fallback_wakes_at_its_retry() {
        let home = TestHome::new();
        let env = home.env(true);
        let now = OffsetDateTime::now_utc();
        let stalled = |until| WaitReason::NoFallback {
            detail: "no fallback for the main agent".to_owned(),
            until,
        };
        let due_now = run("0f01");
        home.run_with(
            &env,
            &due_now,
            &[
                run_started(0, "stalled", None),
                waiting_run(
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
                waiting_run(
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
                waiting_run(1, now, stalled(None), Vec::new()),
            ],
        );
        let due = WaitingRuns::default().due(&env, &Settings::default(), &Board::default(), now);
        assert_eq!(due, vec![(due_now, WakeCause::AgentBack)]);
    }

    #[test]
    fn a_run_wakes_when_its_agent_is_back() {
        let home = TestHome::new();
        let env = home.env(true);
        let now = OffsetDateTime::now_utc();
        let codex = AgentId("codex".to_owned());
        let waiting = run("0b01");
        home.run_with(
            &env,
            &waiting,
            &[
                run_started(0, "waiting", None),
                event_at(
                    1,
                    now,
                    RunEventKind::AgentStatus(AgentStatusEvent::Health(health(
                        &codex,
                        HealthState::Down,
                    ))),
                ),
                waiting_run(
                    2,
                    now,
                    WaitReason::WaitingForAgent {
                        agent_id: codex.clone(),
                        until: None,
                    },
                    Vec::new(),
                ),
            ],
        );
        let plan_usage = run("0b02");
        home.run_with(
            &env,
            &plan_usage,
            &[
                run_started(0, "out of plan usage, never down", None),
                waiting_run(
                    1,
                    now,
                    WaitReason::WaitingForAgent {
                        agent_id: codex.clone(),
                        until: None,
                    },
                    Vec::new(),
                ),
            ],
        );
        let board = Board::default();
        let mut waiting_runs = WaitingRuns::default();
        board.down.lock().unwrap().insert(codex.clone());
        assert!(
            waiting_runs
                .due(&env, &Settings::default(), &board, now)
                .is_empty()
        );
        board.down.lock().unwrap().clear();
        assert_eq!(
            waiting_runs.due(&env, &Settings::default(), &board, now),
            vec![(waiting.clone(), WakeCause::AgentBack)]
        );
        let lock_dir = env.run_dir(&waiting);
        let _live = RunLock::try_acquire(&lock_dir).unwrap().unwrap();
        assert!(
            waiting_runs
                .due(&env, &Settings::default(), &board, now)
                .is_empty(),
            "a live run needs no wake"
        );
    }

    #[test]
    fn a_fallback_approval_wakes_once_ask_timeout_passed() {
        let home = TestHome::new();
        let env = home.env(true);
        let now = OffsetDateTime::now_utc();
        let task = TaskId::new("task-001").unwrap();
        let approval = ApprovalId::numbered(1);
        let plan = FallbackPlan {
            task: task.clone(),
            title: "Export".to_owned(),
            stopped: ModelRef::new("claude-code", None),
            reason: "usage limit".to_owned(),
            resets_at: None,
            work: DiffStat::default(),
            checks_ran: false,
            candidates: Vec::new(),
            mode: FallbackMode::Ask,
            strategy: FallbackStrategy::First,
            race_count: 1,
            also_moving: Vec::new(),
        };
        let asked_at = now - time::Duration::hours(2);
        let asking = run("0c01");
        let mut fallback = opened(
            2,
            &approval,
            ApprovalKind::Fallback,
            Some(&task),
            ApprovalPacket::Fallback(plan),
        );
        fallback.ts = asked_at;
        home.run_with(
            &env,
            &asking,
            &[
                run_started(0, "fallback", None),
                session_started(
                    1,
                    &SessionKey::numbered(1),
                    Some(&task),
                    RoleName::Worker,
                    "claude-code",
                ),
                fallback,
                waiting_run(3, asked_at, WaitReason::Approval, vec![approval]),
            ],
        );

        let mut settings = Settings::default();
        let mut waiting_runs = WaitingRuns::default();
        assert!(
            waiting_runs
                .due(&env, &settings, &Board::default(), now)
                .is_empty(),
            "\"none\" waits for you"
        );
        settings.fallback.ask_timeout = Span::new("1h");
        for role in settings.fallback.roles.values_mut() {
            role.ask_timeout = Span::new("1h");
        }
        assert_eq!(
            waiting_runs.due(&env, &settings, &Board::default(), now),
            vec![(asking, WakeCause::AskTimeout)]
        );
        settings.fallback.ask_timeout = Span::new("3h");
        for role in settings.fallback.roles.values_mut() {
            role.ask_timeout = Span::new("3h");
        }
        assert!(
            waiting_runs
                .due(&env, &settings, &Board::default(), now)
                .is_empty()
        );
    }

    /// A run waiting on a merge question whose debt needs Claude Code: `out` says how the run saw
    /// it go, and `then` follows the wait.
    fn merge_waiting_on_claude(out: Option<RunEvent>, then: Vec<RunEvent>) -> Vec<RunEvent> {
        let approval = ApprovalId::numbered(2);
        let worker = |name: &str| ModelRef::new(name, None);
        let row = MergeRow {
            task: TaskId::new("task-001").unwrap(),
            title: "Export".to_owned(),
            kind: TaskKind::Build,
            stat: DiffStat::default(),
            worker: Some(worker("codex")),
            checks_passed: true,
            review: None,
            debt: vec![QualityDebt {
                since: 1,
                by: worker("codex"),
                needs: worker("claude-code"),
            }],
        };
        let packet = MergePacket {
            target: Some("main".to_owned()),
            branch: "merge".to_owned(),
            rows: vec![row],
            suite_passed_on_latest_main: true,
            checks: Vec::new(),
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
            both_changed: Vec::new(),
            asks_which_branch: false,
            again: None,
        };
        let merge = ApprovalPacket::Merge(packet);
        let waiting_at = OffsetDateTime::now_utc() - time::Duration::hours(1);
        let mut events = vec![run_started(0, "merge", None)];
        events.extend(out);
        events.push(opened(2, &approval, ApprovalKind::Merge, None, merge));
        events.push(waiting_run(
            3,
            waiting_at,
            WaitReason::Approval,
            vec![approval],
        ));
        events.extend(then);
        events
    }

    #[test]
    fn a_merge_task_wakes_once_an_original_its_debt_needs_is_back() {
        let home = TestHome::new();
        let env = home.env(true);
        let now = OffsetDateTime::now_utc();
        let claude = AgentId("claude-code".to_owned());
        let rejected = |resets_at| {
            let reading = PlanUsageReading {
                agent_id: claude.clone(),
                status: PlanUsageStatus::Rejected,
                used: Some(1.0),
                resets_at: Some(resets_at),
                window: Some("five_hour".to_owned()),
                source: PlanUsageSource::RateLimitEvent,
                observed_at: now - time::Duration::hours(2),
                detail: None,
                wrap_up: false,
            };
            Some(event_at(
                1,
                now,
                RunEventKind::AgentStatus(AgentStatusEvent::PlanUsage(reading)),
            ))
        };
        let down = Some(event_at(
            1,
            now,
            RunEventKind::AgentStatus(AgentStatusEvent::Health(health(&claude, HealthState::Down))),
        ));
        let in_an_hour = now + time::Duration::hours(1);
        let a_minute_ago = now - time::Duration::minutes(1);
        let resumed = RunResumed {
            cause: ResumeCause::AgentBack,
            settings_changed: false,
            cli_versions_changed: Vec::new(),
            main_moved_by: None,
            sessions: Vec::new(),
        };
        let woke_again = vec![
            event_at(4, now, RunEventKind::Run(RunLifecycle::Resumed(resumed))),
            waiting_run(5, now, WaitReason::Approval, vec![ApprovalId::numbered(2)]),
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
            // Never out as far as the run saw: the merge rule had its chance before it began to
            // wait.
            ("0d04", merge_waiting_on_claude(None, Vec::new())),
            // Woken once for this question already, it isn't woken for it again.
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
        let due = WaitingRuns::default().due(&env, &Settings::default(), &board, now);
        assert_eq!(due, vec![(run("0d02"), WakeCause::AgentBack)]);
        let due = WaitingRuns::default().due(&env, &Settings::default(), &Board::default(), now);
        assert_eq!(
            due,
            vec![
                (run("0d02"), WakeCause::AgentBack),
                (run("0d03"), WakeCause::AgentBack)
            ]
        );
    }

    #[test]
    fn a_journal_is_read_again_only_once_it_grows() {
        let home = TestHome::new();
        let env = home.env(true);
        let now = OffsetDateTime::now_utc();
        let out_until = |seq, until| {
            waiting_run(
                seq,
                now,
                WaitReason::AllAgentsOut { until: Some(until) },
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
        let mut waiting_runs = WaitingRuns::default();
        assert!(waiting_runs.due(&env, &settings, &board, now).is_empty());
        assert!(waiting_runs.due(&env, &settings, &board, now).is_empty());
        assert_eq!(waiting_runs.reads, 1, "an unchanged journal is read once");
        assert_eq!(
            waiting_runs.due(&env, &settings, &board, now + time::Duration::hours(2)),
            vec![(waiting.clone(), WakeCause::AgentBack)],
            "what the journal said is checked against the time now"
        );
        assert_eq!(waiting_runs.reads, 1);

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
            waiting_runs.due(&env, &settings, &board, now),
            vec![(later.clone(), WakeCause::AgentBack)]
        );
        assert_eq!(waiting_runs.reads, 2, "only the new journal is read");
        home.run_with(
            &env,
            &waiting,
            &[out_until(2, now - time::Duration::minutes(1))],
        );
        assert_eq!(
            waiting_runs.due(&env, &settings, &board, now),
            vec![
                (waiting.clone(), WakeCause::AgentBack),
                (later.clone(), WakeCause::AgentBack)
            ]
        );
        assert_eq!(waiting_runs.reads, 3, "a grown journal is read again");

        std::fs::remove_dir_all(env.run_dir(&waiting)).unwrap();
        assert_eq!(
            waiting_runs.due(&env, &settings, &board, now),
            vec![(later, WakeCause::AgentBack)]
        );
        assert!(
            !waiting_runs.known.contains_key(&waiting),
            "a removed run is forgotten"
        );
    }
}
