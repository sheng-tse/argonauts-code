//! A small table for what a question asks about: merge rows, fallback candidates, plan scores.
//! Columns are measured in display cells. On a narrow screen any column that may cut (an
//! agent's name) and then the one flexible column (a title) are cut with `…` down to a floor,
//! and then the least useful columns go.

use ratatui::text::{Line, Span};

use crate::text::{fit_lines, sanitize_line, str_width};
use crate::theme::{Role, Theme};
use crate::views::layout::{cut, left, right};

const GAP: usize = 2;

/// The flexible column keeps at least this many cells before other columns start to go.
const FLEX_FLOOR: usize = 16;

#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<TableCell>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub header: String,
    /// Higher goes first on a narrow screen, and 0 never goes.
    pub drop_rank: u8,
    /// The one column that shrinks to fit, cut with `…`.
    pub flex: bool,
    /// Cut with `…` down to this many cells, before the flexible column and before any
    /// column goes.
    pub cut_to: Option<usize>,
    pub right: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableCell {
    pub text: String,
    pub role: Role,
}

impl Column {
    pub fn new(header: impl Into<String>, drop_rank: u8) -> Self {
        Self {
            header: header.into(),
            drop_rank,
            flex: false,
            cut_to: None,
            right: false,
        }
    }

    pub fn flex(mut self) -> Self {
        self.flex = true;
        self
    }

    pub fn right(mut self) -> Self {
        self.right = true;
        self
    }

    pub fn cut_to(mut self, cells: usize) -> Self {
        self.cut_to = Some(cells);
        self
    }
}

impl TableCell {
    pub fn new(text: impl AsRef<str>, role: Role) -> Self {
        Self {
            text: sanitize_line(text.as_ref()).trim().to_owned(),
            role,
        }
    }

    pub fn text(text: impl AsRef<str>) -> Self {
        Self::new(text, Role::Text)
    }

    pub fn dim(text: impl AsRef<str>) -> Self {
        Self::new(text, Role::Dim)
    }
}

impl Table {
    pub fn new(columns: Vec<Column>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
        }
    }

    pub fn row(&mut self, cells: Vec<TableCell>) {
        self.rows.push(cells);
    }

    /// Each row's cells beside their column's header, empty ones kept so no value moves
    /// under another header. Read aloud rather than drawn.
    pub fn labeled_rows(&self) -> Vec<Vec<(&str, &str)>> {
        self.rows
            .iter()
            .map(|row| {
                self.columns
                    .iter()
                    .zip(row)
                    .map(|(column, cell)| (column.header.as_str(), cell.text.as_str()))
                    .collect()
            })
            .collect()
    }

    /// Rows `indent` cells in, the header dim.
    pub fn lines(&self, theme: &Theme, width: usize, indent: usize) -> Vec<Line<'static>> {
        let widths = self.fit(width, indent);
        let mut lines: Vec<Line<'static>> = self
            .all_rows()
            .iter()
            .map(|row| self.line(row, &widths, theme, indent))
            .collect();
        fit_lines(&mut lines, width);
        lines
    }

    /// The rows as plain text, for the pager.
    pub fn plain(&self, width: usize, indent: usize) -> Vec<String> {
        let widths = self.fit(width, indent);
        self.all_rows()
            .iter()
            .map(|row| {
                let cells = self.cells(row, &widths);
                let text: String = cells.into_iter().map(|(text, _)| text).collect();
                format!("{}{}", " ".repeat(indent), text.trim_end())
            })
            .collect()
    }

    fn all_rows(&self) -> Vec<Vec<TableCell>> {
        let headers: Vec<TableCell> = self
            .columns
            .iter()
            .map(|column| TableCell::dim(&column.header))
            .collect();
        let mut rows = Vec::new();
        if headers.iter().any(|cell| !cell.text.is_empty()) {
            rows.push(headers);
        }
        rows.extend(self.rows.iter().cloned());
        rows
    }

    fn line(
        &self,
        cells: &[TableCell],
        widths: &[Option<usize>],
        theme: &Theme,
        indent: usize,
    ) -> Line<'static> {
        let mut spans = vec![Span::raw(" ".repeat(indent))];
        for (text, role) in self.cells(cells, widths) {
            match role {
                Some(role) => spans.push(Span::styled(text, theme.style(role))),
                None => spans.push(Span::raw(text)),
            }
        }
        Line::from(spans)
    }

    /// Each shown cell cut and padded to its column, with the gaps between them unstyled.
    fn cells(&self, cells: &[TableCell], widths: &[Option<usize>]) -> Vec<(String, Option<Role>)> {
        let shown: Vec<usize> = (0..self.columns.len())
            .filter(|&index| widths[index].is_some())
            .collect();
        let mut out = Vec::new();
        for (position, &index) in shown.iter().enumerate() {
            let column = &self.columns[index];
            let slot = widths[index].unwrap_or(0);
            let cell = cells.get(index);
            let text = cell.map_or("", |cell| cell.text.as_str());
            let role = cell.map_or(Role::Text, |cell| cell.role);
            let last = position + 1 == shown.len();
            let content = if column.right {
                right(text, slot)
            } else if last {
                cut(text, slot)
            } else {
                left(text, slot)
            };
            if position > 0 {
                out.push((" ".repeat(GAP), None));
            }
            out.push((content, Some(role)));
        }
        out
    }

    /// Each column's width, or `None` for a column that went.
    fn fit(&self, width: usize, indent: usize) -> Vec<Option<usize>> {
        let natural: Vec<usize> = self
            .columns
            .iter()
            .enumerate()
            .map(|(index, column)| {
                self.rows
                    .iter()
                    .filter_map(|row| row.get(index))
                    .map(|cell| str_width(&cell.text))
                    .chain([str_width(sanitize_line(&column.header).trim())])
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let mut widths: Vec<Option<usize>> = natural.iter().copied().map(Some).collect();
        let flex = self.columns.iter().position(|column| column.flex);
        loop {
            let shown: Vec<usize> = widths.iter().flatten().copied().collect();
            let total = indent + shown.iter().sum::<usize>() + GAP * shown.len().saturating_sub(1);
            if total <= width {
                return widths;
            }
            // Names cut first, since the title says what the row is, then the title, all
            // before a column goes.
            let floors = self
                .columns
                .iter()
                .enumerate()
                .filter_map(|(index, column)| Some((index, natural[index].min(column.cut_to?))))
                .chain(flex.map(|flex| (flex, natural[flex].min(FLEX_FLOOR))));
            let mut cut = widths.clone();
            let mut over = total - width;
            for (index, floor) in floors {
                if let Some(current) = cut[index] {
                    let give = current.saturating_sub(floor).min(over);
                    cut[index] = Some(current - give);
                    over -= give;
                }
            }
            if over == 0 {
                return cut;
            }
            let dropping = self
                .columns
                .iter()
                .enumerate()
                .filter(|(index, column)| column.drop_rank > 0 && widths[*index].is_some())
                .max_by_key(|(index, column)| (column.drop_rank, *index))
                .map(|(index, _)| index);
            match dropping {
                Some(index) => widths[index] = None,
                None => {
                    // Nothing left to drop: the flexible column takes what's left, and the
                    // rows are cut at the edge.
                    if let Some(flex) = flex {
                        let current = cut[flex].unwrap_or(0);
                        cut[flex] = Some(current.saturating_sub(over).max(1));
                    }
                    return cut;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A long agent name is cut before the title and before the files column goes, and shows
    /// whole with room.
    #[test]
    fn an_agent_name_cuts_before_a_column_goes() {
        let mut table = Table::new(vec![
            Column::new("title", 0).flex(),
            Column::new("files", 4).right(),
            Column::new("worker", 3).cut_to(16),
        ]);
        table.row(vec![
            TableCell::text("Acceptance tests for the load test"),
            TableCell::dim("1"),
            TableCell::dim("Gemini 3.6 Flash Low on agy"),
        ]);
        let narrow = table.plain(50, 2);
        let text = narrow.join("\n");
        assert!(text.contains("files"), "{text}");
        assert!(text.contains("Gemini 3.6 Flas…"), "{text}");
        assert!(text.contains("Acceptance tests for t…"), "{text}");
        assert!(narrow.iter().all(|line| str_width(line) <= 50), "{text}");
        let wide = table.plain(120, 2).join("\n");
        assert!(wide.contains("Gemini 3.6 Flash Low on agy"), "{wide}");
    }
}
