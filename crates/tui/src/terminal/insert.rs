//! Adapted from Codex's `insert_history.rs` (Apache-2.0). History that fits above the live
//! region prints into the blank rows under the last history row. History that would cover the
//! live region goes out with line feeds through the whole screen, the one scroll every terminal
//! keeps in its scrollback. Some drop rows scrolled out of a region.

use std::borrow::Cow;
use std::io::{self, Write};

use crossterm::cursor::{MoveTo, RestorePosition, SavePosition};
use crossterm::queue;
use crossterm::style::Print;
use crossterm::terminal::{Clear, ClearType};
use ratatui::layout::Size;
use ratatui::style::Color;
use ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

use super::sgr::Pen;

/// Where [`insert_lines`] left history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inserted {
    /// The row under the last history row.
    pub end: u16,
    /// The live region's rows were erased, so the next frame has to draw all of it.
    pub cleared: bool,
}

/// Prints `lines` from row `end`, under the history already on screen, leaving `live` rows at
/// the bottom for the live region. The cursor position is restored, so the call is invisible
/// to the diff renderer.
pub fn insert_lines<W: Write>(
    writer: &mut W,
    lines: &[Line<'_>],
    end: u16,
    live: u16,
    screen: Size,
) -> io::Result<Inserted> {
    if lines.is_empty() || screen.width == 0 || screen.height == 0 {
        return Ok(Inserted {
            end,
            cleared: false,
        });
    }
    let width = screen.width;
    // Lines arrive wrapped, but a line wider than the screen wraps again in the terminal, and
    // history ends after the rows really printed.
    let rows: u16 = lines
        .iter()
        .map(|line| physical_rows(line, width))
        .fold(0u16, u16::saturating_add);
    let room = screen.height.saturating_sub(live).saturating_sub(end);

    queue!(writer, SavePosition)?;
    let inserted = if rows <= room {
        print_into_room(writer, lines, end, rows, screen)?
    } else {
        print_through_screen(writer, lines, end, live, screen)?
    };
    queue!(writer, RestorePosition)?;
    Ok(inserted)
}

/// The rows under history may still show trailing live rows, so each is cleared before the
/// lines print straight in.
fn print_into_room<W: Write>(
    writer: &mut W,
    lines: &[Line<'_>],
    end: u16,
    rows: u16,
    screen: Size,
) -> io::Result<Inserted> {
    for row in end..end + rows {
        queue!(writer, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
    }
    queue!(writer, MoveTo(0, end))?;
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            queue!(writer, Print("\r\n"))?;
        }
        write_line(writer, line, screen.width)?;
    }
    Ok(Inserted {
        end: end + rows,
        cleared: false,
    })
}

/// Codex's full screen strategy: clear from the end of history down, print from there, then
/// step through fresh rows for the live region, so history ends right above it. The lines
/// overflow the room above the region, so the screen scrolls and the region ends at the bottom.
fn print_through_screen<W: Write>(
    writer: &mut W,
    lines: &[Line<'_>],
    end: u16,
    live: u16,
    screen: Size,
) -> io::Result<Inserted> {
    if end < screen.height {
        queue!(writer, MoveTo(0, end), Clear(ClearType::FromCursorDown))?;
    }
    // Each line starts with a line feed from the row above it. That also covers history that
    // already reaches the screen's last row, under which the cursor can't go.
    let mut feed = end > 0;
    if feed {
        queue!(writer, MoveTo(0, end - 1))?;
    }
    for line in lines {
        if feed {
            queue!(writer, Print("\r\n"))?;
        }
        write_line(writer, line, screen.width)?;
        feed = true;
    }
    for _ in 0..live {
        queue!(writer, Print("\r\n"), Clear(ClearType::UntilNewLine))?;
    }
    Ok(Inserted {
        end: screen.height.saturating_sub(live),
        cleared: true,
    })
}

/// Control characters are dropped as a last defense. A line whose style has a background fills
/// the rest of the row with it, unless the row is already full, where erasing would eat the
/// last cell.
pub fn write_line<W: Write>(writer: &mut W, line: &Line<'_>, width: u16) -> io::Result<()> {
    let mut pen = Pen::default();
    // Start from the default colors, so a background left over from before can't bleed in.
    pen.reset(writer)?;
    queue!(writer, Clear(ClearType::UntilNewLine))?;

    let mut used = 0usize;
    for span in &line.spans {
        let text = printable(&span.content);
        if text.is_empty() {
            continue;
        }
        let style = line.style.patch(span.style);
        let modifier = style.add_modifier - style.sub_modifier;
        pen.set(
            writer,
            style.fg.unwrap_or(Color::Reset),
            style.bg.unwrap_or(Color::Reset),
            modifier,
        )?;
        print_with_link(writer, &text)?;
        used += text.width();
    }

    if let Some(fill) = line.style.bg.filter(|bg| *bg != Color::Reset)
        && used < usize::from(width)
    {
        pen.prepare_erase(writer, fill)?;
        queue!(writer, Clear(ClearType::UntilNewLine))?;
    }
    pen.finish(writer)
}

/// Prints `text`, with an artifact's whole address in it marked as a link (OSC 8), so a
/// terminal that knows links opens it on a click. The address is the text itself, so one
/// that doesn't know them shows the same row.
fn print_with_link<W: Write>(writer: &mut W, text: &str) -> io::Result<()> {
    let Some(link) = argonauts_core::artifacts::whole_link_in(text) else {
        return queue!(writer, Print(text));
    };
    let start = text.find(link).unwrap_or(0);
    let (before, after) = (&text[..start], &text[start + link.len()..]);
    queue!(
        writer,
        Print(before),
        Print(format!("\x1b]8;;{link}\x1b\\")),
        Print(link),
        Print("\x1b]8;;\x1b\\"),
        Print(after)
    )
}

/// Counts only what [`write_line`] prints.
pub(super) fn physical_rows(line: &Line<'_>, width: u16) -> u16 {
    let columns: usize = line
        .spans
        .iter()
        .map(|span| printable(&span.content).width())
        .sum();
    let rows = columns.max(1).div_ceil(usize::from(width.max(1)));
    u16::try_from(rows).unwrap_or(u16::MAX)
}

/// `text` without control characters, which would move the cursor or restyle the terminal.
fn printable(text: &str) -> Cow<'_, str> {
    if text.chars().any(char::is_control) {
        Cow::Owned(text.chars().filter(|c| !c.is_control()).collect())
    } else {
        Cow::Borrowed(text)
    }
}
