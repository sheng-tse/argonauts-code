//! The line chart of Claude Code's Models tab, which draws with asciichart: a scale down the
//! left, an axis, and one line per series, each step between two days drawn in the column of
//! the later one. Lines drawn later cross over those drawn before.

/// One cell of the chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    Blank,
    /// The axis, or where a series starts on it.
    Axis(char),
    /// Part of the line of the series at this index.
    Line(char, usize),
}

/// The chart's rows, top first: each row's scale label, and its cells from the axis on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chart {
    pub rows: Vec<(String, Vec<Cell>)>,
}

/// Cells the scale's labels take, right-aligned.
pub const LABEL_CELLS: usize = 6;

/// `series` plotted `height` rows high, the scale written by [`scale_word`].
pub fn plot(series: &[Vec<u64>], height: usize) -> Chart {
    let values = || series.iter().flatten().map(|v| *v as f64);
    let low = values().fold(f64::INFINITY, f64::min);
    let high = values().fold(f64::NEG_INFINITY, f64::max);
    if series.is_empty() || !low.is_finite() {
        return Chart { rows: Vec::new() };
    }
    let range = high - low;
    let ratio = if range > 0.0 {
        height as f64 / range
    } else {
        1.0
    };
    let (low_row, high_row) = (scaled(low, ratio), scaled(high, ratio));
    let rows = (high_row - low_row) as usize;
    let width = series.iter().map(Vec::len).max().unwrap_or(0);
    let mut grid = vec![vec![Cell::Blank; width + 1]; rows + 1];
    for row in &mut grid {
        row[0] = Cell::Axis('┤');
    }
    // A row index from the top, for a value's height above the lowest.
    let top = |level: i64| rows - (level - low_row) as usize;
    for (at, line) in series.iter().enumerate() {
        let Some(first) = line.first() else {
            continue;
        };
        grid[top(scaled(*first as f64, ratio))][0] = Cell::Axis('┼');
        for (x, pair) in line.windows(2).enumerate() {
            let (from, to) = (scaled(pair[0] as f64, ratio), scaled(pair[1] as f64, ratio));
            let column = x + 1;
            if from == to {
                grid[top(from)][column] = Cell::Line('─', at);
                continue;
            }
            let falling = from > to;
            grid[top(to)][column] = Cell::Line(if falling { '╰' } else { '╭' }, at);
            grid[top(from)][column] = Cell::Line(if falling { '╮' } else { '╯' }, at);
            for level in from.min(to) + 1..from.max(to) {
                grid[top(level)][column] = Cell::Line('│', at);
            }
        }
    }
    let label = |row: usize| {
        let value = if rows > 0 {
            high - row as f64 * range / rows as f64
        } else {
            high
        };
        let step = if rows > 0 { range / rows as f64 } else { 0.0 };
        format!("{:>width$}", scale_word(value, step), width = LABEL_CELLS)
    };
    Chart {
        rows: grid
            .into_iter()
            .enumerate()
            .map(|(row, cells)| (label(row), cells))
            .collect(),
    }
}

/// A value's height in rows, rounded half up as asciichart rounds.
fn scaled(value: f64, ratio: f64) -> i64 {
    (value * ratio + 0.5).floor() as i64
}

/// `742M`, `1.2b`, `38.5M`, `12k`: the value in its own unit, to a tenth where the rows are
/// closer together than a whole unit, so no two rows read the same.
pub fn scale_word(value: f64, step: f64) -> String {
    let (unit, letter) = if value >= 999.5e6 {
        (1e9, "b")
    } else if value >= 999.5e3 {
        (1e6, "M")
    } else if value >= 999.5 {
        (1e3, "k")
    } else {
        (1.0, "")
    };
    let places = usize::from(step < unit);
    format!("{:.places$}{letter}", value / unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(chart: &Chart) -> Vec<String> {
        chart
            .rows
            .iter()
            .map(|(label, cells)| {
                let line: String = cells
                    .iter()
                    .map(|cell| match cell {
                        Cell::Blank => ' ',
                        Cell::Axis(c) | Cell::Line(c, _) => *c,
                    })
                    .collect();
                format!("{label} {line}")
            })
            .collect()
    }

    #[test]
    fn a_rise_and_a_fall_draw_their_corners_in_the_later_column() {
        let chart = plot(&[vec![0, 2, 2, 0]], 2);
        assert_eq!(
            text(&chart),
            ["     2 ┤╭─╮ ", "     1 ┤│ │ ", "     0 ┼╯ ╰ "]
        );
    }

    #[test]
    fn the_scale_reads_short() {
        assert_eq!(scale_word(742.3e6, 88e6), "742M");
        assert_eq!(scale_word(38.54e6, 88e6), "39M");
        assert_eq!(scale_word(38.54e6, 0.4e6), "38.5M");
        assert_eq!(scale_word(1.24e9, 88e6), "1.2b");
        assert_eq!(scale_word(12_400.0, 2_000.0), "12k");
        assert_eq!(scale_word(7.0, 0.5), "7.0");
    }
}
