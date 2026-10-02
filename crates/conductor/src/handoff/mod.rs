//! The handoff planner: who takes a stopped card, and what they get. Candidates come from the most
//! specific replacement list (your pick, then the role and vendor, the role, the vendor, then the
//! global list), filtered by the card's rules, health, quota, and cost, with the strategy that
//! applies. The packet is built by code from the card's events, at no token cost, and so is the
//! old session's transcript that `handoff.context` may give the replacement.

pub mod history;
pub mod packet;
pub mod shortlist;
pub mod tier;
pub mod transcript;
pub mod why;

#[cfg(test)]
pub(crate) mod testing;

use std::path::PathBuf;
use std::sync::Arc;

use argonauts_core::events::RunEvent;
use argonauts_core::events::card::{CardKind, CardSpec};
use argonauts_core::limits::{
    Candidate, CandidateQuery, HandoffPacket, HandoffPlanner, HealthBoard, QuotaBoard, Shortlist,
    TrackRecords, TranscriptPaths,
};
use argonauts_core::names::{RoleName, VendorId, WorkerRef};
use argonauts_core::registry::ModelRegistry;
use argonauts_core::settings::Settings;
use argonauts_core::settings::handoff::{
    AppliedHandoff, FallbackEntry, FallbackRules, HandoffContext, HandoffSettings,
};

use crate::slots::Slots;

/// How many more sessions a vendor can start right now, for `spread`.
pub trait FreeSlots: Send + Sync {
    fn free(&self, vendor: &VendorId) -> Option<u32>;
}

/// Counts the way a session starts: it takes every slot it can, both a global and a vendor one,
/// then lets them all go. A session starting elsewhere meanwhile waits one retry at most.
impl FreeSlots for Slots {
    fn free(&self, vendor: &VendorId) -> Option<u32> {
        let mut held = Vec::new();
        loop {
            match self.try_acquire(vendor) {
                Ok(Some(slot)) => held.push(slot),
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(%vendor, %error, "spread can't count this vendor's free slots");
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
    quota: Arc<dyn QuotaBoard>,
    health: Arc<dyn HealthBoard>,
    records: Arc<dyn TrackRecords>,
    slots: Option<Arc<dyn FreeSlots>>,
    /// The run's folder, where a handoff keeps its copy of the transcript.
    run_dir: Option<PathBuf>,
}

impl Planner {
    pub fn new(
        settings: Arc<Settings>,
        registry: Arc<dyn ModelRegistry>,
        quota: Arc<dyn QuotaBoard>,
        health: Arc<dyn HealthBoard>,
        records: Arc<dyn TrackRecords>,
    ) -> Self {
        Self {
            settings,
            registry,
            quota,
            health,
            records,
            slots: None,
            run_dir: None,
        }
    }

    /// Lets a packet come with the old session's transcript, written to the card's worktree and
    /// to `run_dir`. Without it, packets name no transcript.
    pub fn with_run_dir(mut self, run_dir: PathBuf) -> Self {
        self.run_dir = Some(run_dir);
        self
    }

    /// Lets `spread` weigh each vendor's free session slots.
    pub fn with_slots(mut self, slots: Arc<dyn FreeSlots>) -> Self {
        self.slots = Some(slots);
        self
    }

    fn sources(&self) -> shortlist::Sources<'_> {
        shortlist::Sources {
            settings: &self.settings,
            registry: self.registry.as_ref(),
            quota: self.quota.as_ref(),
            health: self.health.as_ref(),
            records: self.records.as_ref(),
            slots: self.slots.as_deref(),
        }
    }

    /// Writes the transcript when the context that applies to the stopped session wants one. A
    /// transcript that can't be written leaves the replacement with the note alone.
    fn transcript(
        &self,
        card: &CardSpec,
        events: &[RunEvent],
        why_stopped: &str,
    ) -> Option<TranscriptPaths> {
        let run_dir = self.run_dir.as_deref()?;
        let session = transcript::old_session(&card.id, events)?;
        let started = session.started;
        let context = applied(&self.settings, started.role, &started.vendor).context;
        if !matches!(
            context,
            HandoffContext::NoteAndTranscript | HandoffContext::NoteAndSummary
        ) {
            return None;
        }
        let Some(worktree) = session.worktree else {
            tracing::warn!(
                card = %card.id,
                "no transcript: the log doesn't say where the card's worktree is"
            );
            return None;
        };
        let text = transcript::render(card, &session, events, why_stopped);
        match transcript::write(&text, session.number, worktree, run_dir, &card.id) {
            Ok(paths) => Some(paths),
            Err(error) => {
                tracing::warn!(
                    card = %card.id,
                    %error,
                    "the handoff transcript couldn't be written"
                );
                None
            }
        }
    }
}

impl HandoffPlanner for Planner {
    fn candidates(&self, query: &CandidateQuery) -> Vec<Candidate> {
        shortlist::shortlist(query, &self.sources()).candidates
    }

    fn packet(&self, card: &CardSpec, events: &[RunEvent]) -> HandoffPacket {
        let limit = card
            .budget
            .limit
            .or_else(|| crate::recovery::per_card_budget(&self.settings.budget));
        let mut packet = packet::build_with_budget(card, events, limit);
        packet.transcript = self.transcript(card, events, &packet.why_stopped);
        packet
    }

    fn shortlist(&self, query: &CandidateQuery) -> Shortlist {
        shortlist::shortlist(query, &self.sources())
    }
}

/// The handoff rules for `role` when `vendor` stops, walked by core from the role's table and
/// the top-level `[handoff]`.
pub fn applied(settings: &Settings, role: RoleName, vendor: &VendorId) -> AppliedHandoff {
    let role_table = settings.roles.get(&role).map(|role| &role.handoff);
    HandoffSettings::applied(&settings.handoff, role_table, vendor)
}

/// The rules an entry for `worker` carries, from the first entry that names it.
pub fn entry_rules<'a>(
    entries: &'a [FallbackEntry],
    worker: &WorkerRef,
) -> Option<&'a FallbackRules> {
    entries
        .iter()
        .find(|entry| entry.worker() == worker)
        .and_then(|entry| match entry {
            FallbackEntry::WithRules(rules) => Some(rules),
            FallbackEntry::Worker(_) => None,
        })
}

/// The task type track records are kept under, the card kind's own tag.
pub fn task_type(card: &CardSpec) -> &'static str {
    match card.kind {
        CardKind::Solo => "solo",
        CardKind::Interface => "interface",
        CardKind::AcceptanceTests { .. } => "acceptance_tests",
        CardKind::Build => "build",
        CardKind::Chore => "chore",
        CardKind::Docs => "docs",
        CardKind::Spike { .. } => "spike",
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use argonauts_core::Paths;
    use argonauts_core::settings::sessions::ConcurrencySettings;

    use super::*;
    use crate::handoff::testing::vendor;
    use crate::slots::SlotGuard;

    /// A test elsewhere that forks a child shares every open lock until the child execs, so a
    /// released slot can look taken for a moment. Counts are read once they settle.
    fn settles_at(slots: &Slots, vendor: &VendorId, expected: u32) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while slots.free(vendor) != Some(expected) {
            if Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        true
    }

    fn hold(slots: &Slots, vendor: &VendorId) -> SlotGuard {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(slot) = slots.try_acquire(vendor).expect("the slots open") {
                return slot;
            }
            assert!(Instant::now() < deadline, "no slot freed up for {vendor}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn free_slots_count_the_global_and_vendor_caps() {
        let home = tempfile::tempdir().expect("a temp dir");
        let settings = ConcurrencySettings {
            max_sessions: 3,
            max_per_vendor: 2,
        };
        // A second board stands for another process, whose locks the count must see.
        let slots = Slots::new(&Paths::new(home.path()), &settings);
        let other = Slots::new(&Paths::new(home.path()), &settings);
        let codex = vendor("codex");
        let gemini = vendor("antigravity-cli");
        assert!(settles_at(&slots, &codex, 2));

        let held = hold(&other, &codex);
        assert!(settles_at(&slots, &codex, 1));
        assert!(settles_at(&slots, &gemini, 2));
        let _second = hold(&other, &codex);
        assert!(settles_at(&slots, &codex, 0));
        // Only the global cap is left for another vendor.
        assert!(settles_at(&slots, &gemini, 1));

        // Counting gave every slot back, so the one freed can be taken again.
        drop(held);
        let _third = hold(&other, &codex);
        assert!(settles_at(&slots, &codex, 0));
        assert!(settles_at(&slots, &gemini, 1));
    }
}
