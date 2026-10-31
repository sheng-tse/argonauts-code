use argonauts_core::app::UltraColour;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::HistoryCell;
use crate::base::draw::render::RenderCtx;
use crate::base::mascot::{self, Look, Mascot};
use crate::base::text::{sanitize_line, str_width, truncate_line};
use crate::base::theme::{Brightness, MascotDrawing, Role};

/// Where the mascot starts, and where the three lines beside it start.
pub const MARK_COL: usize = 1;
pub const TEXT_COL: usize = 11;
/// The narrowest screen the header draws its mascot on. Narrower, its words start where the
/// mascot would.
pub const MARK_MIN_WIDTH: usize = 36;

/// Printed once at startup. Plan usage and spend stay live in the bottom line instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionHeader {
    pub app: String,
    pub version: String,
    /// The conversation's CLI by name, such as `Claude Code`.
    pub main_agent: Option<String>,
    /// Its model by name, such as `Opus 5.5`.
    pub model: Option<String>,
    /// Shown as `with <effort> effort` after the agent, like Claude Code's header.
    pub effort: Option<String>,
    /// Picks the mascot: the argonaut for Captain, the medusa for Orchestrator, the hydra for
    /// Council.
    pub workflow: String,
    /// The main agent's own ultra mode while it is on, which the mascot shows in its colour.
    pub ultra: Option<UltraColour>,
    /// Already shortened with `~`. It is cut from the left when it doesn't fit.
    pub folder: String,
    /// The ones that aren't ready each get a note under the header.
    pub agents: Vec<AgentReady>,
    /// From an earlier session, for `/resume` to reattach to.
    pub runs_going: u32,
    /// Runs whose questions wait on you. The app opens a new conversation all the same, and
    /// `/resume` opens one of them.
    pub runs_need_you: u32,
    /// The conversation `/clear` left, which `/resume` picks up again.
    pub way_back: Option<String>,
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
        let workflow = argonauts_core::base::text::shown_name(&self.workflow);
        let setup: Vec<String> = [self.agent_line(), Some(workflow)]
            .into_iter()
            .flatten()
            .map(|part| sanitize_line(&part).trim().to_owned())
            .filter(|part| !part.is_empty())
            .collect();

        let mark = self.mascot_rows(ctx);
        let text_col = if mark.is_some() { TEXT_COL } else { 2 };
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
            match &mark {
                Some(mark) => {
                    spans.push(Span::raw(" ".repeat(MARK_COL)));
                    spans.extend(mark[row].iter().cloned());
                    spans.push(Span::raw(" ".repeat(TEXT_COL - MARK_COL - mascot::WIDTH)));
                }
                None => spans.push(Span::raw(" ".repeat(text_col))),
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
        // With the blank row History adds, two blank rows separate the header from the
        // conversation.
        lines.push(Line::default());
        lines
    }
}

impl SessionHeader {
    /// The workflow's mascot and the look it takes now.
    pub fn mascot(&self, light: bool) -> (Mascot, Look) {
        (
            Mascot::for_workflow(&self.workflow),
            Look::new(self.ultra, light),
        )
    }

    /// The mascot's three rows of cells beside the lines: blank where its picture covers them,
    /// and none when it isn't drawn (`ui.mascot = "off"`, a terminal too narrow, or the ASCII
    /// glyphs without a picture). An ultra look's frame follows the clock, so a still clock
    /// draws its first.
    fn mascot_rows(&self, ctx: &RenderCtx<'_>) -> Option<[Vec<Span<'static>>; mascot::ROWS]> {
        let theme = ctx.theme;
        let drawn = theme.mascot != MascotDrawing::Off
            && usize::from(ctx.width) >= MARK_MIN_WIDTH
            && (theme.mascot_pictures || !theme.glyphs.is_ascii());
        if !drawn {
            return None;
        }
        if theme.mascot_pictures {
            return Some(std::array::from_fn(|_| {
                vec![Span::raw(" ".repeat(mascot::WIDTH))]
            }));
        }
        let (mascot, look) = self.mascot(theme.brightness == Brightness::Light);
        let frame = if look.is_ultra() && !theme.reduced_motion {
            ctx.clock.frame(mascot.frame_time(), mascot.frames())
        } else {
            0
        };
        Some(mascot::rows(mascot, look, frame, theme))
    }

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
        if let Some(old) = &self.way_back {
            let old = sanitize_line(old);
            notes.push(vec![Span::styled(
                format!("/resume {old} picks the last one up again"),
                dim,
            )]);
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
mod golden;

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use ratatui::style::Color;

    use super::*;
    use crate::base::draw::render::Clock;
    use crate::base::text::plain_text;
    use crate::base::theme::{Glyphs, Theme};

    fn header() -> SessionHeader {
        SessionHeader {
            app: "Argonauts Code".into(),
            version: "0.0.1".into(),
            main_agent: Some("Claude Code".into()),
            model: Some("Opus 5.5".into()),
            effort: Some("xhigh".into()),
            workflow: "council".into(),
            ultra: None,
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
            way_back: None,
        }
    }

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    #[test]
    fn a_mascot_with_three_lines_beside_it_and_quiet_notes_under_it() {
        let theme = theme();
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let lines = header().lines(&ctx);
        assert_eq!(
            plain_text(&lines),
            "\n    ▗█▖ ▗▘ Argonauts Code v0.0.1\n \
             ▗▙  █ ▗▛▘ Claude Code · Opus 5.5 with xhigh effort · Council\n  \
             ▀█████▄▘ ~/github/argonauts\n\
             \n  \
             Codex: logged out · run codex login\n  \
             2 runs going · /resume\n"
        );
        let span = |line: usize, text: &str| {
            lines[line]
                .spans
                .iter()
                .find(|span| span.content.trim() == text)
                .unwrap_or_else(|| panic!("{text:?} on line {line}"))
                .style
        };
        assert_eq!(span(1, "█").fg, Some(Color::Rgb(0xe8, 0xa3, 0x3d)));
        assert!(
            span(1, "Argonauts Code")
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert_eq!(span(1, "v0.0.1"), theme.style(Role::Dim));
        assert_eq!(lines[6].spans[2].style, theme.style(Role::Code));
    }

    #[test]
    fn the_mascot_follows_the_workflow_and_its_look_the_ultra_mode_and_the_background() {
        let first_row = |header: &SessionHeader, theme: &Theme| {
            let ctx = RenderCtx::new(80, theme, Clock::still(Instant::now()));
            header.lines(&ctx)[1].clone()
        };
        let mut captain = header();
        captain.workflow = "captain".into();
        let row = first_row(&captain, &theme());
        assert!(plain_text(&[row]).starts_with("  ▗███▖▐▀▖ Argonauts Code"));
        // Claude Code's ultracode turns it purple, and the shell it holds up gold.
        captain.ultra = Some(UltraColour::Purple);
        let row = first_row(&captain, &theme());
        assert_eq!(row.spans[2].style.fg, Some(Color::Rgb(0xaf, 0x87, 0xff)));
        assert_eq!(row.spans[8].style.fg, Some(Color::Rgb(0xe8, 0xa3, 0x3d)));
        captain.ultra = None;
        let mut light = Theme::light();
        light.glyphs = Glyphs::UNICODE;
        let row = first_row(&captain, &light);
        assert_eq!(row.spans[2].style.fg, Some(Color::Rgb(0xb0, 0x70, 0x20)));
    }

    #[test]
    fn a_picture_leaves_the_cells_blank_and_off_or_ascii_draws_none() {
        let draw = |theme: &Theme| {
            let ctx = RenderCtx::new(80, theme, Clock::still(Instant::now()));
            plain_text(&header().lines(&ctx)[1..2])
        };
        let mut pictures = theme();
        pictures.mascot_pictures = true;
        assert_eq!(draw(&pictures), "           Argonauts Code v0.0.1");
        let mut off = theme();
        off.mascot = MascotDrawing::Off;
        assert_eq!(draw(&off), "  Argonauts Code v0.0.1");
        let mut ascii = theme();
        ascii.glyphs = Glyphs::ASCII;
        assert_eq!(draw(&ascii), "  Argonauts Code v0.0.1");
        ascii.mascot_pictures = true;
        assert_eq!(draw(&ascii), "           Argonauts Code v0.0.1");
    }

    #[test]
    fn an_ultra_look_sweeps_its_light_with_the_clock_unless_motion_is_reduced() {
        let mut header = header();
        header.ultra = Some(UltraColour::Blue);
        let start = Instant::now();
        let lit = Some(Color::Rgb(0xbf, 0xd8, 0xff));
        let lit_at = |theme: &Theme, after: std::time::Duration| {
            let ctx = RenderCtx::new(80, theme, Clock::new(start, start + after));
            header.lines(&ctx)[1..4]
                .iter()
                .flat_map(|line| line.spans.iter())
                .filter(|span| span.style.fg == lit)
                .count()
        };
        let theme = theme();
        assert_eq!(lit_at(&theme, std::time::Duration::ZERO), 0);
        // Halfway through the Hydra's 3.6 s the light is on its middle.
        assert!(lit_at(&theme, std::time::Duration::from_millis(1800)) > 0);
        let mut still = theme.clone();
        still.reduced_motion = true;
        assert_eq!(lit_at(&still, std::time::Duration::from_millis(1800)), 0);
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
            shorten_folder("/Users/you/projects/github/argonauts", 24),
            "/…/github/argonauts"
        );
        assert_eq!(
            shorten_folder("/Users/you/projects/github/argonauts", 14),
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
            "  ▀█████▄▘ /…/work/ui-tests/repo-a"
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
            "\n  Argonauts Code v0.0.1\n  Claude Code · Council\n  ~/github/argonauts"
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
                .chars()
                .skip(TEXT_COL)
                .collect::<String>()
        };
        let mut header = header();
        header.model = None;
        assert_eq!(second(&header), "Claude Code with xhigh effort · Council");
        header.effort = Some("  ".into());
        assert_eq!(second(&header), "Claude Code · Council");
        header.main_agent = None;
        header.effort = Some("high".into());
        assert_eq!(second(&header), "Council");
    }

    /// The header's two lines as the picture of names on screen draws them, from the text
    /// column on: the workflow is named with a capital, as `Captain`.
    #[test]
    fn the_header_names_the_workflow_as_its_picture_does() {
        use crate::testing::pictures::golden::{Picture, Region, assert_matches, theme};
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let shot = Picture::load("names_after");
        let theme = theme();
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let header = SessionHeader {
            model: Some("Haiku 4.5".to_owned()),
            effort: None,
            workflow: "captain".to_owned(),
            ultra: None,
            agents: Vec::new(),
            runs_going: 0,
            ..header()
        };
        let lines = header.lines(&ctx);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, lines.len() as u16));
        for (y, line) in lines.iter().enumerate() {
            buf.set_line(0, y as u16, line, 80);
        }
        // The picture starts its rows where the header's text column starts.
        let text = Region {
            rows: 2..4,
            columns: 0..30,
            at: (TEXT_COL as u16, 1),
        };
        assert_matches(&shot, &buf, &text, &theme);
    }
}
