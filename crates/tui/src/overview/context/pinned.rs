//! The context card pinned above the input: the conversation's context as one bar by
//! category, or a run session's as used and free.

use argonauts_core::events::context::{ContextCategory, ContextKind};

use crate::kit::{ContextCard, ContextPart, Limit, LimitKind};
use crate::state::ContextCount;
use crate::theme::Role;

/// The conversation's context as its CLI last counted it, by category. It needs no answer
/// from the controller: the screen holds the count.
pub fn conversation(count: &ContextCount) -> ContextCard {
    let usage = &count.usage;
    let parts = parts(&usage.categories);
    let window = Some(usage.window).filter(|window| *window > 0);
    let compacts = usage.compacts_at().map(|at| Limit {
        at,
        kind: LimitKind::Compacts,
    });
    ContextCard {
        used: Some(parts.iter().filter_map(|part| part.tokens).sum()),
        window,
        limit: compacts.filter(|limit| window.is_some_and(|window| limit.at < window)),
        parts,
        estimated: false,
        ..ContextCard::estimate(None, None)
    }
}

/// A session that reports only how full it is: `used` of the `cap` where Argonauts Code starts
/// it afresh, in a `window` that may be larger.
pub fn session_use(subject: String, used: u64, cap: u64, window: Option<u64>) -> ContextCard {
    let window = window.filter(|window| *window > 0).unwrap_or(cap);
    let rotates = Limit {
        at: cap,
        kind: LimitKind::Rotates,
    };
    ContextCard {
        subject: Some(subject),
        used: Some(used),
        window: Some(window),
        limit: Some(rotates).filter(|limit| limit.at < window),
        parts: vec![ContextPart::new(
            "in context",
            Role::CatMessages,
            Some(used),
        )],
        estimated: false,
        ..ContextCard::estimate(None, None)
    }
}

/// The used rows as the bar's parts, in the CLI's order. The bar has one part for MCP, so the
/// servers' instructions count with their tools, and a row with no part of its own is `other`.
fn parts(categories: &[ContextCategory]) -> Vec<ContextPart> {
    let mut parts: Vec<ContextPart> = Vec::new();
    for row in categories
        .iter()
        .filter(|row| row.kind == ContextKind::Used && row.tokens > 0)
    {
        // Claude Code 2.1.296's names for its rows.
        let (name, role) = match row.name.as_str() {
            "System prompt" => ("system prompt", Role::CatSystem),
            "System tools" => ("tools", Role::CatTools),
            "MCP tools" | "MCP server instructions" => ("mcp tools", Role::CatMcp),
            "Custom agents" => ("agents", Role::CatAgents),
            "Memory files" => ("memory files", Role::CatMemory),
            "Skills" => ("skills", Role::CatSkills),
            "Messages" => ("messages", Role::CatMessages),
            _ => ("other", Role::CatOther),
        };
        match parts.iter_mut().find(|part| part.name == name) {
            Some(part) => part.tokens = Some(part.tokens.unwrap_or(0) + row.tokens),
            None => parts.push(ContextPart::new(name, role, Some(row.tokens))),
        }
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::super::example;
    use super::*;

    #[test]
    fn the_pinned_card_draws_the_used_rows_and_where_the_session_compacts() {
        let card = conversation(&example::at_three_quarters().count);
        let parts: Vec<(&str, Option<u64>)> = card
            .parts
            .iter()
            .map(|part| (part.name.as_str(), part.tokens))
            .collect();
        assert_eq!(
            parts,
            [
                ("system prompt", Some(6_888)),
                ("tools", Some(15_135)),
                // The servers' instructions count with their tools.
                ("mcp tools", Some(2_952)),
                ("agents", Some(648)),
                ("memory files", Some(3_686)),
                ("skills", Some(710)),
                ("messages", Some(719_981)),
            ]
        );
        assert_eq!((card.used, card.window), (Some(750_000), Some(1_000_000)));
        assert_eq!(
            card.limit,
            Some(Limit {
                at: 967_000,
                kind: LimitKind::Compacts
            })
        );
    }
}
