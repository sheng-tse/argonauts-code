//! The Overview's cards: six tiles, tokens a week by CLI, the models, where the tokens went,
//! what ran through Argonauts, a few things worth knowing, and the comparison line.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::overview::{OverviewData, OverviewPeriod};
use ratatui::style::Style;
use ratatui::text::Span;
use time::{Date, Month};

use super::look::{
    amount, big, cli_style, cli_word, dollars, model_style, model_word, share, thousands,
};
use super::year::{flow, month_word, short_date, swatch};
use crate::overview::stats::draw;
use crate::text::{spans_width, truncate_spans};
use crate::theme::Role;
use crate::widgets::{Drawn, Frame, Kit, fit, merge, side_by_side, spread};

/// Rows the weekly bars stand in.
const BAR_ROWS: usize = 6;
/// Cells for the weekly bars' scale, a space after it.
const SCALE: usize = 5;
/// The least a tile is wide inside, so its value and the short form of its note fit.
const TILE_ROOM: usize = 20;

/// What the cards draw from.
pub struct Cards<'a> {
    pub data: &'a OverviewData,
    pub names: &'a DisplayNames,
}

impl Cards<'_> {
    /// The six tiles, as many to a row as fit `width` with their words whole.
    pub fn tiles(&self, kit: &Kit<'_>, width: usize) -> Vec<Drawn> {
        let tiles = self.tile_words();
        let room = width.saturating_sub(2);
        let per_row = [6, 3, 2, 1]
            .into_iter()
            .find(|n| (room + 1) / n >= TILE_ROOM + 5)
            .unwrap_or(1);
        let tile_width = (room + 1) / per_row - 1;
        tiles
            .chunks(per_row)
            .map(|row| {
                let mut drawn: Option<Drawn> = None;
                for (k, (label, value, note, good)) in row.iter().enumerate() {
                    let frame = Frame {
                        left: if k == 0 { 1 } else { 0 },
                        width: tile_width,
                        toggle: None,
                        active: false,
                    };
                    let inner = frame.inner();
                    let mut body = Drawn::new();
                    body.push(vec![kit.dim(label.clone())]);
                    body.push(vec![Span::styled(value.clone(), kit.bold(Role::Accent))]);
                    let note_style = match good {
                        Some(true) => kit.fg(Role::Success),
                        Some(false) => kit.fg(Role::Warning),
                        None => kit.style(Role::Dim),
                    };
                    // The longest form of the note that fits, else the shortest cut.
                    let words = note
                        .iter()
                        .find(|words| crate::text::str_width(words) <= inner)
                        .or(note.last())
                        .cloned()
                        .unwrap_or_default();
                    body.push(truncate_spans(&[Span::styled(words, note_style)], inner));
                    let tile = frame.draw(kit, body);
                    drawn = Some(match drawn {
                        None => tile,
                        Some(left) => side_by_side(left, tile, 1 + k * (tile_width + 1)),
                    });
                }
                drawn.unwrap_or_default()
            })
            .collect()
    }

    /// Each tile's label, value, its note's forms longest first, and whether the note is good
    /// news or bad.
    fn tile_words(&self) -> Vec<(String, String, Vec<String>, Option<bool>)> {
        let data = self.data;
        let total = data.total.total();
        let span = match data.period {
            OverviewPeriod::Week => 7,
            _ => 30,
        };
        let (trend, good) = match (data.recent, data.before) {
            (_, 0) if data.recent > 0 => (vec![format!("all in the last {span} days")], None),
            (_, 0) => (vec![format!("nothing in the last {span} days")], None),
            (recent, before) => {
                let change = (recent as f64 / before as f64 - 1.0) * 100.0;
                let arrow = if change >= 0.0 { "↑" } else { "↓" };
                let by = format!("{arrow} {:.0}%", change.abs());
                (
                    vec![
                        format!("{by} vs the {span} days before"),
                        format!("{by} vs {span} days before"),
                        format!("{by} vs before"),
                    ],
                    Some(change >= 0.0),
                )
            }
        };
        let by_cli = data
            .sources
            .iter()
            .filter(|source| source.sessions > 0)
            .map(|source| thousands(u64::from(source.sessions)))
            .collect::<Vec<_>>();
        let favourite = data.models.first();
        vec![
            ("Total tokens".into(), big(total), trend, good),
            (
                "API value".into(),
                dollars(data.api_value),
                if data.api_partial {
                    vec![
                        "at list prices, where known".into(),
                        "at list prices".into(),
                    ]
                } else {
                    vec!["at list prices".into()]
                },
                None,
            ),
            (
                "Sessions".into(),
                thousands(u64::from(data.sessions())),
                match by_cli.len() {
                    0 | 1 => vec!["on this computer".into()],
                    _ => vec![format!("{} by CLI", by_cli.join(" · ")), by_cli.join(" · ")],
                },
                None,
            ),
            (
                "Active days".into(),
                thousands(u64::from(data.active_days)),
                vec![format!("of {}", data.span_days)],
                None,
            ),
            (
                "Streak".into(),
                days(data.streak),
                vec![format!("longest {}", data.longest_streak)],
                None,
            ),
            (
                "Favourite".into(),
                favourite
                    .map(|model| model_word(self.names, &model.agent, model.model.as_deref()))
                    .unwrap_or_else(|| "none yet".into()),
                favourite
                    .map(|model| vec![format!("{} of tokens", share(model.tokens.total(), total))])
                    .unwrap_or_default(),
                None,
            ),
        ]
    }

    /// Tokens a week by CLI for the last 26 weeks, the bars stretched across the card.
    pub fn weeks(&self, kit: &Kit<'_>, width: usize) -> Drawn {
        let frame = Frame::view(width);
        let inner = frame.inner();
        let weeks = &self.data.weeks;
        let clis: Vec<String> = self
            .data
            .by_cli()
            .into_iter()
            .map(|(agent, _)| agent)
            .collect();
        let busiest = weeks.iter().max_by_key(|week| (week.tokens(), week.start));
        let mut about = vec![format!("last {} weeks", weeks.len())];
        if let Some(change) = self.weeks_change() {
            about.push(change);
        }
        if let Some(week) = busiest.filter(|week| week.tokens() > 0) {
            about.push(format!(
                "busiest {}, {}",
                short_date(week.start),
                big(week.tokens())
            ));
        }
        let mut body = Drawn::new();
        body.push(header(
            kit,
            "tokens a week, by CLI",
            &about.join(" · "),
            inner,
        ));

        let most = busiest.map_or(0, |week| week.tokens());
        if most == 0 {
            // No scale for nothing: a scale of 1 over bars of 0 reads as noise.
            body.push(vec![
                kit.dim(format!("No tokens in the last {} weeks", weeks.len())),
            ]);
            return frame.draw(kit, body);
        }
        let top = nice_top(most);
        let room = inner.saturating_sub(SCALE + 1);
        let shown = weeks.len().min(room.max(1));
        let weeks = &weeks[weeks.len() - shown..];
        let (bar_widths, gaps) = spread_bars(shown, room);
        // Each week's halves, bottom first, as the style of the CLI filling each.
        let halves: Vec<Vec<Style>> = weeks
            .iter()
            .map(|week| {
                let values: Vec<u64> = clis
                    .iter()
                    .map(|agent| {
                        week.by_cli
                            .iter()
                            .find(|(cli, _)| cli == agent)
                            .map_or(0, |(_, tokens)| *tokens)
                    })
                    .collect();
                let widths = draw::cells_of(&values, top, BAR_ROWS * 2);
                clis.iter()
                    .zip(widths)
                    .flat_map(|(agent, width)| std::iter::repeat_n(cli_style(kit, agent), width))
                    .collect()
            })
            .collect();
        for row in (0..BAR_ROWS).rev() {
            let label = if row == BAR_ROWS - 1 {
                big(top)
            } else if row == BAR_ROWS / 2 - 1 {
                big(top / 2)
            } else if row == 0 {
                "0".to_owned()
            } else {
                String::new()
            };
            let mut line = vec![kit.dim(format!("{label:>SCALE$} "))];
            for (k, column) in halves.iter().enumerate() {
                if k > 0 && gaps[k - 1] > 0 {
                    line.push(Span::raw(" ".repeat(gaps[k - 1])));
                }
                line.push(half_cells(kit, column, row, bar_widths[k]));
            }
            body.push(merge(line));
        }
        body.push(vec![kit.dim(self.week_dates(
            weeks,
            &bar_widths,
            &gaps,
            inner,
        ))]);
        let entries = clis
            .iter()
            .map(|agent| {
                vec![
                    swatch(kit, cli_style(kit, agent)),
                    Span::raw(cli_word(self.names, agent)),
                ]
            })
            .collect();
        for row in flow(entries, inner, 3) {
            body.push(row);
        }
        frame.draw(kit, body)
    }

    /// `↑ 41% since July`: the last half of the weeks against the half before.
    fn weeks_change(&self) -> Option<String> {
        let weeks = &self.data.weeks;
        let half = weeks.len() / 2;
        if half == 0 {
            return None;
        }
        let before: u64 = weeks[..half].iter().map(|week| week.tokens()).sum();
        let after: u64 = weeks[half..].iter().map(|week| week.tokens()).sum();
        if before == 0 || after == 0 {
            return None;
        }
        let change = (after as f64 / before as f64 - 1.0) * 100.0;
        let arrow = if change >= 0.0 { "↑" } else { "↓" };
        Some(format!(
            "{arrow} {:.0}% since {}",
            change.abs(),
            month_name(weeks[half].start.month())
        ))
    }

    /// `Apr 13` under every fourth week's bar, where it fits. One that would run past the
    /// card's edge, `end`, ends at the edge instead.
    fn week_dates(
        &self,
        weeks: &[argonauts_core::overview::OverviewWeek],
        widths: &[usize],
        gaps: &[usize],
        end: usize,
    ) -> String {
        let mut row = " ".repeat(SCALE + 1);
        let mut at = SCALE + 1;
        for (k, week) in weeks.iter().enumerate() {
            let date = short_date(week.start);
            let place = at.min(end.saturating_sub(date.chars().count()));
            if k % 4 == 0 && row.chars().count() <= place {
                row.push_str(&" ".repeat(place - row.chars().count()));
                row.push_str(&date);
                row.push(' ');
            }
            at += widths[k] + gaps.get(k).copied().unwrap_or(0);
        }
        row.trim_end().to_owned()
    }

    /// Every model with its CLI, a bar of its tokens, its share, its API value and sessions.
    /// `picked` is the row the keys are on.
    pub fn models(&self, kit: &Kit<'_>, width: usize, picked: Option<usize>) -> Drawn {
        let frame = Frame::view(width);
        let inner = frame.inner();
        let data = self.data;
        let mut body = Drawn::new();
        body.push(header(
            kit,
            "models",
            "sorted by tokens · enter opens one in Stats",
            inner,
        ));
        let total = data.total.total();
        let top = data.models.first().map_or(0, |model| model.tokens.total());
        // model, on, bar, tokens, share, API value, sessions: the bar takes what's left, and
        // the narrowest screens give up the sessions, the share and the CLI in that order.
        let mut cols = [20usize, 13, 0, 8, 6, 10, 9];
        let fixed = |cols: &[usize; 7]| 2 + cols.iter().sum::<usize>();
        for drop in [6, 4, 1] {
            if fixed(&cols) + 6 <= inner {
                break;
            }
            cols[drop] = 0;
        }
        cols[2] = inner.saturating_sub(fixed(&cols) + 1).max(4);
        let titles = [
            "model",
            "on",
            "",
            "tokens",
            "share",
            "API value",
            "sessions",
        ];
        let mut head = vec![Span::raw("  ")];
        for (title, width) in titles.iter().zip(cols) {
            if width > 0 {
                head.extend(fit(vec![kit.dim(*title)], width));
            }
        }
        body.push(head);
        for (at, model) in data.models.iter().enumerate() {
            let style = model_style(kit, data, &model.agent, model.model.as_deref());
            let name = model_word(self.names, &model.agent, model.model.as_deref());
            let mark = if picked == Some(at) {
                Span::styled("❯ ", kit.bold(Role::Card))
            } else {
                Span::raw("  ")
            };
            let tokens = model.tokens.total();
            let cells: [Vec<Span<'static>>; 7] = [
                vec![swatch(kit, style), Span::raw(name)],
                vec![kit.dim(cli_word(self.names, &model.agent))],
                draw::meter(kit, tokens, top, cols[2].saturating_sub(1), style),
                vec![kit.strong(amount(tokens))],
                vec![kit.dim(share(tokens, total))],
                vec![Span::raw(
                    model.api_value.map(dollars).unwrap_or("—".into()),
                )],
                vec![kit.dim(thousands(u64::from(model.sessions)))],
            ];
            let mut row = vec![mark];
            for (cell, width) in cells.into_iter().zip(cols) {
                if width > 0 {
                    row.extend(fit(cell, width));
                }
            }
            body.push(row);
        }
        if data.models.is_empty() {
            body.push(vec![kit.dim("No model has worked in this period yet.")]);
        }
        frame.draw(kit, body)
    }

    /// Where the tokens went, by folder, beside what ran through Argonauts where both fit.
    pub fn folders_and_argonauts(&self, kit: &Kit<'_>, width: usize) -> Vec<Drawn> {
        if width >= 100 {
            let half = width.saturating_sub(3) / 2;
            let left = Frame {
                left: 1,
                width: half,
                toggle: None,
                active: false,
            };
            let right = Frame {
                left: 0,
                width: width.saturating_sub(3) - half,
                toggle: None,
                active: false,
            };
            let folders = self.folders(kit, left);
            let argonauts = self.argonauts(kit, right);
            // Both frames end on the same row, so neither is cut open.
            let rows = folders.height().max(argonauts.height());
            let (folders, argonauts) = (
                self.folders_padded(kit, left, rows),
                self.argonauts_padded(kit, right, rows),
            );
            return vec![side_by_side(folders, argonauts, 1 + half + 1)];
        }
        vec![
            self.folders(kit, Frame::view(width)),
            self.argonauts(kit, Frame::view(width)),
        ]
    }

    fn folders(&self, kit: &Kit<'_>, frame: Frame) -> Drawn {
        frame.draw(kit, self.folder_rows(kit, frame.inner()))
    }

    fn folders_padded(&self, kit: &Kit<'_>, frame: Frame, rows: usize) -> Drawn {
        let mut body = self.folder_rows(kit, frame.inner());
        while body.height() + 2 < rows {
            body.push(Vec::new());
        }
        frame.draw(kit, body)
    }

    fn folder_rows(&self, kit: &Kit<'_>, inner: usize) -> Drawn {
        let mut body = Drawn::new();
        body.push(header(
            kit,
            "where it went",
            "folders, on this computer only",
            inner,
        ));
        let folders = &self.data.folders;
        let (shown, rest) = if folders.len() > 4 {
            folders.split_at(3)
        } else {
            (&folders[..], &[][..])
        };
        let others: u64 = rest.iter().map(|(_, tokens)| tokens).sum();
        let mut rows: Vec<(String, u64, Style)> = shown
            .iter()
            .map(|(path, tokens)| (folder_name(path), *tokens, kit.fg(Role::Card)))
            .collect();
        if !rest.is_empty() {
            rows.push((
                format!("{} others", rest.len()),
                others,
                kit.fg(Role::Faint),
            ));
        }
        let top = rows.iter().map(|(_, tokens, _)| *tokens).max().unwrap_or(0);
        let name_width = rows
            .iter()
            .map(|(name, ..)| crate::text::str_width(name))
            .max()
            .unwrap_or(0)
            .min(24);
        for (name, tokens, style) in rows {
            let amount = big(tokens);
            let bar = inner.saturating_sub(name_width + amount.len() + 2);
            let mut row = fit(vec![Span::raw(name)], name_width + 1);
            row.extend(draw::meter(kit, tokens, top, bar, style));
            row.push(kit.dim(format!(" {amount}")));
            body.push(row);
        }
        if folders.is_empty() {
            body.push(vec![kit.dim("No folder has tokens in this period yet.")]);
        }
        body
    }

    fn argonauts(&self, kit: &Kit<'_>, frame: Frame) -> Drawn {
        frame.draw(kit, self.argonauts_rows(kit, frame.inner()))
    }

    fn argonauts_padded(&self, kit: &Kit<'_>, frame: Frame, rows: usize) -> Drawn {
        let mut body = self.argonauts_rows(kit, frame.inner());
        while body.height() + 2 < rows {
            body.push(Vec::new());
        }
        frame.draw(kit, body)
    }

    fn argonauts_rows(&self, kit: &Kit<'_>, inner: usize) -> Drawn {
        let mut body = Drawn::new();
        body.push(header(
            kit,
            "through Argonauts",
            "since you installed it",
            inner,
        ));
        let Some(through) =
            self.data.argonauts.as_ref().filter(|through| {
                through.workflows > 0 || through.merged > 0 || through.answered > 0
            })
        else {
            body.push(vec![kit.dim("Nothing has run through Argonauts yet.")]);
            return body;
        };
        let mut kinds: Vec<String> = Vec::new();
        let mut yours = 0;
        for (name, count) in &through.by_workflow {
            if is_builtin_workflow(name) {
                kinds.push(format!("{name} {count}"));
            } else {
                yours += count;
            }
        }
        if yours > 0 {
            kinds.push(format!("yours {yours}"));
        }
        let first_pass = through
            .first_pass
            .map(|share| format!("{:.0}% passed review first time", share * 100.0));
        let rows: [(&str, u32, Vec<String>); 4] = [
            ("workflows", through.workflows, kinds),
            (
                "tasks merged",
                through.merged,
                first_pass.into_iter().collect(),
            ),
            (
                "fallbacks",
                through.fallbacks,
                (through.for_limit > 0)
                    .then(|| format!("{} for a usage limit", through.for_limit))
                    .into_iter()
                    .collect(),
            ),
            (
                "answered for you",
                through.answered,
                vec![match through.answered {
                    1 => "question on autopilot".to_owned(),
                    _ => "questions on autopilot".to_owned(),
                }],
            ),
        ];
        for (label, count, tail) in rows {
            let mut row = fit(vec![Span::raw(label)], 18);
            row.push(kit.strong(thousands(u64::from(count))));
            if !tail.is_empty() {
                let joiner = if label == "answered for you" {
                    " "
                } else {
                    " · "
                };
                row.push(kit.dim(format!("{joiner}{}", tail.join(" · "))));
            }
            body.push(truncate_spans(&row, inner));
        }
        body
    }

    /// A few lines the numbers above add up to, the number in each in bold.
    pub fn worth_knowing(&self, kit: &Kit<'_>, width: usize) -> Option<Drawn> {
        let lines = self.insights();
        if lines.is_empty() {
            return None;
        }
        let frame = Frame::view(width);
        let inner = frame.inner();
        let mut body = Drawn::new();
        body.push(header(
            kit,
            "worth knowing",
            "from the numbers above",
            inner,
        ));
        for (before, bold, after, tail) in lines {
            let mut row = vec![Span::raw(before), kit.strong(bold), Span::raw(after)];
            if let Some(tail) = tail {
                row.push(kit.dim(format!(" · {tail}")));
            }
            body.push(truncate_spans(&row, inner));
        }
        Some(frame.draw(kit, body))
    }

    /// Each line as the words before its number, the number, the words after, and a dim tail.
    fn insights(&self) -> Vec<(String, String, String, Option<String>)> {
        let data = self.data;
        let mut lines = Vec::new();
        let input = data.total.all_input();
        if input > 0 && data.total.cache_read * 2 >= input {
            lines.push((
                String::new(),
                share(data.total.cache_read, input),
                " of your input came from cache".to_owned(),
                Some("long sessions are cheap to continue".to_owned()),
            ));
        }
        let total = data.total.total();
        if total > 0 && data.subagent_tokens * 5 >= total {
            lines.push((
                String::new(),
                share(data.subagent_tokens, total),
                " of tokens ran in subagents".to_owned(),
                Some("a cheaper model for simple subagents would save the most".to_owned()),
            ));
        }
        if let Some(moved) = self.share_moved() {
            lines.push(moved);
        }
        let hours = data.longest_session_secs / 3600;
        if hours >= 1 {
            let (amount, tail) = if hours >= 48 {
                (
                    format!("{} days", hours / 24),
                    Some("probably left open in a terminal".to_owned()),
                )
            } else {
                (
                    format!("{hours}h {}m", data.longest_session_secs / 60 % 60),
                    None,
                )
            };
            lines.push((
                "Your longest session ran ".to_owned(),
                amount,
                String::new(),
                tail,
            ));
        }
        lines
    }

    /// `Codex went from 14% of your tokens in March to 34% in September`: the CLI whose share
    /// grew most between the first month and the last, when it grew ten points or more.
    fn share_moved(&self) -> Option<(String, String, String, Option<String>)> {
        let months = &self.data.months;
        let (first, last) = (months.first()?, months.last()?);
        if months.len() < 2 {
            return None;
        }
        let share_in = |month: &argonauts_core::overview::OverviewMonth, agent: &str| {
            let whole: u64 = month.by_cli.iter().map(|(_, tokens)| tokens).sum();
            let part = month
                .by_cli
                .iter()
                .find(|(cli, _)| cli == agent)
                .map_or(0, |(_, tokens)| *tokens);
            if whole == 0 {
                0.0
            } else {
                part as f64 / whole as f64 * 100.0
            }
        };
        let (agent, from, to) = self
            .data
            .by_cli()
            .into_iter()
            .map(|(agent, _)| {
                let (from, to) = (share_in(first, &agent), share_in(last, &agent));
                (agent, from, to)
            })
            .max_by(|a, b| (a.2 - a.1).total_cmp(&(b.2 - b.1)))?;
        if to - from < 10.0 {
            return None;
        }
        // A year of history starts and ends in a month of the same name, so the years tell
        // them apart.
        let same_name = first.month.month() == last.month.month();
        let named = |month: time::Date| {
            let name = month_name(month.month());
            if same_name {
                format!("{name} {}", month.year())
            } else {
                name.to_string()
            }
        };
        Some((
            format!("{} went from ", cli_word(self.names, &agent)),
            format!("{from:.0}%"),
            format!(
                " of your tokens in {} to {to:.0}% in {}",
                named(first.month),
                named(last.month)
            ),
            None,
        ))
    }

    /// `Your input and output are ~920× the tokens in A Game of Thrones`, as Claude Code's
    /// own stats say it, with the book picked by the day so it holds still while you look.
    pub fn comparison(&self, kit: &Kit<'_>, width: usize) -> Option<Drawn> {
        let said = input_and_output(self.data)?;
        let mut drawn = Drawn::new();
        drawn.push(truncate_spans(&[kit.dim(format!(" {said}"))], width));
        Some(drawn)
    }
}

/// The comparison's words, when the period has any input and output.
pub fn input_and_output(data: &OverviewData) -> Option<String> {
    let tokens = data.total.input + data.total.output;
    let books: Vec<&(&str, u64)> = BOOKS.iter().filter(|(_, size)| tokens >= *size).collect();
    if books.is_empty() {
        return None;
    }
    let (name, size) = books[usize::from(data.today.ordinal()) % books.len()];
    let times = tokens / size;
    Some(if times >= 2 {
        format!(
            "Your input and output are ~{}× the tokens in {name}",
            thousands(times)
        )
    } else {
        format!("Your input and output are about as many tokens as {name}")
    })
}

/// Books and their length in tokens, the list Claude Code's own stats compare with.
const BOOKS: &[(&str, u64)] = &[
    ("The Little Prince", 22_000),
    ("The Old Man and the Sea", 35_000),
    ("A Christmas Carol", 37_000),
    ("Animal Farm", 39_000),
    ("Fahrenheit 451", 60_000),
    ("The Great Gatsby", 62_000),
    ("Slaughterhouse-Five", 64_000),
    ("Brave New World", 83_000),
    ("The Catcher in the Rye", 95_000),
    ("Harry Potter and the Philosopher's Stone", 103_000),
    ("The Hobbit", 123_000),
    ("1984", 123_000),
    ("To Kill a Mockingbird", 130_000),
    ("Pride and Prejudice", 156_000),
    ("Dune", 244_000),
    ("Moby-Dick", 268_000),
    ("Crime and Punishment", 274_000),
    ("A Game of Thrones", 381_000),
    ("Anna Karenina", 468_000),
    ("Don Quixote", 520_000),
    ("The Lord of the Rings", 576_000),
    ("The Count of Monte Cristo", 603_000),
    ("Les Misérables", 689_000),
    ("War and Peace", 730_000),
];

/// `◆ models` at the left of `width` cells, and what it's about dim at the right, cut first.
pub fn header(kit: &Kit<'_>, title: &str, about: &str, width: usize) -> Vec<Span<'static>> {
    let left = vec![
        kit.span(kit.glyphs.widget, Role::Card),
        Span::raw(" "),
        kit.strong(title.to_owned()),
    ];
    let room = width.saturating_sub(spans_width(&left) + 2);
    let right = if crate::text::str_width(about) <= room {
        vec![kit.dim(about.to_owned())]
    } else {
        truncate_spans(&[kit.dim(about.to_owned())], room)
    };
    spread(left, right, width)
}

/// One bar column's cells on `row`, `width` wide: each cell's two halves in the styles that
/// fill them, the lower one first.
fn half_cells(kit: &Kit<'_>, column: &[Style], row: usize, width: usize) -> Span<'static> {
    let lower = column.get(row * 2);
    let upper = column.get(row * 2 + 1);
    let whole = kit
        .glyphs
        .halves
        .map_or(kit.glyphs.used, |[_, whole]| whole);
    match (lower, upper) {
        (None, _) => Span::raw(" ".repeat(width)),
        _ if !kit.has_color() => Span::raw(kit.glyphs.used.repeat(width)),
        (Some(lower), None) => Span::styled(whole.repeat(width), *lower),
        (Some(lower), Some(upper)) if lower == upper => {
            Span::styled(kit.glyphs.columns[7].repeat(width), *lower)
        }
        (Some(lower), Some(upper)) => {
            let mut style = *lower;
            style.bg = upper.fg;
            Span::styled(whole.repeat(width), style)
        }
    }
}

/// Each bar's width when `bars` share `room` cells, and the gap after each but the last. Every
/// bar is as wide as the others, so equal weeks look equal, a cell apart where they can be,
/// and the cells left over widen the gaps evenly so the bars fill the row.
pub fn spread_bars(bars: usize, room: usize) -> (Vec<usize>, Vec<usize>) {
    if bars == 0 {
        return (Vec::new(), Vec::new());
    }
    let gap = usize::from(room >= bars * 2 - 1);
    let width = ((room + gap) / bars).saturating_sub(gap).max(1);
    let between = bars - 1;
    let spare = room.saturating_sub(width * bars);
    let gaps = (0..between)
        .map(|k| (k + 1) * spare / between - k * spare / between)
        .collect();
    (vec![width; bars], gaps)
}

/// A round number at or over `top` for the scale: 1, 1.5, 2, 2.5, 3, 4, 5, 6 or 8 times a
/// power of ten.
pub fn nice_top(top: u64) -> u64 {
    if top == 0 {
        return 1;
    }
    let mut power = 1u64;
    while power.saturating_mul(10) <= top {
        power *= 10;
    }
    for step in [10, 15, 20, 25, 30, 40, 50, 60, 80, 100] {
        let candidate = power.saturating_mul(step) / 10;
        if candidate >= top {
            return candidate;
        }
    }
    power.saturating_mul(10)
}

/// The workflows Argonauts comes with. Any other is one of yours.
fn is_builtin_workflow(name: &str) -> bool {
    matches!(
        name,
        "council" | "orchestrator" | "solo" | "maker-checker" | "best-of-n" | "draft-and-vote"
    ) || name.starts_with("ablate-")
}

/// A folder by its last part, as the card names it.
fn folder_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned())
}

fn days(n: u32) -> String {
    match n {
        1 => "1 day".to_owned(),
        n => format!("{n} days"),
    }
}

fn month_name(month: Month) -> &'static str {
    match month {
        Month::January => "January",
        Month::February => "February",
        Month::March => "March",
        Month::April => "April",
        Month::May => "May",
        Month::June => "June",
        Month::July => "July",
        Month::August => "August",
        Month::September => "September",
        Month::October => "October",
        Month::November => "November",
        Month::December => "December",
    }
}

/// `Oct 2025` for another year than `today`'s, else `Feb`.
pub fn since_word(day: Date, today: Date) -> String {
    if day.year() == today.year() {
        month_word(day.month()).to_owned()
    } else {
        format!("{} {}", month_word(day.month()), day.year())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_fill_the_room_whatever_its_width_and_are_all_as_wide() {
        for room in [26, 40, 51, 80, 113, 157] {
            let (widths, gaps) = spread_bars(26, room);
            let used = widths.iter().sum::<usize>() + gaps.iter().sum::<usize>();
            assert_eq!(used, room, "{room}");
            assert!(
                widths
                    .iter()
                    .all(|width| *width == widths[0] && *width >= 1)
            );
            let (min, max) = (gaps.iter().min(), gaps.iter().max());
            assert!(max.unwrap() - min.unwrap() <= 1, "{room}: {gaps:?}");
        }
    }

    #[test]
    fn the_scale_tops_out_on_a_round_number() {
        assert_eq!(nice_top(2_900_000_000), 3_000_000_000);
        assert_eq!(nice_top(1_200), 1_500);
        assert_eq!(nice_top(7), 8);
        assert_eq!(nice_top(0), 1);
    }
}
