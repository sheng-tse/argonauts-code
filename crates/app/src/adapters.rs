use std::sync::Arc;

use argonauts_adapter_antigravity_cli::AntigravityCli;
use argonauts_adapter_claude_code::ClaudeCodeAdapter;
use argonauts_adapter_codex::CodexAdapter;
use argonauts_adapter_opencode::OpenCodeAdapter;
use argonauts_core::Adapter;

/// Claude Code, Codex, Antigravity CLI, and OpenCode, in the order the setup lists them.
pub fn builtin_adapters() -> Vec<Arc<dyn Adapter>> {
    vec![
        Arc::new(ClaudeCodeAdapter::new()),
        Arc::new(CodexAdapter::new()),
        Arc::new(AntigravityCli::new()),
        Arc::new(OpenCodeAdapter::new()),
    ]
}
