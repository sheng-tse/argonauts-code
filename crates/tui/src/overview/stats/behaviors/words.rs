//! The sub-tab's words under the map: the list of distinctive behaviors, a behavior's actions,
//! an action's card, and the legend under a braille map too narrow to hold it beside. Each line
//! starts one cell in, as the panel's rows do, and a second cell sets it in from the panel's
//! edge. Every line starts with a capital; key hints stay lowercase.

use argonauts_core::behavior_map::{BehaviorMap, BehaviorRow, Comparison, MapAction, Placing};
use argonauts_core::behavior_world::WorldFacts;
use argonauts_core::display_names::DisplayNames;
use ratatui::style::Style;
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

use super::GroupColours;
use super::level::Level;
use super::universe::Universe;
use super::window::Window;
use crate::format::WallClock;
use crate::kit::{Drawn, Kit};
use crate::text::{At, cut};
use crate::theme::Role;
use crate::world::Pointed;

/// The list's behavior column, and the most a card's prose runs across before it wraps.
const BEHAVIOR_CELLS: usize = 54;
const PROSE_CELLS: usize = 100;
/// The narrowest the behavior column gets, so a list on a narrow screen still names each one.
const FEWEST_BEHAVIOR_CELLS: usize = 12;
/// The blanks before a column of numbers.
const BEFORE_NUMBERS: usize = 3;
pub(super) struct Look<'a> {
    pub kit: &'a Kit<'a>,
    pub map: &'a BehaviorMap,
    pub colours: &'a GroupColours,
    pub compared: Option<usize>,
    pub width: usize,
    pub clock: WallClock,
    pub names: &'a DisplayNames,
    pub world: Option<&'a WorldFacts>,
    pub universe: &'a Universe,
    pub level: Level,
    pub picked: Option<Pointed>,
    /// `←  →`, or `h  l` with vim keys.
    pub arrows: &'static str,
    /// Logos stand before names, as the world draws them; braille has none to stand for a
    /// maker, so its names stay whole.
    pub logos: bool,
    /// Which way the planet faces.
    pub planet: crate::world::View,
    /// The map's size and where its legend goes.
    pub fit: super::Fit,
}

/// How much more or less often a compared group does a behavior than the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Class {
    MuchLess,
    Less,
    Same,
    More,
    MuchMore,
}

impl Class {
    pub fn word(self) -> &'static str {
        match self {
            Class::MuchLess => "much less here",
            Class::Less => "less here",
            Class::Same => "about the same here",
            Class::More => "more here",
            Class::MuchMore => "much more here",
        }
    }
}

/// How `group` does `behavior` against the rest: twice as often or more is much more, 1.25
/// times more, 0.8 times less and half or under much less. `None` where either side has under
/// 5 actions, or the behavior too few to tell.
pub(super) fn compare_class(map: &BehaviorMap, group: usize, behavior: &str) -> Option<Class> {
    let group_actions = map.groups.get(group)?.actions as f64;
    let others_actions = map.actions.len() as f64 - group_actions;
    if group_actions < 5.0 || others_actions < 5.0 {
        return None;
    }
    let (mine, others) =
        map.actions
            .iter()
            .filter(|a| a.behavior == behavior)
            .fold((0.0, 0.0), |(m, o), a| {
                if a.group == group {
                    (m + 1.0, o)
                } else {
                    (m, o + 1.0)
                }
            });
    if mine < 3.0 && group_actions * others / others_actions < 3.0 {
        return None;
    }
    let ratio = ((mine / group_actions) / ((others + 1.0) / others_actions)).log2();
    Some(match ratio {
        r if r >= 1.0 => Class::MuchMore,
        r if r >= 0.321928 => Class::More,
        r if r > -0.321928 => Class::Same,
        r if r > -1.0 => Class::Less,
        _ => Class::MuchLess,
    })
}

fn plural(count: usize, one: &str) -> String {
    match count {
        1 => format!("1 {one}"),
        _ => format!("{count} {one}s"),
    }
}

/// `text` cut to `cells` with `…`, then filled out to them.
fn padded(text: &str, cells: usize) -> String {
    let cut = cut(text, cells, At::Char);
    let room = cells.saturating_sub(cut.width());
    format!("{cut}{}", " ".repeat(room))
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

    /// Under a braille map, the groups on one row and what the colours behind the dots say.
    pub fn braille_legend(&self) -> Vec<Vec<Span<'static>>> {
        let kit = self.kit;
        // Every group shows: those a row can't hold go on to the next, each whole.
        let mut out: Vec<Vec<Span<'static>>> = vec![vec![Span::raw(" ")]];
        for (at, group) in self.map.groups.iter().enumerate() {
            let this = self.compared.is_none() || self.compared == Some(at);
            let style = if this {
                kit.style(Role::Text)
            } else {
                kit.style(Role::Dim)
            };
            let entry = [
                Span::styled("⣿ ", self.colours.style(at)),
                Span::styled(format!("{}  ", group.name), style),
            ];
            let row = out.last_mut().expect("a row");
            let fits = crate::text::spans_width(row) + crate::text::spans_width(&entry);
            if row.len() > 1 && fits > self.width {
                out.push(vec![Span::raw(" ")]);
            }
            out.last_mut().expect("a row").extend(entry);
        }
        let mut out: Vec<Vec<Span<'static>>> = out
            .iter()
            .map(|row| crate::text::truncate_spans(row, self.width))
            .collect();
        if let Some(group) = self.compared {
            let comparison = self.map.comparisons.get(group);
            let said = match comparison.and_then(|c| c.why_not.as_deref()) {
                Some(why) => vec![kit.dim(format!(" {why}."))],
                None => vec![
                    kit.dim(" Red behind the dots: "),
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

    /// `Lit: read git history · 6 actions, 4 of them GPT 6 Luna's`, whole: the pointer's rows
    /// hold it.
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
        vec![
            kit.dim(" Lit: "),
            Span::styled(row.behavior.clone(), kit.bold(Role::Text)),
            kit.dim(rest),
        ]
    }

    /// `Distinctive to Opus 5.5 · 63 actions; the others 760`. Summaries say how many of the
    /// group's actions the list holds, and mark what says more than its label; where the line
    /// is short of room, "in the model's words" goes first, since the headline names who wrote
    /// them.
    pub fn list_heading(&self, comparison: &Comparison) -> Vec<Span<'static>> {
        let kit = self.kit;
        let name = self.compared.map_or("", |g| self.group_name(g));
        let heading = Span::styled(format!(" Distinctive to {name}"), kit.bold(Role::Text));
        let room = self.width.saturating_sub(1);
        if !self.summaries() {
            let counts = format!(
                " · {} actions; the others {}",
                comparison.mine, comparison.others
            );
            return crate::text::truncate_spans(&[heading, kit.dim(counts)], room);
        }
        let held = comparison.rows.len();
        let counts = match held < comparison.mine {
            true => format!(" · {held} of its {} actions", comparison.mine),
            false => format!(" · {} actions", comparison.mine),
        };
        let mut note = Vec::new();
        if comparison.rows.iter().any(|row| row.new) {
            note = vec![
                kit.dim(" · "),
                self.new_badge(),
                kit.dim(" says more than its label"),
            ];
        }
        let line = |words: &str| {
            let mut line = vec![heading.clone(), kit.dim(format!("{counts}{words}"))];
            line.extend(note.iter().cloned());
            line
        };
        let whole = line(", in the model's words");
        let line = match crate::text::spans_width(&whole) <= room {
            true => whole,
            false => line(""),
        };
        crate::text::truncate_spans(&line, room)
    }

    /// ` new `, on the rows whose summary names what their label only groups.
    fn new_badge(&self) -> Span<'static> {
        let kit = self.kit;
        let mut badge = kit
            .cell(Role::BadgeText, Role::Warn)
            .add_modifier(ratatui::style::Modifier::BOLD);
        badge.fg = super::colours::paper_colour(kit);
        Span::styled(" new ", badge)
    }

    /// The compared group's distinctive behaviors the window shows, `selected` lit. A label's
    /// counts are measured over every row, so they stay put as the list scrolls, and show
    /// whole; its words give way, cut with `…`. A summary takes the row's room.
    pub fn list(
        &self,
        comparison: &Comparison,
        selected: Option<usize>,
        window: Window,
    ) -> Vec<Vec<Span<'static>>> {
        let kit = self.kit;
        let counted = |row: &BehaviorRow| {
            (
                format!("{:>3}", row.mine),
                format!(
                    "  {:>7}",
                    format!("({})", percent(row.mine, comparison.mine))
                ),
                format!(
                    "   others {:>3}  {:>7}",
                    row.others,
                    format!("({})", percent(row.others, comparison.others))
                ),
            )
        };
        let widest_counts = comparison
            .rows
            .iter()
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
        // The cursor, the badge's five cells and a blank come before a summary.
        let summary_cells = self.width.saturating_sub(1 + 3 + 5 + 1);
        let mut out = Vec::new();
        for at in window.rows() {
            let Some(row) = comparison.rows.get(at) else {
                continue;
            };
            let this = selected == Some(at);
            let cursor = if this { " ❯ " } else { "   " };
            let words = if this {
                kit.style(Role::Question)
            } else {
                kit.style(Role::Text)
            };
            let mut line = vec![Span::styled(cursor, kit.style(Role::Question))];
            if self.summaries() {
                line.push(match row.new {
                    true => self.new_badge(),
                    false => Span::raw("     "),
                });
                line.push(Span::raw(" "));
                line.push(Span::styled(
                    cut(&row.behavior, summary_cells, At::Char),
                    words,
                ));
            } else {
                let (mine, share, others) = counted(row);
                // A label that fills its column still stands three blanks before its count.
                let label = cut(
                    &super::hover::cap(&row.behavior),
                    behavior_cells.saturating_sub(BEFORE_NUMBERS),
                    At::Char,
                );
                line.push(Span::styled(padded(&label, behavior_cells), words));
                line.push(Span::styled(mine, kit.bold(Role::Text)));
                line.push(kit.dim(share));
                line.push(kit.dim(others));
            }
            out.push(line);
        }
        out
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
                super::hover::cap(action.summary.as_deref().unwrap_or(&action.label)),
                kit.bold(Role::Text),
            ),
            kit.dim(rest),
        ];
        vec![
            crate::text::truncate_spans(&line, self.width.saturating_sub(1)),
            vec![kit.dim(
                "   The map stays as it was; move away and the ring goes; click opens its card",
            )],
        ]
    }

    /// The action the keys ring on the map, in the pointer's place: what it was, the model
    /// after its logo, the agent, and its label or its input.
    pub fn ringed_line(&self, action: &MapAction) -> Vec<Span<'static>> {
        let kit = self.kit;
        let mut line = vec![
            Span::styled(" ◉ ", kit.fg(Role::Bright)),
            Span::styled(
                super::hover::cap(action.summary.as_deref().unwrap_or(&action.label)),
                kit.bold(Role::Text),
            ),
            kit.dim(" · "),
        ];
        line.extend(self.group_label(action.group));
        let rest = match &action.summary {
            Some(_) => format!(" · {} · label: {}", action.agent, action.label),
            None => format!(" · {} · {}", action.agent, action.input),
        };
        line.push(kit.dim(rest));
        line
    }

    /// Over a behavior's actions: the summary they're like and how many, with the keys that
    /// ring each where the row has room; or the group, the label and how many times, and a
    /// blank row.
    pub fn actions_heading(&self, behavior: &str, count: usize) -> Vec<Vec<Span<'static>>> {
        let kit = self.kit;
        let room = self.width.saturating_sub(1);
        if self.summaries() {
            let line = |hint: &str| {
                vec![
                    kit.dim(" Actions like "),
                    Span::styled(behavior.to_owned(), kit.bold(Role::Text)),
                    kit.dim(format!(" · {count}{hint}")),
                ]
            };
            let whole = line(" · ↑  ↓  rings each on the map");
            let line = match crate::text::spans_width(&whole) <= room {
                true => whole,
                false => line(""),
            };
            return vec![crate::text::truncate_spans(&line, room)];
        }
        let mut who = vec![Span::raw(" ")];
        if let Some(group) = self.compared {
            who.extend(self.group_label(group));
        }
        who.push(kit.dim(" · "));
        who.push(Span::styled(behavior.to_owned(), kit.bold(Role::Text)));
        who.push(kit.dim(format!(" · {}", plural(count, "time"))));
        vec![crate::text::truncate_spans(&who, room), Vec::new()]
    }

    /// The actions the window shows of a behavior's, `selected` ringed, in columns measured
    /// over every action listed, four blanks apart; the words column gives way, cut with `…`.
    pub fn actions(
        &self,
        actions: &[usize],
        selected: usize,
        window: Window,
    ) -> Vec<Vec<Span<'static>>> {
        let kit = self.kit;
        let all: Vec<&MapAction> = actions
            .iter()
            .filter_map(|at| self.map.actions.get(*at))
            .collect();
        let widest =
            |cells: &dyn Fn(&MapAction) -> usize| all.iter().map(|a| cells(a)).max().unwrap_or(0);
        let said = |a: &MapAction| a.summary.clone().unwrap_or_else(|| a.label.clone());
        let agent_cells = widest(&|a| a.agent.width()) + 4;
        // The cursor's three cells come first.
        let room = self.width.saturating_sub(1 + 3);
        let mut out = Vec::new();
        for at in window.rows() {
            let Some(action) = actions.get(at).and_then(|at| self.map.actions.get(*at)) else {
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
            let run = run_short(action);
            if self.summaries() {
                let quiet = if this {
                    kit.style(Role::Question)
                } else {
                    kit.style(Role::Dim)
                };
                let group_cells = widest(&|a| self.group_name(a.group).width()) + 4;
                // The logo and its blank, then the columns, then four blanks before the run.
                let used = 3 + group_cells + agent_cells + 4 + run.width();
                let summary_cells = widest(&|a| said(a).width())
                    .min(room.saturating_sub(used))
                    .max(1);
                if let Some(logo) = self.group_logo(action.group) {
                    line.extend(crate::kit::logo::logo_spans(&logo, kit.theme));
                    line.push(Span::raw(" "));
                } else {
                    line.push(Span::raw("   "));
                }
                line.push(Span::styled(
                    padded(self.group_name(action.group), group_cells),
                    self.colours.style(action.group),
                ));
                line.push(Span::styled(padded(&action.agent, agent_cells), strong));
                line.push(Span::styled(padded(&said(action), summary_cells), quiet));
                line.push(kit.dim(format!("    {run}")));
            } else {
                let input_cells = room.saturating_sub(agent_cells + run.width() + 4);
                line.push(Span::styled(padded(&action.agent, agent_cells), strong));
                line.push(kit.dim(format!("{run}    ")));
                line.push(kit.dim(cut(&action.input, input_cells, At::Char)));
            }
            out.push(line);
        }
        out
    }

    /// The rows that offer Summaries until they're made, wrapped at a word to the look's width
    /// with each row after the first set in under `Summaries:`. Fonts such as Iosevka draw ✻
    /// two cells wide, over the cell after it, so two blanks follow it and one always shows.
    pub fn offer(&self) -> Vec<Vec<Span<'static>>> {
        const MARK: &str = " ✻  ";
        const WHY: &str =
            "a model summarizes each action, so behaviors no label names show up too.";
        let kit = self.kit;
        let room = self.width.saturating_sub(1);
        let set_in = MARK.width();
        let mut rows = Vec::new();
        let mut row = vec![
            Span::styled(MARK, kit.style(Role::Warn)),
            Span::styled("Summaries:", kit.bold(Role::Text)),
        ];
        let mut used = crate::text::spans_width(&row);
        let words = WHY.split(' ').map(|word| vec![kit.dim(word.to_owned())]);
        // The key and what it does stay together.
        let key = vec![
            Span::styled("d", kit.style(Role::Question)),
            kit.dim(" to set them up"),
        ];
        for word in words.chain([key]) {
            let cells = crate::text::spans_width(&word);
            if used + 1 + cells > room && used > set_in {
                rows.push(std::mem::replace(
                    &mut row,
                    vec![Span::raw(" ".repeat(set_in))],
                ));
                used = set_in;
            } else {
                row.push(Span::raw(" "));
                used += 1;
            }
            row.extend(word);
            used += cells;
        }
        rows.push(row);
        rows
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
        let mut whose = format!("{agent} · {}", action.role);
        if let Some(place) = &action.place {
            whose.push_str(&format!(" · {place}"));
        }
        let mut line = vec![
            Span::styled(format!(" {}", action.agent), kit.bold(Role::Text)),
            kit.dim(" · "),
        ];
        if let Some(logo) = self.group_logo(action.group) {
            line.extend(crate::kit::logo::logo_spans(&logo, kit.theme));
            line.push(Span::raw(" "));
        }
        line.push(kit.dim(whose));
        out.push(line);
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
