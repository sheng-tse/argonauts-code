//! A `[[cli]]` entry in `models.toml` becomes an adapter, for a CLI that prints Argonauts Code's
//! own JSONL, a stream a built-in parser reads, one JSON document at exit, or final text.

mod adapter;
mod command;
mod home;
mod output;
mod placeholders;

use std::path::PathBuf;
use std::sync::Arc;

use argonauts_core::Adapter;
use argonauts_core::registry::CliManifest;

pub fn manifest_adapter(manifest: CliManifest) -> Arc<dyn Adapter> {
    manifest_adapter_with(manifest, &[])
}

/// Like [`manifest_adapter`], and with `output = "parser"` it reuses the parser of the named
/// adapter in `builtins`. This crate can't depend on the adapter crates, so the app passes them.
pub fn manifest_adapter_with(
    manifest: CliManifest,
    builtins: &[Arc<dyn Adapter>],
) -> Arc<dyn Adapter> {
    Arc::new(adapter::ManifestAdapter::new(manifest, builtins, None))
}

/// Like [`manifest_adapter_with`], with home templates read from `templates_root` instead of
/// `~/.argonauts/clis/`.
pub fn manifest_adapter_with_templates(
    manifest: CliManifest,
    builtins: &[Arc<dyn Adapter>],
    templates_root: PathBuf,
) -> Arc<dyn Adapter> {
    Arc::new(adapter::ManifestAdapter::new(
        manifest,
        builtins,
        Some(templates_root),
    ))
}
