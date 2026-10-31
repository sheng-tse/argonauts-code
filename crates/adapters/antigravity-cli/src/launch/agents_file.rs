//! Custom agent files that narrow agy's tools for one session.
//!
//! agy has no read-only flag, and `--mode plan` still writes files in headless runs. A custom
//! agent's `tools` list is the only per-run control over which tools the model sees.

use std::path::{Path, PathBuf};

use anyhow::bail;
use argonauts_core::SpecFile;

/// Every tool agy 1.2.13 lists in its `init` event. A misspelled name in an agent's `tools`
/// can hang agy, so agent files only use names from this list.
pub const KNOWN_TOOLS: &[&str] = &[
    "ask_custom_permission",
    "ask_permission",
    "ask_question",
    "browser_click_element",
    "browser_drag_pixel_to_pixel",
    "browser_get_dom",
    "browser_get_network_request",
    "browser_input",
    "browser_list_network_requests",
    "browser_mouse_down",
    "browser_mouse_up",
    "browser_move_mouse",
    "browser_press_key",
    "browser_refresh_page",
    "browser_resize_window",
    "browser_scroll",
    "browser_scroll_dom",
    "browser_select_option",
    "browser_subagent",
    "call_mcp_tool",
    "capture_browser_console_logs",
    "capture_browser_screenshot",
    "click_browser_pixel",
    "command_status",
    "define_subagent",
    "delete_knowledge",
    "execute_browser_javascript",
    "find_by_name",
    "finish",
    "generate_image",
    "grep_search",
    "invoke_subagent",
    "list_browser_pages",
    "list_dir",
    "list_permissions",
    "list_resources",
    "manage_inbox",
    "manage_subagents",
    "manage_task",
    "multi_replace_file_content",
    "notebook_edit",
    "notebook_execution",
    "open_browser_url",
    "read_browser_page",
    "read_resource",
    "read_url_content",
    "replace_file_content",
    "run_command",
    "schedule",
    "search_web",
    "sed_file",
    "send_command_input",
    "send_message",
    "view_file",
    "wait",
    "wait_5_seconds",
    "write_to_file",
];

/// Tools that start, define, or steer subagents.
pub const SUBAGENT_TOOLS: &[&str] = &[
    "invoke_subagent",
    "define_subagent",
    "manage_subagents",
    "browser_subagent",
];

/// Reading files is all a read-only session may do: no edits, commands, browser, or network.
const READ_ONLY_TOOLS: &[&str] = &[
    "view_file",
    "list_dir",
    "find_by_name",
    "grep_search",
    "finish",
];

/// The folder under the scratch directory that agy gets through `--add-dir`, so it finds the
/// agent file and nothing else of the session's.
const AGENT_ROOT: &str = "agy-agent";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentProfile {
    /// Reads files and answers. No edit, shell, browser, network, or subagent tools.
    ReadOnly,
    /// Every tool except the subagent ones, for `native_fanout = "off"`.
    NoSubagents,
    /// No tools at all, for a session that answers from its prompt alone.
    NoTools,
}

impl AgentProfile {
    pub fn name(self) -> &'static str {
        match self {
            Self::ReadOnly => "argonauts-read-only",
            Self::NoSubagents => "argonauts-no-subagents",
            Self::NoTools => "argonauts-no-tools",
        }
    }

    pub fn tools(self) -> Vec<&'static str> {
        match self {
            Self::ReadOnly => READ_ONLY_TOOLS.to_vec(),
            Self::NoSubagents => KNOWN_TOOLS
                .iter()
                .copied()
                .filter(|tool| !SUBAGENT_TOOLS.contains(tool))
                .collect(),
            Self::NoTools => Vec::new(),
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::ReadOnly => "Reads files and answers. It never edits files or runs commands.",
            Self::NoSubagents => "Works on the task itself, without starting subagents.",
            Self::NoTools => "Answers from what it is given, with no tools.",
        }
    }

    fn instructions(self) -> &'static str {
        match self {
            Self::ReadOnly => {
                "You work read-only. Read and search files to answer, and never try to change \
                 files or run commands."
            }
            Self::NoSubagents => "Do the work yourself. You can't start subagents.",
            Self::NoTools => {
                "Answer from what the prompt gives you. You have no tools, so don't try to read \
                 files or run commands."
            }
        }
    }

    /// The read-only agent skips the repository's hooks, rules, and skills, since a read-only
    /// session may be looking at a repository nobody reviewed yet.
    fn inherit_customizations(self) -> bool {
        matches!(self, Self::NoSubagents)
    }

    /// `off` keeps the model from asking for commands at all, on top of having no shell tool.
    fn command_execution_policy(self) -> &'static str {
        match self {
            Self::ReadOnly | Self::NoTools => "off",
            Self::NoSubagents => "sandbox",
        }
    }
}

/// The agent file in the session's scratch directory, and the flags that select it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLaunch {
    pub file: SpecFile,
    pub args: Vec<String>,
}

pub fn agent_launch(profile: AgentProfile, scratch_dir: &Path) -> anyhow::Result<AgentLaunch> {
    if !scratch_dir.is_absolute() {
        bail!(
            "the agy agent directory must be absolute: {}",
            scratch_dir.display()
        );
    }
    let root = scratch_dir.join(AGENT_ROOT);
    let file = SpecFile {
        path: agent_path(&root, profile),
        contents: render(profile)?,
    };
    let args = vec![
        "--add-dir".to_owned(),
        root.display().to_string(),
        "--agent".to_owned(),
        profile.name().to_owned(),
    ];
    Ok(AgentLaunch { file, args })
}

fn agent_path(root: &Path, profile: AgentProfile) -> PathBuf {
    root.join(".agents")
        .join("agents")
        .join(format!("{}.md", profile.name()))
}

fn render(profile: AgentProfile) -> anyhow::Result<String> {
    let tools = profile.tools();
    if let Some(unknown) = tools.iter().find(|tool| !KNOWN_TOOLS.contains(tool)) {
        bail!("agy has no tool named {unknown:?}");
    }
    // An empty `tools:` would be null, which isn't an empty list.
    let tool_lines: String = if tools.is_empty() {
        " []\n".to_owned()
    } else {
        let lines: String = tools.iter().map(|tool| format!("  - {tool}\n")).collect();
        format!("\n{lines}")
    };
    Ok(format!(
        "---\n\
         name: {name}\n\
         description: {description}\n\
         mainAgent: true\n\
         model: inherit\n\
         commandExecutionPolicy: {policy}\n\
         inheritCustomizations: {inherit}\n\
         tools:{tool_lines}\
         ---\n\
         {instructions}\n",
        name = profile.name(),
        description = profile.description(),
        policy = profile.command_execution_policy(),
        inherit = profile.inherit_customizations(),
        instructions = profile.instructions(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frontmatter_tools(contents: &str) -> Vec<&str> {
        contents
            .lines()
            .skip_while(|line| *line != "tools:")
            .skip(1)
            .map_while(|line| line.strip_prefix("  - "))
            .collect()
    }

    #[test]
    fn read_only_agent_has_no_write_shell_or_subagent_tools() {
        let tools = AgentProfile::ReadOnly.tools();
        for banned in [
            "write_to_file",
            "replace_file_content",
            "multi_replace_file_content",
            "sed_file",
            "notebook_edit",
            "run_command",
            "send_command_input",
            "call_mcp_tool",
            "read_url_content",
            "invoke_subagent",
            "define_subagent",
        ] {
            assert!(!tools.contains(&banned), "{banned}");
        }
        assert!(tools.contains(&"view_file") && tools.contains(&"grep_search"));
    }

    #[test]
    fn no_subagents_agent_keeps_everything_else() {
        let tools = AgentProfile::NoSubagents.tools();
        for tool in SUBAGENT_TOOLS {
            assert!(!tools.contains(tool), "{tool}");
        }
        assert_eq!(tools.len(), KNOWN_TOOLS.len() - SUBAGENT_TOOLS.len());
        assert!(tools.contains(&"write_to_file") && tools.contains(&"run_command"));
    }

    #[test]
    fn every_listed_tool_is_known() {
        for profile in [AgentProfile::ReadOnly, AgentProfile::NoSubagents] {
            for tool in profile.tools() {
                assert!(KNOWN_TOOLS.contains(&tool), "{tool}");
            }
        }
        for tool in SUBAGENT_TOOLS {
            assert!(KNOWN_TOOLS.contains(tool), "{tool}");
        }
    }

    #[test]
    fn launch_writes_the_file_under_its_own_folder_and_selects_it() {
        let launch = agent_launch(AgentProfile::ReadOnly, Path::new("/s/session")).unwrap();
        assert_eq!(
            launch.file.path,
            Path::new("/s/session/agy-agent/.agents/agents/argonauts-read-only.md")
        );
        assert_eq!(
            launch.args,
            [
                "--add-dir",
                "/s/session/agy-agent",
                "--agent",
                "argonauts-read-only"
            ]
        );
    }

    #[test]
    fn the_file_is_frontmatter_then_instructions() {
        let launch = agent_launch(AgentProfile::ReadOnly, Path::new("/s")).unwrap();
        let contents = &launch.file.contents;
        assert!(contents.starts_with("---\nname: argonauts-read-only\n"));
        assert!(contents.contains("\ninheritCustomizations: false\n"));
        assert!(contents.contains("\ncommandExecutionPolicy: off\n"));
        assert!(contents.contains("\nmodel: inherit\n"));
        assert_eq!(frontmatter_tools(contents), AgentProfile::ReadOnly.tools());
        let body = contents.splitn(3, "---\n").nth(2).unwrap();
        assert!(body.starts_with("You work read-only."));

        let open = agent_launch(AgentProfile::NoSubagents, Path::new("/s")).unwrap();
        assert!(
            open.file
                .contents
                .contains("\ninheritCustomizations: true\n")
        );
        assert_eq!(
            frontmatter_tools(&open.file.contents),
            AgentProfile::NoSubagents.tools()
        );
    }

    #[test]
    fn the_agent_with_no_tools_lists_an_empty_list_and_runs_no_commands() {
        let launch = agent_launch(AgentProfile::NoTools, Path::new("/s")).unwrap();
        let contents = &launch.file.contents;
        assert!(contents.contains("\ntools: []\n---\n"), "{contents}");
        assert!(contents.contains("commandExecutionPolicy: off\n"));
        assert!(contents.contains("inheritCustomizations: false\n"));
    }

    #[test]
    fn relative_scratch_dirs_are_refused() {
        assert!(agent_launch(AgentProfile::ReadOnly, Path::new("rel/dir")).is_err());
    }
}
