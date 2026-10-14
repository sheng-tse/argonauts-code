//! What `resolve` does between reading the layers and returning: the workflow layer, the
//! top-level `[fallback]` merged into each role, and the checks serde can't make.

pub(crate) mod autopilot;
pub(crate) mod crew;
pub(crate) mod fallback;
pub(crate) mod limit_units;
pub(crate) mod passing_over;
pub(crate) mod plain;
pub(crate) mod roles;
pub(crate) mod set_by;
pub mod tighten;
pub(crate) mod validate;
pub(crate) mod workflow_layer;

#[cfg(test)]
mod tests;
