//! The frame the workflow, agent, and context views share: a two-row header, one rounded box
//! holding a left and a right pane with a divider, and a footer of key hints. ratatui lays it
//! out from the screen size, so borders stay in one column at any width. Also the marks every
//! row uses for its state, so a state reads the same in the panel and in the views.
//!
//! With the ASCII glyph set the box is `+`, `-`, and `|`. That set is also the one for
//! terminals that draw ambiguous glyphs two cells wide, and every box-drawing glyph is
//! ambiguous.

use argonauts_core::Spend;
use argonauts_core::views::RowStatus;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Widget};

use crate::format;
use crate::text::{spans_width, str_width, truncate_line};
use crate::theme::{Role, Theme};

use super::layout::{cut, right};

/// Rows above the box: the name, the description, and a blank row.
const HEADER_ROWS: u16 = 3;

/// Where the views draw, all inside the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Panes {
    /// The name row and the description row.
    pub header: Rect,
    /// The box, borders included.
    pub body: Rect,
    /// Inside the box, left of the divider. Zero wide for a one-pane frame.
    pub left: Rect,
    pub right: Rect,
    pub footer: Rect,
}

/// Splits the screen. `left_width` is the left pane's share in cells, before borders, and 0
/// leaves one pane. The right pane always keeps at least half the box.
pub fn split(area: Rect, left_width: u16) -> Panes {
    let [header, _gap, body, footer] = Layout::vertical([
        Constraint::Length(HEADER_ROWS - 1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    let inner = Block::bordered().inner(body);
    // One cell of padding inside each pane, so text never touches a border.
    let pad = |rect: Rect| Rect {
        x: rect.x.saturating_add(1).min(rect.right()),
        width: rect.width.saturating_sub(2),
        ..rect
    };
    if left_width == 0 || inner.width < 8 {
        return Panes {
            header,
            body,
            left: Rect { width: 0, ..inner },
            right: pad(inner),
            footer,
        };
    }
    let left_width = left_width
        .saturating_add(2)
        .min(inner.width.saturating_sub(1) / 2);
    let [left, _divider, right_pane] = Layout::horizontal([
        Constraint::Length(left_width),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);
    Panes {
        header,
        body,
        left: pad(left),
        right: pad(right_pane),
        footer,
    }
}

/// Which pane the keys move in; its title is drawn in the label color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Side {
    #[default]
    Left,
    Right,
}

/// Draws the box, the divider, and the pane titles on the top edge.
pub fn draw_box(buf: &mut Buffer, panes: &Panes, titles: [&str; 2], focus: Side, theme: &Theme) {
    if panes.body.width < 2 || panes.body.height < 2 {
        return;
    }
    let rule = theme.style(Role::Rule);
    let frame = Frame::for_theme(theme);
    Block::bordered()
        .border_set(frame.border)
        .border_style(rule)
        .render(panes.body, buf);
    let top = panes.body.y;
    let title_style = |side: Side| {
        if side == focus {
            theme.style(Role::Label).add_modifier(Modifier::BOLD)
        } else {
            theme.style(Role::Text)
        }
    };
    let right_title_x = if panes.left.width > 0 {
        // The divider sits one cell right of the left pane's padding.
        let divider = panes.left.right() + 1;
        let bottom = panes.body.bottom().saturating_sub(1);
        for y in top..=bottom {
            // The divider joins the box's top and bottom edges.
            let glyph = match y {
                y if y == top => frame.divider_top,
                y if y == bottom => frame.divider_bottom,
                _ => frame.divider,
            };
            if let Some(cell) = buf.cell_mut((divider, y)) {
                cell.set_symbol(glyph);
                cell.set_style(rule);
            }
        }
        let start = panes.body.x + 1;
        put_title(
            buf,
            start,
            top,
            titles[0],
            divider - start,
            title_style(Side::Left),
        );
        divider + 1
    } else {
        panes.body.x + 1
    };
    let room = panes.body.right().saturating_sub(1 + right_title_x);
    put_title(
        buf,
        right_title_x,
        top,
        titles[1],
        room,
        title_style(Side::Right),
    );
}

/// The box's glyphs, with the divider's joins to its top and bottom edges.
struct Frame {
    border: border::Set<'static>,
    divider_top: &'static str,
    divider: &'static str,
    divider_bottom: &'static str,
}

impl Frame {
    fn for_theme(theme: &Theme) -> Self {
        if is_ascii(theme) {
            Self {
                border: border::Set {
                    top_left: "+",
                    top_right: "+",
                    bottom_left: "+",
                    bottom_right: "+",
                    vertical_left: "|",
                    vertical_right: "|",
                    horizontal_top: "-",
                    horizontal_bottom: "-",
                },
                divider_top: "+",
                divider: "|",
                divider_bottom: "+",
            }
        } else {
            Self {
                border: border::ROUNDED,
                divider_top: "┬",
                divider: "│",
                divider_bottom: "┴",
            }
        }
    }
}

fn put_title(buf: &mut Buffer, x: u16, y: u16, title: &str, room: u16, style: Style) {
    if title.is_empty() || room < 3 {
        return;
    }
    let line = Line::from(Span::styled(format!(" {title} "), style));
    put(buf, x, y, room, &line);
}

/// What the two header rows say.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Header {
    /// The name, or a breadcrumb down to it.
    pub title: Line<'static>,
    /// One line, cut with `…`.
    pub description: String,
    /// Right-aligned beside the description, such as `8/9 agents · 2h44m · $4.10`.
    pub summary: String,
}

/// The name row, and under it the dim description with the summary right-aligned beside it.
pub fn draw_header(buf: &mut Buffer, area: Rect, header: &Header, theme: &Theme) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let width = area.width;
    let summary = header.summary.as_str();
    let description = crate::text::sanitize_line(&header.description);
    put(buf, area.x, area.y, width, &header.title);
    if area.height < 2 {
        return;
    }
    let dim = theme.style(Role::Dim);
    let summary_width = (str_width(summary) as u16).min(width / 2);
    let gap = if summary_width > 0 { 2 } else { 0 };
    let description_room = width.saturating_sub(summary_width + gap);
    let line = Line::from(Span::styled(description.trim().to_owned(), dim));
    put(buf, area.x, area.y + 1, description_room, &line);
    if summary_width > 0 {
        let text = right(summary, usize::from(summary_width));
        let x = area.right() - summary_width;
        put(
            buf,
            x,
            area.y + 1,
            summary_width,
            &Line::from(Span::styled(text, dim)),
        );
    }
}

/// What the footer row says.
#[derive(Debug, Clone, PartialEq)]
pub enum Footer {
    /// Key hints, each with how long it stays when room runs short: the lowest goes first,
    /// whole, and 0 never goes. The last hint is the way out, such as `esc back`.
    Hints(Vec<(String, u8)>),
    /// A line drawn as it is, such as the steer input.
    Line(Line<'static>),
}

impl Footer {
    pub fn hints(parts: &[(&str, u8)]) -> Self {
        Self::Hints(
            parts
                .iter()
                .map(|&(part, keep)| (part.to_owned(), keep))
                .collect(),
        )
    }
}

/// How every list and pane view leaves, so the footers say it the same way.
pub const ESC_BACK: &str = "esc back";

/// The footer, with an optional mark such as `1-24 of 34 ↓` at the right.
pub fn draw_footer(buf: &mut Buffer, area: Rect, footer: Footer, mark: &str, theme: &Theme) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let mark_width = (str_width(mark) as u16).min(area.width / 2);
    let gap = if mark_width > 0 { 2 } else { 0 };
    let room = area.width.saturating_sub(mark_width + gap);
    let line = match footer {
        Footer::Hints(parts) => {
            let dim = theme.style(Role::Dim);
            let parts: Vec<(String, u8)> = parts
                .into_iter()
                .map(|(part, keep)| (key_names(&part, theme), keep))
                .collect();
            let text = fit_hints(&parts, usize::from(room));
            Line::from(Span::styled(text, dim.add_modifier(Modifier::ITALIC)))
        }
        Footer::Line(line) => line,
    };
    put(buf, area.x, area.y, room, &line);
    if mark_width > 0 {
        let text = right(mark, usize::from(mark_width));
        let line = Line::from(Span::styled(text, theme.style(Role::Dim)));
        put(buf, area.right() - mark_width, area.y, mark_width, &line);
    }
}

/// A hint's arrow and Enter keys spelled out for the ASCII set: `up/down select`.
pub fn key_names(text: &str, theme: &Theme) -> String {
    if !is_ascii(theme) {
        return text.to_owned();
    }
    text.replace("↑↓", "up/down")
        .replace('↑', "up")
        .replace('↓', "down")
        .replace('←', "left")
        .replace('→', "right")
        .replace('⏎', "enter")
}

/// Room below which a cut hint says nothing, so the way out shows alone.
const LEAST_CUT: usize = 4;

/// `parts` joined by dots in `width` cells. Whole hints go first, from the lowest keep; if the
/// ones that never go still don't fit, those before the last are cut so the way out stays whole.
fn fit_hints(parts: &[(String, u8)], width: usize) -> String {
    let mut kept: Vec<&(String, u8)> = parts.iter().collect();
    let joined_width = |kept: &[&(String, u8)]| {
        kept.iter().map(|(part, _)| str_width(part)).sum::<usize>()
            + 3 * kept.len().saturating_sub(1)
    };
    while joined_width(&kept) > width {
        let Some(drop) = kept
            .iter()
            .enumerate()
            .filter(|(_, (_, keep))| *keep > 0)
            .min_by_key(|(i, (_, keep))| (*keep, usize::MAX - i))
            .map(|(i, _)| i)
        else {
            break;
        };
        kept.remove(drop);
    }
    let texts: Vec<&str> = kept.iter().map(|(part, _)| part.as_str()).collect();
    let joined = texts.join(" · ");
    if str_width(&joined) <= width {
        return joined;
    }
    let Some((last, rest)) = texts.split_last() else {
        return joined;
    };
    let tail = format!(" · {last}");
    let room = width.saturating_sub(str_width(&tail));
    if rest.is_empty() || room < LEAST_CUT {
        return cut(last, width);
    }
    format!("{}{tail}", cut(&rest.join(" · "), room))
}

/// Writes `line` at (x, y), cut with `…` to `width` cells.
pub fn put(buf: &mut Buffer, x: u16, y: u16, width: u16, line: &Line<'_>) {
    if width == 0 || y >= buf.area.bottom() || x >= buf.area.right() {
        return;
    }
    let width = width.min(buf.area.right() - x);
    let line = if spans_width(&line.spans) > usize::from(width) {
        truncate_line(line, usize::from(width))
    } else {
        line.clone()
    };
    buf.set_line(x, y, &line, width);
}

/// Draws `lines` into `area` from its top, one per row, each cut to the width.
pub fn put_lines(buf: &mut Buffer, area: Rect, lines: &[Line<'_>]) {
    for (row, line) in lines.iter().take(usize::from(area.height)).enumerate() {
        put(buf, area.x, area.y + row as u16, area.width, line);
    }
}

/// The glyphs the frames use beyond the theme's set, picked by whether the set is ASCII. Each is
/// one cell, except the nested workflow's `<>` in ASCII, which rows pad to [`Marks::slot`].
#[derive(Debug, Clone, Copy)]
pub struct Marks {
    pub done: &'static str,
    pub running: &'static str,
    pub waiting: &'static str,
    pub failed: &'static str,
    pub needs_you: &'static str,
    pub stopped: &'static str,
    pub nested: &'static str,
    pub phase_waiting: &'static str,
    pub selected: &'static str,
    pub expand: &'static str,
    pub stands_in: &'static str,
    pub debt: &'static str,
    pub warning: &'static str,
    pub crumb: &'static str,
    pub down: &'static str,
    pub up: &'static str,
    /// Where you type, such as steering text.
    pub cursor: &'static str,
}

impl Marks {
    pub fn for_theme(theme: &Theme) -> Self {
        let glyphs = &theme.glyphs;
        if is_ascii(theme) {
            Self {
                done: "v",
                running: "*",
                waiting: glyphs.agent_waiting,
                failed: "x",
                needs_you: glyphs.card,
                stopped: "-",
                nested: "<>",
                phase_waiting: ".",
                selected: glyphs.prompt,
                expand: ">",
                stands_in: "~",
                debt: "o",
                warning: "!",
                crumb: ">",
                down: "v",
                up: "^",
                cursor: "|",
            }
        } else {
            Self {
                done: "✓",
                running: "●",
                waiting: glyphs.agent_waiting,
                failed: "✗",
                needs_you: glyphs.card,
                stopped: "■",
                nested: "◇",
                phase_waiting: "·",
                selected: glyphs.prompt,
                // `⏎` is drawn two cells wide by some fonts, which breaks the frame around it.
                expand: "↵",
                stands_in: "↺",
                debt: "◌",
                warning: "!",
                crumb: "›",
                down: "↓",
                up: "↑",
                cursor: "▏",
            }
        }
    }

    /// Cells a row's state glyph takes, so rows with `<>` still line up.
    pub fn slot(&self) -> usize {
        [
            self.done,
            self.running,
            self.waiting,
            self.failed,
            self.needs_you,
            self.nested,
        ]
        .iter()
        .map(|glyph| str_width(glyph))
        .max()
        .unwrap_or(1)
    }

    /// A row's state as glyph, word, and color, which every row shows together.
    pub fn status(&self, status: RowStatus) -> (&'static str, &'static str, Role) {
        match status {
            RowStatus::Waiting => (self.waiting, "waiting", Role::Dim),
            RowStatus::Running => (self.running, "running", Role::Dim),
            RowStatus::NeedsYou => (self.needs_you, "needs you", Role::Card),
            RowStatus::Done => (self.done, "done", Role::Success),
            RowStatus::Failed => (self.failed, "failed", Role::Error),
        }
    }
}

fn is_ascii(theme: &Theme) -> bool {
    theme.glyphs.prompt.is_ascii()
}

/// `glyph` padded to `slot` cells.
pub fn pad_glyph(glyph: &str, slot: usize) -> String {
    format!(
        "{glyph}{}",
        " ".repeat(slot.saturating_sub(str_width(glyph)))
    )
}

/// Elapsed time for fixed slots: `45s`, `6m02s`, `2h44m`.
pub fn elapsed(ms: u64) -> String {
    let secs = ms / 1000;
    let (hours, minutes, seconds) = (secs / 3600, secs / 60 % 60, secs % 60);
    if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

/// The slot every duration takes: `12h34m` and `59m59s` both fit.
pub const ELAPSED_SLOT: usize = 6;

/// Tokens for a row: `747k tok`.
pub fn tokens(count: u64) -> String {
    format!("{} tok", format::tokens(count))
}

/// Spend in dollars, `~` when estimated and `+` when some usage had no price. `None` before
/// anything was priced.
pub fn spend(spent: &Spend) -> Option<String> {
    if spent.cost_usd <= 0.0 && !spent.cost_partial {
        return None;
    }
    Some(format::spend(spent))
}

/// `3 agents`, `1 agent`.
pub fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stability;
    use crate::theme::Glyphs;

    #[test]
    fn the_right_pane_keeps_half_the_box() {
        let panes = split(Rect::new(0, 0, 40, 20), 30);
        assert!(panes.right.width >= panes.left.width, "{panes:?}");
        assert_eq!(panes.header.height, 2);
        assert_eq!(panes.footer.y, 19);
        assert_eq!(panes.body, Rect::new(0, 3, 40, 16));

        let one = split(Rect::new(0, 0, 60, 10), 0);
        assert_eq!(one.left.width, 0);
        assert_eq!(one.right, Rect::new(2, 4, 56, 4));
    }

    #[test]
    fn the_frame_holds_at_every_width() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        for width in stability::WIDTHS {
            let area = Rect::new(0, 0, width, 12);
            let mut buf = Buffer::empty(area);
            let panes = split(area, 14);
            let header = Header {
                title: Line::from(stability::samples::CJK),
                description: stability::samples::EMOJI.to_owned(),
                summary: "8/9 agents · 2h44m · $4.10".to_owned(),
            };
            draw_header(&mut buf, panes.header, &header, &theme);
            draw_box(
                &mut buf,
                &panes,
                [stability::samples::CJK, &stability::samples::long_word()],
                Side::Right,
                &theme,
            );
            draw_footer(
                &mut buf,
                panes.footer,
                Footer::hints(&[("↑↓ select", 0), (ESC_BACK, 0)]),
                "1-4 of 9 ↓",
                &theme,
            );
            stability::assert_frames(&buf);
        }
    }

    fn fit(parts: &[(&str, u8)], width: usize) -> String {
        let Footer::Hints(parts) = Footer::hints(parts) else {
            unreachable!()
        };
        fit_hints(&parts, width)
    }

    #[test]
    fn hints_go_whole_by_keep_and_the_way_out_stays() {
        let parts = [("↑↓ move", 0), ("g group", 1), ("x stop", 2), (ESC_BACK, 0)];
        assert_eq!(fit(&parts, 80), "↑↓ move · g group · x stop · esc back");
        assert_eq!(fit(&parts, 28), "↑↓ move · x stop · esc back");
        assert_eq!(fit(&parts, 18), "↑↓ move · esc back");
        // Past the hints that go, the rest is cut and `esc back` stays whole.
        assert_eq!(fit(&parts, 17), "↑↓ mo… · esc back");
        assert_eq!(fit(&parts, 10), "esc back");
        assert_eq!(fit(&parts, 5), "esc …");
        assert_eq!(fit(&[], 5), "");
    }

    #[test]
    fn ascii_glyphs_draw_ascii_edges() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::ASCII;
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        let panes = split(area, 14);
        draw_box(&mut buf, &panes, ["", ""], Side::Left, &theme);
        for y in panes.body.y..panes.body.bottom() {
            let row: String = (0..area.width)
                .map(|x| buf[(x, y)].symbol().to_owned())
                .collect();
            assert!(row.is_ascii(), "{row:?}");
        }
        let top: String = (0..area.width)
            .map(|x| buf[(x, panes.body.y)].symbol().to_owned())
            .collect();
        assert!(top.starts_with("+-") && top.ends_with("-+"), "{top:?}");
    }

    #[test]
    fn durations_fit_their_slot() {
        assert_eq!(elapsed(45_000), "45s");
        assert_eq!(elapsed(362_000), "6m02s");
        assert_eq!(elapsed(9_840_000), "2h44m");
        assert!(str_width(&elapsed(45_240_000)) <= ELAPSED_SLOT);
    }
}
