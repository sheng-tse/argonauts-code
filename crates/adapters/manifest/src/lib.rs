//! A `[[harness]]` entry in `models.toml` becomes an adapter, for any agent that prints one
//! normalized `AgentEvent` JSON object per line and reads its prompt on stdin. Until it's built,
//! the adapter describes the harness and refuses to build a command.

use std::sync::Arc;

use argonauts_core::registry::HarnessManifest;
use argonauts_core::{
    Adapter, AgentEvent, CommandSpec, SessionRequest, StopPlan, StreamParser, extract_version,
};

pub fn manifest_adapter(manifest: HarnessManifest) -> Arc<dyn Adapter> {
    manifest_adapter_with(manifest, &[])
}

/// Like [`manifest_adapter`], and with `output = "parser"` it reuses the parser of the named
/// adapter in `builtins`. This crate can't depend on the adapter crates, so the app passes them.
pub fn manifest_adapter_with(
    manifest: HarnessManifest,
    builtins: &[Arc<dyn Adapter>],
) -> Arc<dyn Adapter> {
    let _ = builtins;
    Arc::new(ManifestAdapter::new(manifest))
}

/// `Adapter` hands out `&'static str`s but a manifest is read at run time, so its names are
/// leaked once. Manifests are loaded once per process.
struct ManifestAdapter {
    id: &'static str,
    display_name: &'static str,
    binary: &'static str,
    family: &'static str,
    version_args: &'static [&'static str],
    manifest: HarnessManifest,
}

fn leak(text: &str) -> &'static str {
    Box::leak(text.to_owned().into_boxed_str())
}

impl ManifestAdapter {
    fn new(manifest: HarnessManifest) -> Self {
        let version_args: Vec<&'static str> =
            manifest.version_args.iter().map(|arg| leak(arg)).collect();
        Self {
            id: leak(&manifest.id),
            display_name: leak(&manifest.display_name),
            binary: leak(&manifest.binary),
            family: leak(&manifest.family),
            version_args: Box::leak(version_args.into_boxed_slice()),
            manifest,
        }
    }
}

impl Adapter for ManifestAdapter {
    fn id(&self) -> &'static str {
        self.id
    }

    fn display_name(&self) -> &'static str {
        self.display_name
    }

    fn binary(&self) -> &'static str {
        self.binary
    }

    fn command(&self, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
        let _ = req;
        anyhow::bail!(
            "the manifest adapter for {} is not implemented yet",
            self.manifest.id
        )
    }

    fn parser(&self) -> Box<dyn StreamParser> {
        Box::new(KeepEverything)
    }

    fn version_args(&self) -> &'static [&'static str] {
        self.version_args
    }

    fn parse_version(&self, output: &str) -> Option<String> {
        extract_version(output)
    }

    /// A manifest's tested versions are run-time strings; none count until the adapter is built.
    fn tested_versions(&self) -> &'static [&'static str] {
        &[]
    }

    /// A manifest names no model, so there is no cheapest one.
    fn cheapest_model(&self) -> (&'static str, Option<&'static str>) {
        ("<model>", None)
    }

    fn stop_plan(&self) -> StopPlan {
        StopPlan::default()
    }

    fn default_family(&self) -> &'static str {
        self.family
    }
}

struct KeepEverything;

impl StreamParser for KeepEverything {
    fn parse_stdout_line(&mut self, line: &str) -> Vec<AgentEvent> {
        vec![AgentEvent::unknown_line(line)]
    }

    fn parse_stderr_line(&mut self, line: &str) -> Vec<AgentEvent> {
        vec![AgentEvent::unknown_line(line)]
    }

    fn finish(&mut self, exit_code: Option<i32>) -> Vec<AgentEvent> {
        let _ = exit_code;
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> HarnessManifest {
        HarnessManifest {
            id: "my-agent".into(),
            display_name: "My Agent".into(),
            binary: "my-agent".into(),
            version_args: vec!["--version".into()],
            tested_versions: vec!["1.0.0".into()],
            read_only_args: vec!["--read-only".into(), "--model".into(), "{model}".into()],
            build_args: vec!["--model".into(), "{model}".into()],
            resume_args: Vec::new(),
            family: "mine".into(),
            env: Vec::new(),
            ..HarnessManifest::default()
        }
    }

    #[test]
    fn the_placeholder_describes_the_harness_and_refuses_commands() {
        let adapter = manifest_adapter(manifest());
        assert_eq!(adapter.id(), "my-agent");
        assert_eq!(adapter.display_name(), "My Agent");
        assert_eq!(adapter.binary(), "my-agent");
        assert_eq!(adapter.version_args(), ["--version"]);
        assert_eq!(adapter.default_family(), "mine");
        assert!(adapter.command(&SessionRequest::default()).is_err());
    }

    #[test]
    fn the_placeholder_with_builtins_still_refuses_commands() {
        let builtin = manifest_adapter(HarnessManifest {
            id: "builtin".into(),
            ..HarnessManifest::default()
        });
        let adapter = manifest_adapter_with(manifest(), &[builtin]);
        assert_eq!(adapter.id(), "my-agent");
        let err = adapter.command(&SessionRequest::default()).unwrap_err();
        assert!(err.to_string().contains("not implemented yet"), "{err}");
    }
}
