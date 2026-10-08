use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::HistoryCell;
use crate::render::RenderCtx;
use crate::text::{sanitize_line, str_width, truncate_line};
use crate::theme::Role;

const MARK_COL: usize = 1;
const TEXT_COL: usize = 11;
const MARK_MIN_WIDTH: usize = 36;

/// Printed once at startup. Quotas and spend stay live in the status line instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionHeader {
    pub app: String,
    pub version: String,
    /// The chat's CLI by name, such as `Claude Code`.
    pub main_agent: Option<String>,
    /// Its model by name, such as `Opus 5.5`.
    pub model: Option<String>,
    /// Shown as `with <effort> effort` after the agent, like Claude Code's header.
    pub effort: Option<String>,
    pub workflow: String,
    /// Already shortened with `~`. It is cut from the left when it doesn't fit.
    pub folder: String,
    /// The ones that aren't ready each get a note under the header.
    pub agents: Vec<AgentReady>,
    /// From an earlier session, for `/resume` to reattach to.
    pub runs_going: u32,
    /// Runs whose cards wait on you. The app opens a new conversation all the same, and
    /// `/resume` opens one of them.
    pub runs_need_you: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentReady {
    /// The CLI by name, such as `Codex`.
    pub name: String,
    pub ready: bool,
    /// Why it isn't ready, or its version when it is.
    pub note: Option<String>,
}

impl HistoryCell for SessionHeader {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let width = usize::from(ctx.width);
        let dim = theme.style(Role::Dim);

        let version = sanitize_line(&self.version);
        let version = if version.starts_with(|c: char| c.is_ascii_digit()) {
            format!("v{version}")
        } else {
            version.into_owned()
        };
        let mut title = vec![Span::styled(
            sanitize_line(&self.app).into_owned(),
            theme.style(Role::Text).add_modifier(Modifier::BOLD),
        )];
        if !version.is_empty() {
            title.push(Span::styled(format!(" {version}"), dim));
        }
        let setup: Vec<String> = [self.agent_line(), Some(self.workflow.clone())]
            .into_iter()
            .flatten()
            .map(|part| sanitize_line(&part).trim().to_owned())
            .filter(|part| !part.is_empty())
            .collect();

        let with_mark = width >= MARK_MIN_WIDTH;
        let text_col = if with_mark { TEXT_COL } else { 2 };
        let room = width.saturating_sub(text_col);
        let folder = shorten_folder(&sanitize_line(&self.folder), room);
        let beside = [
            title,
            vec![Span::styled(setup.join(" · "), dim)],
            vec![Span::styled(folder, dim)],
        ];

        // A blank row on top, as Claude Code leaves above its header.
        let mut lines = vec![Line::default()];
        for (row, text) in beside.into_iter().enumerate() {
            let mut spans = Vec::new();
            if with_mark {
                spans.push(Span::raw(" ".repeat(MARK_COL)));
                spans.push(Span::styled(
                    theme.glyphs.mark[row],
                    theme.style(Role::Accent),
                ));
                spans.push(Span::raw(" ".repeat(TEXT_COL - MARK_COL - 8)));
            } else {
                spans.push(Span::raw(" ".repeat(text_col)));
            }
            spans.extend(text);
            lines.push(truncate_line(&Line::from(spans), width));
        }

        let notes = self.notes(ctx);
        if !notes.is_empty() {
            lines.push(Line::default());
            for note in notes {
                let mut spans = vec![Span::raw("  ")];
                spans.extend(note);
                lines.push(truncate_line(&Line::from(spans), width));
            }
        }
        // With the blank row History adds, two blank rows separate the header from the chat.
        lines.push(Line::default());
        lines
    }
}

impl SessionHeader {
    /// `Claude Code · Opus 5.5 with xhigh effort`, leaving out what isn't known.
    fn agent_line(&self) -> Option<String> {
        let known = |part: &Option<String>| {
            part.as_deref()
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
        };
        let mut line = known(&self.main_agent)?;
        if let Some(model) = known(&self.model) {
            line.push_str(&format!(" · {model}"));
        }
        if let Some(effort) = known(&self.effort) {
            line.push_str(&format!(" with {effort} effort"));
        }
        Some(line)
    }

    fn notes(&self, ctx: &RenderCtx<'_>) -> Vec<Vec<Span<'static>>> {
        let theme = ctx.theme;
        let dim = theme.style(Role::Dim);
        let mut notes = Vec::new();
        for agent in self.agents.iter().filter(|agent| !agent.ready) {
            let name = sanitize_line(&agent.name).into_owned();
            let note = agent
                .note
                .as_deref()
                .map(|note| sanitize_line(note).trim().to_owned())
                .filter(|note| !note.is_empty())
                .unwrap_or_else(|| "not ready".to_owned());
            notes.push(vec![Span::styled(format!("{name}: {note}"), dim)]);
        }
        let mut runs = Vec::new();
        if self.runs_going > 0 {
            runs.push(format!("{} going", runs_of(self.runs_going)));
        }
        if self.runs_need_you > 0 {
            let verb = if self.runs_need_you == 1 {
                "needs"
            } else {
                "need"
            };
            runs.push(format!("{} {verb} you", runs_of(self.runs_need_you)));
        }
        if !runs.is_empty() {
            notes.push(vec![
                Span::styled(format!("{} · ", runs.join(" · ")), dim),
                Span::styled("/resume", theme.style(Role::Code)),
            ]);
        }
        notes
    }
}

/// `1 run` or `3 runs`.
fn runs_of(count: u32) -> String {
    let unit = if count == 1 { "run" } else { "runs" };
    format!("{count} {unit}")
}

/// Cuts from the left as `/…/<last parts>`, or to the end of the last part when even that
/// won't fit.
fn shorten_folder(folder: &str, room: usize) -> String {
    let folder = folder.trim();
    if str_width(folder) <= room {
        return folder.to_owned();
    }
    let parts: Vec<&str> = folder.split('/').filter(|part| !part.is_empty()).collect();
    for keep in (1..parts.len()).rev() {
        let shortened = format!("/…/{}", parts[parts.len() - keep..].join("/"));
        if str_width(&shortened) <= room {
            return shortened;
        }
    }
    let last = parts.last().copied().unwrap_or(folder);
    let mut tail = String::new();
    for c in last.chars().rev() {
        let mut next = String::from(c);
        next.push_str(&tail);
        if str_width(&next) + 1 > room {
            break;
        }
        tail = next;
    }
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::render::Clock;
    use crate::text::plain_text;
    use crate::theme::{Glyphs, Theme};

    fn header() -> SessionHeader {
        SessionHeader {
            app: "Argonauts Code".into(),
            version: "0.0.1".into(),
            main_agent: Some("Claude Code".into()),
            model: Some("Opus 5.5".into()),
            effort: Some("xhigh".into()),
            workflow: "council".into(),
            folder: "~/github/argonauts".into(),
            agents: vec![
                AgentReady {
                    name: "Claude Code".into(),
                    ready: true,
                    note: Some("2.1.284".into()),
                },
                AgentReady {
                    name: "Codex".into(),
                    ready: false,
                    note: Some("logged out · run codex login".into()),
                },
            ],
            runs_going: 2,
            runs_need_you: 0,
        }
    }

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    #[test]
    fn a_mark_with_three_lines_beside_it_and_quiet_notes_under_it() {
        let theme = theme();
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let lines = header().lines(&ctx);
        assert_eq!(
            plain_text(&lines),
            "\n    ▟▙     Argonauts Code v0.0.1\n   \
             ▟██▙    Claude Code · Opus 5.5 with xhigh effort · council\n \
             ▝▀▀▀▀▀▀▘  ~/github/argonauts\n\
             \n  \
             Codex: logged out · run codex login\n  \
             2 runs going · /resume\n"
        );
        assert_eq!(lines[1].spans[1].style, theme.style(Role::Accent));
        assert!(
            lines[1].spans[3]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert_eq!(lines[1].spans[4].style, theme.style(Role::Dim));
        assert_eq!(lines[6].spans[2].style, theme.style(Role::Code));
    }

    #[test]
    fn runs_that_need_you_get_a_line_under_the_header() {
        let theme = theme();
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let note = |going: u32, need_you: u32| {
            let mut header = header();
            header.agents.clear();
            header.runs_going = going;
            header.runs_need_you = need_you;
            let lines = header.lines(&ctx);
            plain_text(&lines[5..]).trim_end().to_owned()
        };
        assert_eq!(note(0, 1), "  1 run needs you · /resume");
        assert_eq!(note(0, 2), "  2 runs need you · /resume");
        assert_eq!(note(1, 1), "  1 run going · 1 run needs you · /resume");
        assert_eq!(note(0, 0), "");
    }

    #[test]
    fn a_long_folder_is_cut_from_the_left() {
        assert_eq!(
            shorten_folder("/Users/you/github/argonauts", 24),
            "/…/github/argonauts"
        );
        assert_eq!(
            shorten_folder("/Users/you/github/argonauts", 14),
            "/…/argonauts"
        );
        assert_eq!(shorten_folder("/a/averyveryverylongname", 10), "…ylongname");
        assert_eq!(shorten_folder("~/short", 40), "~/short");

        let theme = theme();
        let ctx = RenderCtx::new(40, &theme, Clock::still(Instant::now()));
        let mut long = header();
        long.folder = "/Users/you/projects/playground/work/ui-tests/repo-a".into();
        let lines = long.lines(&ctx);
        assert_eq!(
            plain_text(&lines[3..4]),
            " ▝▀▀▀▀▀▀▘  /…/work/ui-tests/repo-a"
        );
        for line in &lines {
            assert!(line.width() <= 40, "{line:?}");
        }
    }

    #[test]
    fn narrow_terminals_drop_the_mark() {
        let theme = theme();
        let ctx = RenderCtx::new(34, &theme, Clock::still(Instant::now()));
        let mut header = header();
        header.model = None;
        header.effort = None;
        let lines = header.lines(&ctx);
        assert_eq!(
            plain_text(&lines[..4]),
            "\n  Argonauts Code v0.0.1\n  Claude Code · council\n  ~/github/argonauts"
        );
    }

    /// Like Claude Code's `Opus 5.5 with xhigh effort · Claude Max`, with each part you never
    /// set left out rather than shown as `None` or `?`.
    #[test]
    fn the_second_line_leaves_out_what_isnt_known() {
        let theme = theme();
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let second = |header: &SessionHeader| {
            plain_text(&header.lines(&ctx)[2..3])
                .trim_start_matches([' ', '▟', '█', '▙'])
                .to_owned()
        };
        let mut header = header();
        header.model = None;
        assert_eq!(second(&header), "Claude Code with xhigh effort · council");
        header.effort = Some("  ".into());
        assert_eq!(second(&header), "Claude Code · council");
        header.main_agent = None;
        header.effort = Some("high".into());
        assert_eq!(second(&header), "council");
    }
}
