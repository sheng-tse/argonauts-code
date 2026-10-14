//! Built from a run's events by code, at no token cost: the workflow, agent, and context views,
//! `/rewind`, and the condensed replay a resumed run shows.

mod agent;
mod asked;
mod checkpoints;
mod context;
mod goal;
mod goal_feed;
mod replay;
pub(crate) mod required;
pub(crate) mod sessions;
mod verdicts;
mod workflow;

pub(crate) use agent::agent_view;
pub(crate) use checkpoints::checkpoint_rows;
pub(crate) use context::context_rows;
pub(crate) use goal::goal_view;
pub(crate) use replay::condensed_replay;
pub(crate) use sessions::{add_spend, crew_rows};
pub(crate) use workflow::{inner_agent_view, saved_workflow, workflow_view};
