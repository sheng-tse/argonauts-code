//! What the messages are made of: a bar a kind, and beside them every tool's results with
//! what a suggestion about the tool would save. Claude Code counts these and draws none of
//! them.

use argonauts_core::events::context::{ContextUsage, MessageBreakdown, ToolTokens};
use ratatui::text::Span;

use super::lists::header;
use super::place::{Block, Columns, Piece, at, beside, clip, ending, stacked};
use super::suggestions::{Severity, Suggestion};
use super::usage::bold;
use super::words::{MESSAGES, count, share, tool_name};
use crate::format;
use crate::kit::{Drawn, Kit, bars};
use crate::text::{sanitize_line, spans_width, str_width};
use crate::theme::Role;

/// A kind's bar: where it starts from the list's start, and its cells.
const KIND_BAR: (usize, usize) = (22, 18);
/// Where a kind's tokens end, from the list's start. Its share ends at the list's end.
const KIND_TOKENS: usize = 48;
/// A tool's bar: where it starts from the list's start, and its cells.
const TOOL_BAR: (usize, usize) = (24, 12);

/// The mark before a suggestion, and its colour.
pub(super) fn mark(kit: &Kit<'_>, severity: Severity) -> Span<'static> {
    let ascii = kit.theme.glyphs.rule.is_ascii();
    let (glyph, role) = match severity {
        Severity::Warning => (if ascii { "!" } else { "\u{26A0}" }, Role::Warn),
        Severity::Note => (if ascii { "i" } else { "\u{2139}" }, Role::AgentBlue),
    };
    Span::styled(format!("{glyph} "), kit.fg(role))
}

/// The kinds of message that hold something, the largest first, with what no kind accounts
/// for last.
fn kinds(messages: &MessageBreakdown) -> Vec<(&'static str, u64)> {
    let mut kinds = vec![
        ("Tool results", messages.tool_result_tokens),
        ("Assistant messages", messages.assistant_tokens),
        ("Attachments", messages.attachment_tokens),
        ("Tool calls", messages.tool_call_tokens),
        ("Your messages", messages.user_tokens),
        ("Sent to files", messages.redirected_tokens),
    ];
    kinds.sort_by_key(|(_, tokens)| std::cmp::Reverse(*tokens));
    kinds.push(("Unattributed", messages.unattributed_tokens));
    kinds.retain(|(_, tokens)| *tokens > 0);
    kinds
}

/// `Messages · by kind` with the messages' tokens and share, then a bar a kind. Where the
/// kinds don't add up to the messages, as when a count comes mid-turn, the header says the
/// bars are estimated, and each is a share of the kinds' own total.
fn by_kind(
    kit: &Kit<'_>,
    block: Block,
    usage: &ContextUsage,
    messages: &MessageBreakdown,
) -> Vec<Vec<Piece>> {
    let kinds = kinds(messages);
    if kinds.is_empty() {
        return Vec::new();
    }
    let whole = usage
        .category("Messages")
        .map_or(0, |messages| messages.tokens);
    let sum: u64 = kinds.iter().map(|(_, tokens)| tokens).sum();
    let mut title = vec![bold(kit, "Messages"), kit.dim(" \u{b7} by kind")];
    if sum != whole {
        title.push(kit.dim(" \u{b7} "));
        title.push(Span::raw("estimated"));
    }
    let tokens_end = (block.start + KIND_TOKENS).min(block.end);
    let mut rows = vec![vec![
        at(block.start, title),
        ending(tokens_end, vec![bold(kit, format::tokens(whole))]),
        ending(block.end, vec![kit.dim(share(whole, usage.raw_window))]),
    ]];
    for (name, tokens) in kinds {
        rows.push(vec![
            at(block.start, vec![Span::raw(name)]),
            at(
                block.start + KIND_BAR.0,
                bars::thin_bar(kit, tokens as f64 / sum as f64, KIND_BAR.1, MESSAGES),
            ),
            ending(tokens_end, vec![Span::raw(format::tokens(tokens))]),
            ending(block.end, vec![kit.dim(share(tokens, usage.raw_window))]),
        ]);
    }
    rows
}

/// What a tool's row says after its name: the mark of the suggestion about it, and what
/// that would save.
fn saving(kit: &Kit<'_>, tool: &ToolTokens, suggestions: &[Suggestion]) -> Vec<Span<'static>> {
    suggestions
        .iter()
        .find(|suggestion| suggestion.tool.as_deref() == Some(tool.name.as_str()))
        .and_then(|suggestion| Some((suggestion.severity, suggestion.savings?)))
        .map(|(severity, savings)| {
            let saves = format!("save ~{}", format::tokens(savings));
            vec![
                Span::raw("  "),
                mark(kit, severity),
                match severity {
                    Severity::Warning => Span::raw(saves),
                    Severity::Note => kit.dim(saves),
                },
            ]
        })
        .unwrap_or_default()
}

/// `Tool results · by tool`: every tool with results, the largest first, each a share of all
/// the results. A tool a suggestion is about carries the suggestion's mark and its saving.
/// The bars keep one column: long names shorten them, down to half, and then give way.
fn by_tool(
    kit: &Kit<'_>,
    block: Block,
    messages: &MessageBreakdown,
    suggestions: &[Suggestion],
) -> Vec<Vec<Piece>> {
    let mut tools: Vec<&ToolTokens> = messages
        .tools
        .iter()
        .filter(|tool| tool.result_tokens > 0)
        .collect();
    if tools.is_empty() {
        return Vec::new();
    }
    tools.sort_by_key(|tool| std::cmp::Reverse(tool.result_tokens));
    let results: u64 = tools.iter().map(|tool| tool.result_tokens).sum();
    let mut rows = vec![header(
        kit,
        block,
        "Tool results",
        " \u{b7} by tool",
        count(tools.len(), "tool"),
        format::tokens(results),
    )];
    let labels: Vec<(String, Vec<Span<'static>>)> = tools
        .iter()
        .map(|tool| {
            let name = sanitize_line(tool_name(&tool.name)).trim().to_owned();
            (name, saving(kit, tool, suggestions))
        })
        .collect();
    // A branch and a blank, the name, what it would save, and two cells before the bar.
    let widest = labels
        .iter()
        .map(|(name, note)| 2 + str_width(name) + spans_width(note) + 2)
        .max()
        .unwrap_or(0);
    let bar_end = block.start + TOOL_BAR.0 + TOOL_BAR.1;
    let bar_at = (block.start + widest).clamp(block.start + TOOL_BAR.0, bar_end - TOOL_BAR.1 / 2);
    let glyphs = &kit.theme.glyphs;
    for (at_row, (tool, (name, note))) in tools.iter().zip(labels).enumerate() {
        let branch = if at_row + 1 == tools.len() {
            glyphs.tree_end
        } else {
            glyphs.tree_mid
        };
        let room = bar_at.saturating_sub(block.start + 2 + spans_width(&note) + 2);
        let mut label = vec![kit.dim(format!("{branch} ")), Span::raw(clip(&name, room))];
        label.extend(note);
        rows.push(vec![
            at(block.start, label),
            at(
                bar_at,
                bars::thin_bar(
                    kit,
                    tool.result_tokens as f64 / results as f64,
                    bar_end - bar_at,
                    MESSAGES,
                ),
            ),
            ending(
                block.end,
                vec![Span::raw(format::tokens(tool.result_tokens))],
            ),
        ]);
    }
    rows
}

/// The messages by kind and the tool results by tool, side by side where there's room.
pub(super) fn messages(
    kit: &Kit<'_>,
    columns: &Columns,
    usage: &ContextUsage,
    suggestions: &[Suggestion],
) -> Drawn {
    let Some(messages) = &usage.messages else {
        return Drawn::new();
    };
    let kinds = by_kind(kit, columns.left, usage, messages);
    let tools = by_tool(kit, columns.second(), messages, suggestions);
    if columns.designed() {
        beside(kinds, tools)
    } else {
        stacked([kinds, tools])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_read_largest_first_with_the_rest_last() {
        let messages = MessageBreakdown {
            tool_call_tokens: 29_000,
            tool_result_tokens: 528_600,
            attachment_tokens: 44_100,
            assistant_tokens: 104_300,
            user_tokens: 9_800,
            redirected_tokens: 0,
            unattributed_tokens: 60_000,
            ..MessageBreakdown::default()
        };
        assert_eq!(
            kinds(&messages),
            [
                ("Tool results", 528_600),
                ("Assistant messages", 104_300),
                ("Attachments", 44_100),
                ("Tool calls", 29_000),
                ("Your messages", 9_800),
                ("Unattributed", 60_000),
            ]
        );
    }
}
