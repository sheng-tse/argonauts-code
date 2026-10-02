//! A `[[harness]]` entry in `models.toml` becomes an adapter, for a CLI that prints Argonauts'
//! own JSONL, a stream a built-in parser reads, one JSON document at exit, or final text.

mod adapter;
mod command;
mod home;
mod output;
mod placeholders;

use std::path::PathBuf;
use std::sync::Arc;

use argonauts_core::Adapter;
use argonauts_core::registry::HarnessManifest;

pub fn manifest_adapter(manifest: HarnessManifest) -> Arc<dyn Adapter> {
    manifest_adapter_with(manifest, &[])
}

/// Like [`manifest_adapter`], and with `output = "parser"` it reuses the parser of the named
/// adapter in `builtins`. This crate can't depend on the adapter crates, so the app passes them.
pub fn manifest_adapter_with(
    manifest: HarnessManifest,
    builtins: &[Arc<dyn Adapter>],
) -> Arc<dyn Adapter> {
    Arc::new(adapter::ManifestAdapter::new(manifest, builtins, None))
}

/// Like [`manifest_adapter_with`], with home templates read from `templates_root` instead of
/// `~/.argonauts/harnesses/`.
pub fn manifest_adapter_with_templates(
    manifest: HarnessManifest,
    builtins: &[Arc<dyn Adapter>],
    templates_root: PathBuf,
) -> Arc<dyn Adapter> {
    Arc::new(adapter::ManifestAdapter::new(
        manifest,
        builtins,
        Some(templates_root),
    ))
}
