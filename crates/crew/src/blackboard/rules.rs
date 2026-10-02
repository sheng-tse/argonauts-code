//! Who decides a change request, by the paths it touches: local and reversible, the owner of
//! that piece; cross-cutting, the affected owners by majority, the phase lead breaking a tie
//! only when it isn't a party; irreversible or goal-changing, you. A request that moves a card
//! into a protected path sends the run back to the plan checkpoint.

use argonauts_core::CardId;
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::Settings;
use argonauts_core::steps::card::ChangeRequestOut;

use crate::blackboard::refs;
use crate::blackboard::route::still_working;
use crate::state::RunState;
use crate::state::cards::CardRecord;

/// Agents never write here, on top of `limits.protected_paths`.
pub const CONDUCTOR_PATHS: &str = ".argonauts/**";

/// The approved goal files. A request touching one asks to change the goal.
pub const GOAL_PATHS: &str = ".argonauts/goal/**";

/// Changes nobody can take back once they run elsewhere: data migrations and release or
/// deployment config, each with the outside action it leads to, as
/// `limits.irreversible_allowlist` names them.
pub const IRREVERSIBLE_PATHS: &[(&str, &str)] = &[
    ("**/migrations/**", "migrate"),
    ("**/migrate/**", "migrate"),
    ("**/deploy/**", "deploy"),
    ("**/release/**", "release"),
    (".github/workflows/release*", "release"),
    (".github/workflows/deploy*", "deploy"),
];

/// The outside actions a change to `paths` leads to, each once, in the table's order.
pub fn irreversible_actions(paths: &[String]) -> Vec<String> {
    let mut actions: Vec<String> = Vec::new();
    for (pattern, action) in IRREVERSIBLE_PATHS {
        let touched = paths.iter().any(|path| refs::under(pattern, path));
        if touched && !actions.iter().any(|seen| seen == action) {
            actions.push((*action).to_owned());
        }
    }
    actions
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decider {
    Owner(WorkerRef),
    /// Each affected owner posts one `Decision` in their next report.
    Owners(Vec<WorkerRef>),
    /// Irreversible, or nobody's piece.
    User,
    /// It changes the goal: `approvals.goal_changes` says what happens.
    Goal,
    /// Back to the plan checkpoint.
    Replan,
}

pub fn decider(state: &RunState, settings: &Settings, request: &ChangeRequestOut) -> Decider {
    decider_for(state, settings, None, request)
}

/// The decider, leaving out the requesting card: it's a party, never a voter on its own request.
pub fn decider_for(
    state: &RunState,
    settings: &Settings,
    requester: Option<&CardId>,
    request: &ChangeRequestOut,
) -> Decider {
    let touches = |patterns: &[&str]| {
        request
            .paths
            .iter()
            .any(|path| patterns.iter().any(|pattern| refs::under(pattern, path)))
    };
    if touches(&[GOAL_PATHS]) {
        return Decider::Goal;
    }
    let protected: Vec<&str> = settings
        .limits
        .protected_paths
        .iter()
        .map(String::as_str)
        .chain([CONDUCTOR_PATHS])
        .collect();
    if touches(&protected) {
        return Decider::Replan;
    }
    if !irreversible_actions(&request.paths).is_empty() {
        return Decider::User;
    }

    // Owners of the other cards still working that own or read a touched path, owners first.
    let affected = state.cards.by_id.values().filter(|record| {
        Some(&record.spec.id) != requester
            && still_working(record)
            && (any_under(&record.spec.owned_paths, &request.paths)
                || any_under(&record.spec.read_only_paths, &request.paths))
    });
    let (owning, reading): (Vec<&CardRecord>, Vec<&CardRecord>) =
        affected.partition(|record| any_under(&record.spec.owned_paths, &request.paths));
    let mut owners: Vec<WorkerRef> = Vec::new();
    for record in owning.iter().chain(&reading) {
        if let Some(owner) = owner_of(record)
            && !owners.contains(&owner)
        {
            owners.push(owner);
        }
    }
    // One owner decides alone whether the change is local or read by its other cards; nobody's
    // piece is yours.
    match owners.len() {
        0 => Decider::User,
        1 => Decider::Owner(owners.remove(0)),
        _ => Decider::Owners(owners),
    }
}

/// Cards that depend on the paths the request touches: they own or read one, or depend, at any
/// distance, on a card that owns one. They move to `Blocked` first.
pub fn dependents(state: &RunState, request: &ChangeRequestOut) -> Vec<CardId> {
    let cards = &state.cards.by_id;
    let mut found: Vec<CardId> = cards
        .values()
        .filter(|record| {
            any_under(&record.spec.owned_paths, &request.paths)
                || any_under(&record.spec.read_only_paths, &request.paths)
        })
        .map(|record| record.spec.id.clone())
        .collect();
    loop {
        let before = found.len();
        for record in cards.values() {
            if !found.contains(&record.spec.id)
                && record
                    .spec
                    .depends_on
                    .iter()
                    .any(|card| found.contains(card))
            {
                found.push(record.spec.id.clone());
            }
        }
        if found.len() == before {
            break;
        }
    }
    found.retain(|card| cards.get(card).is_some_and(still_working));
    found.sort();
    found
}

/// The outcome of the owners' votes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tally {
    Accepted,
    Rejected,
    /// A tie the phase lead isn't a party to: its one vote decides.
    LeadBreaks(WorkerRef),
    /// A tie the phase lead can't break because it's a party, or there's no lead.
    YouDecide,
}

/// `votes` are `(owner, accepts)`; `lead` breaks a tie only when it didn't vote.
pub fn tally(votes: &[(WorkerRef, bool)], lead: Option<&WorkerRef>) -> Tally {
    let accepts = votes.iter().filter(|(_, accepts)| *accepts).count();
    let rejects = votes.len() - accepts;
    if accepts > rejects {
        return Tally::Accepted;
    }
    if rejects > accepts {
        return Tally::Rejected;
    }
    match lead {
        Some(lead) if !votes.iter().any(|(owner, _)| owner == lead) => {
            Tally::LeadBreaks(lead.clone())
        }
        _ => Tally::YouDecide,
    }
}

fn owner_of(record: &CardRecord) -> Option<WorkerRef> {
    record.owner.clone().or_else(|| record.spec.owner.clone())
}

fn any_under(patterns: &[String], paths: &[String]) -> bool {
    paths
        .iter()
        .any(|path| patterns.iter().any(|pattern| refs::under(pattern, path)))
}
