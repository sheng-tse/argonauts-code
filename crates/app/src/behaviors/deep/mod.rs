//! Summaries: each action summarized by one model, the summaries embedded and
//! projected to two dimensions, so behaviors no label names show up too. Everything is kept in
//! `~/.argonauts/behaviors/` and never made twice; opening the map only reads it.

pub(crate) mod action;
pub(crate) mod choose;
pub(crate) mod command;
pub(crate) mod embed;
pub(crate) mod gather;
mod linalg;
pub(crate) mod offer;
pub(crate) mod places;
pub(crate) mod project;
pub(crate) mod prompt;
mod quadtree;
pub(crate) mod records;
pub(crate) mod service;
pub(crate) mod session;
pub(crate) mod store;
pub(crate) mod summaries;
pub(crate) mod summarize;
pub(crate) mod tab;
pub(crate) mod update;
