//! `[[cli]]` entries: the built-in ACP profiles, then `~/.argonauts/models.toml`, then the
//! project's `.argonauts/models.toml` once you trust it. An entry names a program to run, so
//! every one is checked before it counts.

mod builtin;
mod load;
mod refusals;
mod turned_on;
mod validate;
mod wrappers;

#[cfg(test)]
mod tests;

use argonauts_core::registry::CliManifest;

pub(crate) use builtin::every_builtin_cli;
pub use builtin::{builtin_clis, builtin_home_template};
pub use load::cli_manifests;
pub(crate) use load::declared_cli_ids;
pub use turned_on::agents_turned_on;
pub(crate) use turned_on::{builtin_counts, enabled_agent};
pub use validate::{
    ManifestCheck, PARSERS, PLACEHOLDERS, check_builtin_manifest, check_manifest, placeholders_in,
};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CliEntries {
    pub manifests: Vec<CliManifest>,
    /// Entries left out and why, each naming its file, plus warnings about the ones kept.
    pub warnings: Vec<String>,
}
