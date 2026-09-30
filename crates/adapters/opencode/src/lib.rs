//! The OpenCode adapter: `opencode run --format json`, with open-weight models through
//! OpenCode's providers.

use argonauts_core::{
    Adapter, AgentEvent, CommandSpec, SessionRequest, StopPlan, StreamParser, extract_version,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct OpenCodeAdapter;

impl OpenCodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Adapter for OpenCodeAdapter {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn display_name(&self) -> &'static str {
        "OpenCode"
    }

    fn binary(&self) -> &'static str {
        "opencode"
    }

    fn command(&self, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
        let _ = req;
        anyhow::bail!("the OpenCode adapter is not implemented yet")
    }

    fn parser(&self) -> Box<dyn StreamParser> {
        Box::new(KeepEverything)
    }

    fn version_args(&self) -> &'static [&'static str] {
        &["--version"]
    }

    fn parse_version(&self, output: &str) -> Option<String> {
        extract_version(output)
    }

    /// No OpenCode version has recorded fixtures yet.
    fn tested_versions(&self) -> &'static [&'static str] {
        &[]
    }

    /// The live-test rules name no OpenCode model, so this stays a placeholder.
    fn cheapest_model(&self) -> (&'static str, Option<&'static str>) {
        ("<provider>/<model>", None)
    }

    fn stop_plan(&self) -> StopPlan {
        StopPlan::default()
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

    #[test]
    fn the_placeholder_refuses_to_build_commands_and_keeps_every_line() {
        let adapter = OpenCodeAdapter;
        assert_eq!(adapter.id(), "opencode");
        let err = adapter.command(&SessionRequest::default()).unwrap_err();
        assert!(err.to_string().contains("not implemented"));
        let events = adapter.parser().parse_stdout_line(r#"{"type":"step"}"#);
        assert!(matches!(events.as_slice(), [AgentEvent::Unknown { .. }]));
        assert_eq!(
            adapter.parse_version("opencode 1.2.3").as_deref(),
            Some("1.2.3")
        );
    }
}
