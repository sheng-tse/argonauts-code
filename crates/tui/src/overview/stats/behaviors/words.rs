//! The sub-tab's words: the line under the tabs, the legend beside the map, the list of
//! distinctive behaviors, a behavior's actions, and an action's card. Each line starts one cell
//! in, as the panel's rows do, and a second cell sets it in from the panel's edge.

use argonauts_core::behavior_map::{
    BehaviorMap, BehaviorRow, Comparison, Grouping, MapAction, Placing, Scope,
};
use argonauts_core::overview::OverviewPeriod;
use ratatui::style::Style;
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

use super::{GroupColours, LEGEND_GAP, MAP_COLUMNS, MAP_INSET, MAP_ROWS};
use crate::format::WallClock;
use crate::kit::{Drawn, Kit};
use crate::theme::Role;

/// The list's behavior column, and the most a card's prose runs across before it wraps.
const BEHAVIOR_CELLS: usize = 54;
const PROSE_CELLS: usize = 100;
/// The narrowest the behavior column gets, so a list on a narrow screen still names each one.
const FEWEST_BEHAVIOR_CELLS: usize = 12;
/// The legend's widest line, `red: Luna does this more`, which a screen too narrow to hold it
/// beside the map shows under it instead.
const LEGEND_CELLS: usize = 24;

/// Whether a screen `width` wide holds the legend beside the map.
pub(super) fn legend_beside(width: usize) -> bool {
    width >= 1 + MAP_INSET + MAP_COLUMNS + LEGEND_GAP + LEGEND_CELLS
}

pub(super) struct Look<'a> {
    pub kit: &'a Kit<'a>,
    pub map: &'a BehaviorMap,
    pub colours: &'a GroupColours,
    pub compared: Option<usize>,
    pub width: usize,
    pub clock: WallClock,
}

fn plural(count: usize, one: &str) -> String {
    match count {
        1 => format!("1 {one}"),
        _ => format!("{count} {one}s"),
    }
}

/// `What your agents did, from 562 actions in 19 runs across 6 projects · by model · Argonauts'
/// labels`, or with no action to show, where none were and what shows more.
pub(super) fn headline_words(map: &BehaviorMap) -> String {
    let request = map.request;
    let grouping = request.grouping.word();
    if map.actions.is_empty() {
        return empty_words(map);
    }
    let actions = plural(map.actions.len(), "action");
    if let Placing::Summaries { by } = &map.placing {
        let words = if map.by_words {
            " · placed by word vectors"
        } else {
            ""
        };
        return format!("Deeper map · {actions} summarized by {by} · by {grouping}{words}");
    }
    let sources = match (map.runs, map.conversations) {
        (runs, 0) => plural(runs, "run"),
        (0, talks) => plural(talks, "conversation"),
        (runs, talks) => format!(
            "{} and {}",
            plural(runs, "run"),
            plural(talks, "conversation")
        ),
    };
    let place = match (request.scope, map.projects.as_slice()) {
        (Scope::ThisProject, _) => map.here.as_deref().map(|here| format!(" in {here}")),
        (Scope::Everywhere, [one]) => Some(format!(" in {one}")),
        (Scope::Everywhere, []) => None,
        (Scope::Everywhere, many) => Some(format!(" across {}", plural(many.len(), "project"))),
    };
    let period = match request.period {
        OverviewPeriod::All => String::new(),
        other => format!(" · {}", other.title().to_lowercase()),
    };
    format!(
        "What your agents did, from {actions} in {sources}{}{period} · by {grouping} · Argonauts' labels",
        place.unwrap_or_default()
    )
}

/// `No actions in the last 7 days · p changes the period`.
fn empty_words(map: &BehaviorMap) -> String {
    let request = map.request;
    let place = match (request.scope, map.here.as_deref()) {
        (Scope::ThisProject, Some(here)) => format!(" in {here}"),
        _ => String::new(),
    };
    match (request.period, request.scope) {
        (OverviewPeriod::All, Scope::Everywhere) => {
            "No actions recorded yet: the map fills in as your agents work".to_owned()
        }
        (OverviewPeriod::All, Scope::ThisProject) => {
            format!("No actions recorded{place} yet · r shows every project")
        }
        (period, _) => format!(
            "No actions{place} in the {} · p changes the period",
            period.title().to_lowercase()
        ),
    }
}

/// What the legend calls a group in "red: … does this more": a model by its name without the
/// maker's word and the version, `Luna` for GPT 6 Luna and `Haiku` for Haiku 5.5. A group
/// named for its CLI alone, or any group but a model, keeps its whole name.
fn short_name(name: &str, grouping: Grouping, model: bool) -> String {
    if grouping != Grouping::Model || !model {
        return name.to_owned();
    }
    let words: Vec<&str> = name
        .split_whitespace()
        .filter(|word| !word.starts_with(|c: char| c.is_ascii_digit()))
        .collect();
    match words.as_slice() {
        [] => name.to_owned(),
        [one] => (*one).to_owned(),
        [.., last] => (*last).to_owned(),
    }
}

/// `text` cut to `cells` with `…`, then filled out to them.
fn padded(text: &str, cells: usize) -> String {
    let cut = cut_with_dots(text, cells);
    let room = cells.saturating_sub(cut.width());
    format!("{cut}{}", " ".repeat(room))
}

/// `text` cut to `cells` and filled out to `width`, cut short with no mark, as a column of
/// summaries or labels lines up beside the next.
fn column(text: &str, cells: usize, width: usize) -> String {
    let mut kept = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let cell = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + cell > cells {
            break;
        }
        used += cell;
        kept.push(ch);
    }
    format!("{kept}{}", " ".repeat(width.saturating_sub(used)))
}

/// `text` cut to `cells`, with `…` in the last cell when it ran on.
fn cut_with_dots(text: &str, cells: usize) -> String {
    crate::text::truncate_spans(&[Span::raw(text)], cells)
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn percent(part: usize, whole: usize) -> String {
    format!("{:.1}%", part as f64 * 100.0 / whole.max(1) as f64)
}

/// `run-20261020-123036-4e50` as `20261020-123036`; a conversation's turn by its time.
fn run_short(action: &MapAction) -> String {
    match &action.run {
        Some(run) => {
            let inner = run.strip_prefix("run-").unwrap_or(run);
            match inner.rsplit_once('-') {
                Some((stamp, _)) if stamp.contains('-') => stamp.to_owned(),
                _ => inner.to_owned(),
            }
        }
        None => {
            let at = action.at;
            format!(
                "{:04}{:02}{:02}-{:02}{:02}{:02}",
                at.year(),
                u8::from(at.month()),
                at.day(),
                at.hour(),
                at.minute(),
                at.second()
            )
        }
    }
}

impl Look<'_> {
    fn group_name(&self, group: usize) -> &str {
        self.map.groups.get(group).map_or("", |g| g.name.as_str())
    }

    pub fn summaries(&self) -> bool {
        matches!(self.map.placing, Placing::Summaries { .. })
    }

    /// The line under the tabs.
    pub fn headline(&self) -> Drawn {
        let mut out = Drawn::new();
        let words = format!(" {}", headline_words(self.map));
        out.push(vec![
            self.kit
                .dim(cut_with_dots(&words, self.width.saturating_sub(1))),
        ]);
        out
    }

    /// The map's rows with the legend beside them, for a picture drawn over the map's cells.
    pub fn beside_picture(&self) -> Drawn {
        let legend = self.legend();
        let mut out = Drawn::new();
        for row in 0..MAP_ROWS {
            let mut line = vec![Span::raw(" ".repeat(MAP_INSET + MAP_COLUMNS + LEGEND_GAP))];
            if let Some(words) = legend.get(row) {
                line.extend(words.iter().cloned());
            }
            out.push(line);
        }
        out
    }

    /// The map's rows left blank for its picture, and the legend under them, for a screen too
    /// narrow to hold the legend beside the picture.
    pub fn under_picture(&self) -> Drawn {
        let mut out = Drawn::new();
        for _ in 0..MAP_ROWS {
            out.push(Vec::new());
        }
        out.push(Vec::new());
        for line in self.braille_legend() {
            out.push(line);
        }
        out
    }

    /// The legend's cells across.
    fn legend_cells(&self) -> usize {
        self.width
            .saturating_sub(1 + MAP_INSET + MAP_COLUMNS + LEGEND_GAP)
            .max(20)
    }

    /// The legend: what is compared and what the colours say, then each group with its count.
    pub fn legend(&self) -> Vec<Vec<Span<'static>>> {
        let kit = self.kit;
        let grouping = self.map.request.grouping;
        let cells = self.legend_cells();
        let mut out: Vec<Vec<Span<'static>>> = Vec::new();
        match self.compared {
            None => {
                out.push(vec![Span::styled(
                    format!("Every {}", grouping.word()),
                    kit.bold(Role::Text),
                )]);
                out.push(vec![kit.dim("nothing compared")]);
                out.push(vec![kit.dim("←  →  compares one with")]);
                out.push(vec![kit.dim("the rest")]);
            }
            Some(group) => {
                let name = self.group_name(group).to_owned();
                out.push(vec![
                    kit.dim("Comparing "),
                    Span::styled(name.clone(), kit.bold(Role::Text)),
                ]);
                out.push(vec![
                    kit.dim(format!("with the other {}", grouping.plural())),
                ]);
                let comparison = self.map.comparisons.get(group);
                // A group with no list says why under the map, where its list would be.
                let listed = comparison.is_some_and(|c| !c.rows.is_empty());
                match comparison.and_then(|c| c.why_not.as_deref()) {
                    Some(why) if listed => {
                        let style = kit.style(Role::Dim);
                        for line in crate::text::wrap_text(why, style, cells, &[], &[]) {
                            out.push(line.spans);
                        }
                    }
                    Some(_) => {}
                    None => {
                        let model = self.map.groups[group].model.is_some();
                        let short = short_name(&name, grouping, model);
                        let text = kit.style(Role::Text);
                        out.push(vec![
                            kit.dim("red: "),
                            Span::styled(format!("{short} does this more"), text),
                        ]);
                        out.push(vec![
                            kit.dim("blue: "),
                            Span::styled(format!("{short} does this less"), text),
                        ]);
                    }
                }
            }
        }
        out.push(Vec::new());
        let room = MAP_ROWS.saturating_sub(out.len());
        let groups = self.map.groups.len();
        let shown = if groups > room {
            room.saturating_sub(1)
        } else {
            groups
        };
        // The group compared always has its row, in the last place shown when it's further on.
        let mut rows: Vec<usize> = (0..shown).collect();
        if let Some(compared) = self.compared.filter(|compared| *compared >= shown)
            && let Some(last) = rows.last_mut()
        {
            *last = compared;
        }
        for (at, group) in rows
            .into_iter()
            .filter_map(|at| Some((at, self.map.groups.get(at)?)))
        {
            let this = self.compared == Some(at);
            let mark = if self.compared.is_none() || this {
                "● "
            } else {
                "○ "
            };
            let name_style = if self.compared.is_none() || this {
                kit.style(Role::Text)
            } else {
                kit.style(Role::Dim)
            };
            let name = padded(&group.name, 17);
            out.push(vec![
                Span::styled(mark, self.colours.style(at)),
                Span::styled(name, name_style),
                kit.dim(format!("{:>4}", group.actions)),
            ]);
        }
        if shown < groups {
            out.push(vec![kit.dim(format!("and {} more", groups - shown))]);
        }
        out
    }

    /// Under a braille map, the groups on one row and what the colours behind the dots say.
    pub fn braille_legend(&self) -> Vec<Vec<Span<'static>>> {
        let kit = self.kit;
        let mut groups = vec![Span::raw(" ")];
        for (at, group) in self.map.groups.iter().enumerate() {
            let this = self.compared.is_none() || self.compared == Some(at);
            let style = if this {
                kit.style(Role::Text)
            } else {
                kit.style(Role::Dim)
            };
            groups.push(Span::styled("⣿ ", self.colours.style(at)));
            groups.push(Span::styled(format!("{}  ", group.name), style));
        }
        let mut out = vec![crate::text::truncate_spans(&groups, self.width)];
        if let Some(group) = self.compared {
            let comparison = self.map.comparisons.get(group);
            let said = match comparison.and_then(|c| c.why_not.as_deref()) {
                Some(why) => vec![kit.dim(format!(" {why}."))],
                None => vec![
                    kit.dim(" red behind the dots: "),
                    Span::styled("  ", super::braille::more_style(kit)),
                    kit.dim(format!(
                        " {} does this more · blue: less",
                        self.group_name(group)
                    )),
                ],
            };
            out.push(crate::text::truncate_spans(&said, self.width));
        }
        out
    }

    /// With nothing compared, what ← → does, while there's anything to compare.
    pub fn pick_note(&self, grouping: Grouping) -> Vec<Span<'static>> {
        if self.map.actions.is_empty() {
            return Vec::new();
        }
        vec![self.kit.dim(format!(
            " Pick a {} with ←  →  to see what it does more or less than the rest.",
            grouping.word()
        ))]
    }

    /// `lit: read git history · 6 actions, 4 of them GPT 6 Luna's`.
    pub fn lit_line(&self, row: &BehaviorRow) -> Vec<Span<'static>> {
        let kit = self.kit;
        let rest = if self.summaries() {
            format!(
                " and the {} actions summarized most like it",
                row.lit.len().saturating_sub(1)
            )
        } else {
            let name = self.compared.map_or("", |g| self.group_name(g));
            format!(
                " · {}, {} of them {name}'s",
                plural(row.lit.len(), "action"),
                row.mine
            )
        };
        let line = vec![
            kit.dim(" lit: "),
            Span::styled(row.behavior.clone(), kit.bold(Role::Text)),
            kit.dim(rest),
        ];
        crate::text::truncate_spans(&line, self.width.saturating_sub(1))
    }

    /// The compared group's distinctive behaviors, at most `fits` of them, `selected` lit.
    /// The behaviors from `first`, at most `fits` of them, `selected` lit.
    pub fn list(
        &self,
        comparison: &Comparison,
        selected: Option<usize>,
        first: usize,
        fits: usize,
    ) -> Vec<Vec<Span<'static>>> {
        let kit = self.kit;
        if comparison.rows.is_empty() {
            return Vec::new();
        }
        let name = self.compared.map_or("", |g| self.group_name(g)).to_owned();
        let counts = if self.summaries() {
            format!(" · {} actions, in the model's words", comparison.mine)
        } else {
            format!(
                " · {} actions; the others {}",
                comparison.mine, comparison.others
            )
        };
        let heading = vec![
            Span::styled(format!(" Distinctive to {name}"), kit.bold(Role::Text)),
            kit.dim(counts),
        ];
        let mut out = vec![crate::text::truncate_spans(
            &heading,
            self.width.saturating_sub(1),
        )];
        let counted = |row: &BehaviorRow| {
            (
                format!("{:>3}", row.mine),
                format!(" ({})", percent(row.mine, comparison.mine)),
                format!(
                    "   others {:>3} ({})",
                    row.others,
                    percent(row.others, comparison.others)
                ),
            )
        };
        // The counts always show whole; the behavior's words give way, cut with `…`.
        let widest_counts = comparison
            .rows
            .iter()
            .skip(first)
            .take(fits)
            .map(|row| {
                let (mine, share, others) = counted(row);
                mine.width() + share.width() + others.width()
            })
            .max()
            .unwrap_or(0);
        let behavior_cells = self
            .width
            .saturating_sub(1 + 3 + widest_counts)
            .clamp(FEWEST_BEHAVIOR_CELLS, BEHAVIOR_CELLS);
        let rows = comparison.rows.iter().enumerate().skip(first).take(fits);
        let widest = comparison
            .rows
            .iter()
            .skip(first)
            .take(fits)
            .map(|r| r.behavior.width())
            .max()
            .unwrap_or(0);
        for (at, row) in rows {
            let this = selected == Some(at);
            let cursor = if this { " ❯ " } else { "   " };
            let words = if this {
                kit.style(Role::Question)
            } else {
                kit.style(Role::Text)
            };
            let mut line = vec![Span::styled(cursor, kit.style(Role::Question))];
            if self.summaries() {
                line.push(if row.new {
                    let mut badge = kit
                        .cell(Role::BadgeText, Role::Warn)
                        .add_modifier(ratatui::style::Modifier::BOLD);
                    badge.fg = super::picture::paper_colour(kit);
                    Span::styled(" new ", badge)
                } else {
                    Span::raw("     ")
                });
                line.push(Span::raw(" "));
                line.push(Span::styled(padded(&row.behavior, widest + 2), words));
                let label = row.label.as_deref().unwrap_or_default();
                line.push(kit.dim(format!("label: {}", column(label, 36, 0))));
            } else {
                let (mine, share, others) = counted(row);
                line.push(Span::styled(padded(&row.behavior, behavior_cells), words));
                line.push(Span::styled(mine, kit.bold(Role::Text)));
                line.push(kit.dim(share));
                line.push(kit.dim(others));
            }
            out.push(line);
        }
        out
    }

    /// How many behaviors the list holds above and below the `fits` it shows from `first`,
    /// where it runs on.
    pub fn more(
        &self,
        comparison: &Comparison,
        first: usize,
        fits: usize,
    ) -> Option<Vec<Span<'static>>> {
        let (above, below) = (first, comparison.rows.len().saturating_sub(first + fits));
        let words = match (above, below) {
            (0, 0) => return None,
            (0, below) => format!("   {below} more below"),
            (above, 0) => format!("   {above} more above"),
            (above, below) => format!("   {above} more above, {below} below"),
        };
        Some(vec![self.kit.dim(words)])
    }

    /// One action on one line under the map, its dot ringed: what it was, whose, and where.
    pub fn pointed(&self, action: &MapAction) -> Vec<Vec<Span<'static>>> {
        let kit = self.kit;
        let group = self.group_name(action.group);
        let rest = match &action.summary {
            Some(_) => format!(" · {group} · {} · label: {}", action.agent, action.label),
            None => format!(" · {group} · {} · {}", action.agent, action.input),
        };
        let line = vec![
            Span::styled(" ◉ ", kit.fg(Role::Bright)),
            Span::styled(
                action
                    .summary
                    .clone()
                    .unwrap_or_else(|| action.label.clone()),
                kit.bold(Role::Text),
            ),
            kit.dim(rest),
        ];
        vec![
            crate::text::truncate_spans(&line, self.width.saturating_sub(1)),
            vec![kit.dim(
                "   the map stays as it was; move away and the ring goes; click opens its card",
            )],
        ]
    }

    /// A behavior's actions, `selected` ringed, from `first`, at most `fits` of them.
    pub fn actions(
        &self,
        behavior: &str,
        actions: &[usize],
        selected: usize,
        first: usize,
        fits: usize,
    ) -> Vec<Vec<Span<'static>>> {
        let kit = self.kit;
        let name = self.compared.map_or("", |g| self.group_name(g)).to_owned();
        let mut out = vec![if self.summaries() {
            vec![
                kit.dim(" Actions like "),
                Span::styled(behavior.to_owned(), kit.bold(Role::Text)),
                kit.dim(format!(
                    " · {} · ↑  ↓  rings each on the map",
                    actions.len()
                )),
            ]
        } else {
            vec![
                kit.dim(format!(" {name} · ")),
                Span::styled(behavior.to_owned(), kit.bold(Role::Text)),
                kit.dim(format!(" · {}", plural(actions.len(), "time"))),
            ]
        }];
        if !self.summaries() {
            out.push(Vec::new());
        }
        for (at, index) in actions.iter().enumerate().skip(first).take(fits) {
            let Some(action) = self.map.actions.get(*index) else {
                continue;
            };
            let this = at == selected;
            let cursor = if this { " ❯ " } else { "   " };
            let strong = if this {
                kit.style(Role::Question)
            } else {
                kit.style(Role::Text)
            };
            let mut line = vec![Span::styled(cursor, kit.style(Role::Question))];
            if self.summaries() {
                let summary = action.summary.as_deref().unwrap_or(&action.label);
                let quiet = if this {
                    kit.style(Role::Question)
                } else {
                    kit.style(Role::Dim)
                };
                line.push(Span::styled(
                    padded(self.group_name(action.group), 17),
                    self.colours.style(action.group),
                ));
                line.push(Span::styled(column(&action.agent, 22, 23), strong));
                line.push(Span::styled(column(summary, 50, 51), quiet));
                line.push(kit.dim(run_short(action)));
            } else {
                line.push(Span::styled(column(&action.agent, 22, 23), strong));
                line.push(kit.dim(format!("{}  ", run_short(action))));
                line.push(kit.dim(cut_with_dots(&action.input, 61)));
            }
            out.push(line);
        }
        out
    }

    /// The line that offers the deeper map until one is made.
    pub fn offer(&self) -> Vec<Span<'static>> {
        let kit = self.kit;
        vec![
            Span::styled(" ✻ ", kit.style(Role::Warn)),
            Span::styled("Deeper map: ", kit.bold(Role::Text)),
            kit.dim("a model summarizes each action, so behaviors no label names show up too. "),
            Span::styled("d", kit.style(Role::Question)),
            kit.dim(" to set it up"),
        ]
    }

    /// An action's card: whose it is and where, what it did, why it's on the map, and what its
    /// run says about it where the run says anything.
    pub fn card(&self, action: &MapAction) -> Drawn {
        let kit = self.kit;
        let mut out = Drawn::new();
        let agent = if action.model == action.cli {
            action.cli.clone()
        } else {
            format!("{} on {}", action.model, action.cli)
        };
        let mut whose = format!(" · {agent} · {}", action.role);
        if let Some(place) = &action.place {
            whose.push_str(&format!(" · {place}"));
        }
        out.push(vec![
            Span::styled(format!(" {}", action.agent), kit.bold(Role::Text)),
            kit.dim(whose),
        ]);
        let run = action
            .run
            .clone()
            .unwrap_or_else(|| "a conversation".to_owned());
        let mut when = vec![run, format!("{} project", action.project)];
        if !action.workflow.is_empty() {
            when.push(action.workflow.clone());
        }
        when.push(self.clock.clock(action.at).to_string());
        when.push(format!("its action {} of {}", action.nth, action.of));
        out.push(vec![kit.dim(format!(" {}", when.join(" · ")))]);
        let prose = self.width.saturating_sub(4).min(PROSE_CELLS);
        let section = |out: &mut Drawn, title: &str, words: &str, style: Style| {
            out.push(Vec::new());
            out.push(vec![Span::styled(
                format!(" {title}"),
                kit.bold(Role::Text),
            )]);
            for line in crate::text::wrap_text(words, style, prose, &[], &[]) {
                let mut spans = vec![Span::raw("   ")];
                spans.extend(line.spans);
                out.push(spans);
            }
        };
        section(
            &mut out,
            "What it did",
            &format!("{}: {}", action.label, action.input),
            kit.style(Role::Text),
        );
        section(
            &mut out,
            "Why it's on the map",
            &self.why(action),
            kit.style(Role::Dim),
        );
        if let Some(shows) = &action.run_shows {
            section(&mut out, "What the run shows", shows, kit.style(Role::Dim));
        }
        out
    }

    /// `GPT 6 Luna read git history in 13.6% of its actions (9 of 66); the other models in 1.8%
    /// (9 of 496).`
    fn why(&self, action: &MapAction) -> String {
        let group = action.group;
        let mine = self.map.groups.get(group).map_or(0, |g| g.actions);
        let others = self.map.actions.len().saturating_sub(mine);
        let (in_mine, in_others) = self.map.actions.iter().fold((0, 0), |(m, o), a| {
            match (a.behavior == action.behavior, a.group == group) {
                (true, true) => (m + 1, o),
                (true, false) => (m, o + 1),
                _ => (m, o),
            }
        });
        format!(
            "{} {} in {} of its actions ({in_mine} of {mine}); the other {} in {} ({in_others} of {others}).",
            self.group_name(group),
            action.behavior,
            percent(in_mine, mine),
            self.map.request.grouping.plural(),
            percent(in_others, others),
        )
    }
}
