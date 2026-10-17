//! The usage panel: plan usage, spend, tokens, history and workflows as cards, on the Usage tab
//! and pinned above the input, with the run's status and the sessions' context on tabs of
//! their own.

#[cfg(test)]
mod bar_gap_tests;
pub mod cards;
pub mod config;
mod context;
#[cfg(test)]
pub(crate) mod golden;
mod health;
mod home;
mod limits;
pub mod periods;
mod plan_windows;
#[cfg(test)]
pub(crate) mod sample;
pub mod shades;
mod spend;
pub mod stats;
mod status;
pub mod status_tab;
mod subtabs;
mod usage;
mod view;
mod watch;

pub use cards::{UsageCard, UsageCards, cli_role, period_words};
pub use context::{
    ContextPrinted, conversation as conversation_context, session_use as session_context,
};
pub use home::spoken as spoken_overview;
pub use status::{StatusCard, StatusCards};
pub use view::{OverviewView, Tab};
pub use watch::UsageWatch;
