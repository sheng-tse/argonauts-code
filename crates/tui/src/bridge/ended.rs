//! An agent that failed ends on one row that says why in plain words, as Claude Code's own
//! failed agent does:
//!
//! ```text
//! ✗ review:due-dates · GPT 5.6 Luna on Codex · 2s
//!   ⎿  Codex refused the answer format Argonauts asked for · r shows the raw error
//! ```

use std::time::Duration;

use crate::cell::{ToolCall, ToolItem, ToolState};
use crate::format;

/// The row a failed agent ends with: `✗`, its name, how long it ran, and why it failed, with
/// the hint to its raw error when there is one.
pub(super) fn failed_row(
    whole: &ToolCall,
    why: Option<String>,
    raw_hint: Option<&str>,
    ran: Duration,
) -> ToolCall {
    let mut row = whole.summarized(None);
    row.state = ToolState::Failed;
    row.crossed = true;
    row.tail = Some(format!(" · {}", format::duration(ran)));
    let mut said = why.unwrap_or_else(|| "It failed".to_owned());
    if let Some(hint) = raw_hint {
        said.push_str(" · ");
        said.push_str(hint);
    }
    row.body = vec![ToolItem::text(said)];
    row
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::cell::HistoryCell;
    use crate::render::{Clock, RenderCtx};
    use crate::text::plain_text;
    use crate::theme::{Glyphs, Role, Theme};

    /// Codex turning down the answer format reads as one plain line under a cross.
    #[test]
    fn a_refusal_reads_as_one_plain_line_under_a_cross() {
        let whole = ToolCall::agent("review:due-dates", None, "GPT 5.6 Luna on Codex", "")
            .with_state(ToolState::Failed);
        let row = failed_row(
            &whole,
            Some("Codex refused the answer format Argonauts asked for".to_owned()),
            Some("r shows the raw error"),
            Duration::from_secs(2),
        );
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let lines = row.lines(&RenderCtx::new(100, &theme, Clock::still(Instant::now())));
        assert_eq!(
            plain_text(&lines),
            "✗ review:due-dates · GPT 5.6 Luna on Codex · 2s\n  ⎿  Codex refused the answer \
             format Argonauts asked for · r shows the raw error"
        );
        assert_eq!(lines[0].spans[0].style.fg, theme.style(Role::Error).fg);
    }
}
