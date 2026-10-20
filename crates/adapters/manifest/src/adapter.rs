//! The adapter a manifest describes.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use argonauts_core::registry::CliManifest;
use argonauts_core::registry::cli::OutputKind;
use argonauts_core::settings::Span;
use argonauts_core::{
    Adapter, Capabilities, CommandSpec, SessionRequest, StopPlan, StreamParser, extract_version,
};

/// `Adapter` hands out `&'static str`s but a manifest is read at run time, so its names are
/// leaked once. Manifests are loaded once per process.
pub(crate) struct ManifestAdapter {
    id: &'static str,
    display_name: &'static str,
    binary: &'static str,
    provider: &'static str,
    version_args: &'static [&'static str],
    tested_versions: &'static [&'static str],
    manifest: CliManifest,
    /// With `output = "parser"`, the built-in adapter whose parser this one reuses.
    reused: Option<Arc<dyn Adapter>>,
    /// Where home templates live. `None` is `~/.argonauts/clis/`, looked up at launch.
    templates_root: Option<PathBuf>,
}

/// What [`StopPlan::default`] waits after each signal.
const DEFAULT_WAIT: Duration = Duration::from_secs(5);

fn leak(text: &str) -> &'static str {
    Box::leak(text.to_owned().into_boxed_str())
}

fn leak_all(texts: &[String]) -> &'static [&'static str] {
    let leaked: Vec<&'static str> = texts.iter().map(|text| leak(text)).collect();
    Box::leak(leaked.into_boxed_slice())
}

impl ManifestAdapter {
    pub(crate) fn new(
        manifest: CliManifest,
        builtins: &[Arc<dyn Adapter>],
        templates_root: Option<PathBuf>,
    ) -> Self {
        let reused = match manifest.output {
            OutputKind::Parser => manifest.parser.as_deref().and_then(|name| {
                builtins
                    .iter()
                    .find(|builtin| builtin.id() == name)
                    .cloned()
            }),
            _ => None,
        };
        Self {
            id: leak(&manifest.id),
            display_name: leak(&manifest.display_name),
            binary: leak(&manifest.binary),
            provider: leak(&manifest.provider),
            version_args: leak_all(&manifest.version_args),
            tested_versions: leak_all(&manifest.tested_versions),
            manifest,
            reused,
            templates_root,
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
        if self.manifest.output == OutputKind::Parser && self.reused.is_none() {
            anyhow::bail!(
                "{} reuses the {} parser, which this build didn't pass in",
                self.id,
                self.manifest.parser.as_deref().unwrap_or("unnamed")
            );
        }
        crate::command::build(&self.manifest, req, self.templates_root.as_deref())
    }

    fn parser(&self) -> Box<dyn StreamParser> {
        crate::output::parser_for(&self.manifest, self.reused.as_ref())
    }

    fn version_args(&self) -> &'static [&'static str] {
        self.version_args
    }

    fn parse_version(&self, output: &str) -> Option<String> {
        extract_version(output)
    }

    fn tested_versions(&self) -> &'static [&'static str] {
        self.tested_versions
    }

    fn talks_to_own_server(&self, _req: &SessionRequest) -> bool {
        self.manifest.own_server
    }

    /// A manifest names no model, so there is no cheapest one.
    fn cheapest_model(&self) -> (&'static str, Option<&'static str>) {
        ("<model>", None)
    }

    fn stop_plan(&self) -> StopPlan {
        let Some(stop) = &self.manifest.stop else {
            return StopPlan::default();
        };
        // A wait that doesn't parse, or `none`, keeps the default's.
        let wait = |span: &Span| span.parse().ok().flatten().unwrap_or(DEFAULT_WAIT);
        StopPlan::stepped(wait(&stop.after_interrupt), wait(&stop.after_terminate))
    }

    /// Only a live stream reports usage as it goes, a document at exit says what its pointers
    /// reach, and a reused parser brings its adapter's flags.
    fn capabilities(&self) -> Capabilities {
        let pointers = self.manifest.final_json.as_ref();
        let reported = match self.manifest.output {
            OutputKind::ArgonautsJsonl => Capabilities::default(),
            OutputKind::Parser => self
                .reused
                .as_ref()
                .map_or_else(Capabilities::default, |reused| reused.capabilities()),
            OutputKind::FinalJson => Capabilities {
                live_usage: false,
                reports_cost: pointers.is_some_and(|p| p.cost_usd.is_some()),
                reports_model: pointers.is_some_and(|p| p.model.is_some()),
                ..Capabilities::default()
            },
            OutputKind::Text => Capabilities {
                live_usage: false,
                reports_model: false,
                ..Capabilities::default()
            },
        };
        // Nothing checks that a manifest's `read_only_args` block writes, and some CLIs have no
        // such flag, so read-only sessions need the read-only worktree Argonauts Code keeps.
        Capabilities {
            enforces_read_only: false,
            ..reported
        }
    }

    fn interactive_resume(&self, req: &SessionRequest) -> Option<CommandSpec> {
        // `command` refuses to launch a manifest whose take-over doesn't fill, so no session
        // of this CLI reaches here with an error to show.
        let session_id = req.resume_session.as_deref()?;
        crate::command::interactive_resume(&self.manifest, session_id, &req.workdir)
            .ok()
            .flatten()
    }

    fn default_provider(&self) -> &'static str {
        self.provider
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::registry::cli::FinalJsonPointers;

    /// A built-in whose capabilities are different from the default in every flag.
    struct Builtin;

    impl Adapter for Builtin {
        fn id(&self) -> &'static str {
            "builtin"
        }
        fn display_name(&self) -> &'static str {
            "Built-in"
        }
        fn binary(&self) -> &'static str {
            "builtin"
        }
        fn command(&self, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
            Ok(CommandSpec::new("builtin", &req.workdir))
        }
        fn parser(&self) -> Box<dyn StreamParser> {
            crate::output::parser_for(&CliManifest::default(), None)
        }
        fn version_args(&self) -> &'static [&'static str] {
            &[]
        }
        fn parse_version(&self, output: &str) -> Option<String> {
            extract_version(output)
        }
        fn tested_versions(&self) -> &'static [&'static str] {
            &[]
        }
        fn cheapest_model(&self) -> (&'static str, Option<&'static str>) {
            ("tiny", None)
        }
        fn stop_plan(&self) -> StopPlan {
            StopPlan::default()
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                live_usage: false,
                reports_cost: true,
                reports_model: false,
                resume_totals_include_earlier: false,
                enforces_read_only: true,
                input_joins_running_turn: false,
                ..Capabilities::default()
            }
        }
    }

    fn adapter(output: OutputKind) -> ManifestAdapter {
        let manifest = CliManifest {
            id: "cli".into(),
            read_only_args: vec!["--read-only".into()],
            output,
            parser: Some("builtin".into()),
            final_json: Some(FinalJsonPointers::default()),
            ..CliManifest::default()
        };
        let builtins: Vec<Arc<dyn Adapter>> = vec![Arc::new(Builtin)];
        ManifestAdapter::new(manifest, &builtins, None)
    }

    #[test]
    fn no_manifest_claims_to_enforce_read_only() {
        for output in [
            OutputKind::ArgonautsJsonl,
            OutputKind::Parser,
            OutputKind::FinalJson,
            OutputKind::Text,
        ] {
            let caps = adapter(output).capabilities();
            assert!(!caps.enforces_read_only, "{output:?}");
        }
    }

    #[test]
    fn a_reused_parser_brings_its_adapters_flags() {
        let caps = adapter(OutputKind::Parser).capabilities();
        assert_eq!(
            caps,
            Capabilities {
                enforces_read_only: false,
                ..Builtin.capabilities()
            }
        );
    }

    #[test]
    fn a_manifest_says_when_its_sessions_talk_to_a_server_of_their_own() {
        let request = SessionRequest::default();
        let plain = adapter(OutputKind::ArgonautsJsonl);
        assert!(!plain.talks_to_own_server(&request));
        let builtins: Vec<Arc<dyn Adapter>> = vec![Arc::new(Builtin)];
        let own = CliManifest {
            own_server: true,
            ..plain.manifest.clone()
        };
        assert!(ManifestAdapter::new(own, &builtins, None).talks_to_own_server(&request));
    }
}
