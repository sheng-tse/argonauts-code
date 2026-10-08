//! Stopping an agent: why the worker did it, and the adapter's stop plan run step by step on the
//! process group, always ending in SIGKILL so a stopped agent never keeps running.

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use argonauts_core::events::session::{EndReason, Watchdog};
use argonauts_core::{BudgetDimension, ErrorCategory, StopPlan, StopSignal, StopStep};
use time::OffsetDateTime;
use tokio::time::Instant;

use crate::process::AgentProcess;

/// Why the worker stopped a session.
#[derive(Debug, Clone, PartialEq)]
pub enum StopCause {
    /// The meter reached 100% of a limit.
    Limit(BudgetDimension),
    Stuck(Watchdog),
    /// The context reached `sessions.rotate_at`.
    Rotate {
        used: u64,
        cap: u64,
    },
    /// `StopAfterTool`.
    Paused,
    /// `Stop`, or every control sender dropped.
    Stopped,
    /// The harness ran more subagents than its cap, where only counting can enforce it.
    Fanout {
        running: u32,
        max: u32,
    },
    /// The group couldn't be recorded, registered, or guarded, so it must not run.
    Unsafe(String),
    /// The CLI is finishing on its wrap-up allowance, and `handoff.wrap_up = "hand_off"` saves
    /// it for later: the turn ends here and the card moves on at once.
    WrapUp {
        resets_at: Option<OffsetDateTime>,
    },
    /// The provider's plan window is as full as your `limits.plan_usage` lets it get, and
    /// `limits.when_reached` is `fallback`: the turn ends as if the plan had run out, so the
    /// fallbacks take over.
    PlanLimit {
        resets_at: Option<OffsetDateTime>,
    },
    /// An isolated session loaded what its role mustn't see.
    IsolationBroken {
        plugins: Vec<String>,
        skills: Vec<String>,
    },
}

impl StopCause {
    pub fn reason(&self) -> EndReason {
        match self {
            Self::Limit(dimension) => EndReason::BudgetHit {
                dimension: *dimension,
            },
            Self::Stuck(watchdog) => EndReason::Stuck {
                watchdog: *watchdog,
            },
            Self::Rotate { .. } => EndReason::ContextFull,
            Self::Paused => EndReason::Paused,
            Self::Stopped => EndReason::Stopped,
            Self::Fanout { running, max } => EndReason::VendorError {
                category: ErrorCategory::Interrupted,
                message: format!(
                    "interrupted: {running} subagents were running, and the cap is {max}"
                ),
            },
            Self::Unsafe(message) => EndReason::StartFailed {
                message: message.clone(),
            },
            Self::WrapUp { resets_at } | Self::PlanLimit { resets_at } => EndReason::UsageLimit {
                resets_at: *resets_at,
            },
            Self::IsolationBroken { plugins, skills } => EndReason::IsolationBroken {
                plugins: plugins.clone(),
                skills: skills.clone(),
            },
        }
    }
}

/// What the signals are for.
#[derive(Debug, Clone, PartialEq)]
pub enum Purpose {
    End(StopCause),
    /// Interrupt, then resume the same vendor session with a message.
    Relaunch,
}

#[derive(Debug)]
pub struct Stopping {
    purpose: Purpose,
    steps: VecDeque<StopStep>,
    next_at: Option<Instant>,
}

impl Stopping {
    /// The plan's first signal is due at once. The session sends each signal with
    /// [`Stopping::step`], after it noted the process tree the signal may break up.
    pub fn begin(purpose: Purpose, plan: StopPlan) -> Self {
        Self {
            purpose,
            steps: complete_plan(plan).into(),
            next_at: Some(Instant::now()),
        }
    }

    pub fn purpose(&self) -> &Purpose {
        &self.purpose
    }

    /// When the next, harder signal is due.
    pub fn next_at(&self) -> Option<Instant> {
        self.next_at
    }

    /// Sends the signal that is due. The plan goes on even when it fails.
    pub fn step(&mut self, process: &AgentProcess) -> io::Result<()> {
        let Some(step) = self.steps.pop_front() else {
            self.next_at = None;
            return Ok(());
        };
        self.next_at = Some(Instant::now() + step.wait);
        process.signal(step.signal)
    }

    /// A relaunch becomes an end, keeping the signals already sent. An end keeps its first
    /// cause.
    pub fn end_instead(&mut self, cause: StopCause) {
        if self.purpose == Purpose::Relaunch {
            self.purpose = Purpose::End(cause);
        }
    }
}

/// The plan's steps, ending in SIGKILL.
pub fn complete_plan(plan: StopPlan) -> Vec<StopStep> {
    let mut steps = plan.steps;
    if steps
        .last()
        .is_none_or(|step| step.signal != StopSignal::Kill)
    {
        steps.push(StopStep {
            signal: StopSignal::Kill,
            wait: Duration::from_secs(2),
        });
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_always_end_in_kill() {
        let polite = StopPlan {
            steps: vec![StopStep {
                signal: StopSignal::Interrupt,
                wait: Duration::from_secs(1),
            }],
        };
        let steps = complete_plan(polite);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[1].signal, StopSignal::Kill);
        assert_eq!(complete_plan(StopPlan { steps: vec![] }).len(), 1);
        assert_eq!(complete_plan(StopPlan::default()).len(), 3);
    }

    #[test]
    fn causes_become_reasons() {
        assert_eq!(StopCause::Stopped.reason(), EndReason::Stopped);
        assert_eq!(StopCause::Paused.reason(), EndReason::Paused);
        assert_eq!(
            StopCause::Rotate { used: 9, cap: 8 }.reason(),
            EndReason::ContextFull
        );
        assert_eq!(
            StopCause::Limit(BudgetDimension::Minutes).reason(),
            EndReason::BudgetHit {
                dimension: BudgetDimension::Minutes
            }
        );
        assert!(matches!(
            StopCause::Unsafe("no guard".into()).reason(),
            EndReason::StartFailed { .. }
        ));
        let leak = StopCause::IsolationBroken {
            plugins: vec!["memory-keeper".into()],
            skills: Vec::new(),
        };
        assert_eq!(
            leak.reason(),
            EndReason::IsolationBroken {
                plugins: vec!["memory-keeper".into()],
                skills: Vec::new()
            }
        );
    }
}
