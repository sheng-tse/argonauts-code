//! A race: one card to several candidates at once (`race_count`), each in its own worktree,
//! sharing the card's remaining budget. The first attempt that passes its gates and its review
//! wins. The others stop, and once their sessions end their work is committed on their own
//! branches under their names.
//!
//! Attempts take numbers no earlier attempt used, read from the log, so a tree or branch is
//! never reused. A race runs while the card stays in one state, so the attempts since the
//! card's last state change are this race's: re-entered after a restart, the race finds the
//! same trees, and its sessions the slots replay matches.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicU32;
use std::time::Duration;

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use tokio_util::sync::CancellationToken;

use argonauts_core::CardId;
use argonauts_core::events::card::CardEvent;
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::limits::HandoffPacket;
use argonauts_core::names::{RoleName, SessionKey, WorkerRef};
use argonauts_core::steps::StepName;
use argonauts_core::worker::SessionControl;

use crate::card::pipeline::{self, AttemptCall, AttemptEnd};
use crate::card::support::card_events;
use crate::card::worktree::{self, CardTree};
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::handoff::{assign, notes, rules, upgrade};
use crate::state::RunState;
use crate::state::sessions::SessionRecord;
use crate::workflow::StepAt;

/// How often a decided race looks again for loser sessions to stop, since one may start late.
const STOP_POLL: Duration = Duration::from_millis(100);

/// One candidate and its own tree.
struct Entrant {
    worker: WorkerRef,
    tree: CardTree,
    first_slot: u32,
    /// What its first session is told, naming its own copy of the transcript.
    packet: HandoffPacket,
}

/// Where this race sits in the card's log.
struct RaceLog {
    /// The seq of the card's last state change, which this race's events all come after.
    /// `None` when the log can't be read, and then every attempt starts fresh.
    since: Option<u64>,
    /// The highest attempt number any tree or branch of the card was given.
    highest: u32,
    /// Attempts this race already started, by worker.
    started: BTreeMap<WorkerRef, u32>,
}

/// Returns the winner.
pub async fn run(
    host: &Host,
    card: &CardId,
    to: &[WorkerRef],
    packet: &HandoffPacket,
) -> EngineResult<Option<WorkerRef>> {
    let at = rules::step_now(host)?;
    // Every attempt starts from this packet, built when the owner stopped. Built again later,
    // it could name an attempt that started first as the session that stopped.
    assign::write_packet(host, card, packet).await?;
    let record = rules::record(host, card)?;
    let start = match &record.branch {
        Some(branch) => Some(host.git().resolve(&host.repo().root, branch).await?),
        None => None,
    };
    let log = race_log(host, card)?;
    let entrants = entrants(host, card, to, &at, &log, packet).await?;
    let stop = CancellationToken::new();
    let rounds = AtomicU32::new(first_round(host, card)?);
    let (winner, error) = race(host, card, &entrants, &at, &rounds, &stop).await;
    if matches!(error, Some(EngineError::Interrupted)) {
        return Err(EngineError::Interrupted);
    }
    for entrant in &entrants {
        if Some(&entrant.worker) != winner.as_ref() {
            keep(host, card, entrant, log.since).await?;
        }
    }
    let Some(winner) = winner else {
        notes::catch_up(host, card).await?;
        return error.map_or(Ok(None), Err);
    };
    if let Some(entrant) = entrants.iter().find(|entrant| entrant.worker == winner) {
        take_the_winners_work(host, card, &entrant.tree, start.as_deref()).await?;
    }
    notes::catch_up(host, card).await?;
    Ok(Some(winner))
}

/// Runs every attempt at once until one passes its gates and review, then stops the others
/// and waits for each to end. Also returns the first error, for when none passed.
async fn race(
    host: &Host,
    card: &CardId,
    entrants: &[Entrant],
    at: &StepAt,
    rounds: &AtomicU32,
    stop: &CancellationToken,
) -> (Option<WorkerRef>, Option<EngineError>) {
    let mut running: FuturesUnordered<_> = entrants
        .iter()
        .map(|entrant| async move {
            let call = AttemptCall {
                card,
                owner: &entrant.worker,
                tree: &entrant.tree,
                at,
                first_slot: entrant.first_slot,
                packet: &entrant.packet,
                rounds,
                stop,
            };
            (&entrant.worker, pipeline::run_attempt(host, call).await)
        })
        .collect();
    let mut winner: Option<WorkerRef> = None;
    let mut error = None;
    let mut stopped: Vec<SessionKey> = Vec::new();
    loop {
        let next = if stop.is_cancelled() {
            stop_losers(host, card, entrants, winner.as_ref(), &mut stopped);
            tokio::select! {
                next = running.next() => next,
                () = tokio::time::sleep(STOP_POLL) => continue,
            }
        } else {
            running.next().await
        };
        let Some((worker, end)) = next else {
            break;
        };
        match end {
            Ok(AttemptEnd::Passed) if winner.is_none() => {
                winner = Some(worker.clone());
                stop.cancel();
            }
            Ok(_) => {}
            Err(EngineError::Interrupted) => return (winner, Some(EngineError::Interrupted)),
            Err(err) => {
                error.get_or_insert(err);
            }
        }
    }
    (winner, error)
}

/// Stops the owner sessions of the attempts that lost, each once.
fn stop_losers(
    host: &Host,
    card: &CardId,
    entrants: &[Entrant],
    winner: Option<&WorkerRef>,
    stopped: &mut Vec<SessionKey>,
) {
    let keys: Vec<SessionKey> = host.read(|state| {
        state
            .sessions
            .running()
            .filter(|session| {
                session.card.as_ref() == Some(card)
                    && session.started.role == RoleName::Owner
                    && Some(&session.started.worker) != winner
                    && entrants
                        .iter()
                        .any(|entrant| entrant.worker == session.started.worker)
                    && !stopped.contains(&session.key)
            })
            .map(|session| session.key.clone())
            .collect()
    });
    for key in keys {
        // A session that ended meanwhile refuses the control, and one with a full control
        // queue is tried again at the next poll; either way its attempt still ends.
        if host.control(&key, SessionControl::Stop).is_ok() {
            stopped.push(key);
        }
    }
}

/// Commits what a losing attempt left uncommitted, as its newest session's work. Its sessions
/// commit when they end, so only an attempt that broke off leaves anything.
async fn keep(
    host: &Host,
    card: &CardId,
    entrant: &Entrant,
    since: Option<u64>,
) -> EngineResult<()> {
    let session = host.read(|state| {
        owner_sessions(state, card, &entrant.worker)
            .filter(|session| since.is_some_and(|since| session.since > since))
            .max_by_key(|session| session.since)
            .map(|session| session.key.clone())
    });
    let by = Some(entrant.worker.clone());
    worktree::commit_in(host, card, &entrant.tree, session.as_ref(), by)
        .await
        .map(drop)
}

/// A tree for each distinct candidate: the one this race already gave it, else a number no
/// tree or branch of the card has had.
async fn entrants(
    host: &Host,
    card: &CardId,
    to: &[WorkerRef],
    at: &StepAt,
    log: &RaceLog,
    packet: &HandoffPacket,
) -> EngineResult<Vec<Entrant>> {
    let card_tree = rules::record(host, card)?.worktree;
    let mut next = log.started.values().copied().fold(log.highest, u32::max) + 1;
    let mut entrants: Vec<Entrant> = Vec::new();
    for worker in to {
        if entrants.iter().any(|entrant| entrant.worker == *worker) {
            continue;
        }
        let attempt = match log.started.get(worker) {
            Some(attempt) => *attempt,
            None => {
                while worktree::attempt_taken(host, card, next).await? {
                    next += 1;
                }
                next += 1;
                next - 1
            }
        };
        // A new tree starts at the card's committed work; one this race made keeps its own.
        let tree = worktree::attempt_tree(host, card, attempt).await?;
        let first_slot = host.read(|state| sessions_before(state, card, worker, at, log.since));
        let packet = own_copy(packet, card_tree.as_deref(), &tree.path).await;
        entrants.push(Entrant {
            worker: worker.clone(),
            tree,
            first_slot,
            packet,
        });
    }
    Ok(entrants)
}

/// The packet naming a copy of the transcript in the attempt's own tree, since a session reads
/// no worktree but its own. The git exclude that keeps the card's copy out of commits covers
/// every worktree of the repository. Without a copy, it still names the card's.
async fn own_copy(packet: &HandoffPacket, card_tree: Option<&Path>, tree: &Path) -> HandoffPacket {
    let mut packet = packet.clone();
    let Some(paths) = packet.transcript.as_mut() else {
        return packet;
    };
    let Some(inside) = card_tree.and_then(|card_tree| paths.worktree.strip_prefix(card_tree).ok())
    else {
        return packet;
    };
    let own = tree.join(inside);
    let copied = async {
        if let Some(dir) = own.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        tokio::fs::copy(&paths.worktree, &own).await
    };
    match copied.await {
        Ok(_) => paths.worktree = own,
        Err(error) => tracing::warn!(
            path = %own.display(),
            %error,
            "a race attempt is told of the card's transcript, not a copy in its own tree"
        ),
    }
    packet
}

/// Reads the card's attempts from the log: every attempt tree a session worked in and every
/// attempt branch a commit went to.
fn race_log(host: &Host, card: &CardId) -> EngineResult<RaceLog> {
    let events = card_events(host, card)?;
    let run = host.run_id().clone();
    // The card's commits are in the state too, for a run that keeps no log on disk.
    let committed = host.read(|state| {
        state.cards.get(card).map_or(0, |record| {
            record
                .commits
                .iter()
                .filter_map(|commit| worktree::attempt_of_branch(&run, card, &commit.branch))
                .max()
                .unwrap_or(0)
        })
    });
    let ours = |event: &&RunEvent| event.card_id.as_ref() == Some(card);
    let since = events
        .iter()
        .filter(ours)
        .filter(|event| matches!(event.kind, RunEventKind::Card(CardEvent::State { .. })))
        .map(|event| event.seq)
        .max();
    let mut log = RaceLog {
        since,
        highest: committed,
        started: BTreeMap::new(),
    };
    for event in events.iter().filter(ours) {
        let (attempt, tree) = match &event.kind {
            RunEventKind::Card(CardEvent::Committed(commit)) => (
                worktree::attempt_of_branch(&run, card, &commit.branch),
                false,
            ),
            RunEventKind::Session(SessionEvent::Input(input)) => {
                (worktree::attempt_of_tree(card, &input.launch.cwd), true)
            }
            _ => (None, false),
        };
        let Some(attempt) = attempt else {
            continue;
        };
        log.highest = log.highest.max(attempt);
        let this_race = since.is_some_and(|since| event.seq > since);
        if !(tree && this_race) {
            continue;
        }
        let worker = event.session.as_ref().and_then(|key| {
            host.read(|state| {
                state
                    .sessions
                    .get(key)
                    .map(|session| session.started.worker.clone())
            })
        });
        if let Some(worker) = worker {
            log.started.entry(worker).or_insert(attempt);
        }
    }
    Ok(log)
}

/// How many owner sessions `worker` had on the card in this step's round before the race,
/// which is the slot of its attempt's first session: replay finds a session by its place
/// among the worker's sessions of the same card, role, step, and round.
fn sessions_before(
    state: &RunState,
    card: &CardId,
    worker: &WorkerRef,
    at: &StepAt,
    since: Option<u64>,
) -> u32 {
    let round_start = state
        .workflows
        .by_id
        .get(&at.step.workflow)
        .and_then(|workflow| {
            workflow
                .steps
                .iter()
                .filter(|progress| progress.step == at.step && progress.round == at.round)
                .map(|progress| progress.since)
                .min()
        })
        .unwrap_or(0);
    let before = owner_sessions(state, card, worker)
        .filter(|session| {
            session.started.workflow_step.as_ref() == Some(&at.step)
                && session.since >= round_start
                && since.is_none_or(|since| session.since < since)
        })
        .count();
    u32::try_from(before).unwrap_or(u32::MAX)
}

fn owner_sessions<'a>(
    state: &'a RunState,
    card: &'a CardId,
    worker: &'a WorkerRef,
) -> impl Iterator<Item = &'a SessionRecord> {
    state.sessions.by_key.values().filter(move |session| {
        session.card.as_ref() == Some(card)
            && session.started.worker == *worker
            && session.started.role == RoleName::Owner
            && session.started.step == Some(StepName::Build)
    })
}

/// The first review round of a race: above every round the card's own review can reach, so
/// that review never takes an attempt's verdict for its own.
fn first_round(host: &Host, card: &CardId) -> EngineResult<u32> {
    let record = rules::record(host, card)?;
    let above_own = host.settings().review.max_change_rounds + 2;
    Ok(upgrade::next_round(&record.reviews).max(above_own))
}

/// Moves the card's branch to the winning attempt, which started from the card's head.
async fn take_the_winners_work(
    host: &Host,
    card: &CardId,
    tree: &CardTree,
    start: Option<&str>,
) -> EngineResult<()> {
    let git = host.git();
    let record = rules::record(host, card)?;
    let head = git.resolve(&tree.path, "HEAD").await?;
    match (&record.worktree, &record.branch) {
        (Some(path), _) => {
            if !git.fast_forward(path, &head).await? {
                git.reset_hard(path, &head).await?;
            }
            // The losers' commits came last, so the card's saved diff is the winner's again.
            let own = worktree::ensure(host, card).await?;
            let own_git = worktree::tree_git(host, &own.path).await?;
            worktree::save_diff(host, card, &own_git, &own, &head).await?;
        }
        (None, Some(branch)) => {
            let root = host.repo().root.clone();
            let old = match start {
                Some(start) => start.to_owned(),
                None => git.resolve(&root, branch).await?,
            };
            git.update_branch(&root, branch, &head, &old).await?;
        }
        (None, None) => {}
    }
    Ok(())
}
