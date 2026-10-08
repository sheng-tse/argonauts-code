//! What a harness session can do, from its adapter: steering through open input where the
//! adapter makes input lines, else interrupt and resume; how much of its subagents shows; the
//! status tool only where a per-run MCP config is verified (Claude Code) or the agent takes stdio
//! MCP servers (ACP).

use std::path::{Path, PathBuf};

use argonauts_core::worker::{SteerSupport, SubagentVisibility, WorkerCapabilities};
use argonauts_core::{Adapter, SessionRequest, SessionRole};

pub const CLAUDE_CODE: &str = "claude-code";
pub const CODEX: &str = "codex";
pub const ANTIGRAVITY: &str = "antigravity-cli";

pub fn capabilities_for(adapter: &dyn Adapter) -> WorkerCapabilities {
    let driven = adapter.session_driver().is_some();
    let id = adapter.id();
    WorkerCapabilities {
        // Codex exec sends whole messages only.
        streams_text: id != CODEX,
        steer: steer_support(adapter),
        resume: driven || resumes(adapter),
        structured_output: !driven && matches!(id, CLAUDE_CODE | CODEX),
        subagents: match id {
            CLAUDE_CODE => SubagentVisibility::Full,
            CODEX => SubagentVisibility::ThreadsOnly,
            ANTIGRAVITY => SubagentVisibility::Linked,
            _ => SubagentVisibility::None,
        },
        native_budget_cap: id == CLAUDE_CODE,
        takeover: adapter
            .interactive_resume("probe-session", Path::new("/"))
            .is_some(),
        can_build: builds(adapter),
        status_tool: id == CLAUDE_CODE || driven,
    }
}

/// Whether the main session's tools can be given to this CLI's sessions while Argonauts
/// drives it: only where the per-run MCP config that carries them is verified, which is Claude
/// Code. A CLI without them keeps the verdict at the end of each chat reply.
pub fn takes_session_tools(adapter: &dyn Adapter) -> bool {
    adapter.id() == CLAUDE_CODE
}

/// Whether the CLI has a place of its own for a few standing lines at the end of its
/// instructions. Claude Code takes them with `--append-system-prompt`.
pub fn takes_a_standing_note(adapter: &dyn Adapter) -> bool {
    adapter.id() == CLAUDE_CODE
}

/// Open input where the adapter makes input lines; otherwise interrupt and resume, which needs
/// a harness that can resume.
pub fn steer_support(adapter: &dyn Adapter) -> SteerSupport {
    if adapter.input_line("x").is_some() {
        SteerSupport::Input
    } else if adapter.session_driver().is_some() || resumes(adapter) {
        SteerSupport::InterruptResume
    } else {
        SteerSupport::NextTurnOnly
    }
}

fn probe_request() -> SessionRequest {
    SessionRequest {
        workdir: PathBuf::from("/probe/work"),
        prompt: "x".to_owned(),
        // What a real launch has, since adapters refuse one without: Codex a model, agy a
        // scratch directory for its agent file, and Claude Code one outside the worktree.
        model: Some("probe-model".to_owned()),
        scratch_dir: Some(PathBuf::from("/probe/scratch")),
        ..SessionRequest::default()
    }
}

/// `Adapter::command` is pure, so asking it for a resumed launch shows whether it can resume.
fn resumes(adapter: &dyn Adapter) -> bool {
    let fresh = adapter.command(&probe_request());
    let resumed = adapter.command(&SessionRequest {
        resume_session: Some("probe-session".to_owned()),
        ..probe_request()
    });
    match (fresh, resumed) {
        (Ok(fresh), Ok(resumed)) => fresh.args != resumed.args || fresh.stdin != resumed.stdin,
        _ => false,
    }
}

fn builds(adapter: &dyn Adapter) -> bool {
    adapter
        .command(&SessionRequest {
            role: SessionRole::Build,
            goal_hash: Some("probe".to_owned()),
            ..probe_request()
        })
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness_worker::testing::{FakeDriver, TestAdapter};

    #[test]
    fn steer_follows_the_input_line() {
        let open = TestAdapter::new("true").with_input_lines();
        assert_eq!(capabilities_for(&open).steer, SteerSupport::Input);
        let resumable = TestAdapter::new("true");
        assert_eq!(
            capabilities_for(&resumable).steer,
            SteerSupport::InterruptResume
        );
        assert!(capabilities_for(&resumable).resume);
        let one_shot = TestAdapter::new("true").without_resume();
        assert_eq!(
            capabilities_for(&one_shot).steer,
            SteerSupport::NextTurnOnly
        );
        assert!(!capabilities_for(&one_shot).resume);
    }

    #[test]
    fn vendors_resume_their_sessions() {
        let agy = capabilities_for(&argonauts_adapter_antigravity_cli::AntigravityCli);
        assert!(agy.resume);
        let claude = capabilities_for(&argonauts_adapter_claude_code::ClaudeCodeAdapter);
        assert!(claude.resume);
        let codex = capabilities_for(&argonauts_adapter_codex::CodexAdapter);
        assert!(codex.resume);
    }

    #[test]
    fn subagents_status_tool_and_caps_by_vendor() {
        let claude = capabilities_for(&TestAdapter::new("true").with_id(CLAUDE_CODE));
        assert_eq!(claude.subagents, SubagentVisibility::Full);
        assert!(claude.status_tool && claude.native_budget_cap && claude.streams_text);
        let codex = capabilities_for(&TestAdapter::new("true").with_id(CODEX));
        assert_eq!(codex.subagents, SubagentVisibility::ThreadsOnly);
        assert!(!codex.status_tool && !codex.native_budget_cap && !codex.streams_text);
        let agy = capabilities_for(&TestAdapter::new("true").with_id(ANTIGRAVITY));
        assert_eq!(agy.subagents, SubagentVisibility::Linked);
        assert!(!agy.status_tool);
        let other = capabilities_for(&TestAdapter::new("true"));
        assert_eq!(other.subagents, SubagentVisibility::None);
    }

    #[test]
    fn driven_agents_get_the_status_tool() {
        let adapter = TestAdapter::new("true").with_driver(FakeDriver::default());
        let caps = capabilities_for(&adapter);
        assert!(caps.status_tool && caps.resume && caps.streams_text);
        assert!(!caps.structured_output);
    }

    #[test]
    fn takeover_needs_an_interactive_resume() {
        assert!(!capabilities_for(&TestAdapter::new("true")).takeover);
        let adapter = TestAdapter::new("true").with_takeover();
        assert!(capabilities_for(&adapter).takeover);
    }

    #[test]
    fn can_build_unless_the_adapter_refuses_build_launches() {
        assert!(capabilities_for(&TestAdapter::new("true")).can_build);
        let adapter = TestAdapter::new("true").read_only();
        assert!(!capabilities_for(&adapter).can_build);
    }
}
