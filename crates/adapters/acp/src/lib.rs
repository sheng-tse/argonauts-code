//! The generic ACP worker: any agent that speaks the Agent Client Protocol joins the crew
//! through a profile of data. The harness worker spawns and sandboxes the agent, then hands its
//! pipes to this crate's session driver. Until the driver is built, it ends every session at
//! once with a "not built yet" error.

use std::sync::Arc;

use argonauts_core::adapter::Transport;
use argonauts_core::driver::{DriverEnd, DriverIo, DriverStart, DriverStop, SessionDriver};
use argonauts_core::future::BoxFuture;
use argonauts_core::registry::HarnessManifest;
use argonauts_core::{
    Adapter, AgentEvent, Capabilities, CommandSpec, ErrorCategory, SessionRequest, StopPlan,
    StreamParser, extract_version,
};

const NOT_BUILT: &str = "the ACP worker is not built yet";

/// The adapter for a `[[harness]]` entry with `transport = "acp"`.
pub fn acp_adapter(manifest: HarnessManifest) -> Arc<dyn Adapter> {
    Arc::new(AcpAdapter::new(manifest))
}

/// `Adapter` hands out `&'static str`s but a profile is read at run time, so its names are
/// leaked once. Profiles are loaded once per process.
struct AcpAdapter {
    id: &'static str,
    display_name: &'static str,
    binary: &'static str,
    family: &'static str,
    version_args: &'static [&'static str],
}

fn leak(text: &str) -> &'static str {
    Box::leak(text.to_owned().into_boxed_str())
}

impl AcpAdapter {
    fn new(manifest: HarnessManifest) -> Self {
        let version_args: Vec<&'static str> =
            manifest.version_args.iter().map(|arg| leak(arg)).collect();
        Self {
            id: leak(&manifest.id),
            display_name: leak(&manifest.display_name),
            binary: leak(&manifest.binary),
            family: leak(&manifest.family),
            version_args: Box::leak(version_args.into_boxed_slice()),
        }
    }
}

impl Adapter for AcpAdapter {
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
        anyhow::bail!("{} ({NOT_BUILT})", self.id)
    }

    /// Never used: the driver reads the pipes.
    fn parser(&self) -> Box<dyn StreamParser> {
        Box::new(KeepEverything)
    }

    fn version_args(&self) -> &'static [&'static str] {
        self.version_args
    }

    fn parse_version(&self, output: &str) -> Option<String> {
        extract_version(output)
    }

    /// A profile's tested versions are run-time strings; none count until the worker is built.
    fn tested_versions(&self) -> &'static [&'static str] {
        &[]
    }

    /// A profile names no model, so there is no cheapest one.
    fn cheapest_model(&self) -> (&'static str, Option<&'static str>) {
        ("<model>", None)
    }

    fn stop_plan(&self) -> StopPlan {
        StopPlan::default()
    }

    /// Read-only sessions lean on the sandbox and the empty-diff check.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            enforces_read_only: false,
            ..Capabilities::default()
        }
    }

    fn default_family(&self) -> &'static str {
        self.family
    }

    fn transport(&self) -> Transport {
        Transport::Acp
    }

    fn session_driver(&self) -> Option<Arc<dyn SessionDriver>> {
        Some(Arc::new(NotBuilt))
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

struct NotBuilt;

impl SessionDriver for NotBuilt {
    fn run(&self, io: DriverIo, start: DriverStart) -> BoxFuture<'static, DriverEnd> {
        drop((io, start));
        Box::pin(async {
            DriverEnd {
                vendor_session: None,
                stop: DriverStop::Failed {
                    category: ErrorCategory::Cli,
                    message: NOT_BUILT.to_owned(),
                },
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> HarnessManifest {
        HarnessManifest {
            id: "hermes".into(),
            display_name: "Hermes Agent".into(),
            binary: "/opt/hermes/bin/hermes".into(),
            read_only_args: vec!["acp".into()],
            build_args: vec!["acp".into()],
            family: "from-model".into(),
            transport: Transport::Acp,
            ..HarnessManifest::default()
        }
    }

    #[test]
    fn the_placeholder_describes_the_agent_and_says_it_is_not_built() {
        let adapter = acp_adapter(profile());
        assert_eq!(adapter.id(), "hermes");
        assert_eq!(adapter.transport(), Transport::Acp);
        assert!(!adapter.capabilities().enforces_read_only);
        assert!(adapter.session_driver().is_some());
        let err = adapter.command(&SessionRequest::default()).unwrap_err();
        assert!(err.to_string().contains("not built yet"), "{err}");
    }

    #[tokio::test]
    async fn the_placeholder_driver_ends_the_session_with_an_error() {
        use tokio::sync::mpsc;

        let driver = acp_adapter(profile()).session_driver().unwrap();
        let (events, _) = mpsc::unbounded_channel();
        let (frames, _) = mpsc::unbounded_channel();
        let (_, stderr_lines) = mpsc::channel(1);
        let (_, control) = mpsc::channel(1);
        let io = DriverIo {
            stdin: Box::new(tokio::io::sink()),
            stdout: Box::new(tokio::io::empty()),
            stderr_lines,
            events,
            control,
            frames,
        };
        let start = DriverStart {
            cwd: "/tmp".into(),
            prompt: "hi".into(),
            model: None,
            effort: None,
            access: argonauts_core::worker::Access::ReadOnly,
            resume: None,
            mcp_servers: Vec::new(),
            scope: Default::default(),
            json_schema: None,
            report_file: None,
        };
        let end = driver.run(io, start).await;
        assert!(matches!(
            end.stop,
            DriverStop::Failed { category: ErrorCategory::Cli, ref message } if message.contains("not built yet")
        ));
    }
}
