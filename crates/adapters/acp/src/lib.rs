//! The generic ACP worker: any agent that speaks the Agent Client Protocol joins the crew
//! through a profile of data. The CLI runner spawns and sandboxes the agent, then hands its
//! pipes to this crate's session driver, which speaks ACP v1 as newline-delimited JSON-RPC.

mod adapter;
mod driver;
mod errors;
mod hermes;
mod home;
mod mapping;
mod model;
mod permission;
mod report;
mod rpc;

#[cfg(test)]
mod replay;
#[cfg(test)]
mod testing;

pub use adapter::acp_adapter;
