//! The OpenCode adapter: `opencode run --format json`, with open-weight models through
//! OpenCode's providers.

mod command;
mod parser;
mod provider;
mod providers;
mod user_config;

use argonauts_core::keywords::Keyword;
use argonauts_core::{
    Adapter, Capabilities, CommandSpec, SessionRequest, SetupHints, StopPlan, StreamParser,
    extract_version,
};

pub use provider::{LocalUrls, local_provider_config};

/// Words OpenCode treats specially in a prompt: none known. It wasn't installed to check its
/// program text, and nothing in this adapter's notes or fixtures names one.
pub const KEYWORDS: &[Keyword] = &[];

/// The versions the recordings in `fixtures/` were made with or also match, oldest first,
/// read from the `.meta.json` beside each recording when the crate is built. Files written
/// by hand stand for no version.
pub const TESTED_VERSIONS: &[&str] = include!(concat!(env!("OUT_DIR"), "/tested_versions.rs"));

#[derive(Debug, Clone, Default)]
pub struct OpenCodeAdapter {
    local_urls: LocalUrls,
}

impl OpenCodeAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    /// With your `[endpoints.ollama]` and `[endpoints.lmstudio]` base URLs, which reach
    /// OpenCode in the provider config it's sent for a local model.
    pub fn with_local_urls(local_urls: LocalUrls) -> Self {
        Self { local_urls }
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
        command::build(req, &self.local_urls)
    }

    fn parser(&self) -> Box<dyn StreamParser> {
        Box::new(parser::OpenCodeParser::default())
    }

    fn version_args(&self) -> &'static [&'static str] {
        &["--version"]
    }

    fn parse_version(&self, output: &str) -> Option<String> {
        extract_version(output)
    }

    fn keywords(&self) -> &'static [Keyword] {
        KEYWORDS
    }

    fn tested_versions(&self) -> &'static [&'static str] {
        TESTED_VERSIONS
    }

    /// The live-test rules name no OpenCode model, so this stays a placeholder.
    fn cheapest_model(&self) -> (&'static str, Option<&'static str>) {
        ("<provider>/<model>", None)
    }

    fn stop_plan(&self) -> StopPlan {
        StopPlan::default()
    }

    /// Usage comes with each finished step, with its cost, but no event names the model.
    /// OpenCode can't enforce read-only by flag: its default permissions mostly allow, and
    /// `--auto` approves anything not denied. A read-only session must run in a read-only
    /// worktree that Argonauts Code owns.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            live_usage: true,
            reports_cost: true,
            reports_model: false,
            resume_totals_include_earlier: false,
            enforces_read_only: false,
            input_joins_running_turn: false,
            ..Capabilities::default()
        }
    }

    /// `opencode upgrade` is OpenCode's own updater.
    fn update_args(&self) -> Option<&'static [&'static str]> {
        Some(&["upgrade"])
    }

    /// Its own installer keeps it under `~/.opencode/bin` and names no address for the newest
    /// version, so that is only asked of Homebrew or npm.
    fn install_sources(&self) -> argonauts_core::adapter::InstallSources {
        argonauts_core::adapter::InstallSources {
            homebrew: Some("opencode"),
            npm: Some("opencode-ai"),
            own: Some(argonauts_core::adapter::OwnInstaller {
                path_part: "/.opencode/bin/",
                newest_url: None,
            }),
        }
    }

    fn updater_off_env(&self) -> &'static [(&'static str, &'static str)] {
        &[(command::AUTO_UPDATE_ENV, "1")]
    }

    fn provider_checks(&self) -> &'static [&'static [&'static str]] {
        providers::CHECKS
    }

    fn providers(&self, outputs: &[String]) -> Option<argonauts_core::adapter::CliProviders> {
        providers::providers(outputs)
    }

    /// The npm command from OpenCode's README at v1.18.33, unverified since OpenCode isn't
    /// installed here. Providers log in inside OpenCode (`/connect`), and no check for that
    /// is verified, so none is claimed.
    fn setup_hints(&self) -> SetupHints {
        SetupHints {
            install: Some("npm i -g opencode-ai@latest".to_owned()),
            login: None,
            login_check: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_is_left_to_the_worktree() {
        let adapter = OpenCodeAdapter::new();
        assert_eq!(adapter.id(), "opencode");
        assert_eq!(adapter.binary(), "opencode");
        assert_eq!(adapter.version_args(), ["--version"]);
        assert!(!adapter.capabilities().enforces_read_only);
        let hints = adapter.setup_hints();
        assert!(hints.install.is_some());
        assert_eq!(hints.login_check, None);
        assert_eq!(adapter.cheapest_model(), ("<provider>/<model>", None));
        assert_eq!(adapter.input_line("more"), None);
        assert_eq!(
            adapter.parse_version("opencode 1.18.33").as_deref(),
            Some("1.18.33")
        );
    }
}
