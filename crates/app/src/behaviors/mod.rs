//! Behaviors: every action your agents took, labelled from a fixed list, placed on a map and
//! compared group against group, and on the deeper map in a model's own words.

pub(crate) mod actions;
mod command;
pub(crate) mod deep;
mod hodoscope;
pub(crate) mod labeller;
pub(crate) mod map;
pub(crate) mod records;
pub(crate) mod report;
pub(crate) mod turn_times;

pub use command::{BehaviorPeriod, behaviors_report, export_hodoscope};
