//! What a CLI session can do, from its adapter: steering through open input where the
//! adapter makes input lines, else interrupt and resume; how much of its subagents shows; the
//! status tool only where a per-run MCP config is verified (Claude Code) or the agent takes stdio
//! MCP servers (ACP).

use std::path::{Path, PathBuf};

use argonauts_core::agents::session_runner::{
    RunnerCapabilities, SteerSupport, SubagentVisibility,
};
use argonauts_core::{Adapter, SessionRequest, SessionRole};

pub const CLAUDE_CODE: &str = "claude-code";
pub const CODEX: &str = "codex";
pub const ANTIGRAVITY: &str = "antigravity-cli";

pub fn capabilities_for(adapter: &dyn Adapter) -> RunnerCapabilities {
    let driven = adapter.session_driver().is_some();
    let id = adapter.id();
    RunnerCapabilities {
        // Codex exec sends whole messages only.
        streams_text: id != CODEX,
        steer: steer_support(adapter),
        resume: driven || resumes(adapter),
        fork: !driven && forks(adapter),
        structured_output: !driven && matches!(id, CLAUDE_CODE | CODEX),
        subagents: match id {
            CLAUDE_CODE => SubagentVisibility::Full,
            CODEX => SubagentVisibility::ThreadsOnly,
            ANTIGRAVITY => SubagentVisibility::Linked,
            _ => SubagentVisibility::None,
        },
        native_limit_cap: id == CLAUDE_CODE,
        take_over: adapter
            .interactive_resume(&SessionRequest::reopening("probe-session", Path::new("/")))
            .is_some(),
        can_build: builds(adapter),
        status_tool: id == CLAUDE_CODE || driven,
    }
}

/// Whether the main session's tools can be given to this CLI's sessions while Argonauts Code
/// drives it: only where the per-run MCP config that carries them is verified, which is Claude
/// Code. A CLI without them keeps the verdict at the end of each conversation reply.
pub fn takes_session_tools(adapter: &dyn Adapter) -> bool {
    adapter.id() == CLAUDE_CODE
}

/// Whether the CLI has a place of its own for a few standing lines at the end of its
/// instructions. Claude Code takes them with `--append-system-prompt`.
pub fn takes_a_standing_note(adapter: &dyn Adapter) -> bool {
    adapter.id() == CLAUDE_CODE
}

/// Open input where the adapter makes input lines; otherwise interrupt and resume, which needs
/// a CLI that can resume.
pub fn steer_support(adapter: &dyn Adapter) -> SteerSupport {
    if adapter.input_line("x").is_some() {
        SteerSupport::Input
    } else if adapter.session_driver().is_some() || resumes(adapter) {
        SteerSupport::InterruptResume
    } else {
        SteerSupport::NextTurnOnly
    }
}

/// A launch the adapter accepts, to ask it what else it can do: with the CLI's own default
/// model, else with the model its live tests use, since a CLI may take a model only in a form
/// of its own (OpenCode's `provider/model`) and refuse an invented one. It also has what a real
/// launch has: agy a scratch directory for its agent file, and Claude Code one outside the
/// worktree.
fn probe_request(adapter: &dyn Adapter) -> SessionRequest {
    let with = |model: Option<&str>| SessionRequest {
        workdir: PathBuf::from("/probe/work"),
        prompt: "x".to_owned(),
        model: model.map(str::to_owned),
        scratch_dir: Some(PathBuf::from("/probe/scratch")),
        ..SessionRequest::default()
    };
    let default = with(None);
    if adapter.command(&default).is_ok() {
        return default;
    }
    with(Some(adapter.cheapest_model().0))
}

/// `Adapter::command` is pure, so asking it for a resumed launch shows whether it can resume.
fn resumes(adapter: &dyn Adapter) -> bool {
    let probe = probe_request(adapter);
    let fresh = adapter.command(&probe);
    let resumed = adapter.command(&SessionRequest {
        resume_session: Some("probe-session".to_owned()),
        ..probe
    });
    match (fresh, resumed) {
        (Ok(fresh), Ok(resumed)) => fresh.args != resumed.args || fresh.stdin != resumed.stdin,
        _ => false,
    }
}

/// Only an adapter that knows its CLI's fork gives a fork another command than a resume.
fn forks(adapter: &dyn Adapter) -> bool {
    let resumed = SessionRequest {
        resume_session: Some("probe-session".to_owned()),
        ..probe_request(adapter)
    };
    let forked = SessionRequest {
        fork_session: true,
        ..resumed.clone()
    };
    match (adapter.command(&resumed), adapter.command(&forked)) {
        (Ok(resumed), Ok(forked)) => resumed.args != forked.args,
        _ => false,
    }
}

fn builds(adapter: &dyn Adapter) -> bool {
    adapter
        .command(&SessionRequest {
            role: SessionRole::Build,
            goal_hash: Some("probe".to_owned()),
            ..probe_request(adapter)
        })
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_runner::testing::{FakeDriver, TestAdapter};

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

    /// Each built-in CLI resumes its sessions, builds, and opens its own screen to take one
    /// over, OpenCode included, whose models are named only as `provider/model`.
    #[test]
    fn every_built_in_cli_passes_the_checks_it_should() {
        let opencode = argonauts_adapter_opencode::OpenCodeAdapter::new();
        let clis: [(&str, &dyn Adapter); 4] = [
            ("agy", &argonauts_adapter_antigravity_cli::AntigravityCli),
            (
                "Claude Code",
                &argonauts_adapter_claude_code::ClaudeCodeAdapter,
            ),
            ("Codex", &argonauts_adapter_codex::CodexAdapter),
            ("OpenCode", &opencode),
        ];
        for (name, adapter) in clis {
            let can = capabilities_for(adapter);
            assert!(can.resume, "{name} resumes");
            assert!(can.can_build, "{name} builds");
            assert!(can.take_over, "{name} can be taken over");
            assert_ne!(can.steer, SteerSupport::NextTurnOnly, "{name} steers");
        }
    }

    #[test]
    fn an_invented_model_never_decides_what_a_cli_can_do() {
        let opencode = argonauts_adapter_opencode::OpenCodeAdapter::new();
        assert!(opencode.command(&probe_request(&opencode)).is_ok());
        assert_eq!(probe_request(&opencode).model, None);
    }

    /// Claude Code and Codex copy a session with a flag or a subcommand of their own. A CLI
    /// with neither would resume the original, so it never says it forks.
    #[test]
    fn only_a_cli_with_a_fork_of_its_own_forks() {
        assert!(capabilities_for(&argonauts_adapter_claude_code::ClaudeCodeAdapter).fork);
        assert!(capabilities_for(&argonauts_adapter_codex::CodexAdapter).fork);
        assert!(!capabilities_for(&argonauts_adapter_antigravity_cli::AntigravityCli).fork);
        assert!(!capabilities_for(&TestAdapter::new("true")).fork);
        assert!(capabilities_for(&TestAdapter::new("true").with_fork()).fork);
        let driven = TestAdapter::new("true").with_driver(FakeDriver::default());
        assert!(!capabilities_for(&driven).fork);
    }

    #[test]
    fn subagents_status_tool_and_caps_by_agent() {
        let claude = capabilities_for(&TestAdapter::new("true").with_id(CLAUDE_CODE));
        assert_eq!(claude.subagents, SubagentVisibility::Full);
        assert!(claude.status_tool && claude.native_limit_cap && claude.streams_text);
        let codex = capabilities_for(&TestAdapter::new("true").with_id(CODEX));
        assert_eq!(codex.subagents, SubagentVisibility::ThreadsOnly);
        assert!(!codex.status_tool && !codex.native_limit_cap && !codex.streams_text);
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
    fn take_over_needs_an_interactive_resume() {
        assert!(!capabilities_for(&TestAdapter::new("true")).take_over);
        let adapter = TestAdapter::new("true").with_take_over();
        assert!(capabilities_for(&adapter).take_over);
    }

    #[test]
    fn can_build_unless_the_adapter_refuses_build_launches() {
        assert!(capabilities_for(&TestAdapter::new("true")).can_build);
        let adapter = TestAdapter::new("true").read_only();
        assert!(!capabilities_for(&adapter).can_build);
    }
}
