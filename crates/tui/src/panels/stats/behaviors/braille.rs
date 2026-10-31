//! The map in braille, for a terminal that draws no pictures: two dots across and four down in
//! each cell, each cell in the colour of the group with the most dots in it, and behind them the
//! comparison, red where the compared group acts more and blue where less.

use argonauts_core::behaviors::behavior_map::{BehaviorMap, GRID};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use super::words::Look;
use super::{Fit, MAP_INSET};
use crate::base::kit::{Drawn, Kit};
use crate::base::theme::{ColorLevel, Role, nearest_ansi256};

/// Each dot's bit in its cell's character, by its row and column in the cell.
const BITS: [(usize, usize, u32); 8] = [
    (0, 0, 0x01),
    (1, 0, 0x02),
    (2, 0, 0x04),
    (0, 1, 0x08),
    (1, 1, 0x10),
    (2, 1, 0x20),
    (3, 0, 0x40),
    (3, 1, 0x80),
];
/// A cell is shaded where the difference is more than this share of its largest.
const SHADED: f32 = 0.25;
const MORE: (u8, u8, u8) = (0x4a, 0x2a, 0x33);
const LESS: (u8, u8, u8) = (0x2a, 0x35, 0x50);

fn fill(kit: &Kit<'_>, (r, g, b): (u8, u8, u8)) -> Style {
    match kit.theme.level {
        ColorLevel::TrueColor => Style::new().bg(Color::Rgb(r, g, b)),
        ColorLevel::Ansi256 => Style::new().bg(Color::Indexed(nearest_ansi256((r, g, b)))),
        _ => Style::new(),
    }
}

/// Behind a cell where the compared group acts more.
pub(super) fn more_style(kit: &Kit<'_>) -> Style {
    fill(kit, MORE)
}

/// Behind a cell where the compared group acts less.
pub(super) fn less_style(kit: &Kit<'_>) -> Style {
    fill(kit, LESS)
}

/// One braille cell: its dots, the group whose colour it takes, and whether it holds something lit or
/// ringed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Cell {
    pub dots: u32,
    pub group: Option<usize>,
    pub lit: bool,
    pub ringed: bool,
    /// `Some(true)` where the compared group acts more, `Some(false)` where less.
    pub shade: Option<bool>,
}

impl Cell {
    pub fn glyph(&self) -> char {
        match self.dots {
            0 => ' ',
            dots => char::from_u32(0x2800 + dots).unwrap_or(' '),
        }
    }
}

/// The map's cells, `fit`'s rows from the top.
pub(super) fn cells(
    map: &BehaviorMap,
    fit: Fit,
    overlay: Option<&[f32]>,
    lit: &[usize],
    ringed: Option<usize>,
) -> Vec<Vec<Cell>> {
    let (columns, rows) = (fit.columns, fit.rows);
    let mut cells = vec![vec![Cell::default(); columns]; rows];
    // Each cell's groups by their dots, in the order met, so a tie goes to the first.
    let mut counts: Vec<Vec<Vec<(usize, usize)>>> = vec![vec![Vec::new(); columns]; rows];
    for (at, action) in map.actions.iter().enumerate() {
        let column = ((action.x * (columns * 2) as f32) as usize).min(columns * 2 - 1);
        let row = (((1.0 - action.y) * (rows * 4) as f32) as usize).min(rows * 4 - 1);
        let cell = &mut cells[row / 4][column / 2];
        let bit = BITS
            .iter()
            .find(|(r, c, _)| *r == row % 4 && *c == column % 2)
            .map_or(0, |(_, _, bit)| *bit);
        cell.dots |= bit;
        cell.lit |= lit.contains(&at);
        cell.ringed |= ringed == Some(at);
        let seen = &mut counts[row / 4][column / 2];
        match seen.iter_mut().find(|(group, _)| *group == action.group) {
            Some((_, count)) => *count += 1,
            None => seen.push((action.group, 1)),
        }
    }
    for (row, line) in cells.iter_mut().enumerate() {
        for (column, cell) in line.iter_mut().enumerate() {
            let seen = &counts[row][column];
            let most = seen.iter().map(|(_, count)| *count).max().unwrap_or(0);
            cell.group = seen
                .iter()
                .find(|(_, count)| *count == most)
                .map(|(g, _)| *g);
            if let Some(grid) = overlay {
                let x = ((column as f32 + 0.5) / columns as f32 * GRID as f32) as usize;
                let y = ((1.0 - (row as f32 + 0.5) / rows as f32) * GRID as f32) as usize;
                let value = grid
                    .get(y.min(GRID - 1) * GRID + x.min(GRID - 1))
                    .copied()
                    .unwrap_or(0.0);
                cell.shade = (value.abs() > SHADED).then_some(value > 0.0);
            }
        }
    }
    cells
}

/// The braille rows with the legend beside them, where the screen holds it there, or under
/// them.
pub(super) fn block(look: &Look<'_>, lit: &[usize], ringed: Option<usize>, beside: bool) -> Drawn {
    use super::level::Level;
    let rows = map_rows(look, lit, ringed);
    let mut out = Drawn::new();
    if beside {
        let legend = legend_beside(look);
        for (at, mut row) in rows.into_iter().enumerate() {
            if let Some(words) = legend.get(at).filter(|words| !words.is_empty()) {
                row.push(Span::raw(" ".repeat(super::LEGEND_GAP)));
                row.extend(crate::base::text::truncate_spans(words, look.legend_room()));
            }
            out.push(row);
        }
        return out;
    }
    for row in rows {
        out.push(row);
    }
    out.push(Vec::new());
    match look.level {
        Level::Map | Level::Planet => {
            for line in look.braille_legend() {
                out.push(line);
            }
        }
        _ => {
            for words in look.world_legend() {
                let mut line = vec![Span::raw(" ")];
                line.extend(words);
                out.push(line);
            }
        }
    }
    out
}

/// The level's braille rows: the map, the planet or the universe in the map's cells.
pub(super) fn map_rows(
    look: &Look<'_>,
    lit: &[usize],
    ringed: Option<usize>,
) -> Vec<Vec<Span<'static>>> {
    use super::level::Level;
    match look.level {
        Level::Map => rows(look, lit, ringed),
        Level::Planet => planet_rows(look, lit, ringed),
        _ => cosmos_rows(look),
    }
}

/// What stands beside the braille rows: the groups on the map and the planet, the world's
/// legend in the universe.
pub(super) fn legend_beside(look: &Look<'_>) -> Vec<Vec<Span<'static>>> {
    use super::level::Level;
    match look.level {
        Level::Map | Level::Planet => look.braille_beside(),
        _ => look.world_legend(),
    }
}

/// The map's braille rows, each the map's inset and its cells across.
fn rows(look: &Look<'_>, lit: &[usize], ringed: Option<usize>) -> Vec<Vec<Span<'static>>> {
    let kit = look.kit;
    let overlay = look
        .compared
        .and_then(|group| look.map.comparisons.get(group))
        .and_then(|c| c.overlay.as_deref());
    let mut out = Vec::new();
    for line in cells(look.map, look.fit, overlay, lit, ringed) {
        let mut spans = vec![Span::raw(" ".repeat(MAP_INSET))];
        for cell in line {
            let mut style = match cell.shade {
                Some(true) => more_style(kit),
                Some(false) => fill(kit, LESS),
                None => Style::new(),
            };
            if cell.dots != 0 {
                let dim = !lit.is_empty() && !cell.lit && !cell.ringed;
                style = match (cell.ringed, dim, cell.group) {
                    (true, _, _) => style
                        .patch(kit.fg(Role::Bright))
                        .add_modifier(Modifier::BOLD),
                    (_, true, _) => style.patch(kit.fg(Role::Faint)),
                    (_, _, Some(group)) => style.patch(look.colours.style(group)),
                    _ => style,
                };
            }
            spans.push(Span::styled(cell.glyph().to_string(), style));
        }
        out.push(merge(spans));
    }
    out
}

/// Neighbouring cells of one style as one span.
fn merge(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    for span in spans {
        match out.last_mut() {
            Some(last) if last.style == span.style => {
                last.content = format!("{}{}", last.content, span.content).into();
            }
            _ => out.push(span),
        }
    }
    out
}

/// Dots across and down a dot's cell: braille has two across and four down in a cell, and a
/// cell is 18 × 42 px, so a dot is 9 px across and 10.5 px down. A round shape takes this many
/// times as many dots across as down.
const ACROSS: f64 = 10.5 / 9.0;

/// The levels without a map of dots of their own, drawn in braille: the planet's disc with your
/// actions on its near side, and the universe's makers, models and bodies as knots of dots. Each
/// cell takes the style of the most telling thing in it.
struct DotGrid {
    columns: usize,
    rows: usize,
    dots: Vec<u32>,
    ink: Vec<Option<(u8, Style)>>,
}

impl DotGrid {
    fn new(fit: Fit) -> Self {
        let cells = fit.columns * fit.rows;
        Self {
            columns: fit.columns,
            rows: fit.rows,
            dots: vec![0; cells],
            ink: vec![None; cells],
        }
    }

    /// One dot at `x` across and `y` down, in dots from the top left; `rank` says how telling
    /// it is, so a cell takes the style of its highest.
    fn set(&mut self, x: f64, y: f64, rank: u8, style: Style) {
        if !(x >= 0.0 && y >= 0.0) {
            return;
        }
        let (column, row) = (x as usize, y as usize);
        if column >= self.columns * 2 || row >= self.rows * 4 {
            return;
        }
        let cell = (row / 4) * self.columns + column / 2;
        let bit = BITS
            .iter()
            .find(|(r, c, _)| *r == row % 4 && *c == column % 2)
            .map_or(0, |(_, _, bit)| *bit);
        self.dots[cell] |= bit;
        if self.ink[cell].is_none_or(|(held, _)| rank > held) {
            self.ink[cell] = Some((rank, style));
        }
    }

    /// A filled disc `radius` dots down, as wide across as it is round.
    fn disc(&mut self, (cx, cy): (f64, f64), radius: f64, rank: u8, style: Style) {
        let across = radius * ACROSS;
        let mut y = (cy - radius).floor();
        while y <= cy + radius {
            let mut x = (cx - across).floor();
            while x <= cx + across {
                let (dx, dy) = ((x + 0.5 - cx) / across, (y + 0.5 - cy) / radius);
                if dx * dx + dy * dy <= 1.0 {
                    self.set(x, y, rank, style);
                }
                x += 1.0;
            }
            y += 1.0;
        }
    }

    /// An ellipse's outline `rx` across and `ry` down, a dot every `step` degrees.
    fn ring(
        &mut self,
        (cx, cy): (f64, f64),
        (rx, ry): (f64, f64),
        step: f64,
        rank: u8,
        style: Style,
    ) {
        let mut angle: f64 = 0.0;
        while angle < 360.0 {
            let (sin, cos) = angle.to_radians().sin_cos();
            self.set(cx + cos * rx, cy + sin * ry, rank, style);
            angle += step;
        }
    }

    fn rows(&self) -> Vec<Vec<Span<'static>>> {
        (0..self.rows)
            .map(|row| {
                let mut spans = vec![Span::raw(" ".repeat(MAP_INSET))];
                for column in 0..self.columns {
                    let at = row * self.columns + column;
                    let cell = Cell {
                        dots: self.dots[at],
                        ..Cell::default()
                    };
                    let style = self.ink[at].map_or_else(Style::new, |(_, style)| style);
                    spans.push(Span::styled(cell.glyph().to_string(), style));
                }
                merge(spans)
            })
            .collect()
    }
}

/// The middle of the map's cells, in dots.
fn middle(fit: Fit) -> (f64, f64) {
    (fit.columns as f64, (fit.rows * 2) as f64)
}

/// The planet's radius in braille, in dots down: inside the map's rows with a little to spare.
pub(super) fn planet_radius(fit: Fit) -> f64 {
    (middle(fit).1 - 0.5) * 0.96
}

/// The planet in braille: its rim, and each action on its near side where the map wraps round
/// it, turned to face the map's middle.
pub(super) fn planet_rows(
    look: &Look<'_>,
    lit: &[usize],
    ringed: Option<usize>,
) -> Vec<Vec<Span<'static>>> {
    let kit = look.kit;
    let mut grid = DotGrid::new(look.fit);
    let middle = middle(look.fit);
    let radius = middle.1 - 0.5;
    grid.ring(
        middle,
        (radius * ACROSS, radius),
        2.0,
        1,
        kit.fg(Role::Faint),
    );
    for (at, action) in look.map.actions.iter().enumerate() {
        let Some((x, y)) = planet_dot(action.x, action.y, look.planet, look.fit) else {
            continue;
        };
        let (rank, style) = if ringed == Some(at) {
            (4, kit.fg(Role::Bright).add_modifier(Modifier::BOLD))
        } else if !lit.is_empty() && !lit.contains(&at) {
            (2, kit.fg(Role::Faint))
        } else {
            (3, look.colours.style(action.group))
        };
        grid.set(x, y, rank, style);
    }
    grid.rows()
}

/// Where a place on the map, 0..1 across and up, falls among the braille dots on the planet
/// in `fit`'s cells, while it is on its near side.
pub(super) fn planet_dot(x: f32, y: f32, view: crate::world::View, fit: Fit) -> Option<(f64, f64)> {
    use crate::world::{Globe, Plate};

    let globe = Globe::new(view, (0.0, 0.0), planet_radius(fit));
    let middle = middle(fit);
    let (px, py) = Plate::MAP.land_pixel(f64::from(x), f64::from(y));
    let point = globe.project(Plate::MAP.place(px, py));
    (point.z > 0.08).then_some((middle.0 + point.x * ACROSS, middle.1 + point.y))
}

/// The universe, a galaxy or a system in braille, where [`places`](super::places) puts each
/// body: knots of dots as large as their share, orbits as sparse dots, what ← → picked bright.
pub(super) fn cosmos_rows(look: &Look<'_>) -> Vec<Vec<Span<'static>>> {
    use super::places::{Kind, places};

    let kit = look.kit;
    let universe = look.universe;
    let mut grid = DotGrid::new(look.fit);
    let middle = middle(look.fit);
    let bright = kit.fg(Role::Bright).add_modifier(Modifier::BOLD);
    let (across, down) = ((look.fit.columns * 2) as f64, (look.fit.rows * 4) as f64);
    let dots = |(x, y): (f64, f64)| (x * across, y * down);
    for place in places(universe, look.level) {
        let picked = look.picked == Some(place.pointed);
        let at = dots(place.at);
        let radius = place.radius * down;
        let style = if picked {
            bright
        } else {
            look.body_style(place.pointed)
        };
        match place.kind {
            Kind::Home => {
                let ring = 8.0;
                grid.ring(at, (ring * ACROSS, ring), 15.0, 1, kit.fg(Role::Faint));
                grid.disc(at, radius, 3, style);
            }
            Kind::Galaxy | Kind::Star => grid.disc(at, radius, 2, style),
            Kind::Planet { orbit } => {
                grid.ring(middle, dots(orbit), 14.0, 1, kit.fg(Role::Faint));
                grid.disc(at, radius, 2, style);
            }
            Kind::Belt { orbit } => {
                let style = if picked { bright } else { kit.fg(Role::Dim) };
                grid.ring(middle, dots(orbit), 4.0, 1, style);
            }
        }
    }
    grid.rows()
}
