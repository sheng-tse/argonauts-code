use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::HistoryCell;
use crate::render::RenderCtx;
use crate::text::marks;
use crate::text::{fit_lines, sanitize_line, str_width, wrap_spans};
use crate::theme::{AgentColor, Role};

/// One column in from the text, as Claude Code draws its background agents.
const TREE_COL: usize = 3;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentsStarted {
    pub agents: Vec<StartedAgent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedAgent {
    /// Its display name, such as `Opus 5.5 on Claude Code`.
    pub name: String,
    pub color: Option<AgentColor>,
    /// Such as `worker · task-002 Add the JSON writer`.
    pub detail: String,
}

impl StartedAgent {
    pub fn new(
        name: impl Into<String>,
        color: Option<AgentColor>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            color,
            detail: detail.into(),
        }
    }
}

impl AgentsStarted {
    pub fn new(agents: Vec<StartedAgent>) -> Self {
        Self { agents }
    }
}

impl HistoryCell for AgentsStarted {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let width = usize::from(ctx.width);
        let count = self.agents.len();
        let noun = if count == 1 { "agent" } else { "agents" };
        let text = theme.style(Role::Text);
        let head = [
            Span::styled(count.to_string(), text.add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {noun} started"), text),
            Span::styled(" (↓ to manage)", theme.style(Role::Dim)),
        ];
        // The launch is done by the time this prints.
        let bullet = [
            marks::decorative(theme.glyphs.bullet, theme.style(Role::Success)),
            Span::raw(" "),
        ];
        let mut lines = wrap_spans(&head, width, &bullet, &[Span::raw("  ")]);
        for (i, agent) in self.agents.iter().enumerate() {
            let branch = if i + 1 == count {
                theme.glyphs.tree_end
            } else {
                theme.glyphs.tree_mid
            };
            let opening = [
                Span::raw(" ".repeat(TREE_COL)),
                Span::styled(branch, theme.style(Role::Dim)),
                Span::raw(" "),
            ];
            let hang = [Span::raw(" ".repeat(TREE_COL + str_width(branch) + 1))];
            let name_style = agent.color.map_or(text, |color| theme.style(color.role()));
            let name = sanitize_line(&agent.name).trim().to_owned();
            let mut spans = vec![Span::styled(name, name_style)];
            let detail = sanitize_line(&agent.detail);
            if !detail.trim().is_empty() {
                spans.push(Span::styled(
                    format!(" · {}", detail.trim()),
                    theme.style(Role::Text),
                ));
            }
            lines.extend(wrap_spans(&spans, width, &opening, &hang));
        }
        fit_lines(&mut lines, width);
        lines
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::render::Clock;
    use crate::text::plain_text;
    use crate::theme::{Glyphs, Theme};

    #[test]
    fn it_reads_like_claude_codes_launch_record() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let cell = AgentsStarted::new(vec![
            StartedAgent::new(
                "Opus 5.5 on Claude Code",
                Some(AgentColor::Blue),
                "worker · task-002 Add the JSON writer",
            ),
            StartedAgent::new(
                "Gemini 3.6 on Antigravity CLI",
                None,
                "reviewer · review\x1b[2J",
            ),
        ]);
        let lines = cell.lines(&ctx);
        assert_eq!(
            plain_text(&lines),
            "● 2 agents started (↓ to manage)\n   \
             ├ Opus 5.5 on Claude Code · worker · task-002 Add the JSON writer\n   \
             └ Gemini 3.6 on Antigravity CLI · reviewer · review"
        );
        assert_eq!(
            lines[0].spans[0].style,
            marks::decoration(theme.style(Role::Success))
        );
        assert!(
            lines[0].spans[2]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert_eq!(lines[1].spans[3].style, theme.style(Role::AgentBlue));
    }
}
