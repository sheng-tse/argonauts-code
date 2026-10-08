//! The frame the workflow and agent views draw in the bottom region, under the conversation,
//! where the input, the mode line, and the panel were, as Claude Code's open
//! workflow view sits: a white rule, the name, the description with its totals, at most two
//! status lines, a blank row, one rounded white box with two panes, and the footer.
//!
//! The rule stops six columns short of the right edge, and everything under it sits one column
//! inside it on each side, as in Claude Code's captures.

use argonauts_core::views::{StatusKind, StatusLine};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Widget};

use crate::text::{marks, sanitize_line, spans_width, str_width, truncate_spans};
use crate::theme::{Role, Theme};
use crate::widgets::State;

use super::layout::cut;
use super::panes::{self, Footer, Panes};
use super::states;

/// The least the frame takes, rule to footer. Under it the frame takes the whole region.
pub const MIN_ROWS: u16 = 12;

/// Claude Code's view ends this many columns short of the right edge.
const RIGHT_MARGIN: u16 = 6;

/// Status lines under the description, at most.
pub const MAX_STATUS: usize = 2;

/// Rows from the rule to the footer on a screen `rows` tall: about half of it, as Claude
/// Code's takes (29 of 64 rows), at least [`MIN_ROWS`], and every row left when that doesn't
/// fit. Two rows stay above it, as they do above the input.
pub fn height(rows: u16) -> u16 {
    (rows / 2)
        .saturating_sub(3)
        .max(MIN_ROWS)
        .min(rows.saturating_sub(2))
}

/// Where the docked frame draws, inside the region it was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dock {
    pub rule: Rect,
    /// The status lines, zero to two rows.
    pub status: Rect,
    /// The name and description rows, the box, its panes, and the footer.
    pub panes: Panes,
}

/// Splits `area` for a frame with `status_rows` status lines. `left_width` is the left pane's
/// contents in cells, and 0 leaves one pane.
pub fn split(area: Rect, left_width: u16, status_rows: u16) -> Dock {
    let rule_width = if area.width > RIGHT_MARGIN * 4 {
        area.width - RIGHT_MARGIN
    } else {
        area.width
    };
    let rule = Rect::new(area.x, area.y, rule_width, area.height.min(1));
    // One column in from each end of the rule.
    let inner = Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: rule_width.saturating_sub(2),
        height: area.height.saturating_sub(1),
    };
    let status_rows = status_rows.min(MAX_STATUS as u16);
    let header_rows = 2.min(inner.height);
    let status = Rect {
        y: inner.y + header_rows,
        height: status_rows.min(inner.height - header_rows),
        ..inner
    };
    // The header and status lines take their rows; the blank row, box, and footer split the
    // rest as every pane frame does.
    let rest = Rect {
        y: status.bottom(),
        height: inner.bottom().saturating_sub(status.bottom()),
        ..inner
    };
    let mut panes = panes::split(
        Rect {
            y: rest.y.saturating_sub(2),
            height: rest.height + 2,
            ..rest
        },
        left_width,
    );
    panes.header = Rect {
        height: header_rows,
        ..inner
    };
    Dock {
        rule,
        status,
        panes,
    }
}

/// The white rule across the top.
pub fn draw_rule(buf: &mut Buffer, dock: &Dock, theme: &Theme) {
    if dock.rule.height == 0 {
        return;
    }
    let glyph = if theme.glyphs.prompt.is_ascii() {
        "-"
    } else {
        "─"
    };
    let style = marks::decoration(theme.style(Role::Bright));
    let line = Line::from(Span::styled(
        glyph.repeat(usize::from(dock.rule.width)),
        style,
    ));
    buf.set_line(dock.rule.x, dock.rule.y, &line, dock.rule.width);
}

/// What the header rows say.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Heading {
    /// From the run's workflow down to the one shown; the last is its own name.
    pub names: Vec<String>,
    /// What kind of workflow it is, dim after the name: such as `orchestrator workflow`.
    pub notes: Vec<String>,
    pub description: String,
    /// Such as `6/8 agents · 4m31s`, ending under the box's right border.
    pub totals: String,
}

/// The name in the accent color, with a nested workflow's parents dim before it, and under it
/// the dim description cut with `…`, one space, and the totals.
pub fn draw_heading(buf: &mut Buffer, dock: &Dock, heading: &Heading, theme: &Theme) {
    let area = dock.panes.header;
    if area.height == 0 || area.width == 0 {
        return;
    }
    let dim = theme.style(Role::Dim);
    let marks = panes::Marks::for_theme(theme);
    let names: Vec<String> = heading
        .names
        .iter()
        .map(|name| sanitize_line(name).trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect();
    let mut spans = Vec::new();
    if let Some((own, parents)) = names.split_last() {
        for parent in parents {
            spans.push(Span::styled(parent.clone(), dim));
            spans.push(Span::styled(format!(" {} ", marks.crumb), dim));
        }
        spans.push(Span::styled(own.clone(), theme.style(Role::Card)));
    }
    for note in &heading.notes {
        let note = sanitize_line(note);
        if !note.trim().is_empty() {
            spans.push(Span::styled(format!(" · {}", note.trim()), dim));
        }
    }
    let spans = truncate_spans(&spans, usize::from(area.width));
    panes::put(buf, area.x, area.y, area.width, &Line::from(spans));
    if area.height < 2 {
        return;
    }
    let width = usize::from(area.width);
    let totals = cut(&heading.totals, width / 2);
    let totals_width = str_width(&totals);
    let gap = usize::from(totals_width > 0);
    let room = width.saturating_sub(totals_width + gap);
    let description = sanitize_line(&heading.description);
    let mut text = cut(description.trim(), room);
    let used = str_width(&text);
    text.push_str(&" ".repeat(width.saturating_sub(used + totals_width)));
    text.push_str(&totals);
    panes::put(
        buf,
        area.x,
        area.y + 1,
        area.width,
        &Line::from(Span::styled(text, dim)),
    );
}

/// The status lines, dim, each with its mark: `◎` for the goal, `◆` in the needs-you color for
/// what needs you, as every view draws that state, and `›` for a note. How the run ended reads
/// alone.
pub fn draw_status(buf: &mut Buffer, dock: &Dock, lines: &[StatusLine], theme: &Theme) {
    let area = dock.status;
    let dim = theme.style(Role::Dim);
    let ascii = theme.glyphs.prompt.is_ascii();
    for (row, status) in lines.iter().take(usize::from(area.height)).enumerate() {
        let (mark, role) = match status.kind {
            StatusKind::Goal => (Some(if ascii { "@" } else { "◎" }), Role::Card),
            StatusKind::NeedsYou => {
                let (glyph, role) = states::mark(State::NeedsYou, theme);
                (Some(glyph), role)
            }
            StatusKind::Note => (Some(if ascii { ">" } else { "›" }), Role::Dim),
            StatusKind::Ended => (None, Role::Dim),
        };
        let mut spans = Vec::new();
        if let Some(mark) = mark {
            spans.push(Span::styled(mark.to_owned(), theme.style(role)));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            sanitize_line(&status.text).trim().to_owned(),
            dim,
        ));
        panes::put(
            buf,
            area.x,
            area.y + row as u16,
            area.width,
            &Line::from(spans),
        );
    }
}

/// The box in white with both pane titles bold white on its top edge, whichever pane has the
/// keys, and `mark`, such as `1–24 of 34 ↓`, on its bottom edge at the right.
pub fn draw_box(buf: &mut Buffer, dock: &Dock, titles: [&str; 2], mark: &str, theme: &Theme) {
    let body = dock.panes.body;
    if body.width < 2 || body.height < 2 {
        return;
    }
    let border = marks::decoration(theme.style(Role::Bright));
    let ascii = theme.glyphs.prompt.is_ascii();
    let set = if ascii {
        ratatui::symbols::border::Set {
            top_left: "+",
            top_right: "+",
            bottom_left: "+",
            bottom_right: "+",
            vertical_left: "|",
            vertical_right: "|",
            horizontal_top: "-",
            horizontal_bottom: "-",
        }
    } else {
        ratatui::symbols::border::ROUNDED
    };
    Block::bordered()
        .border_set(set)
        .border_style(border)
        .render(body, buf);
    let (top_join, side, bottom_join) = if ascii {
        ("+", "|", "+")
    } else {
        ("┬", "│", "┴")
    };
    let title = Style::new()
        .patch(theme.style(Role::Bright))
        .add_modifier(Modifier::BOLD);
    let top = body.y;
    let bottom = body.bottom().saturating_sub(1);
    let right_title_x = if dock.panes.left.width > 0 {
        let divider = dock.panes.left.right() + 1;
        for y in top..=bottom {
            let glyph = match y {
                y if y == top => top_join,
                y if y == bottom => bottom_join,
                _ => side,
            };
            if let Some(cell) = buf.cell_mut((divider, y)) {
                cell.set_symbol(glyph);
                cell.set_style(border);
            }
        }
        let start = body.x + 1;
        put_title(buf, start, top, titles[0], divider - start, title);
        divider + 1
    } else {
        body.x + 1
    };
    let room = body.right().saturating_sub(1 + right_title_x);
    put_title(buf, right_title_x, top, titles[1], room, title);
    if !mark.is_empty() && body.height > 1 {
        let text = format!(" {mark} ");
        let width = str_width(&text) as u16;
        if width + 4 < body.width {
            let x = body.right() - 2 - width;
            let line = Line::from(Span::styled(text, theme.style(Role::Dim)));
            panes::put(buf, x, bottom, width, &line);
        }
    }
}

/// Takes `rows` off the bottom of the box for a second box under it, such as the goal panel,
/// and says where that box goes. `None`, and nothing taken, when the first box would be left
/// with fewer than `keep` rows.
pub fn carve_below(dock: &mut Dock, rows: u16, keep: u16) -> Option<Rect> {
    let body = dock.panes.body;
    if rows < 3 || body.height < rows.saturating_add(keep) {
        return None;
    }
    let shorter = |rect: Rect| Rect {
        height: rect.height.saturating_sub(rows),
        ..rect
    };
    dock.panes.body = shorter(body);
    dock.panes.left = shorter(dock.panes.left);
    dock.panes.right = shorter(dock.panes.right);
    Some(Rect {
        y: dock.panes.body.bottom(),
        height: rows,
        ..body
    })
}

/// A second box in the frame, drawn as the first is: `title` on its top edge at the left and
/// `note` at the right. Returns where its rows go, one cell in from each border.
pub fn draw_panel(
    buf: &mut Buffer,
    area: Rect,
    title: &[Span<'static>],
    note: &[Span<'static>],
    theme: &Theme,
) -> Rect {
    if area.width < 4 || area.height < 2 {
        return Rect::default();
    }
    let border = marks::decoration(theme.style(Role::Bright));
    let set = if theme.glyphs.prompt.is_ascii() {
        ASCII_BORDER
    } else {
        ratatui::symbols::border::ROUNDED
    };
    Block::bordered()
        .border_set(set)
        .border_style(border)
        .render(area, buf);
    let edge = |buf: &mut Buffer, x: u16, spans: &[Span<'static>]| {
        let mut text = vec![Span::raw(" ")];
        text.extend(spans.iter().cloned());
        text.push(Span::raw(" "));
        let width = spans_width(&text) as u16;
        for cell_x in x..x.saturating_add(width) {
            if let Some(cell) = buf.cell_mut((cell_x, area.y)) {
                cell.reset();
            }
        }
        panes::put(buf, x, area.y, width, &Line::from(text));
    };
    let room = usize::from(area.width.saturating_sub(4));
    let title = truncate_spans(title, room);
    let title_width = spans_width(&title);
    if title_width > 0 {
        edge(buf, area.x + 1, &title);
    }
    // The note needs its own two spaces and three cells of border after the title.
    let note_width = spans_width(note);
    if note_width > 0 && title_width + 2 + 3 + note_width + 2 < room + 2 {
        // One cell of border stays between the note and the corner.
        let x = area.right().saturating_sub(note_width as u16 + 4);
        edge(buf, x, note);
    }
    Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width - 4,
        height: area.height - 2,
    }
}

const ASCII_BORDER: ratatui::symbols::border::Set = ratatui::symbols::border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

/// A dim note at the right end of the box's top edge, such as who does a phase's work. It is
/// left out when the right pane's title, `title_width` cells of it, leaves it no room.
pub fn draw_title_note(
    buf: &mut Buffer,
    dock: &Dock,
    note: &str,
    title_width: usize,
    theme: &Theme,
) {
    let body = dock.panes.body;
    let note = sanitize_line(note);
    let note = note.trim();
    if note.is_empty() || body.width < 4 || body.height == 0 {
        return;
    }
    let text = format!(" {note} ");
    let width = str_width(&text);
    let pane_start = if dock.panes.left.width > 0 {
        usize::from(dock.panes.left.right()) + 2
    } else {
        usize::from(body.x) + 1
    };
    // The title's two spaces, and at least three cells of border between the two.
    let free_from = pane_start + title_width + 2 + 3;
    let Some(x) = usize::from(body.right()).checked_sub(2 + width) else {
        return;
    };
    if x < free_from {
        return;
    }
    let Ok(x) = u16::try_from(x) else {
        return;
    };
    for cell_x in x..x.saturating_add(width as u16) {
        if let Some(cell) = buf.cell_mut((cell_x, body.y)) {
            cell.reset();
        }
    }
    let line = Line::from(Span::styled(text, theme.style(Role::Dim)));
    panes::put(buf, x, body.y, width as u16, &line);
}

/// A title on the top edge, ` Phases `, in its own style rather than the border's, so the
/// border's decoration mark doesn't carry into it.
fn put_title(buf: &mut Buffer, x: u16, y: u16, title: &str, room: u16, style: Style) {
    if title.is_empty() || room < 3 {
        return;
    }
    let text = cut(
        &format!(" {} ", sanitize_line(title).trim()),
        usize::from(room),
    );
    let width = str_width(&text) as u16;
    for cell_x in x..x.saturating_add(width) {
        if let Some(cell) = buf.cell_mut((cell_x, y)) {
            cell.reset();
        }
    }
    panes::put(buf, x, y, room, &Line::from(Span::styled(text, style)));
}

/// The footer, dim italic, from the box's left edge.
pub fn draw_footer(buf: &mut Buffer, dock: &Dock, footer: Footer, theme: &Theme) {
    panes::draw_footer(buf, dock.panes.footer, footer, "", theme);
}

/// `1–24 of 34 ↓` for a list `total` long showing `shown` rows from `first`, or nothing when
/// it all fits.
pub fn position(first: usize, shown: usize, total: usize, theme: &Theme) -> String {
    if total <= shown || shown == 0 {
        return String::new();
    }
    let marks = panes::Marks::for_theme(theme);
    let dash = if theme.glyphs.prompt.is_ascii() {
        "-"
    } else {
        "–"
    };
    let end = (first + shown).min(total);
    let more = if end < total {
        format!(" {}", marks.down)
    } else {
        String::new()
    };
    format!("{}{dash}{end} of {total}{more}", first + 1)
}

/// What the last row of a panel says while the answer goes on above it.
pub const PANEL_HINT: &str = "esc closes this · the answer keeps running";

/// A view made for the whole screen, drawn as a panel under the working line because an
/// answer was running when it opened: Claude Code's dialog rule in the card
/// color, the view in the rows under it, and while the answer still runs a last row saying
/// Esc closes the panel and not the answer.
pub fn render_in_panel(
    view: &mut dyn super::View,
    area: Rect,
    buf: &mut Buffer,
    ctx: &super::ViewCtx<'_>,
    answer_running: bool,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let theme = ctx.theme;
    let rule = Line::from(marks::decorative(
        theme.glyphs.rule.repeat(usize::from(area.width)),
        theme.style(Role::Card),
    ));
    buf.set_line(area.x, area.y, &rule, area.width);
    let hint_rows = u16::from(answer_running && area.height > 3);
    let inner = Rect {
        y: area.y + 1,
        height: area.height - 1 - hint_rows,
        ..area
    };
    view.render(inner, buf, ctx);
    if hint_rows == 1 {
        let hint = Line::from(Span::styled(
            format!("  {PANEL_HINT}"),
            theme.style(Role::Dim),
        ));
        buf.set_line(area.x, area.bottom() - 1, &hint, area.width);
    }
}

/// The frame's rows as lines, for the bottom region to paint.
pub fn lines(buf: &Buffer) -> Vec<Line<'static>> {
    let area = buf.area;
    (area.top()..area.bottom())
        .map(|y| {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut x = area.left();
            while x < area.right() {
                let cell = &buf[(x, y)];
                let symbol = cell.symbol();
                let style = cell.style();
                match spans.last_mut() {
                    Some(last) if last.style == style => last.content.to_mut().push_str(symbol),
                    _ => spans.push(Span::styled(symbol.to_owned(), style)),
                }
                // A wide glyph's cell covers the next one, which the buffer left blank.
                x = x.saturating_add(str_width(symbol).max(1) as u16);
            }
            Line::from(spans)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stability;
    use crate::theme::Glyphs;

    #[test]
    fn the_frame_takes_about_half_the_screen() {
        assert_eq!(height(64), 29);
        assert_eq!(height(66), 30);
        assert_eq!(height(24), MIN_ROWS);
        // A short screen gives the frame every row but the two kept above it.
        assert_eq!(height(12), 10);
    }

    #[test]
    fn the_rule_stops_short_and_the_box_sits_inside_it() {
        let dock = split(Rect::new(0, 0, 100, 20), 18, 1);
        assert_eq!(dock.rule, Rect::new(0, 0, 94, 1));
        assert_eq!(dock.panes.header, Rect::new(1, 1, 92, 2));
        assert_eq!(dock.status, Rect::new(1, 3, 92, 1));
        assert_eq!(dock.panes.body.x, 1);
        assert_eq!(dock.panes.body.right(), 93);
        // A blank row between the status line and the box.
        assert_eq!(dock.panes.body.y, 5);
        assert_eq!(dock.panes.footer, Rect::new(1, 19, 92, 1));
    }

    #[test]
    fn the_frame_holds_at_every_width() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        for width in stability::WIDTHS {
            let area = Rect::new(0, 0, width, 14);
            let mut buf = Buffer::empty(area);
            let dock = split(area, 14, 2);
            draw_rule(&mut buf, &dock, &theme);
            let heading = Heading {
                names: vec![stability::samples::CJK.to_owned()],
                description: stability::samples::EMOJI.to_owned(),
                totals: "8/9 agents · 2h44m".to_owned(),
                ..Heading::default()
            };
            draw_heading(&mut buf, &dock, &heading, &theme);
            draw_box(
                &mut buf,
                &dock,
                [stability::samples::CJK, &stability::samples::long_word()],
                "1–4 of 9 ↓",
                &theme,
            );
            stability::assert_frames(&buf);
        }
    }

    /// A view made for the whole screen keeps inside the panel's rows at every width: the rule
    /// first, the hint last while the answer runs, and the view between them.
    #[test]
    fn a_whole_screen_view_fits_in_a_panel_at_every_width() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let state = crate::state::AppState::default();
        let ctx = crate::views::ViewCtx {
            theme: &theme,
            state: &state,
            now: std::time::Instant::now(),
        };
        let mut help = crate::views::help::HelpView::new(crate::keymap::Keymap::default());
        for width in stability::WIDTHS {
            for running in [true, false] {
                let area = Rect::new(0, 0, width, MIN_ROWS);
                let mut buf = Buffer::empty(area);
                render_in_panel(&mut help, area, &mut buf, &ctx, running);
                stability::assert_frames(&buf);
                let rows: Vec<String> = lines(&buf)
                    .iter()
                    .map(|line| line.to_string().trim_end().to_owned())
                    .collect();
                assert!(rows[0].chars().all(|c| c == '─'), "{width}: {rows:#?}");
                let hint = rows
                    .last()
                    .is_some_and(|row| row.contains("esc closes this"));
                assert_eq!(hint, running, "{width}: {rows:#?}");
                assert!(rows[1..].iter().any(|row| !row.is_empty()), "{rows:#?}");
            }
        }
    }

    #[test]
    fn the_description_ends_one_space_before_the_totals() {
        let theme = Theme::dark();
        let area = Rect::new(0, 0, 60, 14);
        let mut buf = Buffer::empty(area);
        let dock = split(area, 10, 0);
        let heading = Heading {
            names: vec!["fix-it".to_owned()],
            description: "a description far too long for the row it has".to_owned(),
            totals: "6/8 agents · 4m31s".to_owned(),
            ..Heading::default()
        };
        draw_heading(&mut buf, &dock, &heading, &theme);
        let row: String = (0..area.width)
            .map(|x| buf[(x, 2)].symbol().to_owned())
            .collect();
        assert!(row.trim_end().ends_with("… 6/8 agents · 4m31s"), "{row:?}");
        // The totals end under the box's right border.
        assert_eq!(
            str_width(row.trim_end()),
            usize::from(dock.panes.body.right())
        );
    }

    /// What needs you is marked as that state is in every view, so a theme that moves the
    /// state's color moves this line's mark too.
    #[test]
    fn a_line_that_needs_you_carries_that_states_mark_in_its_color() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let area = Rect::new(0, 0, 60, 14);
        let mut buf = Buffer::empty(area);
        let dock = split(area, 10, 1);
        let line = StatusLine {
            kind: StatusKind::NeedsYou,
            text: "the plan approval".to_owned(),
        };
        draw_status(&mut buf, &dock, &[line], &theme);
        let mark = &buf[(dock.status.x, dock.status.y)];
        assert_eq!(mark.symbol(), "◆");
        assert_eq!(mark.fg, theme.style(Role::StateNeedsYou).fg.unwrap());
    }

    #[test]
    fn lines_keep_wide_glyphs_whole() {
        let area = Rect::new(0, 0, 6, 1);
        let mut buf = Buffer::empty(area);
        buf.set_string(0, 0, "a漢b", Style::new());
        let lines = lines(&buf);
        let text: String = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(text, "a漢b  ");
    }
}
