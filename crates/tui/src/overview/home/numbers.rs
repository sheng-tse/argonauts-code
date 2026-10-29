//! The Overview sub-tab under the year: three groups of numbers, Tokens, Days and Sessions,
//! each a column of labels, a column of values in the accent and dim notes after them, so
//! everything lines up across and down. Under them the tokens drawn as two thin bars, by kind
//! and by CLI, with their legends, each CLI's under its logo, and a third, of who paid, where
//! anything was free or paid; then one fun fact, what's worth knowing, and where the numbers
//! came from.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::session::Billing;
use argonauts_core::logos::{Logo, logo_for};
use argonauts_core::overview::{OverviewData, OverviewPeriod, OwnCount};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use time::Duration;

use super::facts::{fun_fact, worth_knowing};
use super::look::{big, days, dollars, duration, hour_word, model_word, share, short_date};
use crate::format::{self, UNKNOWN, thousands};
use crate::kit::legend::{self, BEFORE_NUMBERS, BEFORE_SHARE, BETWEEN_WORDS, Entry};
use crate::kit::{Drawn, Kit, fit, ljust};
use crate::overview::stats::draw;
use crate::text::{str_width, truncate_spans};
use crate::theme::Role;

/// The app's own count, the longest of Tokens' labels, and what stands after it where
/// Claude Code's own count is shown too.
const OURS: &str = "Argonauts Code";
const ADDS: &str = "+ workflow agents";
/// In a group the label's column and the value's, and the second group's column.
const LABEL: usize = 13;
const VALUE: usize = 10;
const DAYS: usize = 39;
/// The block's widest, so its bars stay bars on a wide screen.
const WIDEST: usize = 116;
/// What stands before each bar.
const BY_KIND: &str = "By kind";
const BY_CLI: &str = "By CLI";
const WHO_PAID: &str = "Who paid";
/// Blank cells between the first bar and the second's words.
const BETWEEN_BARS: usize = BETWEEN_WORDS;
/// The most entries a legend puts on a row.
const LEGEND_COLUMNS: usize = 4;
/// The most rows a legend takes under a bar that has another beside it.
const ROWS_BESIDE: usize = 2;

/// The kinds of tokens in the order their bar draws them, the bulk that came from cache
/// first and quiet, then what was written to it, what the models wrote, and new input.
const KINDS: [(&str, Role); 4] = [
    ("Cache read", Role::TokensCached),
    ("Write", Role::TokensInput),
    ("Output", Role::TokensOutput),
    ("Input", Role::Success),
];

/// Who paid for tokens, in the order their bar draws them: the plans' quiet bulk first, then
/// what cost nothing, then what a key paid for.
const PAYERS: [(Billing, &str, Role); 3] = [
    (Billing::Plan, "Plan", Role::Faint),
    (Billing::Free, "Free", Role::Success),
    (Billing::Paid, "Paid", Role::AgentPurple),
];

/// Tokens' column of labels: the longest, the app's own name, and a number's room.
fn names_column() -> usize {
    str_width(OURS) + BEFORE_NUMBERS
}

/// The first group's column, the widest: its labels are names and its notes words, with a
/// column of words' room after them.
fn tokens_column() -> usize {
    names_column() + VALUE + str_width(ADDS) + BETWEEN_WORDS
}

/// From this width the groups stand side by side: two columns, then the third's label, its
/// value and a share after it.
fn side_by_side() -> usize {
    tokens_column() + DAYS + LABEL + VALUE + 3
}

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

    /// The label in a column `labels` cells wide and the value in its own, a long one keeping
    /// a number's room after the label and a share's after the value.
    fn spans(&self, kit: &Kit<'_>, labels: usize) -> Vec<Span<'static>> {
        let label = labels.max(str_width(&self.label) + BEFORE_NUMBERS);
        let value = VALUE.max(str_width(&self.value) + BEFORE_SHARE);
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

/// A part of a bar: its name in the legend, its tokens, and its colour, with the logo its
/// legend shows before the name where the part is a CLI's.
struct Part {
    name: String,
    tokens: u64,
    role: Role,
    logo: Option<Logo>,
}

/// The bars' rows, and the cells of the kinds' bar, which the payers' bar under them is as
/// wide as.
struct Bars {
    rows: Vec<Vec<Span<'static>>>,
    kind_cells: usize,
}

/// Cells of a bar's words and the blanks before the bar.
fn label(words: &str) -> usize {
    str_width(words) + BEFORE_NUMBERS
}

/// Where the bars under one another start, past the longest of their words, so switching to a
/// period with a payers' bar never moves the others.
fn bars_at() -> usize {
    [BY_KIND, BY_CLI, WHO_PAID]
        .map(label)
        .into_iter()
        .max()
        .unwrap_or(0)
}

/// What the numbers draw from.
pub struct Numbers<'a> {
    pub data: &'a OverviewData,
    pub names: &'a DisplayNames,
}

impl Numbers<'_> {
    /// The three groups, then the bars, in `width` cells.
    pub fn block(&self, kit: &Kit<'_>, width: usize) -> Drawn {
        let width = width.min(WIDEST);
        let mut drawn = self.groups(kit, width);
        drawn.push(Vec::new());
        let bars = self.bars(kit, width);
        let who_paid = self.who_paid(kit, width, &bars);
        for row in bars.rows {
            drawn.push(row);
        }
        if let Some(rows) = who_paid {
            drawn.push(Vec::new());
            for row in rows {
                drawn.push(row);
            }
        }
        drawn
    }

    /// The groups side by side under their headings, or one under another on a screen too
    /// narrow for three columns.
    pub fn groups(&self, kit: &Kit<'_>, width: usize) -> Drawn {
        let groups = self.facts();
        let mut drawn = Drawn::new();
        let tokens = tokens_column();
        if width >= side_by_side() {
            // The third group has what the two columns leave, and a long name is cut in it.
            let side_by_side = |cells: [Vec<Span<'static>>; 3]| {
                let [first, second, third] = cells;
                let mut row = fit(first, tokens);
                row.extend(fit(second, DAYS));
                row.extend(truncate_spans(&third, width - tokens - DAYS));
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
        let adds = own.map_or("", |_| ADDS);
        tokens.push(Fact::new(OURS, big(data.total.total()), adds));
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
                "Active",
                data.active_days.to_string(),
                format!("of {}", data.span_days),
            ),
            Fact::new(
                "Streak",
                days(data.streak),
                format!("longest {}", data.longest_streak),
            ),
            Fact::new(
                "Busiest",
                data.peak_day.map_or(UNKNOWN.to_owned(), short_date),
                peak_hour.unwrap_or_default(),
            ),
        ];

        let longest = match data.longest_session_secs {
            0 => UNKNOWN.to_owned(),
            secs => duration(secs),
        };
        let mut sessions = vec![
            Fact::new("Sessions", thousands(u64::from(data.sessions())), ""),
            Fact::new("Longest", longest, ""),
        ];
        if let Some(model) = data.models.first() {
            sessions.push(Fact::new(
                "Favorite",
                model_word(self.names, &model.agent, model.model.as_deref()),
                share(model.tokens.total(), data.total.total()),
            ));
        }
        [
            Group {
                title: "Tokens",
                labels: names_column(),
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

    fn kinds(&self) -> Vec<Part> {
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
                role: *role,
                logo: None,
            })
            .collect()
    }

    fn clis(&self) -> Vec<Part> {
        let clis = self.data.by_cli().into_iter();
        clis.map(|(agent, tokens)| Part {
            name: self.names.cli(&agent),
            tokens,
            role: Role::for_cli(&agent),
            logo: Some(logo_for(&agent, None)),
        })
        .collect()
    }

    /// The tokens by kind and, where more than one CLI worked, by CLI: side by side with each
    /// legend under its bar where both fit in two rows, else one bar a row. A blank row
    /// stands between each bar and its legend.
    fn bars(&self, kit: &Kit<'_>, width: usize) -> Bars {
        let (kinds, clis) = (self.kinds(), self.clis());
        let (kind_entries, cli_entries) = (entries(&kinds), entries(&clis));
        let at = bars_at();
        // Side by side the first bar takes five cells in eight of what the words leave, and
        // the CLIs' bar more where its legend needs them, so each legend stays under its bar.
        let room = width.saturating_sub(at + BETWEEN_BARS + label(BY_CLI));
        let beside = |entries: &[Entry]| legend::width_in_rows(kit, entries, ROWS_BESIDE);
        let cli_cells = (room - room * 5 / 8).max(beside(&cli_entries)).min(room);
        let kind_cells = room - cli_cells;
        let fits = beside(&kind_entries) <= kind_cells && beside(&cli_entries) <= cli_cells;
        if clis.len() > 1 && fits {
            let mut row = vec![kit.dim(ljust(BY_KIND, at))];
            row.extend(bar(kit, &kinds, kind_cells));
            row.push(Span::raw(" ".repeat(BETWEEN_BARS)));
            row.push(kit.dim(ljust(BY_CLI, label(BY_CLI))));
            row.extend(bar(kit, &clis, cli_cells));
            let kinds_legend = legend::legend(kit, &kind_entries, kind_cells, LEGEND_COLUMNS);
            let clis_legend = legend::legend(kit, &cli_entries, cli_cells, LEGEND_COLUMNS);
            let cli_at = width - cli_cells;
            let mut rows = vec![row, Vec::new()];
            for line in 0..kinds_legend.len().max(clis_legend.len()) {
                let mut under = vec![Span::raw(" ".repeat(at))];
                under.extend(kinds_legend.get(line).cloned().unwrap_or_default());
                let mut under = fit(under, cli_at);
                under.extend(clis_legend.get(line).cloned().unwrap_or_default());
                rows.push(under);
            }
            return Bars { rows, kind_cells };
        }
        let cells = width.saturating_sub(at);
        let mut rows = under_one_another(kit, BY_KIND, &kinds, &kind_entries, cells);
        if clis.len() > 1 {
            rows.push(Vec::new());
            rows.extend(under_one_another(kit, BY_CLI, &clis, &cli_entries, cells));
        }
        Bars {
            rows,
            kind_cells: cells,
        }
    }

    /// The period's parts by who paid, those with tokens. None where nothing was free or paid,
    /// since a period all on plans has nothing to say, or where the card leaves them out.
    fn payers(&self) -> Option<Vec<Part>> {
        let paid = self.data.who_paid?;
        if paid.free == 0 && paid.paid == 0 {
            return None;
        }
        let parts = PAYERS
            .iter()
            .filter(|(billing, ..)| paid.tokens(*billing) > 0)
            .map(|(billing, name, role)| Part {
                name: (*name).to_owned(),
                tokens: paid.tokens(*billing),
                role: *role,
                logo: None,
            });
        Some(parts.collect())
    }

    /// What the paid tokens cost, `~$41.20`, where any came with a cost.
    fn paid_cost(&self) -> Option<String> {
        let paid = self.data.who_paid?;
        let cost = paid.paid_cost();
        (paid.paid > 0 && (cost.cost_usd > 0.0 || !cost.cost_partial)).then(|| format::spend(&cost))
    }

    /// Who paid for the period's tokens as one more bar, under the kinds' bar and as wide,
    /// with its legend under it and what the paid part cost after the legend.
    fn who_paid(
        &self,
        kit: &Kit<'_>,
        width: usize,
        bars: &Bars,
    ) -> Option<Vec<Vec<Span<'static>>>> {
        let parts = self.payers()?;
        let at = bars_at();
        let mut rows = vec![{
            let mut row = vec![kit.dim(ljust(WHO_PAID, at))];
            row.extend(bar(kit, &parts, bars.kind_cells));
            row
        }];
        rows.push(Vec::new());
        let cost = self.paid_cost().map(|cost| kit.dim(format!(" · {cost}")));
        let after = cost.as_ref().map_or(0, |cost| str_width(&cost.content));
        let room = width.saturating_sub(at + after);
        let mut legend = legend::legend(kit, &entries(&parts), room, LEGEND_COLUMNS);
        if let (Some(last), Some(cost)) = (legend.last_mut(), cost) {
            last.push(cost);
        }
        for line in legend {
            let mut under = vec![Span::raw(" ".repeat(at))];
            under.extend(line);
            rows.push(truncate_spans(&under, width));
        }
        Some(rows)
    }

    /// The tokens by kind and by CLI in words, for where bars can't be read.
    pub fn shares(&self) -> Vec<String> {
        let words = |parts: &[Part]| {
            let whole: u64 = parts.iter().map(|part| part.tokens).sum();
            parts
                .iter()
                .map(|part| format!("{} {}", part.name, share(part.tokens, whole)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut lines = vec![format!("{BY_KIND}: {}", words(&self.kinds()))];
        let clis = self.clis();
        if clis.len() > 1 {
            lines.push(format!("{BY_CLI}: {}", words(&clis)));
        }
        if let Some(payers) = self.payers() {
            let cost = self.paid_cost().map(|cost| format!(" · {cost}"));
            lines.push(format!(
                "{WHO_PAID}: {}{}",
                words(&payers),
                cost.unwrap_or_default()
            ));
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
    let parts: Vec<(u64, Style)> = parts
        .iter()
        .map(|part| (part.tokens, kit.fg(part.role)))
        .collect();
    draw::stacked(kit, &parts, cells)
}

/// `▌ Cache read    90%      ▌ Write     5%`: each part's swatch in its colour, a CLI's logo
/// after it, its name and its share. The swatch keys the part of the bar, the logo says who.
fn entries(parts: &[Part]) -> Vec<Entry> {
    let whole: u64 = parts.iter().map(|part| part.tokens).sum();
    let entry = |part: &Part| {
        let entry = Entry::new(part.role, part.name.clone(), "").shared(share(part.tokens, whole));
        match &part.logo {
            Some(logo) => entry.with_logo(logo.clone()),
            None => entry,
        }
    };
    parts.iter().map(entry).collect()
}

/// A bar the width's room after its words, a blank row, and its legend under the bar.
fn under_one_another(
    kit: &Kit<'_>,
    words: &str,
    parts: &[Part],
    entries: &[Entry],
    cells: usize,
) -> Vec<Vec<Span<'static>>> {
    let at = bars_at();
    let mut row = vec![kit.dim(ljust(words, at))];
    row.extend(bar(kit, parts, cells));
    let mut rows = vec![row, Vec::new()];
    for line in legend::legend(kit, entries, cells, LEGEND_COLUMNS) {
        let mut under = vec![Span::raw(" ".repeat(at))];
        under.extend(line);
        rows.push(under);
    }
    rows
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
