//! A conversation turn: its session, how its reply reads, the roles it asks, and its recap.

pub(crate) mod ask_role;
pub(super) mod captain_or_crew;
#[cfg(test)]
pub(super) mod fixture_replay;
pub(super) mod recap;
pub(super) mod seat_key;
pub(super) mod tries;
pub(crate) mod turn;
pub(crate) mod ultra;
