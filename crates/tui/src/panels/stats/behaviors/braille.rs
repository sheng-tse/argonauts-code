//! The map in braille, for a terminal that draws no pictures, as close to the map's picture as
//! cells allow: the picture's paper behind every cell, the comparison on it in the picture's red
//! and blue at the picture's strength, and each action a round dot of two braille dots across
//! and two down, the compared group's three down and in its colour, the rest faded into what's
//! behind them.

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
/// The map picture's paper, red and blue.
const PAPER: Rgb = (31, 32, 47);
const MORE: Rgb = (226, 85, 98);
const LESS: Rgb = (86, 132, 214);
/// The comparison shows where the difference is more than this share of its largest, mixed over
/// the paper by the difference times [`STRENGTH`], as the picture paints it.
const SHOWN: f32 = 0.08;
const STRENGTH: f32 = 150.0 / 255.0;
/// The difference is shown in this many steps each way, so neighbouring cells share their
/// shade and the fields read as soft blocks, as the picture's did.
const STEPS: f32 = 6.0;
/// How much of its own colour a dot of a group not compared keeps over what's behind it: the
/// picture's 110/255, a little more, as a braille dot is thinner than the picture's disc.
const FADED: f32 = 110.0 / 255.0 + 0.25;

type Rgb = (u8, u8, u8);

fn mix(from: Rgb, to: Rgb, share: f32) -> Rgb {
    let part = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * share).round() as u8;
    (part(from.0, to.0), part(from.1, to.1), part(from.2, to.2))
}

/// `rgb` as the terminal can show it; `None` on one with sixteen colours.
fn colour(kit: &Kit<'_>, rgb: Rgb) -> Option<Color> {
    match kit.theme.level {
        ColorLevel::TrueColor => Some(Color::Rgb(rgb.0, rgb.1, rgb.2)),
        ColorLevel::Ansi256 => Some(Color::Indexed(nearest_ansi256(rgb))),
        _ => None,
    }
}

/// What's behind a cell whose comparison reads `difference`: the paper, with red where the
/// compared group acts more and blue where less.
fn behind(difference: f32) -> Rgb {
    if difference.abs() <= SHOWN {
        return PAPER;
    }
    let toward = if difference > 0.0 { MORE } else { LESS };
    let stepped = (difference.abs().min(1.0) * STEPS).ceil() / STEPS;
    mix(PAPER, toward, stepped * STRENGTH)
}

fn fill(kit: &Kit<'_>, rgb: Rgb) -> Style {
    colour(kit, rgb).map_or_else(Style::new, |bg| Style::new().bg(bg))
}

/// What stands out among the dots: the behavior the list lights, the action it rings, and the
/// action the pointer rests on, by their places in the map's actions.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Marks<'a> {
    pub lit: &'a [usize],
    pub ringed: Option<usize>,
    pub pointer: Option<usize>,
}

/// One braille cell: its dots, the group whose colour it takes, and whether it holds something lit,
/// ringed or under the pointer.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct Cell {
    pub dots: u32,
    pub group: Option<usize>,
    /// It holds one of the compared group's actions.
    pub compared: bool,
    pub lit: bool,
    pub ringed: bool,
    pub pointer: bool,
    /// The comparison here, from -1 where the compared group acts least to 1 where most.
    pub difference: f32,
}

impl Cell {
    pub fn glyph(&self) -> char {
        match self.dots {
            0 => ' ',
            dots => char::from_u32(0x2800 + dots).unwrap_or(' '),
        }
    }
}

/// The dots an action lights, down and across from its own: two across and two down, the
/// compared group's three down, round dots about the size of the picture's.
fn spread(compared: bool) -> &'static [(isize, isize)] {
    if compared {
        &[(-1, 0), (-1, 1), (0, 0), (0, 1), (1, 0), (1, 1)]
    } else {
        &[(0, 0), (0, 1), (1, 0), (1, 1)]
    }
}

/// The braille dot at `(row, column)`, in dots from the top left, as its cell and its bit there.
fn dot_at(row: usize, column: usize) -> (usize, usize, u32) {
    let bit = BITS
        .iter()
        .find(|(r, c, _)| *r == row % 4 && *c == column % 2)
        .map_or(0, |(_, _, bit)| *bit);
    (row / 4, column / 2, bit)
}

/// The map's cells, `fit`'s rows from the top.
pub(super) fn cells(
    map: &BehaviorMap,
    fit: Fit,
    overlay: Option<&[f32]>,
    compared: Option<usize>,
    marks: Marks<'_>,
) -> Vec<Vec<Cell>> {
    let (columns, rows) = (fit.columns, fit.rows);
    let mut cells = vec![vec![Cell::default(); columns]; rows];
    // Each cell's groups by the actions in it, in the order met, so a tie goes to the first.
    let mut counts: Vec<Vec<Vec<(usize, usize)>>> = vec![vec![Vec::new(); columns]; rows];
    // A cell only an action's round dot reaches takes that action's group, the compared one
    // first.
    let mut reached: Vec<Vec<Option<usize>>> = vec![vec![None; columns]; rows];
    for (at, action) in map.actions.iter().enumerate() {
        let column = ((action.x * (columns * 2) as f32) as usize).min(columns * 2 - 1);
        let row = (((1.0 - action.y) * (rows * 4) as f32) as usize).min(rows * 4 - 1);
        let is_compared = compared == Some(action.group);
        for (down, across) in spread(is_compared) {
            let (Some(r), Some(c)) = (
                row.checked_add_signed(*down).filter(|r| *r < rows * 4),
                column
                    .checked_add_signed(*across)
                    .filter(|c| *c < columns * 2),
            ) else {
                continue;
            };
            let (r, c, bit) = dot_at(r, c);
            cells[r][c].dots |= bit;
            let reach = &mut reached[r][c];
            if reach.is_none() || is_compared {
                *reach = Some(action.group);
            }
        }
        let cell = &mut cells[row / 4][column / 2];
        cell.lit |= marks.lit.contains(&at);
        cell.ringed |= marks.ringed == Some(at);
        cell.pointer |= marks.pointer == Some(at);
        cell.compared |= is_compared;
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
                .map(|(g, _)| *g)
                .or(reached[row][column]);
            cell.compared |=
                seen.is_empty() && compared.is_some() && reached[row][column] == compared;
            if let Some(grid) = overlay {
                let x = ((column as f32 + 0.5) / columns as f32 * GRID as f32) as usize;
                let y = ((1.0 - (row as f32 + 0.5) / rows as f32) * GRID as f32) as usize;
                let value = grid
                    .get(y.min(GRID - 1) * GRID + x.min(GRID - 1))
                    .copied()
                    .unwrap_or(0.0);
                cell.difference = value;
            }
        }
    }
    cells
}

/// The braille rows with the legend beside them, where the screen holds it there, or under
/// them.
pub(super) fn block(look: &Look<'_>, marks: Marks<'_>, beside: bool) -> Drawn {
    use super::level::Level;
    let rows = map_rows(look, marks);
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
pub(super) fn map_rows(look: &Look<'_>, marks: Marks<'_>) -> Vec<Vec<Span<'static>>> {
    use super::level::Level;
    match look.level {
        Level::Map => rows(look, marks),
        Level::Planet => planet_rows(look, marks),
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
fn rows(look: &Look<'_>, marks: Marks<'_>) -> Vec<Vec<Span<'static>>> {
    let kit = look.kit;
    let overlay = look
        .compared
        .and_then(|group| look.map.comparisons.get(group))
        .and_then(|c| c.overlay.as_deref());
    let mut out = Vec::new();
    let compared = look.compared.filter(|_| overlay.is_some());
    for line in cells(look.map, look.fit, overlay, compared, marks) {
        let mut spans = vec![Span::raw(" ".repeat(MAP_INSET))];
        for cell in line {
            let back = behind(cell.difference);
            let mut style = fill(kit, back);
            if cell.dots != 0 {
                let dim = !marks.lit.is_empty() && !cell.lit && !cell.ringed && !cell.pointer;
                let faded = compared.is_some() && !cell.compared;
                style = match (cell.ringed, dim, cell.group) {
                    _ if cell.pointer => pointer_style(kit),
                    (true, _, _) => style
                        .patch(kit.fg(Role::Bright))
                        .add_modifier(Modifier::BOLD),
                    (_, true, _) => style.patch(kit.fg(Role::Faint)),
                    (_, _, Some(group)) if faded => {
                        let rgb = mix(back, look.colours.rgb(group), FADED);
                        match colour(kit, rgb) {
                            Some(fg) => style.fg(fg),
                            None => style.patch(kit.fg(Role::Faint)),
                        }
                    }
                    (_, _, Some(group)) if cell.compared => style
                        .patch(look.colours.style(group))
                        .add_modifier(Modifier::BOLD),
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

/// The cell the pointer rests on, lit: dark dots on white, so it shows among the dots where a
/// ring of white dots alone is too faint. Without colour it is drawn reversed.
fn pointer_style(kit: &Kit<'_>) -> Style {
    let style = match kit.has_color() {
        true => kit.cell(Role::BadgeText, Role::Bright),
        false => Style::new().add_modifier(Modifier::REVERSED),
    };
    style.add_modifier(Modifier::BOLD)
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
pub(super) fn planet_rows(look: &Look<'_>, marks: Marks<'_>) -> Vec<Vec<Span<'static>>> {
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
        let (rank, style) = if marks.pointer == Some(at) {
            (5, pointer_style(kit))
        } else if marks.ringed == Some(at) {
            (4, kit.fg(Role::Bright).add_modifier(Modifier::BOLD))
        } else if !marks.lit.is_empty() && !marks.lit.contains(&at) {
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
