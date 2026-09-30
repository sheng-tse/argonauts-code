//! Antigravity CLI (`agy`) adapter for Argonauts.

mod command;
mod errors;
mod parser;

use argonauts_core::{
    Adapter, Capabilities, CommandSpec, SessionRequest, StopPlan, StreamParser, extract_version,
};

pub use command::{API_KEY_ENV, AUTO_UPDATE_ENV, AUTO_UPDATE_VALUE, build_command};
pub use parser::AgyStreamParser;

pub const ADAPTER_ID: &str = "antigravity-cli";

pub const CHEAPEST_MODEL: &str = "gemini-3.6-flash-low";

pub const CHEAPEST_EFFORT: &str = "low";

pub const TESTED_VERSIONS: &[&str] = &["1.2.13"];

#[derive(Debug, Clone, Copy, Default)]
pub struct AntigravityCli;

impl AntigravityCli {
    pub fn new() -> Self {
        Self
    }
}

impl Adapter for AntigravityCli {
    fn id(&self) -> &'static str {
        ADAPTER_ID
    }

    fn display_name(&self) -> &'static str {
        "Antigravity CLI"
    }

    fn binary(&self) -> &'static str {
        "agy"
    }

    fn command(&self, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
        build_command(self.binary(), req)
    }

    fn parser(&self) -> Box<dyn StreamParser> {
        Box::new(AgyStreamParser::new())
    }

    fn version_args(&self) -> &'static [&'static str] {
        &["--version"]
    }

    fn parse_version(&self, output: &str) -> Option<String> {
        extract_version(output)
    }

    fn tested_versions(&self) -> &'static [&'static str] {
        TESTED_VERSIONS
    }

    fn cheapest_model(&self) -> (&'static str, Option<&'static str>) {
        (CHEAPEST_MODEL, Some(CHEAPEST_EFFORT))
    }

    /// agy ends a headless turn on SIGINT with status `INTERRUPTED`, then stops its background tasks.
    fn stop_plan(&self) -> StopPlan {
        StopPlan::default()
    }

    fn capabilities(&self) -> Capabilities {
        // Plan mode is advisory in headless runs, and --sandbox still allows writes in the workdir.
        Capabilities {
            enforces_read_only: false,
            ..Capabilities::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::{KNOWN_AGENT_IDS, SessionRole, StopSignal};

    #[test]
    fn identity_matches_the_contract() {
        let adapter = AntigravityCli::new();
        assert_eq!(adapter.id(), "antigravity-cli");
        assert!(KNOWN_AGENT_IDS.contains(&adapter.id()));
        assert_eq!(adapter.binary(), "agy");
        assert_eq!(adapter.display_name(), "Antigravity CLI");
    }

    #[test]
    fn cheapest_model_is_flash_low() {
        assert_eq!(
            AntigravityCli.cheapest_model(),
            ("gemini-3.6-flash-low", Some("low"))
        );
    }

    #[test]
    fn version_comes_from_the_bare_number() {
        let adapter = AntigravityCli;
        assert_eq!(adapter.version_args(), ["--version"]);
        assert_eq!(adapter.parse_version("1.2.13\n").as_deref(), Some("1.2.13"));
        assert_eq!(adapter.parse_version("garbage"), None);
        assert!(adapter.tested_versions().contains(&"1.2.13"));
    }

    #[test]
    fn stop_plan_starts_with_an_interrupt() {
        let plan = AntigravityCli.stop_plan();
        assert_eq!(plan.steps[0].signal, StopSignal::Interrupt);
        assert_eq!(plan.steps.last().map(|s| s.signal), Some(StopSignal::Kill));
    }

    #[test]
    fn wrap_up_falls_back_to_an_interrupt() {
        assert!(AntigravityCli.wrap_up_input("wrap up").is_none());
    }

    #[test]
    fn read_only_is_not_enforced() {
        let caps = AntigravityCli.capabilities();
        assert!(!caps.enforces_read_only);
        assert!(caps.live_usage && caps.reports_model);
    }

    #[test]
    fn command_goes_through_the_adapter() {
        let req = SessionRequest {
            prompt: "hi".into(),
            role: SessionRole::Build,
            ..SessionRequest::default()
        };
        let spec = AntigravityCli.command(&req).unwrap();
        assert_eq!(spec.program, "agy");
        let mut parser = AntigravityCli.parser();
        assert_eq!(parser.parse_stdout_line("nope").len(), 1);
    }
}
