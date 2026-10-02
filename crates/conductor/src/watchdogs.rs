//! The watchdogs inside every session: inactivity (no events, no output, and no CPU in the
//! process tree), the same tool call repeated, a stall with no heartbeat, and a harness's own
//! subagents past the fan-out cap where only counting can enforce it (agy).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use argonauts_core::AgentEvent;
use argonauts_core::events::session::{SubagentStatus, SubagentUpdate, Watchdog};
use argonauts_core::worker::SessionLimits;
use serde_json::Value;

/// Claude Code sends a heartbeat every 30 seconds, so longer silence is worth a warning.
pub const STALL_AFTER: Duration = Duration::from_secs(30);

/// The fixed note an agent gets for repeating the same call.
pub const REPEAT_NOTE: &str = "You have made the same tool call several times in a row with the \
same input. Stop repeating it: try a different approach, or finish and report what blocks you.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchdogVerdict {
    Quiet,
    /// No event for longer than [`STALL_AFTER`]; shown, not acted on.
    Stalled {
        silent_for: Duration,
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
            started: Instant::now(),
            last_event: None,
            last_output: None,
            streaks: HashMap::new(),
            running: HashSet::new(),
        }
    }

    pub fn observe(&mut self, event: &AgentEvent, now: Instant) -> WatchdogVerdict {
        self.last_event = Some(now);
        match event {
            AgentEvent::ToolStarted {
                name,
                input,
                parent,
                ..
            } => self.repeat(parent, name, input),
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
        if inactive_after.is_some_and(|limit| now.saturating_duration_since(last_active) >= limit) {
            return WatchdogVerdict::Stuck(Watchdog::Inactivity);
        }
        let silent_for = now.saturating_duration_since(self.last_event.unwrap_or(self.started));
        if silent_for >= STALL_AFTER {
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
        assert!(matches!(
            dogs.tick(t0 + MINUTE * 14, None),
            WatchdogVerdict::Stalled { .. }
        ));
        assert_eq!(
            dogs.tick(t0 + MINUTE * 16, None),
            WatchdogVerdict::Stuck(Watchdog::Inactivity)
        );

        // Output and CPU each keep it alive, though neither is an event.
        dogs.output(t0 + MINUTE * 10);
        assert!(matches!(
            dogs.tick(t0 + MINUTE * 16, None),
            WatchdogVerdict::Stalled { .. }
        ));
        assert!(matches!(
            dogs.tick(t0 + MINUTE * 30, Some(t0 + MINUTE * 20)),
            WatchdogVerdict::Stalled { .. }
        ));
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
        assert_eq!(
            dogs.tick(at + MINUTE, None),
            WatchdogVerdict::Stalled { silent_for: MINUTE }
        );

        let mut unlimited = Watchdogs::new(&SessionLimits::default(), None);
        for _ in 0..20 {
            assert_eq!(
                unlimited.observe(&call("ls", None), at),
                WatchdogVerdict::Quiet
            );
        }
        assert!(matches!(
            unlimited.tick(at + MINUTE * 600, None),
            WatchdogVerdict::Stalled { .. }
        ));
    }

    #[test]
    fn stall_warning_after_30_seconds() {
        let mut dogs = Watchdogs::new(&limits(), None);
        let t0 = Instant::now();
        dogs.observe(&text(), t0);
        assert_eq!(
            dogs.tick(t0 + Duration::from_secs(29), None),
            WatchdogVerdict::Quiet
        );
        // Output without events is no heartbeat.
        dogs.output(t0 + Duration::from_secs(30));
        assert_eq!(
            dogs.tick(t0 + Duration::from_secs(31), None),
            WatchdogVerdict::Stalled {
                silent_for: Duration::from_secs(31)
            }
        );
        dogs.observe(&text(), t0 + Duration::from_secs(32));
        assert_eq!(
            dogs.tick(t0 + Duration::from_secs(33), None),
            WatchdogVerdict::Quiet
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
