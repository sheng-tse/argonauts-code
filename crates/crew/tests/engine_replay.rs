//! Crash anywhere, resume, and nothing happens twice: a full run's log is cut after every
//! event, rebuilt (from its start or from its latest snapshot), and driven on against the repo
//! the full run left behind, as after a crash that came after git moved but before the log did.

mod support;

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::CardId;
use argonauts_core::engine::EngineExit;
use argonauts_core::events::card::{CardEvent, IntegrationEvent};
use argonauts_core::events::checkpoint::CheckpointEvent;
use argonauts_core::events::plan::PlanEvent;
use argonauts_core::events::run::{RunLifecycle, RunMode};
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::shared::BlackboardEvent;
use argonauts_core::events::workflow::WorkflowEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::steps::StepName;
use argonauts_crew::idempotency::card_line;
use argonauts_crew::state::RunState;
use argonauts_testkit::services::FakeServices;

use support::{Crew, names};

const OWNER_FILE: &str = "verbose.txt";

/// The ids an event creates, and the side effects it records. Seeing one of these twice means
/// something happened twice.
fn created(event: &RunEvent) -> Option<String> {
    let on = event
        .card_id
        .as_ref()
        .map_or_else(|| "the run".to_owned(), ToString::to_string);
    match &event.kind {
        RunEventKind::Checkpoint(CheckpointEvent::Opened(opened)) => {
            Some(format!("approval {}", opened.id))
        }
        RunEventKind::Checkpoint(CheckpointEvent::Decided(decided)) => {
            Some(format!("decision on {}", decided.id))
        }
        RunEventKind::Plan(PlanEvent::Sealed { draft, .. }) => Some(format!("draft {draft}")),
        RunEventKind::Plan(PlanEvent::Decided(decision)) => {
            Some(format!("decision record {}", decision.id))
        }
        RunEventKind::Blackboard(BlackboardEvent::Posted(message)) => {
            Some(format!("message {}", message.id))
        }
        RunEventKind::Blackboard(BlackboardEvent::Delivered { message, to }) => {
            Some(format!("delivery of {message} to {to}"))
        }
        RunEventKind::Workflow(WorkflowEvent::Started(started)) => {
            Some(format!("workflow {}", started.id))
        }
        RunEventKind::Card(CardEvent::Committed(commit)) => Some(match &commit.sha {
            Some(sha) => format!("commit {sha}"),
            None => format!("empty commit on {on} for {:?}", commit.message),
        }),
        RunEventKind::Integration(IntegrationEvent::Merged { card, .. }) => {
            Some(format!("merge of {card}"))
        }
        RunEventKind::Integration(IntegrationEvent::Landed { target, .. }) => {
            Some(format!("landing on {target}"))
        }
        RunEventKind::Integration(IntegrationEvent::FinalCheck { .. }) => {
            Some("final check".to_owned())
        }
        RunEventKind::Gate(gate) => Some(format!("{:?} check {} on {on}", gate.kind, gate.name)),
        RunEventKind::Run(RunLifecycle::Finished { .. }) => Some("finish".to_owned()),
        _ => None,
    }
}

/// What `prefix` already did. A gate counts once its card moved on: a crash in the middle of
/// a card's gates runs them all again, but a finished round of them never runs twice.
fn done(prefix: &[RunEvent]) -> BTreeSet<String> {
    let mut done = BTreeSet::new();
    let mut checking: BTreeMap<Option<CardId>, Vec<String>> = BTreeMap::new();
    for event in prefix {
        match &event.kind {
            RunEventKind::Gate(_) => {
                let gates = checking.entry(event.card_id.clone()).or_default();
                gates.extend(created(event));
                continue;
            }
            RunEventKind::Card(CardEvent::State { .. }) => {
                done.extend(checking.remove(&event.card_id).unwrap_or_default());
            }
            _ => {}
        }
        done.extend(created(event));
    }
    done
}

/// What the supervisor hands the engine: the log from its latest snapshot on.
fn history_from(prefix: &[RunEvent]) -> Vec<RunEvent> {
    let start = prefix
        .iter()
        .rposition(|event| matches!(event.kind, RunEventKind::Run(RunLifecycle::Snapshot(_))))
        .unwrap_or(0);
    prefix[start..].to_vec()
}

/// A fresh crew over `crew`'s repo: the owner writes a file, so the card commits and merges.
fn enlist(crew: &mut Crew) {
    crew.building_worker("claude-code/opus", "claude", OWNER_FILE, "--verbose\n");
    crew.building_worker("codex/gpt-x", "gpt", OWNER_FILE, "--verbose\n");
}

/// How many merge commits on the run's branches bring in each card, and how often main moved.
fn merges_and_moves(crew: &Crew, cards: &BTreeSet<CardId>) -> anyhow::Result<(Vec<usize>, usize)> {
    let merges = crew.repo.git(&[
        "log",
        "--merges",
        "--format=%H%n%B%x1e",
        "--branches=argonauts/*",
    ])?;
    let per_task = cards
        .iter()
        .map(|card| {
            let line = card_line(card);
            merges
                .split('\x1e')
                .filter(|entry| entry.lines().any(|text| text.trim_end() == line))
                .count()
        })
        .collect();
    let moves = crew
        .repo
        .git(&["reflog", "show", "--format=%H", "refs/heads/main"])?
        .lines()
        .count()
        .saturating_sub(1);
    Ok((per_task, moves))
}

async fn replay_every_prefix(snapshot_every: u32) -> anyhow::Result<()> {
    let mut crew = Crew::new()?;
    crew.settings.supervisor.snapshot_every = snapshot_every;
    enlist(&mut crew);
    let outcomes = crew
        .run_to_end(RunMode::Solo, "add a --verbose flag", 12)
        .await?;
    let last = &outcomes[outcomes.len() - 1];
    assert!(
        matches!(last.exit, EngineExit::Finished(_)),
        "{:?}: {:?}",
        last.exit,
        names(&last.events)
    );
    let full = last.events.clone();
    if snapshot_every > 0 {
        assert!(names(&full).contains(&"run/snapshot".to_owned()));
    }
    let cards: BTreeSet<CardId> = full
        .iter()
        .filter_map(|event| match &event.kind {
            RunEventKind::Integration(IntegrationEvent::Merged { card, .. }) => Some(card.clone()),
            _ => None,
        })
        .collect();
    assert!(!cards.is_empty(), "no task merged: {:?}", names(&full));
    let landed = full.iter().any(|event| {
        matches!(
            event.kind,
            RunEventKind::Integration(IntegrationEvent::Landed { .. })
        )
    });
    assert!(landed, "the run never merged: {:?}", names(&full));
    let once = (vec![1; cards.len()], 1);
    assert_eq!(
        merges_and_moves(&crew, &cards)?,
        once,
        "one merge commit per task and one move of main"
    );

    for cut in 1..full.len() {
        let prefix = &full[..cut];
        let before = RunState::fold(prefix);
        let history = history_from(prefix);
        // Fresh fakes, so every session they see is one the resumed run started.
        crew.services = FakeServices::new();
        enlist(&mut crew);
        let resumed = crew.resume(history.clone(), Vec::new()).await?;
        let after = &resumed.events[history.len()..];

        let mut seen = done(prefix);
        for event in after {
            if let Some(id) = created(event) {
                assert!(
                    seen.insert(id.clone()),
                    "cut at {cut} (snapshots every {snapshot_every}): {id} again, after {:?}",
                    names(after)
                );
            }
            if let RunEventKind::Session(SessionEvent::Started(started)) = &event.kind {
                let key = event.session.as_ref().expect("a session event has its key");
                if let Some(earlier) = before.sessions.get(key) {
                    assert!(
                        earlier.resumable(),
                        "cut at {cut}: {key} ended but started again"
                    );
                    assert_eq!(
                        started.resumed_from, earlier.vendor_session,
                        "cut at {cut}: {key} resumes its own agent session"
                    );
                }
            }
        }
        // A crash mid-build resumes the owner's unfinished session under its own key.
        for unfinished in before.sessions.by_key.values().filter(|session| {
            session.end.is_none() && session.started.step == Some(StepName::Build)
        }) {
            let next_build = after.iter().find_map(|event| match &event.kind {
                RunEventKind::Session(SessionEvent::Started(started))
                    if started.step == Some(StepName::Build)
                        && event.card_id == unfinished.card =>
                {
                    event.session.clone()
                }
                _ => None,
            });
            if let Some(next_build) = next_build {
                assert_eq!(
                    next_build, unfinished.key,
                    "cut at {cut}: the build started a new session instead of resuming"
                );
            }
        }
        let fresh: Vec<u32> = after
            .iter()
            .filter_map(|event| event.session.as_ref())
            .filter(|key| before.sessions.get(key).is_none())
            .filter_map(|key| argonauts_crew::state::ids::number(key.as_str()))
            .collect();
        assert!(
            fresh.iter().all(|n| *n > before.ids.sessions),
            "cut at {cut}: a new session reused a number"
        );
    }
    // Every cut resumed against the same repo, so a second merge or landing anywhere shows.
    assert_eq!(
        merges_and_moves(&crew, &cards)?,
        once,
        "merged or merged again"
    );
    Ok(())
}

#[tokio::test]
async fn replay_from_the_start_repeats_nothing() -> anyhow::Result<()> {
    replay_every_prefix(0).await
}

#[tokio::test]
async fn replay_from_snapshots_repeats_nothing() -> anyhow::Result<()> {
    replay_every_prefix(7).await
}
