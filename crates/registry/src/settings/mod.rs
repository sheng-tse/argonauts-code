//! What `resolve` does between reading the layers and returning: the preset layer, the
//! top-level `[handoff]` merged into each role, and the checks serde can't make.

pub(crate) mod budget_units;
pub(crate) mod handoff;
pub(crate) mod plain;
pub(crate) mod preset_layer;
pub(crate) mod roles;
pub(crate) mod set_by;
pub(crate) mod validate;

#[cfg(test)]
mod tests;
