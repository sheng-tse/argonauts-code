//! What the session loads beside its messages, in Claude Code's order: MCP tools, custom
//! agents, memory files and skills. Each list folds to one line, as Claude Code's `/context`
//! folds them, and opens to a line a server, a group or a file.

use argonauts_core::RunId;
use argonauts_core::runs::events::context::{ContextSkill, ContextUsage, McpTool};
use ratatui::text::Span;

use super::block::INDENT;
use super::words::{count, grouped, source_label};
use crate::base::format::context_tokens;
use crate::base::kit::{Drawn, Kit};
use crate::base::text::{sanitize_line, str_width};
use crate::base::theme::Role;
use crate::frame::state::Places;

/// One of the tab's lists, which Enter folds and opens.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum List {
    McpTools,
    CustomAgents,
    MemoryFiles,
    Skills,
    /// A run's crew, by the run.
    Crew(RunId),
}

/// One list as the tab draws it: its title, and its lines open or folded.
pub(super) struct Listed {
    pub list: List,
    title: &'static str,
    hint: String,
    /// What the folded line says of the whole list.
    whole: String,
    /// A line a server, group or file: its name and what it holds.
    lines: Vec<(String, String)>,
}

impl Listed {
    /// The list's rows: the title, then one line for the whole list, or a line each with the
    /// names in a column `names` cells wide.
    pub fn drawn(&self, kit: &Kit<'_>, open: bool, names: usize) -> Drawn {
        let mut drawn = Drawn::new();
        drawn.push(title(kit, self.title, &self.hint));
        if !open {
            drawn.push(folded(kit, &self.whole));
            return drawn;
        }
        for (at, (name, holds)) in self.lines.iter().enumerate() {
            let pad = names.saturating_sub(str_width(name)).max(2);
            drawn.push(vec![
                Span::raw(" ".repeat(INDENT)),
                kit.dim(branch(at, self.lines.len())),
                Span::raw(format!("{name}{}", " ".repeat(pad))),
                kit.dim(holds.clone()),
            ]);
        }
        drawn
    }

    /// How wide its longest name is.
    pub fn longest_name(&self) -> usize {
        self.lines
            .iter()
            .map(|(name, _)| str_width(name))
            .max()
            .unwrap_or(0)
    }
}

/// `MCP tools · /mcp (loaded on-demand)`.
pub(super) fn title(kit: &Kit<'_>, title: &str, hint: &str) -> Vec<Span<'static>> {
    vec![
        Span::raw(" ".repeat(INDENT)),
        Span::styled(title.to_owned(), kit.bold(Role::Text)),
        kit.dim(hint.to_owned()),
    ]
}

/// `└ 14 tools · 0 tokens`.
pub(super) fn folded(kit: &Kit<'_>, whole: &str) -> Vec<Span<'static>> {
    vec![
        Span::raw(" ".repeat(INDENT)),
        kit.dim(format!("\u{2514} {whole}")),
    ]
}

/// `├ ` before every line of a list but the last, which takes `└ `.
pub(super) fn branch(at: usize, of: usize) -> &'static str {
    if at + 1 == of {
        "\u{2514} "
    } else {
        "\u{251c} "
    }
}

/// `9 tools · 2.1k tokens`.
fn holds(n: usize, noun: &str, tokens: u64) -> String {
    format!(
        "{} \u{b7} {} tokens",
        count(n, noun),
        context_tokens(tokens)
    )
}

fn clean(text: &str) -> String {
    sanitize_line(text).trim().to_owned()
}

/// The lists the session's count has, in Claude Code's order. A list with nothing in it is
/// left out, as Claude Code leaves it out.
pub(super) fn of_usage(usage: &ContextUsage, places: &Places) -> Vec<Listed> {
    let mut lists = Vec::new();
    if !usage.mcp_tools.is_empty() {
        lists.push(mcp_tools(&usage.mcp_tools));
    }
    if !usage.agents.is_empty() {
        let groups = grouped(
            usage.agents.iter(),
            |agent| source_label(&agent.source, None),
            |agent| agent.tokens,
        );
        let total = usage.agents.iter().map(|agent| agent.tokens).sum();
        lists.push(Listed {
            list: List::CustomAgents,
            title: "Custom agents",
            hint: " \u{b7} .claude/agents/".to_owned(),
            whole: holds(usage.agents.len(), "agent", total),
            lines: groups
                .into_iter()
                .map(|(label, agents)| {
                    let tokens = agents.iter().map(|agent| agent.tokens).sum();
                    (clean(&label), holds(agents.len(), "agent", tokens))
                })
                .collect(),
        });
    }
    if !usage.memory_files.is_empty() {
        let files = &usage.memory_files;
        let total = files.iter().map(|file| file.tokens).sum();
        lists.push(Listed {
            list: List::MemoryFiles,
            title: "Memory files",
            hint: " \u{b7} /memory".to_owned(),
            whole: holds(files.len(), "file", total),
            lines: files
                .iter()
                .map(|file| {
                    (
                        clean(&places.show(&file.path)),
                        format!("{} tokens", context_tokens(file.tokens)),
                    )
                })
                .collect(),
        });
    }
    if let Some(skills) = usage.skills.as_ref().filter(|skills| skills.tokens > 0) {
        let groups = grouped(
            skills.items.iter(),
            |skill: &&ContextSkill| source_label(&skill.source, skill.plugin.as_deref()),
            |skill| skill.tokens,
        );
        lists.push(Listed {
            list: List::Skills,
            title: "Skills",
            hint: " \u{b7} /skills".to_owned(),
            whole: holds(skills.items.len(), "skill", skills.tokens),
            lines: groups
                .into_iter()
                .map(|(label, items)| {
                    let tokens = items.iter().map(|skill| skill.tokens).sum();
                    (clean(&label), holds(items.len(), "skill", tokens))
                })
                .collect(),
        });
    }
    lists
}

/// MCP tools by server, the server with the most first. Where some load only when used, the
/// tokens are those of the ones loaded, as Claude Code counts them.
fn mcp_tools(tools: &[McpTool]) -> Listed {
    let on_demand = tools.iter().any(|tool| !tool.loaded);
    let counted = |tool: &McpTool| {
        if !on_demand || tool.loaded {
            tool.tokens
        } else {
            0
        }
    };
    let mut servers: Vec<(String, usize, u64)> = Vec::new();
    for tool in tools {
        let server = clean(&tool.server);
        match servers.iter_mut().find(|(known, _, _)| *known == server) {
            Some((_, n, tokens)) => {
                *n += 1;
                *tokens += counted(tool);
            }
            None => servers.push((server, 1, counted(tool))),
        }
    }
    servers.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let total = tools.iter().map(counted).sum();
    Listed {
        list: List::McpTools,
        title: "MCP tools",
        hint: if on_demand {
            " \u{b7} /mcp (loaded on-demand)".to_owned()
        } else {
            " \u{b7} /mcp".to_owned()
        },
        whole: holds(tools.len(), "tool", total),
        lines: servers
            .into_iter()
            .map(|(server, n, tokens)| (server, holds(n, "tool", tokens)))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::runs::events::context::ContextSkills;

    fn tool(server: &str, loaded: bool, tokens: u64) -> McpTool {
        McpTool {
            name: format!("mcp__{server}__tool"),
            server: server.to_owned(),
            tokens,
            loaded,
        }
    }

    #[test]
    fn mcp_tools_fold_to_their_count_and_open_by_server_the_most_first() {
        let tools = vec![
            tool("argonauts", false, 300),
            tool("crm", false, 200),
            tool("crm", true, 120),
        ];
        let listed = mcp_tools(&tools);
        assert_eq!(listed.hint, " \u{b7} /mcp (loaded on-demand)");
        assert_eq!(listed.whole, "3 tools \u{b7} 120 tokens");
        assert_eq!(
            listed.lines,
            [
                ("crm".to_owned(), "2 tools \u{b7} 120 tokens".to_owned()),
                ("argonauts".to_owned(), "1 tool \u{b7} 0 tokens".to_owned()),
            ]
        );
        let all_loaded = mcp_tools(&[tool("crm", true, 1_200)]);
        assert_eq!(all_loaded.hint, " \u{b7} /mcp");
        assert_eq!(all_loaded.whole, "1 tool \u{b7} 1.2k tokens");
    }

    #[test]
    fn skills_open_by_group_in_claude_codes_order() {
        let skill = |source: &str, tokens| ContextSkill {
            name: "s".to_owned(),
            source: source.to_owned(),
            plugin: None,
            tokens,
        };
        let usage = ContextUsage {
            skills: Some(ContextSkills {
                total: 3,
                included: 3,
                tokens: 3_310,
                items: vec![
                    skill("built-in", 2_700),
                    skill("userSettings", 110),
                    skill("syncedSkills", 500),
                ],
            }),
            ..ContextUsage::default()
        };
        let lists = of_usage(&usage, &Places::default());
        assert_eq!(lists.len(), 1);
        assert_eq!(lists[0].whole, "3 skills \u{b7} 3.3k tokens");
        let names: Vec<&str> = lists[0]
            .lines
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(names, ["User", "claude.ai sync", "Built-in"]);
    }
}
