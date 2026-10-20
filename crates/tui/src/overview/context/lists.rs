//! The lists Claude Code's /context ends with, each under a header with its count and its
//! total: MCP tools loaded and available, custom agents and skills grouped by where they come
//! from, and memory files with their paths. Every list shows in full, except that a long list
//! of MCP tools folds the ones not loaded into a row a server, as Claude Code folds them.

use argonauts_core::events::context::{ContextKind, ContextUsage, McpTool};
use ratatui::text::Span;

use super::place::{Block, Columns, Piece, at, beside, clip, ending, place, stacked};
use super::usage::bold;
use super::words::{count, grouped, skill_tokens, source_label, tool_name};
use crate::bridge::tools::Places;
use crate::format;
use crate::kit::{Drawn, Kit};
use crate::text::{sanitize_line, str_width};

/// Where a list's names start, after the column that says where they come from.
const NAMES: usize = 10;
/// Where an MCP tool's server is named, from the list's start.
const SERVER: usize = 32;
/// The most rows MCP tools take before those not loaded fold into a row a server.
const MCP_ROWS: usize = 12;

fn clean(text: &str) -> String {
    sanitize_line(text).trim().to_owned()
}

/// `├ ` before every item but the last, which takes `└ `.
fn branch(kit: &Kit<'_>, at: usize, of: usize) -> Span<'static> {
    let glyphs = &kit.theme.glyphs;
    let glyph = if at + 1 == of {
        glyphs.tree_end
    } else {
        glyphs.tree_mid
    };
    kit.dim(format!("{glyph} "))
}

/// A list's header: its name in bold with Claude Code's hint, and flush right how many it
/// holds and what they come to.
pub(super) fn header(
    kit: &Kit<'_>,
    block: Block,
    title: &str,
    hint: &str,
    count: String,
    total: String,
) -> Vec<Piece> {
    vec![
        at(block.start, vec![bold(kit, title), kit.dim(hint)]),
        ending(
            block.end,
            vec![kit.dim(format!("{count}  ")), bold(kit, total)],
        ),
    ]
}

/// Whether a list of MCP tools shows those not loaded one a row, or folded a row a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    Folded,
    Whole,
}

/// MCP tools, the loaded ones with their tokens and then, where tools load on demand, the
/// ones available by name. A server column says whose each tool is. Past [`MCP_ROWS`] rows,
/// unless `whole`, the available ones fold into a row a server. The fold, where the list is
/// long enough for one, says how the list shows.
pub(super) fn mcp_tools(
    kit: &Kit<'_>,
    columns: &Columns,
    usage: &ContextUsage,
    whole: bool,
) -> (Drawn, Option<Fold>) {
    let tools = &usage.mcp_tools;
    let mut drawn = Drawn::new();
    if tools.is_empty() {
        return (drawn, None);
    }
    let left = columns.left;
    // Claude Code's own test for tools that load on demand.
    let on_demand = usage
        .of_kind(ContextKind::Deferred)
        .any(|row| row.name.contains("MCP"));
    let loaded: Vec<&McpTool> = tools.iter().filter(|tool| tool.loaded).collect();
    let waiting: Vec<&McpTool> = tools.iter().filter(|tool| !tool.loaded).collect();
    let tokens = |tools: &[&McpTool]| format::tokens(tools.iter().map(|tool| tool.tokens).sum());
    let hint = if on_demand {
        " \u{b7} /mcp (loaded on-demand)"
    } else {
        " \u{b7} /mcp"
    };
    let end = columns.second().end;
    let mut summary = vec![kit.dim(format!("{} \u{b7} ", count(tools.len(), "tool")))];
    if on_demand {
        summary.extend([
            bold(kit, tokens(&loaded)),
            kit.dim(" loaded \u{b7} "),
            Span::raw(tokens(&waiting)),
            kit.dim(" on demand"),
        ]);
    } else {
        let all: Vec<&McpTool> = tools.iter().collect();
        summary.push(bold(kit, tokens(&all)));
    }
    drawn.push(place([
        at(left.start, vec![bold(kit, "MCP tools"), kit.dim(hint)]),
        ending(end, summary),
    ]));

    let row = |tool: &McpTool, at_row: usize, of: usize, quiet: bool| {
        let word = |text: String| {
            if quiet {
                kit.dim(text)
            } else {
                Span::raw(text)
            }
        };
        let (server, tokens) = (clean(&tool.server), format::tokens(tool.tokens));
        // A long name gives way so the server and the tokens stay on the row.
        let room = left
            .cells()
            .saturating_sub(str_width(&server) + str_width(&tokens) + 6);
        place([
            at(
                left.start,
                vec![
                    branch(kit, at_row, of),
                    word(clip(&clean(tool_name(&tool.name)), room)),
                ],
            ),
            at(left.start + SERVER, vec![kit.dim(server)]),
            ending(left.end, vec![word(tokens)]),
        ])
    };
    if !on_demand {
        for (at_row, tool) in tools.iter().enumerate() {
            drawn.push(row(tool, at_row, tools.len(), false));
        }
        return (drawn, None);
    }
    let fold = (loaded.len() + waiting.len() > MCP_ROWS).then_some(if whole {
        Fold::Whole
    } else {
        Fold::Folded
    });
    if !loaded.is_empty() {
        drawn.push(place([at(left.start, vec![kit.dim("Loaded")])]));
        for (at_row, tool) in loaded.iter().enumerate() {
            drawn.push(row(tool, at_row, loaded.len(), false));
        }
    }
    if !waiting.is_empty() {
        drawn.push(place([at(
            left.start,
            vec![kit.dim("Available \u{b7} not in the window until called")],
        )]));
        if fold == Some(Fold::Folded) {
            let servers = by_server(&waiting);
            for (at_row, (server, tools, tokens)) in servers.iter().enumerate() {
                let words = format!(
                    "{} \u{b7} {} \u{b7} {} on demand",
                    clean(server),
                    count(*tools, "tool"),
                    format::tokens(*tokens)
                );
                let room = left.cells().saturating_sub(2);
                drawn.push(place([at(
                    left.start,
                    vec![
                        branch(kit, at_row, servers.len()),
                        kit.dim(clip(&words, room)),
                    ],
                )]));
            }
        } else {
            for (at_row, tool) in waiting.iter().enumerate() {
                drawn.push(row(tool, at_row, waiting.len(), true));
            }
        }
    }
    (drawn, fold)
}

/// Tools by their server, in the order the servers first come: how many each has and what
/// they come to.
fn by_server<'a>(tools: &[&'a McpTool]) -> Vec<(&'a str, usize, u64)> {
    let mut servers: Vec<(&str, usize, u64)> = Vec::new();
    for tool in tools {
        match servers
            .iter_mut()
            .find(|(server, ..)| *server == tool.server.as_str())
        {
            Some((_, count, tokens)) => {
                *count += 1;
                *tokens += tool.tokens;
            }
            None => servers.push((tool.server.as_str(), 1, tool.tokens)),
        }
    }
    servers
}

/// The cells a name has between its branch and the two blanks before its tokens.
fn name_room(block: Block, tokens: &str) -> usize {
    block
        .cells()
        .saturating_sub(NAMES + 2 + 2 + str_width(tokens))
}

/// Rows grouped by where they come from: the group's label on its first row, a tree inside
/// each group, and the tokens flush right.
fn grouped_rows(
    kit: &Kit<'_>,
    block: Block,
    groups: Vec<(String, Vec<(String, String)>)>,
) -> Vec<Vec<Piece>> {
    let mut rows = Vec::new();
    for (label, items) in groups {
        let of = items.len();
        for (at_row, (name, tokens)) in items.into_iter().enumerate() {
            let label = if at_row == 0 {
                clip(&clean(&label), NAMES - 1)
            } else {
                String::new()
            };
            rows.push(vec![
                at(block.start, vec![kit.dim(label)]),
                at(
                    block.start + NAMES,
                    vec![
                        branch(kit, at_row, of),
                        Span::raw(clip(&name, name_room(block, &tokens))),
                    ],
                ),
                ending(block.end, vec![Span::raw(tokens)]),
            ]);
        }
    }
    rows
}

fn custom_agents(kit: &Kit<'_>, block: Block, usage: &ContextUsage) -> Vec<Vec<Piece>> {
    if usage.agents.is_empty() {
        return Vec::new();
    }
    let total: u64 = usage.agents.iter().map(|agent| agent.tokens).sum();
    let groups = grouped(
        usage.agents.iter(),
        |agent| source_label(&agent.source, None),
        |agent| agent.tokens,
    );
    let mut rows = vec![header(
        kit,
        block,
        "Custom agents",
        " \u{b7} .claude/agents/",
        count(usage.agents.len(), "agent"),
        format::tokens(total),
    )];
    rows.extend(grouped_rows(
        kit,
        block,
        groups
            .into_iter()
            .map(|(label, agents)| {
                let items = agents
                    .into_iter()
                    .map(|agent| (clean(&agent.name), format::tokens(agent.tokens)))
                    .collect();
                (label, items)
            })
            .collect(),
    ));
    rows
}

/// Memory files in the order the session loads them, each with whose it is and its path from
/// the folder the work happens in, or from home.
fn memory_files(
    kit: &Kit<'_>,
    block: Block,
    usage: &ContextUsage,
    places: &Places,
) -> Vec<Vec<Piece>> {
    let files = &usage.memory_files;
    if files.is_empty() {
        return Vec::new();
    }
    let total: u64 = files.iter().map(|file| file.tokens).sum();
    let mut rows = vec![header(
        kit,
        block,
        "Memory files",
        " \u{b7} /memory",
        count(files.len(), "file"),
        format::tokens(total),
    )];
    for (at_row, file) in files.iter().enumerate() {
        let tokens = format::tokens(file.tokens);
        let path = clip(&clean(&places.show(&file.path)), name_room(block, &tokens));
        rows.push(vec![
            at(
                block.start,
                vec![kit.dim(clip(&clean(&file.scope), NAMES - 1))],
            ),
            at(
                block.start + NAMES,
                vec![branch(kit, at_row, files.len()), Span::raw(path)],
            ),
            ending(block.end, vec![Span::raw(tokens)]),
        ]);
    }
    rows
}

fn skills(kit: &Kit<'_>, block: Block, usage: &ContextUsage) -> Vec<Vec<Piece>> {
    let Some(skills) = usage.skills.as_ref().filter(|skills| skills.tokens > 0) else {
        return Vec::new();
    };
    let groups = grouped(
        skills.items.iter(),
        |skill| source_label(&skill.source, skill.plugin.as_deref()),
        |skill| skill.tokens,
    );
    let mut rows = vec![header(
        kit,
        block,
        "Skills",
        " \u{b7} /skills",
        count(skills.items.len(), "skill"),
        format::tokens(skills.tokens),
    )];
    rows.extend(grouped_rows(
        kit,
        block,
        groups
            .into_iter()
            .map(|(label, skills)| {
                let items = skills
                    .into_iter()
                    .map(|skill| (clean(&skill.name), skill_tokens(skill.tokens)))
                    .collect();
                (label, items)
            })
            .collect(),
    ));
    rows
}

/// What you set up yourself: custom agents and memory files on the left, skills on the right,
/// or all three one under another where the panel is narrow.
pub(super) fn setup(
    kit: &Kit<'_>,
    columns: &Columns,
    usage: &ContextUsage,
    places: &Places,
) -> Drawn {
    let agents = custom_agents(kit, columns.left, usage);
    let memory = memory_files(kit, columns.left, usage, places);
    let skills = skills(kit, columns.second(), usage);
    if !columns.designed() {
        return stacked([agents, memory, skills]);
    }
    let mut left = agents;
    if !left.is_empty() && !memory.is_empty() {
        left.push(Vec::new());
    }
    left.extend(memory);
    beside(left, skills)
}
