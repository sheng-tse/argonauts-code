//! The fallback planner: who takes a stopped task, and what they get. Candidates come from the most
//! specific fallback list (your pick, then the role and agent, the role, the agent, then the
//! global list), filtered by the task's rules, health, plan usage, and cost, with the strategy that
//! applies. The note is built by code from the task's events, at no token cost, and so is the
//! old session's transcript that `fallback.context` may give the fallback.

pub mod history;
pub mod note;
pub mod shortlist;
pub mod tier;
pub mod transcript;
pub mod why;

#[cfg(test)]
pub(crate) mod testing;

use std::path::PathBuf;
use std::sync::Arc;

use argonauts_core::events::RunEvent;
use argonauts_core::events::task::{TaskKind, TaskSpec};
use argonauts_core::limits::{
    Candidate, CandidateQuery, FallbackNote, FallbackPlanner, HealthBoard, PlanUsageBoard,
    Shortlist, TrackRecords, TranscriptPaths,
};
use argonauts_core::names::{AgentId, ModelRef, RoleName};
use argonauts_core::registry::ModelRegistry;
use argonauts_core::session_runner::RunnerDirectory;
use argonauts_core::settings::Settings;
use argonauts_core::settings::fallback::{
    AppliedFallback, FallbackContext, FallbackEntry, FallbackRules,
};

use crate::slots::Slots;

/// How many more sessions an agent can start right now, for `spread`.
pub trait FreeSlots: Send + Sync {
    fn free(&self, agent_id: &AgentId) -> Option<u32>;
}

/// Counts the way a session starts: it takes every slot it can, both a global and an agent one,
/// then lets them all go. A session starting elsewhere meanwhile waits one retry at most.
impl FreeSlots for Slots {
    fn free(&self, agent_id: &AgentId) -> Option<u32> {
        let mut held = Vec::new();
        loop {
            match self.try_acquire(agent_id) {
                Ok(Some(slot)) => held.push(slot),
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(%agent_id, %error, "spread can't count this agent's free slots");
                    return None;
                }
            }
        }
        u32::try_from(held.len()).ok()
    }
}

pub struct Planner {
    settings: Arc<Settings>,
    registry: Arc<dyn ModelRegistry>,
    plan_usage: Arc<dyn PlanUsageBoard>,
    health: Arc<dyn HealthBoard>,
    records: Arc<dyn TrackRecords>,
    slots: Option<Arc<dyn FreeSlots>>,
    workers: Option<Arc<dyn RunnerDirectory>>,
    /// The run's folder, which keeps the transcript a fallback is given.
    run_dir: Option<PathBuf>,
}

impl Planner {
    pub fn new(
        settings: Arc<Settings>,
        registry: Arc<dyn ModelRegistry>,
        plan_usage: Arc<dyn PlanUsageBoard>,
        health: Arc<dyn HealthBoard>,
        records: Arc<dyn TrackRecords>,
    ) -> Self {
        Self {
            settings,
            registry,
            plan_usage,
            health,
            records,
            slots: None,
            workers: None,
            run_dir: None,
        }
    }

    /// Lets a note come with the old session's transcript, written to the task's worktree and
    /// to `run_dir`. Without it, notes name no transcript.
    pub fn with_run_dir(mut self, run_dir: PathBuf) -> Self {
        self.run_dir = Some(run_dir);
        self
    }

    /// Lets `spread` weigh each agent's free session slots.
    pub fn with_slots(mut self, slots: Arc<dyn FreeSlots>) -> Self {
        self.slots = Some(slots);
        self
    }

    /// Leaves out a candidate whose CLI would refuse to start, such as OpenCode with no model
    /// set, so a stopped task never goes to one that can't take it.
    pub fn with_workers(mut self, workers: Arc<dyn RunnerDirectory>) -> Self {
        self.workers = Some(workers);
        self
    }

    fn sources(&self) -> shortlist::Sources<'_> {
        shortlist::Sources {
            settings: &self.settings,
            registry: self.registry.as_ref(),
            plan_usage: self.plan_usage.as_ref(),
            health: self.health.as_ref(),
            records: self.records.as_ref(),
            slots: self.slots.as_deref(),
            workers: self.workers.as_deref(),
        }
    }

    /// Writes the transcript when the context that applies to the stopped session wants one. A
    /// transcript that can't be written leaves the fallback with the note alone.
    fn transcript(
        &self,
        task: &TaskSpec,
        events: &[RunEvent],
        why_stopped: &str,
    ) -> Option<TranscriptPaths> {
        let run_dir = self.run_dir.as_deref()?;
        let session = transcript::old_session(&task.id, events)?;
        let started = session.started;
        let context = applied(&self.settings, started.role, &started.agent_id).context;
        if !matches!(
            context,
            FallbackContext::NoteAndTranscript | FallbackContext::NoteAndSummary
        ) {
            return None;
        }
        let Some(worktree) = session.worktree else {
            tracing::warn!(
                task = %task.id,
                "no transcript: the log doesn't say where the task's worktree is"
            );
            return None;
        };
        let text = transcript::render(task, &session, events, why_stopped);
        match transcript::write(&text, session.number, worktree, run_dir, &task.id) {
            Ok(paths) => Some(paths),
            Err(error) => {
                tracing::warn!(
                    task = %task.id,
                    %error,
                    "the fallback transcript couldn't be written"
                );
                None
            }
        }
    }
}

impl FallbackPlanner for Planner {
    fn candidates(&self, query: &CandidateQuery) -> Vec<Candidate> {
        shortlist::shortlist(query, &self.sources()).candidates
    }

    fn note(&self, task: &TaskSpec, events: &[RunEvent]) -> FallbackNote {
        let limit = task
            .limit
            .limit
            .or_else(|| self.settings.limits.spend.task_limit());
        let mut note = note::build_with_limit(task, events, limit);
        note.transcript = self.transcript(task, events, &note.why_stopped);
        note
    }

    fn shortlist(&self, query: &CandidateQuery) -> Shortlist {
        shortlist::shortlist(query, &self.sources())
    }
}

/// The fallback rules for `role` when `agent` stops, walked by core from the top-level
/// `[fallback]` and the table of the crew role that `role` follows.
pub fn applied(settings: &Settings, role: RoleName, agent_id: &AgentId) -> AppliedFallback {
    settings.fallback.applied_to(role, agent_id)
}

/// The rules an entry for `worker` carries, from the first entry that names it.
pub fn entry_rules<'a>(
    entries: &'a [FallbackEntry],
    worker: &ModelRef,
) -> Option<&'a FallbackRules> {
    entries
        .iter()
        .find(|entry| entry.model() == worker)
        .and_then(|entry| match entry {
            FallbackEntry::WithRules(rules) => Some(rules),
            FallbackEntry::Model(_) => None,
        })
}

/// The task type track records are kept under, the task kind's own tag.
pub fn task_type(task: &TaskSpec) -> &'static str {
    match task.kind {
        TaskKind::Captain => "captain",
        TaskKind::Interface => "interface",
        TaskKind::AcceptanceTests { .. } => "acceptance_tests",
        TaskKind::Build => "build",
        TaskKind::Chore => "chore",
        TaskKind::Docs => "docs",
        TaskKind::Experiment { .. } => "experiment",
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use argonauts_core::Paths;
    use argonauts_core::settings::sessions::ConcurrencySettings;

    use super::*;
    use crate::fallback::testing::agent_id;
    use crate::slots::SlotGuard;

    /// A test elsewhere that forks a child shares every open lock until the child execs, so a
    /// released slot can look taken for a moment. Counts are read once they settle.
    fn settles_at(slots: &Slots, agent_id: &AgentId, expected: u32) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while slots.free(agent_id) != Some(expected) {
            if Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        true
    }

    fn hold(slots: &Slots, agent_id: &AgentId) -> SlotGuard {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(slot) = slots.try_acquire(agent_id).expect("the slots open") {
                return slot;
            }
            assert!(Instant::now() < deadline, "no slot freed up for {agent_id}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn free_slots_count_the_global_and_agent_caps() {
        let home = tempfile::tempdir().expect("a temp dir");
        let settings = ConcurrencySettings {
            max_sessions: 3,
            max_per_agent: 2,
        };
        // A second board stands for another process, whose locks the count must see.
        let slots = Slots::new(&Paths::new(home.path()), &settings);
        let other = Slots::new(&Paths::new(home.path()), &settings);
        let codex = agent_id("codex");
        let gemini = agent_id("antigravity-cli");
        assert!(settles_at(&slots, &codex, 2));

        let held = hold(&other, &codex);
        assert!(settles_at(&slots, &codex, 1));
        assert!(settles_at(&slots, &gemini, 2));
        let _second = hold(&other, &codex);
        assert!(settles_at(&slots, &codex, 0));
        // Only the global cap is left for another agent.
        assert!(settles_at(&slots, &gemini, 1));

        // Counting gave every slot back, so the one freed can be taken again.
        drop(held);
        let _third = hold(&other, &codex);
        assert!(settles_at(&slots, &codex, 0));
        assert!(settles_at(&slots, &gemini, 1));
    }
}
