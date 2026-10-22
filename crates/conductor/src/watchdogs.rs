//! The watchdogs inside every session: inactivity (no events, no output, and no CPU in the
//! process tree), the same tool call repeated, a later try that only looks around, a stall with
//! no heartbeat, and a CLI's own subagents past the fan-out cap where only counting can enforce
//! it (agy).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use argonauts_core::AgentEvent;
use argonauts_core::events::session::{CallCount, SubagentStatus, SubagentUpdate, Watchdog};
use argonauts_core::session_runner::SessionLimits;
use serde_json::Value;

use crate::filediff::writes_files;

/// How late a heartbeat may be before the silence counts as a stall.
pub const HEARTBEAT_GRACE: Duration = Duration::from_secs(15);

/// The fixed note an agent gets for repeating the same call.
pub const REPEAT_NOTE: &str = "You have made the same tool call several times in a row with the \
same input. Stop repeating it: try a different approach, or finish and report what blocks you.";

/// The wrap-up a later try gets once its calls have changed nothing for a while.
pub const NO_PROGRESS_WRAP_UP: &str =
    "The work may already be done. Run the checks once; if they pass, end with your report now.";

/// The calls a later try may still make after that wrap-up without a change: one to run the
/// checks, and one that may hand the report in, as a model on OpenCode may. The next one stops
/// it.
const CALLS_AFTER_WRAP_UP: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchdogVerdict {
    Quiet,
    /// No output past the CLI's heartbeat and [`HEARTBEAT_GRACE`] while a tool call ran;
    /// shown, not acted on.
    Stalled {
        silent_for: Duration,
    },
    /// Quiet, with no output and no CPU, for half the inactivity limit. A forecast of the stop
    /// that claims no stall, for CLIs that send no heartbeat; shown, not acted on.
    Quieting {
        quiet_for: Duration,
        stops_after: Duration,
    },
    /// Deliver [`REPEAT_NOTE`].
    Nudge {
        call: String,
        times: u32,
    },
    /// Send the wrap-up message, then stop if it keeps going. A later try that changes nothing
    /// gets [`NO_PROGRESS_WRAP_UP`].
    SoftStop(Watchdog),
    /// Interrupt, then stop. The end reason is `Stuck`, with the calls counted.
    Stuck {
        watchdog: Watchdog,
        calls: Option<CallCount>,
    },
    /// More subagents than the cap: interrupt the turn.
    FanoutExceeded {
        running: u32,
        max: u32,
    },
}

#[derive(Debug)]
pub struct Watchdogs {
    limits: SessionLimits,
    fanout_cap: Option<u32>,
    /// The CLI's own heartbeat while a tool call runs. Without one, silence is no stall.
    heartbeat: Option<Duration>,
    /// The main agent's tool calls still running. A heartbeat comes only during one that hosts
    /// no subagent, since a subagent's work sends none.
    tools: HashSet<String>,
    started: Instant,
    last_event: Option<Instant>,
    last_output: Option<Instant>,
    /// The latest call of the main agent (`None`) and of each subagent, so their calls don't
    /// break each other's streaks.
    streaks: HashMap<Option<String>, Streak>,
    /// The main agent's calls since its last file change, for a later try on committed work.
    unchanged: u32,
    running: HashSet<String>,
    /// Calls from the session to Argonauts Code still open, such as questions waiting on you.
    asks_open: u32,
    /// The internet is down, so the session waits on it rather than on itself.
    offline: bool,
}

#[derive(Debug)]
struct Streak {
    name: String,
    input: Value,
    times: u32,
}

impl Watchdogs {
    /// `fanout_cap` is set only where Argonauts Code enforces the cap by counting.
    pub fn new(limits: &SessionLimits, fanout_cap: Option<u32>) -> Self {
        Self {
            limits: limits.clone(),
            fanout_cap,
            heartbeat: None,
            tools: HashSet::new(),
            started: Instant::now(),
            last_event: None,
            last_output: None,
            streaks: HashMap::new(),
            unchanged: 0,
            running: HashSet::new(),
            asks_open: 0,
            offline: false,
        }
    }

    /// Warn of a stall once the CLI misses its `heartbeat` during a tool call.
    pub fn with_heartbeat(mut self, heartbeat: Option<Duration>) -> Self {
        self.heartbeat = heartbeat;
        self
    }

    pub fn observe(&mut self, event: &AgentEvent, now: Instant) -> WatchdogVerdict {
        self.last_event = Some(now);
        match event {
            AgentEvent::ToolStarted {
                id,
                name,
                input,
                parent,
            } => {
                let repeated = self.repeat(parent, name, input);
                if parent.is_some() {
                    return repeated;
                }
                self.tools.insert(id.clone());
                let unchanged = self.unchanged(name, input);
                if unchanged.weight() > repeated.weight() {
                    unchanged
                } else {
                    repeated
                }
            }
            AgentEvent::FileChanged { .. } | AgentEvent::FilePatch { .. } => {
                self.unchanged = 0;
                WatchdogVerdict::Quiet
            }
            AgentEvent::ToolFinished { id, .. } => {
                self.tools.remove(id);
                WatchdogVerdict::Quiet
            }
            AgentEvent::Result { .. } => {
                self.tools.clear();
                WatchdogVerdict::Quiet
            }
            AgentEvent::Subagent { update } => self.fanout(update),
            _ => WatchdogVerdict::Quiet,
        }
    }

    /// Any output line, parsed or not, counts as activity.
    pub fn output(&mut self, now: Instant) {
        self.last_output = Some(now);
    }

    /// The worktree changed since the last look, whatever the calls that changed it were
    /// named, such as a shell's `sed -i`, so a later try's count of calls that change nothing
    /// starts over.
    pub fn progress_made(&mut self) {
        self.unchanged = 0;
    }

    /// A call from the session to Argonauts Code opened (`open`) or was answered. While one is
    /// open the session waits on Argonauts Code or you, so it isn't inactive or stalled, and
    /// the inactivity clock starts again from the answer.
    pub fn asking(&mut self, open: bool, now: Instant) {
        if open {
            self.asks_open = self.asks_open.saturating_add(1);
        } else {
            self.asks_open = self.asks_open.saturating_sub(1);
            self.last_event = Some(now);
        }
    }

    /// The internet went (`down`) or came back. While it's down the session waits for it, so
    /// it's neither inactive nor stalled, and its clock starts again once it's back.
    pub fn network(&mut self, down: bool, now: Instant) {
        if self.offline && !down {
            self.last_event = Some(now);
        }
        self.offline = down;
    }

    /// `cpu_busy_at` is the shared sampler's latest busy time for the process tree.
    pub fn tick(&mut self, now: Instant, cpu_busy_at: Option<Instant>) -> WatchdogVerdict {
        if self.asks_open > 0 || self.offline {
            return WatchdogVerdict::Quiet;
        }
        let last_active = [self.last_event, self.last_output, cpu_busy_at]
            .into_iter()
            .flatten()
            .fold(self.started, Instant::max);
        let inactive_after = self
            .limits
            .inactivity_min
            .filter(|minutes| *minutes > 0.0)
            .and_then(|minutes| Duration::try_from_secs_f64(minutes * 60.0).ok());
        let quiet_for = now.saturating_duration_since(last_active);
        if let Some(limit) = inactive_after {
            if quiet_for >= limit {
                return WatchdogVerdict::Stuck {
                    watchdog: Watchdog::Inactivity,
                    calls: None,
                };
            }
            if quiet_for >= limit / 2 {
                return WatchdogVerdict::Quieting {
                    quiet_for,
                    stops_after: limit,
                };
            }
        }
        // A model that thinks sends nothing, and that's normal. Only a tool call comes with a
        // heartbeat, so only then does silence past it mean the CLI stalled. A call that hosts
        // a subagent waits on the subagent's work, which sends none. Its id is the call's.
        let owed = self.tools.iter().any(|id| !self.running.contains(id));
        let Some(heartbeat) = self.heartbeat.filter(|_| owed) else {
            return WatchdogVerdict::Quiet;
        };
        let heard = [self.last_event, self.last_output]
            .into_iter()
            .flatten()
            .fold(self.started, Instant::max);
        let silent_for = now.saturating_duration_since(heard);
        if silent_for >= heartbeat + HEARTBEAT_GRACE {
            return WatchdogVerdict::Stalled { silent_for };
        }
        WatchdogVerdict::Quiet
    }

    /// Subagents running now, as their updates say.
    pub fn running_subagents(&self) -> u32 {
        u32::try_from(self.running.len()).unwrap_or(u32::MAX)
    }

    fn repeat(&mut self, agent: &Option<String>, name: &str, input: &Value) -> WatchdogVerdict {
        let Some(limit) = self.limits.repeat_limit.filter(|limit| *limit > 0) else {
            return WatchdogVerdict::Quiet;
        };
        let times = match self.streaks.get_mut(agent) {
            Some(streak) if streak.name == name && streak.input == *input => {
                streak.times = streak.times.saturating_add(1);
                streak.times
            }
            _ => {
                let streak = Streak {
                    name: name.to_owned(),
                    input: input.clone(),
                    times: 1,
                };
                self.streaks.insert(agent.clone(), streak);
                1
            }
        };
        match times.checked_sub(limit) {
            None => WatchdogVerdict::Quiet,
            Some(0) => WatchdogVerdict::Nudge {
                call: name.to_owned(),
                times,
            },
            Some(1) => WatchdogVerdict::SoftStop(Watchdog::Repeat),
            Some(_) => WatchdogVerdict::Stuck {
                watchdog: Watchdog::Repeat,
                calls: Some(CallCount {
                    name: name.to_owned(),
                    times,
                }),
            },
        }
    }

    /// A later try on committed work whose calls change nothing gets the wrap-up, since the
    /// work may already be done, and is stopped if it still changes nothing and doesn't report.
    /// Only the main agent's calls count: it is the one that reports.
    fn unchanged(&mut self, name: &str, input: &Value) -> WatchdogVerdict {
        let Some(limit) = self.limits.no_progress_calls.filter(|limit| *limit > 0) else {
            return WatchdogVerdict::Quiet;
        };
        // An edit counts as a change as it starts, since some CLIs say what it changed only
        // after it finished.
        if writes_files(name, input) {
            self.unchanged = 0;
            return WatchdogVerdict::Quiet;
        }
        self.unchanged = self.unchanged.saturating_add(1);
        match self.unchanged.checked_sub(limit) {
            None => WatchdogVerdict::Quiet,
            Some(0) => WatchdogVerdict::SoftStop(Watchdog::NoProgress),
            Some(after) if after <= CALLS_AFTER_WRAP_UP => WatchdogVerdict::Quiet,
            Some(_) => WatchdogVerdict::Stuck {
                watchdog: Watchdog::NoProgress,
                calls: Some(CallCount {
                    name: name.to_owned(),
                    times: self.unchanged,
                }),
            },
        }
    }

    fn fanout(&mut self, update: &SubagentUpdate) -> WatchdogVerdict {
        let started = match update.status {
            SubagentStatus::Started | SubagentStatus::Running => {
                self.running.insert(update.id.clone())
            }
            SubagentStatus::Done | SubagentStatus::Failed => {
                self.running.remove(&update.id);
                false
            }
            SubagentStatus::Unknown => false,
        };
        let running = self.running_subagents();
        match self.fanout_cap {
            Some(max) if started && running > max => {
                WatchdogVerdict::FanoutExceeded { running, max }
            }
            _ => WatchdogVerdict::Quiet,
        }
    }
}

impl WatchdogVerdict {
    /// How far it goes, so of two verdicts on one call the stronger one acts.
    fn weight(&self) -> u8 {
        match self {
            Self::Stuck { .. } | Self::FanoutExceeded { .. } => 3,
            Self::SoftStop(_) => 2,
            Self::Nudge { .. } => 1,
            Self::Quiet | Self::Stalled { .. } | Self::Quieting { .. } => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::events::session::SubagentKind;
    use serde_json::json;

    const MINUTE: Duration = Duration::from_secs(60);

    fn limits() -> SessionLimits {
        SessionLimits {
            inactivity_min: Some(15.0),
            repeat_limit: Some(3),
            ..SessionLimits::default()
        }
    }

    fn call(command: &str, parent: Option<&str>) -> AgentEvent {
        AgentEvent::ToolStarted {
            id: format!("call-{command}"),
            name: "Bash".into(),
            input: json!({ "command": command }),
            parent: parent.map(str::to_owned),
        }
    }

    fn finished(id: &str) -> AgentEvent {
        AgentEvent::ToolFinished {
            id: id.into(),
            ok: true,
            output: None,
            parent: None,
        }
    }

    fn subagent(id: &str, status: SubagentStatus) -> AgentEvent {
        AgentEvent::Subagent {
            update: SubagentUpdate {
                id: id.into(),
                parent: None,
                kind: SubagentKind::Subagent,
                label: Some("research".into()),
                prompt: None,
                status,
                last_message: None,
                log_uri: None,
                steps_visible: true,
                tokens: None,
            },
        }
    }

    fn text() -> AgentEvent {
        AgentEvent::TextDelta {
            text: "working".into(),
            parent: None,
        }
    }

    fn later_try() -> SessionLimits {
        SessionLimits {
            no_progress_calls: Some(8),
            ..limits()
        }
    }

    fn edited(path: &str) -> AgentEvent {
        AgentEvent::FileChanged {
            path: path.into(),
            kind: "update".into(),
        }
    }

    #[test]
    fn a_later_try_that_changes_nothing_is_wrapped_up_then_stopped() {
        let mut dogs = Watchdogs::new(&later_try(), None);
        let at = Instant::now();
        for n in 1..8 {
            let verdict = dogs.observe(&call(&format!("cat file-{n}"), None), at);
            assert_eq!(verdict, WatchdogVerdict::Quiet, "call {n}");
        }
        assert_eq!(
            dogs.observe(&call("git log", None), at),
            WatchdogVerdict::SoftStop(Watchdog::NoProgress)
        );
        // The checks, run once as the wrap-up asks, then a call that may hand the report in.
        assert_eq!(
            dogs.observe(&call("cargo test", None), at),
            WatchdogVerdict::Quiet
        );
        assert_eq!(
            dogs.observe(&call("echo report", None), at),
            WatchdogVerdict::Quiet
        );
        assert_eq!(
            dogs.observe(&call("git status", None), at),
            WatchdogVerdict::Stuck {
                watchdog: Watchdog::NoProgress,
                calls: Some(CallCount {
                    name: "Bash".into(),
                    times: 11
                })
            }
        );
    }

    #[test]
    fn a_change_starts_the_count_over() {
        let edit = |id: &str| AgentEvent::ToolStarted {
            id: id.into(),
            name: "Edit".into(),
            input: json!({ "file_path": "slug.py" }),
            parent: None,
        };
        let mut dogs = Watchdogs::new(&later_try(), None);
        let at = Instant::now();
        let look = |dogs: &mut Watchdogs, times: u32| {
            for n in 0..times {
                let verdict = dogs.observe(&call(&format!("cat {n}"), None), at);
                assert_eq!(verdict, WatchdogVerdict::Quiet);
            }
        };
        look(&mut dogs, 7);
        // An edit counts as it starts, before its CLI says what changed.
        assert_eq!(dogs.observe(&edit("e-1"), at), WatchdogVerdict::Quiet);
        look(&mut dogs, 7);
        assert_eq!(dogs.observe(&edited("slug.py"), at), WatchdogVerdict::Quiet);
        look(&mut dogs, 7);
        // OpenCode 2.0.32's patch tool names its files inside the patch.
        let patch = AgentEvent::ToolStarted {
            id: "p-1".into(),
            name: "patch".into(),
            input: json!({ "patchText": "*** Begin Patch\n*** Update File: slug.py\n" }),
            parent: None,
        };
        assert_eq!(dogs.observe(&patch, at), WatchdogVerdict::Quiet);
        look(&mut dogs, 7);
        // A subagent's calls are its main agent's work, and only the main agent reports.
        for n in 0..5 {
            let verdict = dogs.observe(&call(&format!("grep {n}"), Some("task-1")), at);
            assert_eq!(verdict, WatchdogVerdict::Quiet);
        }
        assert_eq!(
            dogs.observe(&call("ls", None), at),
            WatchdogVerdict::SoftStop(Watchdog::NoProgress)
        );

        // A first try, or any session that isn't a later one, is never counted.
        let mut first = Watchdogs::new(&limits(), None);
        for n in 0..30 {
            let verdict = first.observe(&call(&format!("cat {n}"), None), at);
            assert_eq!(verdict, WatchdogVerdict::Quiet);
        }
    }

    #[test]
    fn of_a_repeat_and_a_wrap_up_on_one_call_the_stop_wins() {
        let limits = SessionLimits {
            repeat_limit: Some(3),
            no_progress_calls: Some(3),
            ..SessionLimits::default()
        };
        let mut dogs = Watchdogs::new(&limits, None);
        let at = Instant::now();
        assert_eq!(dogs.observe(&call("ls", None), at), WatchdogVerdict::Quiet);
        assert_eq!(dogs.observe(&call("ls", None), at), WatchdogVerdict::Quiet);
        // The third: the repeat's nudge and the wrap-up for changing nothing.
        assert_eq!(
            dogs.observe(&call("ls", None), at),
            WatchdogVerdict::SoftStop(Watchdog::NoProgress)
        );
        assert_eq!(
            dogs.observe(&call("ls", None), at),
            WatchdogVerdict::SoftStop(Watchdog::Repeat)
        );
        assert!(matches!(
            dogs.observe(&call("ls", None), at),
            WatchdogVerdict::Stuck {
                watchdog: Watchdog::Repeat,
                ..
            }
        ));
    }

    /// The second try of the local Qwen run: on work the first session had committed, it
    /// looked around for 20 minutes, 17 calls 33 to 90 s apart, and nothing stopped it. Here
    /// its 8th call gets the wrap-up and its 11th the stop, 6 and 10 minutes after its first
    /// event.
    #[test]
    fn the_recorded_later_try_is_wrapped_up_at_its_eighth_call() {
        use argonauts_core::events::{RunEvent, RunEventKind};

        let record = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/opencode/later_try_looks_around.events.jsonl"
        ));
        let events: Vec<RunEvent> = record
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let started = events[0].ts;
        let t0 = Instant::now();
        let replay = |limits: &SessionLimits| {
            let mut dogs = Watchdogs::new(limits, None);
            let mut verdicts = Vec::new();
            let mut calls = 0;
            for event in &events {
                let RunEventKind::Agent(agent) = &event.kind else {
                    continue;
                };
                let after = (event.ts - started).unsigned_abs();
                let now = t0 + after;
                calls += u32::from(matches!(agent, AgentEvent::ToolStarted { .. }));
                for verdict in [dogs.tick(now, None), dogs.observe(agent, now)] {
                    if !matches!(verdict, WatchdogVerdict::Quiet) {
                        let stops = matches!(verdict, WatchdogVerdict::Stuck { .. });
                        verdicts.push((calls, after.as_secs() / 60, verdict));
                        if stops {
                            return (calls, verdicts);
                        }
                    }
                }
            }
            (calls, verdicts)
        };

        // As it ran, with the repeat and inactivity watchdogs only.
        let (calls, verdicts) = replay(&SessionLimits {
            inactivity_min: Some(15.0),
            repeat_limit: Some(10),
            ..SessionLimits::default()
        });
        assert_eq!(calls, 17);
        assert_eq!(verdicts, []);

        let (_, verdicts) = replay(&SessionLimits {
            inactivity_min: Some(15.0),
            repeat_limit: Some(10),
            no_progress_calls: Some(8),
            ..SessionLimits::default()
        });
        assert_eq!(
            verdicts,
            [
                (8, 6, WatchdogVerdict::SoftStop(Watchdog::NoProgress)),
                (
                    11,
                    10,
                    WatchdogVerdict::Stuck {
                        watchdog: Watchdog::NoProgress,
                        calls: Some(CallCount {
                            name: "shell".into(),
                            times: 11
                        })
                    }
                ),
            ]
        );
    }

    #[test]
    fn watchdogs_inactivity_repeat() {
        let mut dogs = Watchdogs::new(&limits(), None);
        let t0 = Instant::now();
        assert_eq!(dogs.tick(t0 + MINUTE * 7, None), WatchdogVerdict::Quiet);
        assert_eq!(
            dogs.tick(t0 + MINUTE * 16, None),
            WatchdogVerdict::Stuck {
                watchdog: Watchdog::Inactivity,
                calls: None
            }
        );

        // Output and CPU each keep it alive, though neither is an event.
        dogs.output(t0 + MINUTE * 10);
        assert_eq!(dogs.tick(t0 + MINUTE * 16, None), WatchdogVerdict::Quiet);
        assert_eq!(
            dogs.tick(t0 + MINUTE * 26, Some(t0 + MINUTE * 20)),
            WatchdogVerdict::Quiet
        );
        assert_eq!(
            dogs.tick(t0 + MINUTE * 36, Some(t0 + MINUTE * 20)),
            WatchdogVerdict::Stuck {
                watchdog: Watchdog::Inactivity,
                calls: None
            }
        );

        let at = t0 + MINUTE * 40;
        assert_eq!(dogs.observe(&call("ls", None), at), WatchdogVerdict::Quiet);
        assert_eq!(dogs.observe(&call("ls", None), at), WatchdogVerdict::Quiet);
        // A subagent's calls don't break the main agent's streak.
        assert_eq!(
            dogs.observe(&call("pwd", Some("task-1")), at),
            WatchdogVerdict::Quiet
        );
        assert_eq!(
            dogs.observe(&call("ls", None), at),
            WatchdogVerdict::Nudge {
                call: "Bash".into(),
                times: 3
            }
        );
        assert_eq!(
            dogs.observe(&call("ls", None), at),
            WatchdogVerdict::SoftStop(Watchdog::Repeat)
        );
        assert_eq!(
            dogs.observe(&call("ls", None), at),
            WatchdogVerdict::Stuck {
                watchdog: Watchdog::Repeat,
                calls: Some(CallCount {
                    name: "Bash".into(),
                    times: 5
                })
            }
        );
        // Moving on starts over.
        assert_eq!(
            dogs.observe(&call("ls -la", None), at),
            WatchdogVerdict::Quiet
        );
        // Without a heartbeat, silence during a call is no stall.
        assert_eq!(dogs.tick(at + MINUTE, None), WatchdogVerdict::Quiet);

        let mut unlimited = Watchdogs::new(&SessionLimits::default(), None);
        for _ in 0..20 {
            assert_eq!(
                unlimited.observe(&call("ls", None), at),
                WatchdogVerdict::Quiet
            );
        }
        assert_eq!(
            unlimited.tick(at + MINUTE * 600, None),
            WatchdogVerdict::Quiet
        );
    }

    #[test]
    fn a_stall_shows_past_the_heartbeat_only_while_a_tool_runs() {
        let heartbeat = Duration::from_secs(30);
        let late = heartbeat + HEARTBEAT_GRACE;
        let second = Duration::from_secs(1);
        let mut dogs = Watchdogs::new(&limits(), None).with_heartbeat(Some(heartbeat));
        let t0 = Instant::now();
        // A model that thinks for minutes sends nothing, and that's normal.
        dogs.observe(&text(), t0);
        assert_eq!(dogs.tick(t0 + MINUTE * 5, None), WatchdogVerdict::Quiet);
        // While the main agent's tool call runs, the heartbeat should come.
        let at = t0 + MINUTE * 5;
        dogs.observe(&call("cargo test", None), at);
        assert_eq!(dogs.tick(at + late - second, None), WatchdogVerdict::Quiet);
        assert_eq!(
            dogs.tick(at + late, None),
            WatchdogVerdict::Stalled { silent_for: late }
        );
        // Any output counts, such as Claude Code's heartbeat line, which makes no event.
        dogs.output(at + late);
        assert_eq!(dogs.tick(at + late + second, None), WatchdogVerdict::Quiet);
        // Once the call is done, and while only a subagent's runs, nothing is owed.
        dogs.observe(&finished("call-cargo test"), at + late);
        dogs.observe(&call("ls", Some("task-1")), at + late);
        assert_eq!(dogs.tick(at + late * 3, None), WatchdogVerdict::Quiet);

        // A CLI with no heartbeat never stalls.
        let mut silent = Watchdogs::new(&limits(), None);
        silent.observe(&call("cargo test", None), t0);
        assert_eq!(silent.tick(t0 + MINUTE * 7, None), WatchdogVerdict::Quiet);
    }

    /// Claude Code waits inside its permission prompt tool, sending nothing, for as long as
    /// the question stays open.
    #[test]
    fn a_question_waiting_on_you_is_never_inactivity() {
        let start = Instant::now();
        let mut dogs = Watchdogs::new(&limits(), None);
        dogs.observe(&call("npm test", None), start);
        dogs.asking(true, start + MINUTE);
        assert_eq!(dogs.tick(start + 40 * MINUTE, None), WatchdogVerdict::Quiet);

        // Answered at 40 minutes: the clock starts again from the answer.
        dogs.asking(false, start + 40 * MINUTE);
        assert_eq!(dogs.tick(start + 47 * MINUTE, None), WatchdogVerdict::Quiet);
        assert!(matches!(
            dogs.tick(start + 48 * MINUTE, None),
            WatchdogVerdict::Quieting { .. }
        ));
        assert_eq!(
            dogs.tick(start + 55 * MINUTE, None),
            WatchdogVerdict::Stuck {
                watchdog: Watchdog::Inactivity,
                calls: None,
            }
        );
    }

    #[test]
    fn inactivity_never_stops_a_session_while_offline() {
        let t0 = Instant::now();
        let mut dogs = Watchdogs::new(&limits(), None);
        dogs.network(true, t0 + Duration::from_secs(60));
        assert_eq!(
            dogs.tick(t0 + Duration::from_secs(3600), None),
            WatchdogVerdict::Quiet
        );
        // Back after an hour: the clock starts again from there.
        let back = t0 + Duration::from_secs(3600);
        dogs.network(false, back);
        assert_eq!(
            dogs.tick(back + Duration::from_secs(5 * 60), None),
            WatchdogVerdict::Quiet
        );
        assert!(matches!(
            dogs.tick(back + Duration::from_secs(20 * 60), None),
            WatchdogVerdict::Stuck {
                watchdog: Watchdog::Inactivity,
                ..
            }
        ));
    }

    #[test]
    fn two_questions_open_wait_for_both_answers() {
        let start = Instant::now();
        let mut dogs = Watchdogs::new(&limits(), None);
        dogs.asking(true, start);
        dogs.asking(true, start);
        dogs.asking(false, start + MINUTE);
        assert_eq!(dogs.tick(start + 30 * MINUTE, None), WatchdogVerdict::Quiet);
        dogs.asking(false, start + 30 * MINUTE);
        assert!(matches!(
            dogs.tick(start + 46 * MINUTE, None),
            WatchdogVerdict::Stuck { .. }
        ));
    }

    #[test]
    fn half_the_inactivity_limit_forecasts_the_stop() {
        let limit = MINUTE * 15;
        let half = limit / 2;
        let mut dogs = Watchdogs::new(&limits(), None);
        let t0 = Instant::now();
        dogs.observe(&call("cargo test", None), t0);
        let second = Duration::from_secs(1);
        assert_eq!(dogs.tick(t0 + half - second, None), WatchdogVerdict::Quiet);
        assert_eq!(
            dogs.tick(t0 + half, None),
            WatchdogVerdict::Quieting {
                quiet_for: half,
                stops_after: limit
            }
        );
        assert_eq!(
            dogs.tick(t0 + limit, None),
            WatchdogVerdict::Stuck {
                watchdog: Watchdog::Inactivity,
                calls: None
            }
        );
        // A process tree that uses CPU isn't quiet.
        assert_eq!(
            dogs.tick(t0 + MINUTE * 10, Some(t0 + MINUTE * 9)),
            WatchdogVerdict::Quiet
        );
    }

    #[test]
    fn a_call_that_hosts_a_subagent_owes_no_heartbeat() {
        let late = Duration::from_secs(30) + HEARTBEAT_GRACE;
        let mut dogs =
            Watchdogs::new(&limits(), None).with_heartbeat(Some(Duration::from_secs(30)));
        let t0 = Instant::now();
        let spawn = AgentEvent::ToolStarted {
            id: "task-1".into(),
            name: "Agent".into(),
            input: json!({}),
            parent: None,
        };
        dogs.observe(&spawn, t0);
        dogs.observe(&subagent("task-1", SubagentStatus::Started), t0);
        dogs.observe(&call("cargo test", Some("task-1")), t0);
        // The subagent's two-minute test run sends nothing, and that's normal.
        assert_eq!(dogs.tick(t0 + MINUTE * 5, None), WatchdogVerdict::Quiet);

        // Once it's done, the main agent's own call owes the heartbeat again.
        let at = t0 + MINUTE * 5;
        dogs.observe(&subagent("task-1", SubagentStatus::Done), at);
        dogs.observe(&finished("task-1"), at);
        dogs.observe(&call("ls", None), at);
        assert_eq!(
            dogs.tick(at + late, None),
            WatchdogVerdict::Stalled { silent_for: late }
        );
    }

    #[test]
    fn agy_fanout_enforced_by_counting() {
        let mut dogs = Watchdogs::new(&limits(), Some(2));
        let at = Instant::now();
        assert_eq!(
            dogs.observe(&subagent("a", SubagentStatus::Started), at),
            WatchdogVerdict::Quiet
        );
        assert_eq!(
            dogs.observe(&subagent("b", SubagentStatus::Running), at),
            WatchdogVerdict::Quiet
        );
        assert_eq!(
            dogs.observe(&subagent("c", SubagentStatus::Started), at),
            WatchdogVerdict::FanoutExceeded { running: 3, max: 2 }
        );
        // Updates from one already counted don't repeat the alarm.
        assert_eq!(
            dogs.observe(&subagent("c", SubagentStatus::Running), at),
            WatchdogVerdict::Quiet
        );
        dogs.observe(&subagent("a", SubagentStatus::Done), at);
        dogs.observe(&subagent("c", SubagentStatus::Failed), at);
        assert_eq!(dogs.running_subagents(), 1);
        assert_eq!(
            dogs.observe(&subagent("d", SubagentStatus::Started), at),
            WatchdogVerdict::Quiet
        );
        assert_eq!(
            dogs.observe(&subagent("e", SubagentStatus::Started), at),
            WatchdogVerdict::FanoutExceeded { running: 3, max: 2 }
        );

        let mut uncapped = Watchdogs::new(&limits(), None);
        for id in ["a", "b", "c", "d"] {
            assert_eq!(
                uncapped.observe(&subagent(id, SubagentStatus::Started), at),
                WatchdogVerdict::Quiet
            );
        }
        assert_eq!(uncapped.running_subagents(), 4);
    }
}
