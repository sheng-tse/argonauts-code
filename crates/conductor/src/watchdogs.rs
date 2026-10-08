//! The watchdogs inside every session: inactivity (no events, no output, and no CPU in the
//! process tree), the same tool call repeated, a stall with no heartbeat, and a harness's own
//! subagents past the fan-out cap where only counting can enforce it (agy).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use argonauts_core::AgentEvent;
use argonauts_core::events::session::{SubagentStatus, SubagentUpdate, Watchdog};
use argonauts_core::worker::SessionLimits;
use serde_json::Value;

/// How late a heartbeat may be before the silence counts as a stall.
pub const HEARTBEAT_GRACE: Duration = Duration::from_secs(15);

/// The fixed note an agent gets for repeating the same call.
pub const REPEAT_NOTE: &str = "You have made the same tool call several times in a row with the \
same input. Stop repeating it: try a different approach, or finish and report what blocks you.";

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
    /// Send the wrap-up message, then stop if it keeps going.
    SoftStop(Watchdog),
    /// Interrupt, then stop. The end reason is `Stuck`.
    Stuck(Watchdog),
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
    running: HashSet<String>,
}

#[derive(Debug)]
struct Streak {
    name: String,
    input: Value,
    times: u32,
}

impl Watchdogs {
    /// `fanout_cap` is set only where the conductor enforces the cap by counting.
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
            running: HashSet::new(),
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
                if parent.is_none() {
                    self.tools.insert(id.clone());
                }
                self.repeat(parent, name, input)
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

    /// `cpu_busy_at` is the shared sampler's latest busy time for the process tree.
    pub fn tick(&mut self, now: Instant, cpu_busy_at: Option<Instant>) -> WatchdogVerdict {
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
                return WatchdogVerdict::Stuck(Watchdog::Inactivity);
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
            Some(_) => WatchdogVerdict::Stuck(Watchdog::Repeat),
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

    #[test]
    fn watchdogs_inactivity_repeat() {
        let mut dogs = Watchdogs::new(&limits(), None);
        let t0 = Instant::now();
        assert_eq!(dogs.tick(t0 + MINUTE * 7, None), WatchdogVerdict::Quiet);
        assert_eq!(
            dogs.tick(t0 + MINUTE * 16, None),
            WatchdogVerdict::Stuck(Watchdog::Inactivity)
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
            WatchdogVerdict::Stuck(Watchdog::Inactivity)
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
            WatchdogVerdict::Stuck(Watchdog::Repeat)
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
            WatchdogVerdict::Stuck(Watchdog::Inactivity)
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
