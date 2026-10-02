//! Built from a run's events by code, at no token cost: the workflow, agent, and context views,
//! `/rewind`, and the condensed replay a resumed run shows.

mod agent;
mod checkpoints;
mod context;
mod replay;
mod sessions;
mod workflow;

pub(crate) use agent::agent_view;
pub(crate) use checkpoints::checkpoint_rows;
pub(crate) use context::context_rows;
pub(crate) use replay::condensed_replay;
pub(crate) use sessions::add_spend;
pub(crate) use workflow::workflow_view;
