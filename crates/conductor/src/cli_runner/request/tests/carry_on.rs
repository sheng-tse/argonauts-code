//! How a session carries on in its CLI's own screen is what its adapter builds: each CLI's own
//! command with the session's whole id, as the seat and check views print it.

use std::path::Path;

use argonauts_adapter_antigravity_cli::AntigravityCli;
use argonauts_adapter_claude_code::ClaudeCodeAdapter;
use argonauts_adapter_codex::CodexAdapter;
use argonauts_adapter_opencode::OpenCodeAdapter;
use argonauts_core::SessionRequest;
use argonauts_core::agents::adapter::Adapter;
use argonauts_core::base::names::ModelRef;
use argonauts_core::settings::Settings;

use crate::cli_runner::request::carry_on_command;

#[test]
fn each_cli_carries_a_session_on_by_its_adapters_own_command() {
    let id = "019a2b3c-4d5e-4f60-8a7b-9c0d1e2f3a4b";
    let folder = Path::new("/work/shop-api");
    let claude = ClaudeCodeAdapter;
    let codex = CodexAdapter::new();
    let opencode = OpenCodeAdapter::new();
    let adapters: [(&dyn Adapter, &str, [&str; 3]); 4] = [
        (&claude, "claude-opus-5-5", ["claude", "--resume", id]),
        (&codex, "gpt-6-luna", ["codex", "resume", id]),
        (
            &AntigravityCli,
            "gemini-3.8-flash",
            ["agy", "--conversation", id],
        ),
        (&opencode, "ollama/qwen3-coder:30b", ["opencode", "-s", id]),
    ];
    let settings = Settings::default();
    for (adapter, model, words) in adapters {
        let worker = ModelRef::new(adapter.id(), Some(model.to_owned()));
        let command = carry_on_command(&worker, id, folder, adapter, &settings, None)
            .unwrap_or_else(|| panic!("{} carries no session on", adapter.id()));
        assert_eq!(command.words, words, "{}", adapter.id());
        let reopened = adapter
            .resume_command(&SessionRequest::reopening(id, folder))
            .expect("the adapter's own command");
        assert_eq!(command.words, reopened.words, "{}", adapter.id());
    }
}
