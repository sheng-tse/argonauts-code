//! Beside the picture, only what ← → steps through: the groups on the map and the planet, the
//! makers in the universe, a maker's models in its galaxy, a model's planets in its system, each
//! with its logo and its count. Nature, the land's kinds, the wonders and the kinds of bodies are
//! never listed: the pointer finds each. Above the picture, the row that names the levels.

use argonauts_core::behaviors::behavior_map::Grouping;
use argonauts_core::behaviors::behavior_world::ModelFacts;
use argonauts_core::look::logos::{Logo, logo_for};
use ratatui::style::{Color, Style};
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

use super::hover::cap;
use super::layout::RIGHT_EDGE;
use super::level::Level;
use super::universe::{Body, PLANETS};
use super::window::Window;
use super::words::Look;
use super::{LEGEND_GAP, MAP_INSET};
use crate::base::kit::logo::{ModelLabel, NoPicture};
use crate::base::theme::{ColorLevel, Role, nearest_ansi256};
use crate::world::Pointed;
use crate::world::cosmos::{maker_logo, served_logo};

/// The cells from a row's mark to its logo, and from its logo to its name.
const TO_LOGO: usize = 3;
const TO_NAME: usize = 1;
/// The cells the groupings above the levels row leave at its right: their band's blank and the
/// row's edge.
const GROUPINGS_EDGE: usize = 2;
/// The blanks between a row's name and its count, as between a table's columns.
const COUNT_GAP: usize = 3;

/// The land's tints while a group is compared, from much more to much less, and untinted.
const TINTS: [((u8, u8, u8), &str, &str); 5] = [
    ((0xb4, 0x4a, 0x4e), "Much more", "2x as often or more"),
    ((0x8c, 0x5a, 0x4c), "More", "1.25x to 2x"),
    ((0x4a, 0x62, 0x7c), "Less", "0.5x to 0.8x"),
    ((0x3e, 0x5a, 0x9a), "Much less", "0.5x or less"),
    ((0x4e, 0x5e, 0x48), "Untinted", "About the same, or too few"),
];

/// A swatch of `rgb`, two cells filled, where the terminal shows colour behind text.
fn swatch(kit: &crate::base::kit::Kit<'_>, (r, g, b): (u8, u8, u8)) -> Span<'static> {
    let style = match kit.theme.level {
        ColorLevel::TrueColor => Style::new().bg(Color::Rgb(r, g, b)),
        ColorLevel::Ansi256 => Style::new().bg(Color::Indexed(nearest_ansi256((r, g, b)))),
        _ => Style::new(),
    };
    Span::styled("  ", style)
}

/// Every group of `map`, busiest first, ties in the map's order: the legend's order, which
/// ← → step through.
pub(super) fn busiest_first(
    map: &argonauts_core::behaviors::behavior_map::BehaviorMap,
) -> Vec<usize> {
    let mut order: Vec<usize> = (0..map.groups.len()).collect();
    order.sort_by(|a, b| {
        let (ga, gb) = (&map.groups[*a], &map.groups[*b]);
        gb.actions.cmp(&ga.actions).then(a.cmp(b))
    });
    order
}

impl Look<'_> {
    /// The legend's cells across, from its column to the screen's right edge.
    pub fn legend_room(&self) -> usize {
        self.width
            .saturating_sub(MAP_INSET + self.fit.columns + LEGEND_GAP + RIGHT_EDGE)
            .max(20)
    }

    /// `name` with `count` at the row's right end, after `before`.
    fn counted(
        &self,
        before: Vec<Span<'static>>,
        name: Span<'static>,
        count: Option<String>,
    ) -> Vec<Span<'static>> {
        let room = self.legend_room();
        let used = crate::base::text::spans_width(&before);
        let count = count.unwrap_or_default();
        let fits = room.saturating_sub(used + count.width() + 1);
        let words = cut(name.content.as_ref(), fits);
        let pad = room.saturating_sub(used + words.width() + count.width());
        let mut line = before;
        line.push(Span::styled(words, name.style));
        line.push(Span::raw(" ".repeat(pad)));
        line.push(self.kit.dim(count));
        line
    }

    /// A model's name after its logo, or whole where no logo stands before it.
    fn short(&self, model: &ModelFacts) -> String {
        self.named_after(&served_logo(model), &self.model_name(model))
    }

    /// `name` as it stands after `logo`: without its maker's word only where the logo draws as
    /// its picture, since a colour dot in its place doesn't say who made the model, and whole
    /// where no logo shows, so braille and a screen reader still say who made it.
    pub fn named_after(&self, logo: &Logo, name: &str) -> String {
        match self.logos {
            true => ModelLabel::named(logo, name, self.kit.theme, NoPicture::Dot).name,
            false => name.to_owned(),
        }
    }

    /// A logo and a blank before a heading's name, where logos are drawn.
    fn heading_logo(&self, logo: &Logo) -> Vec<Span<'static>> {
        if !self.logos {
            return Vec::new();
        }
        let mut spans = crate::base::kit::logo::logo_spans(logo, self.kit.theme);
        spans.push(Span::raw(" "));
        spans
    }

    /// A model's display name: the map's own where it groups by model, else as the names say.
    pub fn model_name(&self, model: &ModelFacts) -> String {
        self.map
            .groups
            .iter()
            .find(|group| group.model.as_deref() == Some(model.model.as_str()))
            .map(|group| group.name.clone())
            .or_else(|| self.names.model(&model.agent, Some(&model.model)))
            .unwrap_or_else(|| model.model.clone())
    }

    /// The logo a group's row and hover line stand before its name: the CLI's for a CLI, its
    /// server's for a model, none for a role, a workflow, a project or an outcome.
    pub fn group_logo(&self, group: usize) -> Option<Logo> {
        let group = self.map.groups.get(group)?;
        let agent = group.agent.as_deref()?;
        match self.map.request.grouping {
            Grouping::Model => Some(logo_for(agent, group.model.as_deref())),
            Grouping::Cli => Some(logo_for(agent, None)),
            _ => None,
        }
    }

    /// A group's name after its logo, as the world writes it.
    pub fn group_label(&self, group: usize) -> Vec<Span<'static>> {
        let kit = self.kit;
        let Some(found) = self.map.groups.get(group) else {
            return Vec::new();
        };
        match self.group_logo(group) {
            Some(logo) => {
                let mut spans = crate::base::kit::logo::logo_spans(&logo, kit.theme);
                spans.push(Span::raw(" "));
                let name = match self.map.request.grouping {
                    Grouping::Model => self.named_after(&logo, &found.name),
                    _ => found.name.clone(),
                };
                spans.push(Span::styled(name, kit.style(Role::Text)));
                spans
            }
            None => vec![Span::styled(found.name.clone(), kit.style(Role::Text))],
        }
    }

    /// The row of a group, a maker, a model or a body: its mark, its logo, its name and its
    /// count.
    fn body_row(
        &self,
        mark: Span<'static>,
        logo: Option<&Logo>,
        name: Span<'static>,
        count: Option<String>,
    ) -> Vec<Span<'static>> {
        if !self.logos {
            return self.counted(vec![mark], name, count);
        }
        let mut before = vec![mark, Span::raw(" ".repeat(TO_LOGO - 2))];
        match logo {
            Some(logo) => before.extend(crate::base::kit::logo::logo_spans(logo, self.kit.theme)),
            None => before.push(Span::raw("  ")),
        }
        before.push(Span::raw(" ".repeat(TO_NAME)));
        self.counted(before, name, count)
    }

    /// What ← → picked, marked: a white dot where picked, the place left blank elsewhere.
    fn pick_mark(&self, picked: bool) -> Span<'static> {
        match picked {
            true => Span::styled("● ", self.kit.fg(Role::Bright)),
            false => Span::raw("  "),
        }
    }

    /// The legend beside the world's picture, for the level shown, in the map's rows.
    pub fn world_legend(&self) -> Vec<Vec<Span<'static>>> {
        self.world_legend_in(self.fit.legend_rows())
    }

    /// The legend beside the world's picture in `rows` rows: on the map and the planet every
    /// group, in a list that scrolls to keep the compared one in view.
    pub fn world_legend_in(&self, rows: usize) -> Vec<Vec<Span<'static>>> {
        match self.level {
            Level::Map | Level::Planet => self.groups_legend(rows),
            Level::Universe => self.makers_legend(),
            Level::Galaxy(maker) => self.galaxy_legend(maker),
            Level::System(star) => self.system_legend(star),
        }
    }

    fn groups_legend(&self, rows: usize) -> Vec<Vec<Span<'static>>> {
        let kit = self.kit;
        let arrows = self.arrows;
        let mut out: Vec<Vec<Span<'static>>> = Vec::new();
        match self.compared {
            None => {
                out.push(vec![Span::styled(
                    format!("Every {}", self.map.request.grouping.word()),
                    kit.bold(Role::Text),
                )]);
                out.push(vec![
                    kit.dim(format!("Nothing compared · {arrows}  compares one")),
                ]);
            }
            Some(group) => {
                let mut head = vec![kit.dim("Comparing ")];
                head.extend(self.group_label(group));
                out.push(head);
                let style = kit.style(Role::Dim);
                let room = self.legend_room();
                let comparison = self.map.comparisons.get(group);
                match comparison.and_then(|c| c.why_not.as_deref()) {
                    // The land stays untinted; a group with no list says why under the map,
                    // where its list would be.
                    Some(why) => {
                        if comparison.is_some_and(|c| !c.rows.is_empty()) {
                            for line in crate::base::text::wrap_text(why, style, room, &[], &[]) {
                                out.push(line.spans);
                            }
                        }
                    }
                    None => {
                        for words in [
                            "With the rest, on the land:",
                            "Red where it does more, blue where less",
                        ] {
                            out.push(vec![Span::styled(cut(words, room), style)]);
                        }
                        for (rgb, name, words) in TINTS {
                            out.push(vec![
                                swatch(kit, rgb),
                                Span::raw(" "),
                                Span::styled(format!("{name:<10}"), kit.style(Role::Text)),
                                kit.dim(words),
                            ]);
                        }
                    }
                }
                out.push(Vec::new());
            }
        }
        // Every group with an action, the compared one too, busiest first.
        let order: Vec<usize> = busiest_first(self.map)
            .into_iter()
            .filter(|at| self.map.groups[*at].actions > 0 || self.compared == Some(*at))
            .collect();
        let named = |group: usize| {
            let found = &self.map.groups[group];
            match (self.map.request.grouping, self.group_logo(group)) {
                (Grouping::Model, Some(logo)) => self.named_after(&logo, &found.name),
                _ => found.name.clone(),
            }
        };
        // The names' column as wide as the widest of every group's, the counts right-aligned
        // three blanks after it, so neither moves as the list scrolls.
        let name_cells = order.iter().map(|g| named(*g).width()).max().unwrap_or(0);
        let count_cells = order
            .iter()
            .map(|g| self.map.groups[*g].actions.to_string().width())
            .max()
            .unwrap_or(0);
        let cursor = self
            .compared
            .and_then(|group| order.iter().position(|at| *at == group))
            .unwrap_or(0);
        // A list that runs on keeps a row for its mark: the heading's last rows give way.
        while !out.is_empty()
            && order.len() > rows.saturating_sub(out.len())
            && rows.saturating_sub(out.len()) < 2
        {
            out.pop();
        }
        let window = Window::follow(order.len(), rows.saturating_sub(out.len()), 0, cursor);
        out.extend(window.above_row(kit, ""));
        for group in &order[window.rows()] {
            let this = self.compared == Some(*group);
            let off = self.compared.is_some() && !this;
            let mark = match off {
                true => Span::styled("○ ", kit.style(Role::Dim)),
                false => Span::styled("● ", self.colours.style(*group)),
            };
            let name_style = if off {
                kit.style(Role::Dim)
            } else {
                kit.style(Role::Text)
            };
            let logo = self.group_logo(*group);
            let count = self.map.groups[*group].actions.to_string();
            let name = Span::styled(named(*group), name_style);
            out.push(self.columned(mark, logo.as_ref(), name, name_cells, count, count_cells));
        }
        out.extend(window.below_row(kit, ""));
        out
    }

    /// A group's row in columns: its mark, its logo, its name in `name_cells` (cut with `…`
    /// where the legend's room is short), three blanks, and its count right-aligned in
    /// `count_cells`.
    fn columned(
        &self,
        mark: Span<'static>,
        logo: Option<&Logo>,
        name: Span<'static>,
        name_cells: usize,
        count: String,
        count_cells: usize,
    ) -> Vec<Span<'static>> {
        let mut line = vec![mark];
        if self.logos {
            line.push(Span::raw(" ".repeat(TO_LOGO - 2)));
            match logo {
                Some(logo) => line.extend(crate::base::kit::logo::logo_spans(logo, self.kit.theme)),
                None => line.push(Span::raw("  ")),
            }
            line.push(Span::raw(" ".repeat(TO_NAME)));
        }
        let used = crate::base::text::spans_width(&line);
        let cells = name_cells.min(
            self.legend_room()
                .saturating_sub(used + COUNT_GAP + count_cells),
        );
        let words =
            crate::base::text::cut(name.content.as_ref(), cells, crate::base::text::At::Word);
        let pad = cells.saturating_sub(words.width()) + COUNT_GAP + count_cells
            - count.width().min(count_cells);
        line.push(Span::styled(words, name.style));
        line.push(Span::raw(" ".repeat(pad)));
        line.push(self.kit.dim(count));
        line
    }

    fn makers_legend(&self) -> Vec<Vec<Span<'static>>> {
        let kit = self.kit;
        let mut out = vec![
            vec![Span::styled("Every maker", kit.bold(Role::Text))],
            vec![kit.dim(format!(
                "{} actions · {}  picks one, enter goes in",
                self.universe.actions, self.arrows
            ))],
        ];
        for (at, maker) in self.universe.makers.iter().enumerate() {
            let picked = self.picked == u32::try_from(at).ok().map(Pointed::Galaxy);
            let name = argonauts_core::models::registry::provider::provider_name(&maker.key);
            out.push(self.body_row(
                self.pick_mark(picked),
                Some(&maker_logo(&maker.key)),
                Span::styled(name, kit.style(Role::Text)),
                Some(maker.actions.to_string()),
            ));
        }
        out
    }

    fn galaxy_legend(&self, maker: usize) -> Vec<Vec<Span<'static>>> {
        let kit = self.kit;
        let (Some(found), Some(world)) = (self.universe.makers.get(maker), self.world) else {
            return Vec::new();
        };
        let mut head = self.heading_logo(&maker_logo(&found.key));
        head.push(Span::styled(
            argonauts_core::models::registry::provider::provider_name(&found.key),
            kit.bold(Role::Text),
        ));
        let models = u32::try_from(found.stars.len()).unwrap_or(0);
        let mut out = vec![
            head,
            vec![kit.dim(format!(
                "{} · {}  picks one",
                super::hover::plural(models, "model"),
                self.arrows
            ))],
        ];
        for star in &found.stars {
            let Some(model) = self
                .universe
                .stars
                .get(*star)
                .and_then(|s| world.models.get(s.model))
            else {
                continue;
            };
            let picked = self.picked == u32::try_from(*star).ok().map(Pointed::Star);
            let name = self.short(model);
            out.push(self.body_row(
                self.pick_mark(picked),
                Some(&served_logo(model)),
                Span::styled(name, kit.style(Role::Text)),
                Some(model.actions.to_string()),
            ));
        }
        out
    }

    fn system_legend(&self, star: usize) -> Vec<Vec<Span<'static>>> {
        let kit = self.kit;
        let Some((found, model)) = self.universe.stars.get(star).and_then(|s| {
            let world = self.world?;
            Some((s, world.models.get(s.model)?))
        }) else {
            return Vec::new();
        };
        let mut head = self.heading_logo(&served_logo(model));
        head.push(Span::styled(self.short(model), kit.bold(Role::Text)));
        let behaviors = u32::try_from(model.behaviors.len()).unwrap_or(0);
        let mut out = vec![
            head,
            vec![kit.dim(format!(
                "{} actions in {} · {}  picks one",
                model.actions,
                super::hover::plural(behaviors, "behavior"),
                self.arrows
            ))],
        ];
        for body in &found.bodies {
            let picked = self.picked == u32::try_from(*body).ok().map(Pointed::Body);
            match self.universe.bodies.get(*body) {
                Some(Body::Planet { behavior, .. }) => {
                    let Some(b) = model.behaviors.get(*behavior) else {
                        continue;
                    };
                    out.push(self.body_row(
                        self.pick_mark(picked),
                        None,
                        Span::styled(cap(&b.behavior), kit.style(Role::Text)),
                        Some(b.count.to_string()),
                    ));
                }
                Some(Body::Belt { .. }) => {
                    let rest = &model.behaviors[PLANETS.min(model.behaviors.len())..];
                    let count: u32 = rest.iter().map(|b| b.count).sum();
                    let style = if picked {
                        kit.style(Role::Text)
                    } else {
                        kit.style(Role::Dim)
                    };
                    let words = format!(
                        "And {}",
                        super::hover::plural(
                            u32::try_from(rest.len()).unwrap_or(0),
                            "other behavior"
                        )
                    );
                    out.push(self.body_row(
                        self.pick_mark(picked),
                        None,
                        Span::styled(words, style),
                        Some(count.to_string()),
                    ));
                }
                None => {}
                // Moons and flares are found by pointing, not listed.
                _ => {}
            }
        }
        out
    }

    /// Beside the braille map, the legend with counts and whole names: braille draws no logo to
    /// stand for a maker.
    pub fn braille_beside(&self) -> Vec<Vec<Span<'static>>> {
        let kit = self.kit;
        let arrows = self.arrows;
        let heading = |words: Vec<Span<'static>>| {
            let used = crate::base::text::spans_width(&words);
            let room = self.legend_room();
            let mut line = words;
            line.push(Span::raw(
                " ".repeat(room.saturating_sub(used + "Actions".width())),
            ));
            line.push(kit.dim("Actions"));
            line
        };
        let mut out: Vec<Vec<Span<'static>>> = Vec::new();
        match self.compared {
            None => {
                out.push(heading(vec![Span::styled(
                    format!("Every {}", self.map.request.grouping.word()),
                    kit.bold(Role::Text),
                )]));
                out.push(vec![
                    kit.dim(format!("Nothing compared · {arrows}  compares one")),
                ]);
            }
            Some(group) => {
                let name = self.map.groups.get(group).map_or("", |g| g.name.as_str());
                out.push(heading(vec![
                    kit.dim("Comparing "),
                    Span::styled(name.to_owned(), kit.bold(Role::Text)),
                ]));
                out.push(vec![kit.dim(format!(
                    "With the other {}",
                    self.map.request.grouping.plural()
                ))]);
                let comparison = self.map.comparisons.get(group);
                match comparison.and_then(|c| c.why_not.as_deref()) {
                    Some(why) if comparison.is_some_and(|c| !c.rows.is_empty()) => {
                        let style = kit.style(Role::Dim);
                        for line in
                            crate::base::text::wrap_text(why, style, self.legend_room(), &[], &[])
                        {
                            out.push(line.spans);
                        }
                    }
                    Some(_) => {}
                    None => {
                        for (fill, words) in [
                            (super::braille::more_style(kit), "more"),
                            (super::braille::less_style(kit), "less"),
                        ] {
                            out.push(vec![
                                Span::styled("  ", fill),
                                Span::raw(" "),
                                kit.dim(format!("Behind the dots: {name} does this {words}")),
                            ]);
                        }
                    }
                }
            }
        }
        out.push(Vec::new());
        let room = self.fit.legend_rows().saturating_sub(out.len());
        let groups = self.map.groups.len();
        let window = Window::follow(groups, room, 0, self.compared.unwrap_or(0));
        out.extend(window.above_row(kit, ""));
        for at in window.rows() {
            let Some(group) = self.map.groups.get(at) else {
                continue;
            };
            let this = self.compared.is_none() || self.compared == Some(at);
            let mark = if this { "● " } else { "○ " };
            let style = if this {
                kit.style(Role::Text)
            } else {
                kit.style(Role::Dim)
            };
            out.push(self.counted(
                vec![Span::styled(mark, self.colours.style(at))],
                Span::styled(group.name.clone(), style),
                Some(group.actions.to_string()),
            ));
        }
        out.extend(window.below_row(kit, ""));
        out
    }

    /// `1 Map · 2 Planet · 3 Universe`, the level shown in the accent, and inside the universe
    /// the galaxy and the system it holds, each after its logo.
    pub fn levels_row(&self) -> Vec<Span<'static>> {
        let kit = self.kit;
        let shown = self.level.digit();
        let mut line = vec![Span::raw(" ")];
        for (at, (level, name)) in Level::NAMED.iter().enumerate() {
            if at > 0 {
                line.push(kit.dim(" · "));
            }
            let words = format!("{} {name}", level.digit());
            line.push(if level.digit() == shown {
                Span::styled(words, kit.bold(Role::Question))
            } else {
                kit.dim(words)
            });
        }
        let world = self.world;
        let mut crumb = |logo: Logo, name: String| {
            line.push(kit.dim(" › "));
            line.extend(crate::base::kit::logo::logo_spans(&logo, kit.theme));
            line.push(Span::raw(" "));
            line.push(Span::styled(name, kit.bold(Role::Question)));
        };
        let maker = match self.level {
            Level::Galaxy(maker) => Some(maker),
            Level::System(star) => self.universe.stars.get(star).map(|s| s.maker),
            _ => None,
        };
        if let Some(found) = maker.and_then(|m| self.universe.makers.get(m)) {
            crumb(
                maker_logo(&found.key),
                argonauts_core::models::registry::provider::provider_name(&found.key),
            );
        }
        if let Level::System(star) = self.level
            && let Some(model) = self
                .universe
                .stars
                .get(star)
                .and_then(|s| world?.models.get(s.model))
        {
            let logo = served_logo(model);
            let name = self.named_after(&logo, &self.model_name(model));
            crumb(logo, name);
        }
        let switch = self.maps_switch();
        let used = crate::base::text::spans_width(&line) + crate::base::text::spans_width(&switch);
        // At the row's right, ending where the groupings above it end, where the row has room.
        let end = self.width.saturating_sub(GROUPINGS_EDGE);
        if used + 2 <= end {
            line.push(Span::raw(" ".repeat(end - used)));
            line.extend(switch);
        }
        line
    }

    /// `Labels · Summaries`, the map on screen painted as the level shown, the other and the dot
    /// as the other levels; `d` switches between them.
    fn maps_switch(&self) -> Vec<Span<'static>> {
        let kit = self.kit;
        let paint = |on: bool, words: &'static str| match on {
            true => Span::styled(words, kit.bold(Role::Question)),
            false => kit.dim(words),
        };
        let summaries = self.summaries();
        vec![
            paint(!summaries, "Labels"),
            kit.dim(" · "),
            paint(summaries, "Summaries"),
        ]
    }
}

/// `text` cut to `cells` with `…` in the last cell when it runs on.
fn cut(text: &str, cells: usize) -> String {
    crate::base::text::truncate_spans(&[Span::raw(text.to_owned())], cells)
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

impl Look<'_> {
    /// The colour a body of the universe is drawn in: your planet blue, a maker's galaxy and a
    /// model's star in the colour of the CLI that serves its busiest model, a planet in the
    /// text's.
    pub fn body_style(&self, pointed: Pointed) -> Style {
        let kit = self.kit;
        let star_style = |star: usize| {
            let agent = self
                .universe
                .stars
                .get(star)
                .and_then(|s| self.world?.models.get(s.model))
                .map_or("", |m| m.agent.as_str());
            kit.fg(Role::for_cli(agent))
        };
        let at = |index: u32| usize::try_from(index).unwrap_or(usize::MAX);
        match pointed {
            Pointed::Home => kit.fg(Role::AgentBlue),
            Pointed::Galaxy(maker) => self
                .universe
                .makers
                .get(at(maker))
                .and_then(|m| m.stars.first())
                .map_or(kit.fg(Role::Dim), |star| star_style(*star)),
            Pointed::Star(star) => star_style(at(star)),
            _ => kit.fg(Role::Text),
        }
    }
}
