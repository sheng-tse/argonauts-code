//! What a mod's page is made of: a breadcrumb with the file's path, a framed preview, and
//! groups of rows. A row is one setting with its choices on one line, the one in use in bold,
//! and where its value comes from at the right. Nothing here knows which mod it draws.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::base::draw::panes::{self, Footer};
use crate::base::kit::{Drawn, Kit, spread};
use crate::base::text::{spans_width, str_width, truncate_spans};
use crate::base::theme::{Role, Theme};

/// Cells a row's name takes, the pointer before it included.
const LABEL_CELLS: usize = 20;
/// Cells kept at the right for where a value comes from: `built-in`.
const ORIGIN_CELLS: usize = 10;

#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub title: &'static str,
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub label: &'static str,
    pub cells: Cells,
    /// `built-in`, `yours`, `project`, or `theme`.
    pub origin: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cells {
    /// One of a few, the one in use in bold.
    Choice {
        options: Vec<String>,
        current: usize,
    },
    /// Parts ticked on or off. `cursor` is the one the keys act on while the row is selected.
    Ticks { items: Vec<Tick>, cursor: usize },
    /// Words that say how something is, with nothing to change here.
    Words(Vec<Span<'static>>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tick {
    pub label: String,
    pub on: bool,
    /// Its place among the ticked ones, from 1, where the order matters.
    pub place: Option<usize>,
}

/// `1st`, `2nd`, `3rd`, `4th`.
pub fn ordinal(place: usize) -> String {
    let suffix = match (place % 10, place % 100) {
        (1, n) if n != 11 => "st",
        (2, n) if n != 12 => "nd",
        (3, n) if n != 13 => "rd",
        _ => "th",
    };
    format!("{place}{suffix}")
}

/// The glyphs the mod screens need beyond the kit's, in the kit's own choice between the
/// Unicode set and the ASCII one.
#[derive(Debug, Clone, Copy)]
pub struct PageGlyphs {
    pub crumb: &'static str,
    /// Before the row or the mod the keys act on, and the input's own prompt.
    pub pointer: &'static str,
    pub ticked: &'static str,
    pub unticked: &'static str,
    /// Before the CLI you're talking to, and before the others.
    pub dot: &'static str,
    pub hollow_dot: &'static str,
    /// A color swatch, and one item of a marks body.
    pub square: &'static str,
    /// Before the mode on the bottom line.
    pub mode: &'static str,
}

impl PageGlyphs {
    pub fn for_kit(kit: &Kit<'_>) -> Self {
        if kit.glyphs.across == "-" {
            Self {
                crumb: ">",
                pointer: ">",
                ticked: "[x]",
                unticked: "[ ]",
                dot: "*",
                hollow_dot: "o",
                square: "#",
                mode: ">",
            }
        } else {
            Self {
                crumb: "›",
                pointer: "❯",
                ticked: "☒",
                unticked: "☐",
                dot: "●",
                hollow_dot: "○",
                square: "■",
                mode: "⏵",
            }
        }
    }
}

/// `Config › Mods › bottom line`, the last name in the label color.
pub fn breadcrumb(kit: &Kit<'_>, names: &[&str]) -> Vec<Span<'static>> {
    let glyphs = PageGlyphs::for_kit(kit);
    let mut out = Vec::new();
    for (index, name) in names.iter().enumerate() {
        if index > 0 {
            out.push(kit.dim(format!(" {} ", glyphs.crumb)));
        }
        let role = if index + 1 == names.len() {
            Role::Question
        } else {
            Role::Bright
        };
        out.push(Span::styled((*name).to_owned(), kit.bold(role)));
    }
    out
}

fn cells_spans(kit: &Kit<'_>, cells: &Cells, selected: bool) -> Vec<Span<'static>> {
    let glyphs = PageGlyphs::for_kit(kit);
    let mut out = Vec::new();
    match cells {
        Cells::Choice { options, current } => {
            for (index, option) in options.iter().enumerate() {
                if index > 0 {
                    out.push(Span::raw("  "));
                }
                if index != *current {
                    out.push(kit.dim(option.clone()));
                } else if kit.has_color() {
                    out.push(Span::styled(option.clone(), kit.bold(Role::Question)));
                } else {
                    // Without color the brackets say which one is in use.
                    out.push(Span::styled(format!("[{option}]"), kit.bold(Role::Bright)));
                }
            }
        }
        Cells::Ticks { items, cursor } => {
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(Span::raw("  "));
                }
                let at_cursor = selected && index == *cursor;
                let mark = if item.on {
                    glyphs.ticked
                } else {
                    glyphs.unticked
                };
                let mut style = if item.on {
                    kit.bold(Role::Question)
                } else {
                    kit.style(Role::Dim)
                };
                if at_cursor {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                out.push(Span::styled(format!("{mark} {}", item.label), style));
                if let Some(place) = item.place.filter(|_| item.on) {
                    // Its place reads as part of the tick. The underline stays on the name,
                    // which is what the keys act on.
                    out.push(Span::styled(
                        format!(" {}", ordinal(place)),
                        kit.bold(Role::Question),
                    ));
                }
            }
        }
        Cells::Words(words) => out.extend(words.iter().cloned()),
    }
    out
}

/// One row, `width` cells: the pointer, the name, the choices, and the origin at the right.
fn row_line(kit: &Kit<'_>, row: &Row, selected: bool, width: usize) -> Line<'static> {
    let glyphs = PageGlyphs::for_kit(kit);
    let pointer = if selected { glyphs.pointer } else { " " };
    let label_style = if selected {
        kit.bold(Role::Question)
    } else {
        kit.style(Role::Text)
    };
    let name = format!("{pointer} {}", row.label);
    let pad = LABEL_CELLS.saturating_sub(str_width(&name)).max(1);
    let mut left = vec![Span::styled(name, label_style), Span::raw(" ".repeat(pad))];
    left.extend(cells_spans(kit, &row.cells, selected));
    let room = width.saturating_sub(ORIGIN_CELLS + 1);
    let left = if spans_width(&left) > room {
        truncate_spans(&left, room)
    } else {
        left
    };
    Line::from(spread(left, vec![kit.dim(row.origin)], width))
}

/// The groups as lines, with the selected row's index counted across them. `airy` puts a
/// blank row between two groups.
pub fn group_lines(
    kit: &Kit<'_>,
    groups: &[Group],
    selected: usize,
    width: usize,
    airy: bool,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut index = 0;
    for group in groups {
        if airy && !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.push(Line::from(kit.strong(group.title)));
        for row in &group.rows {
            lines.push(row_line(kit, row, index == selected, width));
            index += 1;
        }
    }
    lines
}

/// How many rows the groups hold.
pub fn row_count(groups: &[Group]) -> usize {
    groups.iter().map(|group| group.rows.len()).sum()
}

/// The row at `index`, counted across the groups.
pub fn row_at(groups: &[Group], index: usize) -> Option<&Row> {
    groups.iter().flat_map(|group| &group.rows).nth(index)
}

/// What a page shows above its rows.
pub struct Top<'a> {
    /// The names of the breadcrumb, such as `["Config", "Mods", "bottom line"]`.
    pub crumbs: &'a [&'a str],
    /// The file, at the right of the breadcrumb.
    pub path: &'a str,
    pub description: &'a str,
    /// The live preview, already drawn to the page's width.
    pub preview: Drawn,
    /// One dim line under the preview, such as where its numbers come from.
    pub caption: Vec<Span<'static>>,
}

/// Draws a page: the top, the groups, and the footer's hints. Rows that don't fit scroll with
/// the selected one.
pub fn draw_page(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    top: Top<'_>,
    groups: &[Group],
    selected: usize,
    footer: Footer,
) {
    if area.width < 4 || area.height < 3 {
        return;
    }
    let kit = Kit::new(theme);
    let inner = Rect {
        x: area.x + 1,
        width: area.width - 2,
        ..area
    };
    let width = usize::from(inner.width);
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(spread(
        breadcrumb(&kit, top.crumbs),
        vec![kit.dim(top.path.to_owned())],
        width,
    )));
    lines.push(Line::from(kit.dim(top.description.to_owned())));
    lines.push(Line::default());
    let fixed_top = lines.len();
    lines.extend(top.preview.lines);
    if !top.caption.is_empty() {
        lines.push(Line::from(top.caption));
    }
    lines.push(Line::default());
    let rows_from = lines.len();
    let room = usize::from(area.height.saturating_sub(2));
    // A blank row sets the groups apart where the screen has the rows for it.
    let gaps = groups.len().saturating_sub(1);
    let airy = rows_from + groups.len() + row_count(groups) + gaps <= room;
    let body = group_lines(&kit, groups, selected, width, airy);
    // The selected row's line among the body's: its index plus the titles and gaps above it.
    let selected_line = {
        let mut line = 0;
        let mut left = selected;
        for (index, group) in groups.iter().enumerate() {
            line += 1 + usize::from(airy && index > 0);
            if left < group.rows.len() {
                line += left;
                break;
            }
            left -= group.rows.len();
            line += group.rows.len();
        }
        line
    };
    lines.extend(body);
    // On a short screen the preview scrolls away before the selected row does.
    let at = rows_from + selected_line;
    let offset = if lines.len() <= room {
        0
    } else {
        (at + 2).saturating_sub(room).min(lines.len() - room)
    };
    let shown: Vec<Line<'static>> = if offset == 0 {
        lines.into_iter().take(room).collect()
    } else {
        // The breadcrumb and description stay put.
        let keep = fixed_top.min(room);
        let mut shown: Vec<Line<'static>> = lines.iter().take(keep).cloned().collect();
        shown.extend(lines.into_iter().skip(keep + offset).take(room - keep));
        shown
    };
    let rows = Rect {
        height: area.height.saturating_sub(2),
        ..inner
    };
    panes::put_lines(buf, rows, &shown);
    let footer_area = Rect {
        y: area.bottom() - 1,
        height: 1,
        ..inner
    };
    panes::draw_footer(buf, footer_area, footer, "", theme);
}

/// Moves `current` one step among `count` options, stopping at the ends.
pub fn step(current: usize, count: usize, forward: bool) -> usize {
    if forward {
        (current + 1).min(count.saturating_sub(1))
    } else {
        current.saturating_sub(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::theme::{ColorLevel, Theme};

    fn text(spans: &[Span<'_>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The page's own name in its breadcrumb is lavender, as the choices in
    /// use are.
    #[test]
    fn the_page_s_name_is_lavender() {
        let theme = Theme::dark();
        let kit = Kit::new(&theme);
        let crumbs = breadcrumb(&kit, &["Config", "Mods", "bottom line"]);
        let last = crumbs.last().expect("the page's name");
        assert_eq!(last.content, "bottom line");
        assert_eq!(last.style.fg, theme.style(Role::Question).fg);
    }

    #[test]
    fn places_read_as_ordinals() {
        let words: Vec<String> = [1, 2, 3, 4, 11, 12, 13, 21, 22, 23]
            .into_iter()
            .map(ordinal)
            .collect();
        assert_eq!(
            words,
            [
                "1st", "2nd", "3rd", "4th", "11th", "12th", "13th", "21st", "22nd", "23rd"
            ]
        );
    }

    #[test]
    fn without_color_brackets_mark_the_choice_in_use() {
        let theme = Theme::dark().with_level(ColorLevel::None);
        let kit = Kit::new(&theme);
        let cells = Cells::Choice {
            options: vec!["used".to_owned(), "left".to_owned()],
            current: 1,
        };
        assert_eq!(text(&cells_spans(&kit, &cells, false)), "used  [left]");
    }

    #[test]
    fn a_row_names_its_setting_its_parts_and_where_they_come_from() {
        let theme = Theme::dark();
        let kit = Kit::new(&theme);
        let row = Row {
            label: "Left",
            cells: Cells::Ticks {
                items: vec![
                    Tick {
                        label: "mode".to_owned(),
                        on: true,
                        place: Some(1),
                    },
                    Tick {
                        label: "checks".to_owned(),
                        on: false,
                        place: None,
                    },
                ],
                cursor: 0,
            },
            origin: "yours",
        };
        let line = row_line(&kit, &row, true, 60);
        let shown = text(&line.spans);
        assert!(
            shown.starts_with("❯ Left              ☒ mode 1st  ☐ checks"),
            "{shown}"
        );
        assert!(shown.ends_with("yours"), "{shown}");
        assert_eq!(str_width(&shown), 60);
        let narrow = text(&row_line(&kit, &row, false, 30).spans);
        assert_eq!(str_width(&narrow), 30);
        assert!(narrow.ends_with("yours"), "{narrow}");
    }

    /// A page of two groups with two rows each and no preview, as its lines of text.
    fn page(height: u16, selected: usize) -> Vec<String> {
        let theme = Theme::dark();
        let row = |label| Row {
            label,
            cells: Cells::Choice {
                options: vec!["on".to_owned(), "off".to_owned()],
                current: 0,
            },
            origin: "yours",
        };
        let groups = [
            Group {
                title: "Look",
                rows: vec![row("Form"), row("Body")],
            },
            Group {
                title: "Shows",
                rows: vec![row("Shown"), row("Places")],
            },
        ];
        let area = Rect::new(0, 0, 50, height);
        let mut buf = Buffer::empty(area);
        let top = Top {
            crumbs: &["Config", "Mods", "checks"],
            path: "checks.toml",
            description: "A stacked bar",
            preview: Drawn::new(),
            caption: Vec::new(),
        };
        let footer = Footer::hints(&[("esc back", 0)]);
        draw_page(&mut buf, area, &theme, top, &groups, selected, footer);
        (0..height)
            .map(|y| {
                let line: String = (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_owned())
                    .collect();
                line.trim().to_owned()
            })
            .collect()
    }

    #[test]
    fn groups_stand_apart_where_the_screen_has_the_rows_for_it() {
        let at =
            |lines: &[String], start: &str| lines.iter().position(|line| line.starts_with(start));
        let tall = page(13, 3);
        let shows = at(&tall, "Shows").unwrap();
        assert_eq!(tall[shows - 1], "");
        assert!(tall[shows - 2].starts_with("Body"), "{tall:?}");
        assert!(at(&tall, "❯ Places").is_some(), "{tall:?}");
        // One row short of that, the groups close up and everything still shows.
        let snug = page(12, 3);
        let shows = at(&snug, "Shows").unwrap();
        assert!(snug[shows - 1].starts_with("Body"), "{snug:?}");
        assert!(at(&snug, "❯ Places").is_some(), "{snug:?}");
        // Shorter still, the rows scroll and the selected one stays on screen.
        let short = page(8, 3);
        assert!(at(&short, "❯ Places").is_some(), "{short:?}");
        assert!(at(&short, "Config").is_some(), "{short:?}");
    }

    #[test]
    fn steps_stop_at_the_ends() {
        assert_eq!(step(0, 3, false), 0);
        assert_eq!(step(0, 3, true), 1);
        assert_eq!(step(2, 3, true), 2);
        assert_eq!(step(0, 0, true), 0);
    }
}
