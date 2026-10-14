//! The Overview sub-tab under the year: three groups of numbers, Tokens, Days and Sessions,
//! each a column of labels, a column of values in the accent and dim notes after them, so
//! everything lines up across and down. Under them the tokens drawn as two thin bars, by kind
//! and by CLI, with their legends; then one fun fact, what's worth knowing, and where the
//! numbers came from.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::overview::{OverviewData, OverviewPeriod, OwnCount};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use time::Duration;

use super::facts::{fun_fact, worth_knowing};
use super::look::{
    big, cli_short, cli_style, days, dollars, duration, hour_word, model_word, share, short_date,
};
use crate::format::{UNKNOWN, thousands};
use crate::kit::{Drawn, Kit, bars, fit, ljust};
use crate::overview::stats::draw;
use crate::text::{spans_width, str_width, truncate_spans};
use crate::theme::Role;

/// The first two groups' columns in cells. Tokens' is the wider: its labels are names, and
/// the longest is the app's own.
const TOKENS: usize = 45;
const DAYS: usize = 39;
/// In a group the label's column, under Tokens the names', and the value's.
const LABEL: usize = 13;
const NAME: usize = 16;
const VALUE: usize = 10;
/// From this width the groups stand side by side: two columns, then the third's label, its
/// value and a share after it.
const SIDE_BY_SIDE: usize = TOKENS + DAYS + LABEL + VALUE + 3;
/// The block's widest, so its bars stay bars on a wide screen.
const WIDEST: usize = 116;
/// What stands before each bar, and the blank cells after the words.
const BY_KIND: &str = "by kind";
const BY_CLI: &str = "by CLI";
const AFTER_WORDS: usize = 2;
/// Blank cells between the first bar and the second's words.
const BETWEEN_BARS: usize = 3;
/// Blank cells between two entries of a legend.
const LEGEND_GAP: usize = 2;

/// The kinds of tokens in the order their bar draws them, the bulk that came from cache
/// first and quiet, then what was written to it, what the models wrote, and new input.
const KINDS: [(&str, Role); 4] = [
    ("cache read", Role::TokensCached),
    ("write", Role::TokensInput),
    ("output", Role::TokensOutput),
    ("input", Role::Success),
];

/// One number: its label, its value, and what's worth saying after it.
struct Fact {
    label: String,
    value: String,
    note: String,
}

impl Fact {
    fn new(label: impl Into<String>, value: impl Into<String>, note: impl Into<String>) -> Self {
        Fact {
            label: label.into(),
            value: value.into(),
            note: note.into(),
        }
    }

    /// The label in a column `labels` cells wide and the value in its own, with a blank kept
    /// after a long one.
    fn spans(&self, kit: &Kit<'_>, labels: usize) -> Vec<Span<'static>> {
        let label = labels.max(str_width(&self.label) + 1);
        let value = VALUE.max(str_width(&self.value) + 1);
        vec![
            kit.dim(ljust(&self.label, label)),
            Span::styled(ljust(&self.value, value), kit.bold(Role::Question)),
            kit.dim(self.note.clone()),
        ]
    }
}

/// A heading and the numbers under it, their labels in a column `labels` cells wide.
struct Group {
    title: &'static str,
    labels: usize,
    facts: Vec<Fact>,
}

/// A part of a bar: its name in the legend, its tokens, and its color.
struct Part {
    name: String,
    tokens: u64,
    style: Style,
}

/// What the numbers draw from.
pub struct Numbers<'a> {
    pub data: &'a OverviewData,
    pub names: &'a DisplayNames,
}

impl Numbers<'_> {
    /// The three groups, then the two bars, in `width` cells.
    pub fn block(&self, kit: &Kit<'_>, width: usize) -> Drawn {
        let width = width.min(WIDEST);
        let mut drawn = self.groups(kit, width);
        drawn.push(Vec::new());
        for row in self.bars(kit, width) {
            drawn.push(row);
        }
        drawn
    }

    /// The groups side by side under their headings, or one under another on a screen too
    /// narrow for three columns.
    pub fn groups(&self, kit: &Kit<'_>, width: usize) -> Drawn {
        let groups = self.facts();
        let mut drawn = Drawn::new();
        if width >= SIDE_BY_SIDE {
            // The third group has what the two columns leave, and a long name is cut in it.
            let side_by_side = |cells: [Vec<Span<'static>>; 3]| {
                let [first, second, third] = cells;
                let mut row = fit(first, TOKENS);
                row.extend(fit(second, DAYS));
                row.extend(truncate_spans(&third, width - TOKENS - DAYS));
                row
            };
            let fact = |group: &Group, at: usize| {
                let fact = group.facts.get(at);
                fact.map(|fact| fact.spans(kit, group.labels))
                    .unwrap_or_default()
            };
            drawn.push(side_by_side(
                [0, 1, 2].map(|at| vec![heading(groups[at].title)]),
            ));
            drawn.push(Vec::new());
            let rows = groups.iter().map(|group| group.facts.len()).max();
            for at in 0..rows.unwrap_or(0) {
                drawn.push(side_by_side(
                    [0, 1, 2].map(|group| fact(&groups[group], at)),
                ));
            }
            return drawn;
        }
        // One under another the groups share the widest column of labels, so the values line
        // up down the screen as they do across it.
        let labels = groups.iter().map(|group| group.labels).max();
        let labels = labels.unwrap_or(LABEL);
        for (at, group) in groups.iter().enumerate() {
            if at > 0 {
                drawn.push(Vec::new());
            }
            drawn.push(vec![heading(group.title)]);
            for fact in &group.facts {
                drawn.push(truncate_spans(&fact.spans(kit, labels), width));
            }
        }
        drawn
    }

    fn facts(&self) -> [Group; 3] {
        let data = self.data;
        let own = data.claude_stats;
        let mut tokens = Vec::new();
        if let Some(own) = own {
            let counted = own_note(&own, data);
            tokens.push(Fact::new(
                self.names.cli("claude-code"),
                big(own.tokens),
                counted,
            ));
        }
        // What /stats leaves out is said only beside its count.
        let adds = own.map_or("", |_| "+ workflow agents");
        tokens.push(Fact::new("Argonauts Code", big(data.total.total()), adds));
        tokens.push(Fact::new("API value", dollars(data.api_value), ""));

        let peak_hour = data
            .hours
            .iter()
            .enumerate()
            .filter(|(_, tokens)| **tokens > 0)
            .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(&a.0)))
            .map(|(hour, _)| format!("peak {}", hour_word(hour)));
        let days_of = vec![
            Fact::new(
                "active",
                data.active_days.to_string(),
                format!("of {}", data.span_days),
            ),
            Fact::new(
                "streak",
                days(data.streak),
                format!("longest {}", data.longest_streak),
            ),
            Fact::new(
                "busiest",
                data.peak_day.map_or(UNKNOWN.to_owned(), short_date),
                peak_hour.unwrap_or_default(),
            ),
        ];

        let longest = match data.longest_session_secs {
            0 => UNKNOWN.to_owned(),
            secs => duration(secs),
        };
        let mut sessions = vec![
            Fact::new("sessions", thousands(u64::from(data.sessions())), ""),
            Fact::new("longest", longest, ""),
        ];
        if let Some(model) = data.models.first() {
            sessions.push(Fact::new(
                "favourite",
                model_word(self.names, &model.agent, model.model.as_deref()),
                share(model.tokens.total(), data.total.total()),
            ));
        }
        [
            Group {
                title: "Tokens",
                labels: NAME,
                facts: tokens,
            },
            Group {
                title: "Days",
                labels: LABEL,
                facts: days_of,
            },
            Group {
                title: "Sessions",
                labels: LABEL,
                facts: sessions,
            },
        ]
    }

    fn kinds(&self, kit: &Kit<'_>) -> Vec<Part> {
        let total = self.data.total;
        let counts = [
            total.cache_read,
            total.cache_write,
            total.output,
            total.input,
        ];
        KINDS
            .iter()
            .zip(counts)
            .map(|((name, role), tokens)| Part {
                name: (*name).to_owned(),
                tokens,
                style: kit.fg(*role),
            })
            .collect()
    }

    fn clis(&self, kit: &Kit<'_>) -> Vec<Part> {
        let clis = self.data.by_cli().into_iter();
        clis.map(|(agent, tokens)| Part {
            name: cli_short(self.names, &agent),
            tokens,
            style: cli_style(kit, &agent),
        })
        .collect()
    }

    /// The tokens by kind and, where more than one CLI worked, by CLI: side by side with each
    /// legend under its bar where both fit, else one bar a row.
    fn bars(&self, kit: &Kit<'_>, width: usize) -> Vec<Vec<Span<'static>>> {
        let kinds = self.kinds(kit);
        let clis = self.clis(kit);
        let (kinds_legend, clis_legend) = (legend(kit, &kinds), legend(kit, &clis));
        let words = |words: &str| str_width(words) + AFTER_WORDS;
        // Side by side the first bar takes five cells in eight of what the words leave.
        let bars = width.saturating_sub(words(BY_KIND) + BETWEEN_BARS + words(BY_CLI));
        let kind_cells = bars * 5 / 8;
        let cli_cells = bars - kind_cells;
        let cli_at = width - cli_cells;
        let fits = words(BY_KIND) + spans_width(&kinds_legend) + LEGEND_GAP <= cli_at
            && spans_width(&clis_legend) <= cli_cells;
        if clis.len() > 1 && fits {
            let mut row = vec![kit.dim(ljust(BY_KIND, words(BY_KIND)))];
            row.extend(bar(kit, &kinds, kind_cells));
            row.push(Span::raw(" ".repeat(BETWEEN_BARS)));
            row.push(kit.dim(ljust(BY_CLI, words(BY_CLI))));
            row.extend(bar(kit, &clis, cli_cells));
            let mut under = vec![Span::raw(" ".repeat(words(BY_KIND)))];
            under.extend(kinds_legend);
            let mut under = fit(under, cli_at);
            under.extend(clis_legend);
            return vec![row, under];
        }
        // One bar a row, the two bars starting in one column.
        let label = words(BY_KIND).max(words(BY_CLI));
        let mut rows = Vec::new();
        let mut one = |name: &str, parts: &[Part], legend: Vec<Span<'static>>| {
            let mut row = vec![kit.dim(ljust(name, label))];
            row.extend(bar(kit, parts, width.saturating_sub(label)));
            rows.push(row);
            let mut under = vec![Span::raw(" ".repeat(label))];
            under.extend(legend);
            rows.push(bars::fit_keeping_bars(under, width));
        };
        one(BY_KIND, &kinds, kinds_legend);
        if clis.len() > 1 {
            one(BY_CLI, &clis, clis_legend);
        }
        rows
    }

    /// The tokens by kind and by CLI in words, for where bars can't be read.
    pub fn shares(&self) -> Vec<String> {
        let theme = crate::theme::Theme::dark().with_level(crate::theme::ColorLevel::None);
        let kit = Kit::new(&theme);
        let words = |parts: &[Part]| {
            let whole: u64 = parts.iter().map(|part| part.tokens).sum();
            parts
                .iter()
                .map(|part| format!("{} {}", part.name, share(part.tokens, whole)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut lines = vec![format!("{BY_KIND}: {}", words(&self.kinds(&kit)))];
        let clis = self.clis(&kit);
        if clis.len() > 1 {
            lines.push(format!("{BY_CLI}: {}", words(&clis)));
        }
        lines
    }

    /// The fun fact in the accent, what's worth knowing, and `source` on its own dim line.
    pub fn words(&self, kit: &Kit<'_>, width: usize, source: String) -> Drawn {
        let mut drawn = Drawn::new();
        if let Some(fact) = fun_fact(self.data) {
            drawn.push(truncate_spans(&[kit.span(fact, Role::Question)], width));
        }
        if let Some(worth) = worth_knowing(self.data) {
            drawn.push(truncate_spans(&[kit.dim(worth)], width));
        }
        drawn.push(truncate_spans(&[kit.dim(source)], width));
        drawn
    }
}

/// What stands after Claude Code's own count: that it is /stats', or the day /stats last
/// counted when that was before yesterday.
fn own_note(own: &OwnCount, data: &OverviewData) -> String {
    if own.through + Duration::days(1) >= data.today {
        "as /stats counts".to_owned()
    } else {
        format!("/stats to {}", short_date(own.through))
    }
}

/// The parts as one thin bar, each its share of the cells.
fn bar(kit: &Kit<'_>, parts: &[Part], cells: usize) -> Vec<Span<'static>> {
    let parts: Vec<(u64, Style)> = parts.iter().map(|part| (part.tokens, part.style)).collect();
    draw::stacked(kit, &parts, cells)
}

/// `■ cache read 97%  ■ write 2%`: each part's square, its name and its share.
fn legend(kit: &Kit<'_>, parts: &[Part]) -> Vec<Span<'static>> {
    let whole: u64 = parts.iter().map(|part| part.tokens).sum();
    let mut row = Vec::new();
    for (at, part) in parts.iter().enumerate() {
        if at > 0 {
            row.push(Span::raw(" ".repeat(LEGEND_GAP)));
        }
        row.extend(draw::swatch(kit, part.style));
        row.push(Span::raw(part.name.clone()));
        row.push(kit.dim(format!(" {}", share(part.tokens, whole))));
    }
    row
}

/// `All time · Last 7 days · Last 30 days`, the one shown bold in the accent.
pub fn periods(kit: &Kit<'_>, shown: OverviewPeriod) -> Drawn {
    let mut row = Vec::new();
    for (at, period) in OverviewPeriod::ALL.into_iter().enumerate() {
        if at > 0 {
            row.push(kit.dim(" · "));
        }
        row.push(if period == shown {
            Span::styled(period.title(), kit.bold(Role::Question))
        } else {
            kit.dim(period.title())
        });
    }
    let mut drawn = Drawn::new();
    drawn.push(row);
    drawn
}

/// Bold in the screen's own text colour, as Claude Code writes a heading or a model's name.
pub fn heading(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::new().add_modifier(Modifier::BOLD))
}
