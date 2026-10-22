//! Reads the pictures golden tests compare screens with. A picture is two files: a `.txt` of its
//! characters, one per terminal cell, and an `.html` page that draws the same characters in
//! spans whose styles give each cell its colour, fill and weight.
//!
//! This reads the cells and their styles as the page writes them. What a colour stands for is
//! the comparison's to say, since each screen's pictures name their colours their own way.

use std::path::Path;

/// One cell of a picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    /// The text's colour as the page writes it, such as `var(--term-dim)` or `#b1b9f9`; `None`
    /// for the terminal's own.
    pub color: Option<String>,
    /// The fill behind the cell, written the same way.
    pub background: Option<String>,
    pub bold: bool,
}

impl Cell {
    /// A cell in the terminal's own colours.
    pub fn plain(ch: char) -> Cell {
        Cell {
            ch,
            color: None,
            background: None,
            bold: false,
        }
    }
}

/// A picture's rows of cells.
pub type Grid = Vec<Vec<Cell>>;

/// The `.txt` and the `.html` of the picture `name` in `dir`.
pub fn read(dir: &Path, name: &str) -> (String, String) {
    let file = |ext: &str| {
        let path = dir.join(format!("{name}.{ext}"));
        std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
    };
    (file("txt"), file("html"))
}

/// The cells of the page's first grid, row by row: what its first `<pre>` holds, or the whole
/// page where it has none. A grid that ends with a line end ends with an empty row.
pub fn grid(html: &str) -> Grid {
    match grid_starts(html).next() {
        Some(start) => cells(&html[start..]),
        None => cells(html),
    }
}

/// Every grid on the page, in order. A picture of a screen at more than one moment draws each
/// in a `<pre>` of its own.
pub fn grids(html: &str) -> Vec<Grid> {
    grid_starts(html)
        .map(|start| cells(&html[start..]))
        .collect()
}

/// The characters of `row`.
pub fn text(row: &[Cell]) -> String {
    row.iter().map(|cell| cell.ch).collect()
}

/// A page that draws `grid`, which reads back as the same cells. For a test that changes a
/// picture where it stands, or compares a part of one.
pub fn page(grid: &[Vec<Cell>]) -> String {
    let mut page = String::from("<pre class=\"term grid\">");
    for (at, row) in grid.iter().enumerate() {
        if at > 0 {
            page.push('\n');
        }
        // Cells in the same style share one span, as the pictures write them.
        let mut cells = row.iter().peekable();
        while let Some(first) = cells.next() {
            let style = style_of(first);
            let mut text = escaped(first.ch);
            while let Some(cell) = cells.next_if(|cell| style_of(cell) == style) {
                text.push_str(&escaped(cell.ch));
            }
            if style.is_empty() {
                page.push_str(&text);
            } else {
                page.push_str(&format!("<span style=\"{style}\">{text}</span>"));
            }
        }
    }
    page.push_str("</pre>");
    page
}

/// The page of `html`'s first grid with `edit` made to it. For a test that a comparison finds
/// one changed cell.
pub fn edited(html: &str, edit: impl FnOnce(&mut Grid)) -> String {
    let mut cells = grid(html);
    edit(&mut cells);
    page(&cells)
}

/// The row and column of the first cell `wanted` picks.
pub fn find(grid: &[Vec<Cell>], wanted: impl Fn(&Cell) -> bool) -> (usize, usize) {
    grid.iter()
        .enumerate()
        .find_map(|(row, cells)| cells.iter().position(&wanted).map(|column| (row, column)))
        .expect("a cell the picture has")
}

/// Where each `<pre>`'s text starts: after its tag.
fn grid_starts(html: &str) -> impl Iterator<Item = usize> + '_ {
    html.match_indices("<pre")
        .filter(|(at, tag)| {
            let next = html[at + tag.len()..].chars().next();
            next.is_some_and(|c| c == '>' || c.is_whitespace())
        })
        .filter_map(|(at, _)| html[at..].find('>').map(|end| at + end + 1))
}

/// The cells of `body` up to its `</pre>`: text between spans is in the terminal's own colours,
/// and a span's text in its style, over the style of any span it sits in.
fn cells(body: &str) -> Grid {
    let body = &body[..body.find("</pre>").unwrap_or(body.len())];
    let mut rows = vec![Vec::new()];
    let mut styles: Vec<Style> = Vec::new();
    let mut rest = body;
    while let Some(c) = rest.chars().next() {
        if let Some(after) = rest.strip_prefix("</span>") {
            styles.pop();
            rest = after;
            continue;
        }
        if let Some(tag) = rest.strip_prefix("<span")
            && tag.starts_with(['>', ' '])
        {
            let end = tag.find('>').expect("a span's tag ends");
            let outer = styles.last().cloned().unwrap_or_default();
            styles.push(outer.under(&attribute(&tag[..end], "style")));
            rest = &tag[end + 1..];
            continue;
        }
        let (ch, len) = entity(rest).unwrap_or((c, c.len_utf8()));
        rest = &rest[len..];
        if ch == '\n' {
            rows.push(Vec::new());
            continue;
        }
        let style = styles.last().cloned().unwrap_or_default();
        rows.last_mut().expect("a row").push(Cell {
            ch,
            color: style.color,
            background: style.background,
            bold: style.bold,
        });
    }
    rows
}

/// What a span's style says of its text.
#[derive(Debug, Clone, Default)]
struct Style {
    color: Option<String>,
    background: Option<String>,
    bold: bool,
}

impl Style {
    /// `css`, such as `color:var(--term-dim);font-weight:700`, over this style.
    fn under(mut self, css: &str) -> Style {
        for part in css.split(';') {
            let Some((key, value)) = part.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "color" => self.color = Some(value.to_owned()),
                "background" => self.background = Some(value.to_owned()),
                "font-weight" => self.bold = value == "700" || value == "bold",
                _ => {}
            }
        }
        self
    }
}

/// The value of the attribute `name` in a tag's text.
fn attribute(tag: &str, name: &str) -> String {
    let key = format!("{name}=\"");
    tag.find(&key)
        .map(|at| {
            let value = &tag[at + key.len()..];
            value[..value.find('"').unwrap_or(value.len())].to_owned()
        })
        .unwrap_or_default()
}

/// The character an entity at the start of `text` stands for, and the entity's length.
fn entity(text: &str) -> Option<(char, usize)> {
    let end = text.strip_prefix('&')?.find(';')? + 1;
    let name = &text[1..end];
    let ch = match name {
        "lt" => '<',
        "gt" => '>',
        "amp" => '&',
        "quot" => '"',
        "apos" => '\'',
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)?
        }
    };
    Some((ch, end + 1))
}

fn style_of(cell: &Cell) -> String {
    let mut style = Vec::new();
    if let Some(color) = &cell.color {
        style.push(format!("color:{color}"));
    }
    if let Some(background) = &cell.background {
        style.push(format!("background:{background}"));
    }
    if cell.bold {
        style.push("font-weight:700".to_owned());
    }
    style.join(";")
}

fn escaped(ch: char) -> String {
    match ch {
        '<' => "&lt;".to_owned(),
        '>' => "&gt;".to_owned(),
        '&' => "&amp;".to_owned(),
        '"' => "&quot;".to_owned(),
        '\'' => "&#x27;".to_owned(),
        ch => ch.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "<div class=\"scroll\"><pre class=\"term grid\"><span style=\"color:var(--term-dim)\">\
                        ● </span>you&#x27;re &amp; <span style=\"color:#b1b9f9;font-weight:700\">ok</span>\n\
                        <span style=\"background:var(--term-band)\">&lt;a&gt;</span>\n</pre></div>";

    #[test]
    fn a_page_reads_as_its_cells_with_their_styles() {
        let grid = grid(PAGE);
        let rows: Vec<String> = grid.iter().map(|row| text(row)).collect();
        assert_eq!(rows, ["● you're & ok", "<a>", ""]);
        assert_eq!(grid[0][0].color.as_deref(), Some("var(--term-dim)"));
        assert_eq!(grid[0][2], Cell::plain('y'));
        let bold = &grid[0][11];
        assert_eq!(
            (bold.ch, bold.color.as_deref(), bold.bold),
            ('o', Some("#b1b9f9"), true)
        );
        assert_eq!(grid[1][1].background.as_deref(), Some("var(--term-band)"));
        assert_eq!(grid[1][1].color, None);
    }

    #[test]
    fn a_span_inside_another_keeps_what_it_doesnt_say() {
        let grid = grid(
            "<pre><span style=\"font-weight:700\">a<span style=\"color:var(--term-acc)\">b</span>c</span>d</pre>",
        );
        let looks: Vec<(char, Option<&str>, bool)> = grid[0]
            .iter()
            .map(|cell| (cell.ch, cell.color.as_deref(), cell.bold))
            .collect();
        assert_eq!(
            looks,
            [
                ('a', None, true),
                ('b', Some("var(--term-acc)"), true),
                ('c', None, true),
                ('d', None, false),
            ]
        );
    }

    #[test]
    fn each_grid_on_a_page_reads_by_itself() {
        let page =
            "<pre class=\"term grid\">one</pre><p>between</p><pre class=\"term grid\">two\n</pre>";
        let grids = grids(page);
        assert_eq!(grids.len(), 2);
        assert_eq!(text(&grids[0][0]), "one");
        assert_eq!(text(&grids[1][0]), "two");
        assert_eq!(grid(page), grids[0]);
    }

    #[test]
    fn a_page_without_a_grid_reads_whole() {
        assert_eq!(text(&grid("plain")[0]), "plain");
    }

    #[test]
    fn a_grid_written_as_a_page_reads_back_the_same() {
        let read = grid(PAGE);
        assert_eq!(grid(&page(&read)), read);
    }
}
