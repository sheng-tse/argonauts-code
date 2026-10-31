//! The context card, above the input and at the end of the Context tab: the conversation's
//! context as one bar by its CLI's categories, in Claude Code's names and colours, or a run
//! session's as used and free.

use argonauts_core::runs::events::context::{ContextCategory, ContextKind};

use crate::base::kit::{ContextCard, ContextPart, Limit, LimitKind};
use crate::base::theme::Role;
use crate::frame::state::ContextCount;

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

/// The used rows as the bar's parts, in the CLI's order, its names and its colours.
fn parts(categories: &[ContextCategory]) -> Vec<ContextPart> {
    categories
        .iter()
        .filter(|row| row.kind == ContextKind::Used && row.tokens > 0)
        .map(|row| ContextPart::named(row.name.clone(), row.color.clone(), Some(row.tokens)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Claude Code's count of a session at 75% of its window.
    const AT_THREE_QUARTERS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adapters/claude-code/tests/data/context-usage-full.usage.json"
    ));

    #[test]
    fn the_pinned_card_draws_the_used_rows_and_where_the_session_compacts() {
        let count = ContextCount {
            usage: serde_json::from_str(AT_THREE_QUARTERS).expect("the kept count"),
            counted: time::OffsetDateTime::UNIX_EPOCH,
        };
        let card = conversation(&count);
        let parts: Vec<(&str, Option<u64>)> = card
            .parts
            .iter()
            .map(|part| (part.name.as_str(), part.tokens))
            .collect();
        assert_eq!(
            parts,
            [
                ("System prompt", Some(6_888)),
                ("System tools", Some(15_135)),
                ("MCP tools", Some(2_540)),
                ("MCP server instructions", Some(412)),
                ("Custom agents", Some(648)),
                ("Memory files", Some(3_686)),
                ("Skills", Some(710)),
                ("Messages", Some(719_981)),
            ]
        );
        assert_eq!(
            card.parts[0].paint,
            crate::base::kit::context::Paint::Named("promptBorder".to_owned())
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
