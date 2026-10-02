//! The `/`, `@`, and `?` lists take the place of the mode line and everything under it, so
//! what you type stays where it was and the conversation never moves. Before you type a
//! letter, the `/` list shows its commands indented under category names in the accent color,
//! dim and bold; typing filters across all of them, best match first. Every entry is one row.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::render::RenderCtx;
use crate::text::{sanitize_line, spans_width, str_width, truncate_line, truncate_spans};
use crate::theme::Role;

/// Columns a command's name is padded to, as Claude Code pads its menu.
const NAME_COLUMNS: usize = 30;
/// Entries start under what you type, after the prompt's `❯ `, grouped or not, so a name never
/// moves as you type. Category names sit out at the prompt's column instead.
const ENTRY_INDENT: usize = 2;
/// Columns between the `?` panel's columns of hints. Claude Code leaves two or three.
const SHORTCUT_GAP: usize = 3;
const MAX_COMMANDS: usize = 12;
const MAX_MENTIONS: usize = 15;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    /// Without its `/` or `@`.
    pub name: String,
    pub description: String,
    /// Shown at the right of an `@` entry, such as `agent` or `card`.
    pub kind: Option<String>,
    /// A command that can't run without an argument, so Enter completes it instead.
    pub needs_argument: bool,
    /// The category a command is listed under, such as `Session`.
    pub group: Option<String>,
}

impl MenuItem {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            kind: None,
            needs_argument: false,
            group: None,
        }
    }

    pub fn in_group(mut self, group: impl Into<String>) -> Self {
        self.group = Some(group.into());
        self
    }

    pub fn needing_argument(mut self) -> Self {
        self.needs_argument = true;
        self
    }

    pub fn with_kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = Some(kind.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MenuKind {
    Commands,
    Mentions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEvent {
    /// The selection moved or the menu closed.
    Moved,
    /// The input's whole new text, such as `/model ` or `ask @codex `.
    Complete(String),
    Run(String),
    Unhandled,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Menus {
    pub commands: Vec<MenuItem>,
    pub mentions: Vec<MenuItem>,
    /// Hints for the `?` panel, such as `ctrl + o for the transcript`.
    pub shortcuts: Vec<String>,
    pub shortcuts_open: bool,
    selected: usize,
    /// The input text Esc closed the menu for. It opens again at the next edit, or once the
    /// text is something else.
    dismissed: Option<String>,
}

/// `letters` are byte offsets into the name.
struct Match<'a> {
    item: &'a MenuItem,
    letters: Vec<usize>,
}

/// The list the input opens: which one, what follows its `/` or `@`, and where that sigil sits.
struct Opened<'t> {
    kind: MenuKind,
    query: &'t str,
    start: usize,
}

impl Menus {
    /// The list `text` opens and the query after its `/` or `@`.
    pub fn open_for<'t>(&self, text: &'t str) -> Option<(MenuKind, &'t str)> {
        self.opened(text).map(|open| (open.kind, open.query))
    }

    /// `/` opens commands at the start or after a space mid-prompt, and `@` opens mentions
    /// at the start of any word.
    fn opened<'t>(&self, text: &'t str) -> Option<Opened<'t>> {
        if self.dismissed.as_deref() == Some(text) {
            return None;
        }
        if let Some(query) = text.strip_prefix('/')
            && !query.contains(char::is_whitespace)
            && !self.commands.is_empty()
        {
            return Some(Opened {
                kind: MenuKind::Commands,
                query,
                start: 0,
            });
        }
        if text.ends_with(char::is_whitespace) {
            return None;
        }
        let start = text
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map_or(0, |(at, c)| at + c.len_utf8());
        let word = &text[start..];
        if let Some(query) = word.strip_prefix('/')
            && start > 0
            && !self.commands.is_empty()
        {
            return Some(Opened {
                kind: MenuKind::Commands,
                query,
                start,
            });
        }
        let query = word
            .strip_prefix('@')
            .filter(|_| !self.mentions.is_empty())?;
        Some(Opened {
            kind: MenuKind::Mentions,
            query,
            start,
        })
    }

    pub fn is_open(&self, text: &str) -> bool {
        self.open_for(text)
            .is_some_and(|(kind, query)| !self.matches(kind, query).is_empty())
    }

    /// Tab completes the top match; Enter runs a command typed at the start and completes
    /// anything else, such as a mention or a `/` mid-prompt.
    pub fn handle_key(&mut self, key: KeyEvent, text: &str) -> MenuEvent {
        let Some(Opened { kind, query, start }) = self.opened(text) else {
            return MenuEvent::Unhandled;
        };
        let count = self.matches(kind, query).len();
        if count == 0 || !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
            return MenuEvent::Unhandled;
        }
        let selected = self.selected.min(count - 1);
        match key.code {
            KeyCode::Up => {
                self.selected = selected.checked_sub(1).unwrap_or(count - 1);
                MenuEvent::Moved
            }
            KeyCode::Down => {
                self.selected = (selected + 1) % count;
                MenuEvent::Moved
            }
            KeyCode::Esc => {
                self.dismissed = Some(text.to_owned());
                MenuEvent::Moved
            }
            KeyCode::Tab | KeyCode::Enter => {
                let item = self.matches(kind, query)[selected].item;
                let (name, needs_argument) = (item.name.clone(), item.needs_argument);
                self.selected = 0;
                let sigil = match kind {
                    MenuKind::Commands => '/',
                    MenuKind::Mentions => '@',
                };
                let runs = kind == MenuKind::Commands && start == 0 && !needs_argument;
                if runs && key.code == KeyCode::Enter {
                    MenuEvent::Run(format!("/{name}"))
                } else {
                    MenuEvent::Complete(format!("{}{sigil}{name} ", &text[..start]))
                }
            }
            _ => MenuEvent::Unhandled,
        }
    }

    /// After any edit, even one that ends on the text Esc closed the menu for.
    pub fn text_changed(&mut self) {
        self.selected = 0;
        self.dismissed = None;
    }

    pub fn rows(
        &self,
        text: &str,
        ctx: &RenderCtx<'_>,
        max_rows: usize,
    ) -> Option<Vec<Line<'static>>> {
        if self.shortcuts_open && text.is_empty() {
            return Some(self.shortcut_rows(ctx));
        }
        let Opened { kind, query, .. } = self.opened(text)?;
        let matches = self.matches(kind, query);
        if matches.is_empty() {
            return None;
        }
        let cap = match kind {
            MenuKind::Commands => MAX_COMMANDS,
            MenuKind::Mentions => MAX_MENTIONS,
        };
        let selected = self.selected.min(matches.len() - 1);
        let grouped = kind == MenuKind::Commands && query.is_empty();
        let group = |i: usize| matches[i].item.group.as_deref().filter(|_| grouped);
        let entries: Vec<Line<'static>> = matches
            .iter()
            .enumerate()
            .map(|(i, m)| entry_row(m, kind, i == selected, ctx))
            .collect();
        // A group's header tops its first entry, and the window's first entry, so the group
        // in view is always named.
        let header = |i: usize, start: usize| {
            group(i).filter(|this| i == start || group(i - 1) != Some(*this))
        };
        let height = |start: usize, end: usize| -> usize {
            (start..end)
                .map(|i| 1 + usize::from(header(i, start).is_some()))
                .sum()
        };
        // The window keeps the pick in view, whole, within both the cap and the rows left.
        let limit = max_rows.max(1);
        let mut start = (selected + 1).saturating_sub(cap);
        let mut end = (start + cap).min(matches.len());
        while end > selected + 1 && height(start, end) > limit {
            end -= 1;
        }
        while start < selected && height(start, end) > limit {
            start += 1;
        }
        let mut rows = Vec::new();
        for (i, entry) in entries.into_iter().enumerate().take(end).skip(start) {
            if let Some(title) = header(i, start) {
                let title = sanitize_line(title).trim().to_owned();
                let line = Line::from(Span::styled(title, header_style(ctx)));
                rows.push(truncate_line(
                    &line,
                    usize::from(ctx.width).saturating_sub(1),
                ));
            }
            rows.push(entry);
        }
        rows.truncate(limit);
        Some(rows)
    }

    /// Entries whose name starts with `query`, ignoring case, then those with a word inside
    /// the name that does, such as `opus` in `claude-code/opus`, then those that match once
    /// the name's separators are gone, such as `adddir` for `add-dir`. Each group keeps the
    /// list's order.
    fn matches(&self, kind: MenuKind, query: &str) -> Vec<Match<'_>> {
        let items = match kind {
            MenuKind::Commands => &self.commands,
            MenuKind::Mentions => &self.mentions,
        };
        let query: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
        let mut found: Vec<(u8, Match<'_>)> = items
            .iter()
            .filter_map(|item| {
                let (rank, letters) = match_name(&item.name, &query)?;
                Some((rank, Match { item, letters }))
            })
            .collect();
        found.sort_by_key(|(rank, _)| *rank);
        found.into_iter().map(|(_, m)| m).collect()
    }

    /// Up to three columns, each as wide as its longest hint, as Claude Code sizes them. A
    /// hint is cut only when even one column is too narrow for it.
    fn shortcut_rows(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let width = usize::from(ctx.width);
        let dim = ctx.theme.style(Role::Dim);
        let hints: Vec<String> = self
            .shortcuts
            .iter()
            .map(|hint| sanitize_line(hint).trim().to_owned())
            .collect();
        let layout = |columns: usize| {
            let per_column = hints.len().div_ceil(columns);
            let widths: Vec<usize> = hints
                .chunks(per_column.max(1))
                .map(|column| column.iter().map(|hint| str_width(hint)).max().unwrap_or(0))
                .collect();
            let total = 2 + widths.iter().sum::<usize>() + SHORTCUT_GAP * (widths.len() - 1);
            (per_column, widths, total)
        };
        let (per_column, widths, _) = (1..=3)
            .rev()
            .map(layout)
            .find(|(_, _, total)| *total <= width)
            .unwrap_or_else(|| layout(1));
        (0..per_column)
            .map(|row| {
                let mut spans = vec![Span::raw("  ")];
                for column in 0..widths.len() {
                    let Some(hint) = hints.get(column * per_column + row) else {
                        continue;
                    };
                    let start = 2 + widths[..column].iter().sum::<usize>() + SHORTCUT_GAP * column;
                    let used: usize = spans.iter().map(Span::width).sum();
                    spans.push(Span::raw(" ".repeat(start.saturating_sub(used))));
                    spans.push(Span::styled(hint.clone(), dim));
                }
                truncate_line(&Line::from(spans), width)
            })
            .collect()
    }
}

fn header_style(ctx: &RenderCtx<'_>) -> Style {
    ctx.theme
        .style(Role::Accent)
        .add_modifier(Modifier::DIM | Modifier::BOLD)
}

/// One row: the name padded to its column, then the description, cut with `…` to fit. Like the
/// mode line, a row leaves the last column empty.
fn entry_row(m: &Match<'_>, kind: MenuKind, selected: bool, ctx: &RenderCtx<'_>) -> Line<'static> {
    let theme = ctx.theme;
    let width = usize::from(ctx.width).saturating_sub(1);
    let indent = ENTRY_INDENT;
    let (name_style, detail_style) = if selected && !theme.has_color() {
        let reversed = Style::new().add_modifier(Modifier::REVERSED);
        (reversed, Style::new())
    } else if selected {
        (theme.style(Role::Card), theme.style(Role::Card))
    } else {
        (theme.style(Role::Dim), theme.style(Role::Dim))
    };
    let sigil = match kind {
        MenuKind::Commands => "/",
        MenuKind::Mentions => "@",
    };
    let mut spans = vec![
        Span::raw(" ".repeat(indent)),
        Span::styled(sigil, name_style),
    ];
    spans.extend(highlight(&m.item.name, &m.letters, name_style));
    let name_width = str_width(sigil) + str_width(&m.item.name);
    let pad = NAME_COLUMNS.saturating_sub(name_width).max(2);
    spans.push(Span::raw(" ".repeat(pad)));
    let kind_word = m
        .item
        .kind
        .as_deref()
        .map(|kind| sanitize_line(kind).trim().to_owned())
        .filter(|kind| !kind.is_empty());
    let kind_width = kind_word.as_deref().map_or(0, |kind| str_width(kind) + 1);
    let text_col = indent + name_width + pad;
    let room = width.saturating_sub(text_col + kind_width);
    let description = sanitize_line(&m.item.description).trim().to_owned();
    let description = truncate_spans(&[Span::styled(description, detail_style)], room);
    let used = text_col + spans_width(&description);
    spans.extend(description);
    if let Some(kind) = kind_word
        && used + kind_width <= width
    {
        spans.push(Span::raw(" ".repeat(width + 1 - used - kind_width)));
        spans.push(Span::styled(kind, theme.style(Role::Dim)));
    }
    truncate_line(&Line::from(spans), width)
}

fn highlight(text: &str, letters: &[usize], style: Style) -> Vec<Span<'static>> {
    let bold = style.add_modifier(Modifier::BOLD);
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (at, c) in text.char_indices() {
        let this = if letters.contains(&at) { bold } else { style };
        match spans.last_mut() {
            Some(last) if last.style == this => last.content.to_mut().push(c),
            _ => spans.push(Span::styled(c.to_string(), this)),
        }
    }
    spans
}

fn is_separator(c: char) -> bool {
    matches!(c, '-' | '_' | '/' | '.' | ':' | ' ')
}

/// How well `name` matches `query` (lowercase), and the byte offsets of the matched letters:
/// 0 for a prefix, 1 for a word inside the name, 2 with separators dropped.
fn match_name(name: &str, query: &[char]) -> Option<(u8, Vec<usize>)> {
    let chars: Vec<(usize, char)> = name.char_indices().collect();
    let prefix_at = |from: usize| -> Option<Vec<usize>> {
        let rest = chars.get(from..)?;
        if rest.len() < query.len() {
            return None;
        }
        rest.iter()
            .zip(query)
            .all(|(&(_, c), &q)| c.to_lowercase().eq(std::iter::once(q)))
            .then(|| rest[..query.len()].iter().map(|&(at, _)| at).collect())
    };
    if let Some(letters) = prefix_at(0) {
        return Some((0, letters));
    }
    let word_starts = (1..chars.len()).filter(|&i| is_separator(chars[i - 1].1));
    for start in word_starts {
        if let Some(letters) = prefix_at(start) {
            return Some((1, letters));
        }
    }
    let solid: Vec<(usize, char)> = chars
        .into_iter()
        .filter(|&(_, c)| !is_separator(c))
        .collect();
    if solid.len() < query.len()
        || !solid
            .iter()
            .zip(query)
            .all(|(&(_, c), &q)| c.to_lowercase().eq(std::iter::once(q)))
    {
        return None;
    }
    Some((2, solid[..query.len()].iter().map(|&(at, _)| at).collect()))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::keymap::Keymap;
    use crate::render::Clock;
    use crate::text::plain_text;
    use crate::theme::{Glyphs, Theme};

    fn menus() -> Menus {
        Menus {
            commands: vec![
                MenuItem::new("model", "Pick the default agent's model"),
                MenuItem::new("preset", "Pick the crew preset"),
                MenuItem::new("resume", "Reattach to a run that is still going"),
            ],
            mentions: vec![
                MenuItem::new("codex", "owner of card-003").with_kind("agent"),
                MenuItem::new("card-002", "Add the JSON writer").with_kind("card"),
            ],
            shortcuts: Keymap::default().shortcut_hints(),
            ..Menus::default()
        }
    }

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn slash_lists_matching_commands_with_the_top_one_in_the_card_color() {
        let theme = theme();
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let menus = menus();
        let rows = menus.rows("/", &ctx, 10).unwrap();
        let row = |name: &str, text: &str| format!("  {name:<30}{text}");
        assert_eq!(
            plain_text(&rows),
            [
                row("/model", "Pick the default agent's model"),
                row("/preset", "Pick the crew preset"),
                row("/resume", "Reattach to a run that is still going"),
            ]
            .join("\n")
        );
        assert_eq!(rows[0].spans[1].style, theme.style(Role::Card));
        assert_eq!(rows[1].spans.last().unwrap().style, theme.style(Role::Dim));
        let rows = menus.rows("/re", &ctx, 10).unwrap();
        assert_eq!(
            plain_text(&rows[..1]),
            row("/resume", "Reattach to a run that is still going")
        );
        assert!(rows[0].spans[2].style.add_modifier.contains(Modifier::BOLD));
        assert!(menus.rows("/zz", &ctx, 10).is_none());
        assert!(menus.rows("/model now", &ctx, 10).is_none());
    }

    fn grouped() -> Menus {
        let item = |name: &str, group: &str| MenuItem::new(name, "x").in_group(group);
        Menus {
            commands: vec![
                item("clear", "Session"),
                item("resume", "Session"),
                item("ask", "Work"),
                item("solo", "Work"),
                item("status", "Runs"),
                item("stop", "Runs"),
            ],
            ..Menus::default()
        }
    }

    #[test]
    fn slash_alone_lists_commands_indented_under_dim_bold_accent_category_names() {
        let theme = theme();
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let menus = grouped();
        let rows = menus.rows("/", &ctx, 20).unwrap();
        let text = plain_text(&rows);
        let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
        assert_eq!(
            lines,
            [
                "Session",
                "  /clear                        x",
                "  /resume                       x",
                "Work",
                "  /ask                          x",
                "  /solo                         x",
                "Runs",
                "  /status                       x",
                "  /stop                         x",
            ]
        );
        let heading = theme
            .style(Role::Accent)
            .add_modifier(Modifier::DIM | Modifier::BOLD);
        assert_eq!(rows[0].spans[0].style, heading);
        assert_eq!(rows[3].spans[0].style, heading);
        // The pick is the first command, never a header.
        assert_eq!(rows[1].spans[1].style, theme.style(Role::Card));
        assert_eq!(rows[2].spans[1].style, theme.style(Role::Dim));
    }

    #[test]
    fn a_name_stays_in_its_column_once_you_type() {
        let theme = theme();
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let menus = grouped();
        let column = |text: &str, name: &str| {
            let rows = plain_text(&menus.rows(text, &ctx, 20).unwrap());
            let row = rows
                .lines()
                .find(|row| row.contains(name))
                .unwrap()
                .to_owned();
            row.find(name).unwrap()
        };
        assert_eq!(column("/", "/status"), 2);
        assert_eq!(column("/st", "/status"), 2);
    }

    #[test]
    fn every_description_is_one_row_cut_with_an_ellipsis() {
        let theme = theme();
        let long = "Where each run stands: goal, plan, cards, budget, quotas. Costs nothing. \
                    s in it asks the narrator for a summary";
        let menus = Menus {
            commands: vec![
                MenuItem::new("status", long).in_group("Runs"),
                MenuItem::new("stop", "Stop a run").in_group("Runs"),
            ],
            ..Menus::default()
        };
        for width in [40, 80, 100, 120, 200] {
            let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
            for query in ["/", "/st"] {
                let rows = menus.rows(query, &ctx, 20).unwrap();
                let text = plain_text(&rows);
                let entries: Vec<&str> = text.lines().filter(|row| row.contains('/')).collect();
                assert_eq!(entries.len(), 2, "{text}");
                for row in &rows {
                    assert!(row.width() < usize::from(width), "{row:?}");
                }
                let status = entries[0];
                if width < 200 {
                    assert!(status.ends_with('…'), "{status:?}");
                } else {
                    assert!(status.ends_with("summary"), "{status:?}");
                }
            }
        }
    }

    #[test]
    fn typing_filters_across_every_category_without_headers() {
        let theme = theme();
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let menus = grouped();
        assert_eq!(names(&menus, "/s", &ctx), ["/solo", "/status", "/stop"]);
        assert_eq!(names(&menus, "/re", &ctx), ["/resume"]);
    }

    #[test]
    fn a_short_window_keeps_the_pick_and_names_its_group() {
        let theme = theme();
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let mut menus = grouped();
        for _ in 0..4 {
            menus.handle_key(key(KeyCode::Down), "/");
        }
        let rows = menus.rows("/", &ctx, 4).unwrap();
        let text = plain_text(&rows);
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        assert_eq!(lines.len(), 4, "{text}");
        assert!(lines.contains(&"/status                       x"), "{text}");
        assert_eq!(lines[0], "Work", "{text}");
        assert_eq!(lines[2], "Runs", "{text}");
    }

    #[test]
    fn keys_pick_complete_run_and_close() {
        let mut menus = menus();
        assert_eq!(menus.handle_key(key(KeyCode::Down), "/"), MenuEvent::Moved);
        assert_eq!(
            menus.handle_key(key(KeyCode::Tab), "/"),
            MenuEvent::Complete("/preset ".into())
        );
        assert_eq!(
            menus.handle_key(key(KeyCode::Enter), "/mo"),
            MenuEvent::Run("/model".into())
        );
        assert_eq!(
            menus.handle_key(key(KeyCode::Enter), "ask @co"),
            MenuEvent::Complete("ask @codex ".into())
        );
        assert_eq!(menus.handle_key(key(KeyCode::Esc), "/"), MenuEvent::Moved);
        assert!(!menus.is_open("/"));
        assert!(menus.is_open("/p"));
        assert_eq!(
            menus.handle_key(key(KeyCode::Char('x')), "/p"),
            MenuEvent::Unhandled
        );
    }

    fn names(menus: &Menus, text: &str, ctx: &RenderCtx<'_>) -> Vec<String> {
        menus
            .rows(text, ctx, 20)
            .unwrap_or_default()
            .iter()
            .map(|row| {
                let text = plain_text(std::slice::from_ref(row));
                text.split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn slash_menu_filters_by_prefix_and_word() {
        let theme = theme();
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let menus = Menus {
            commands: ["set-model", "model", "add-dir", "checkpoints", "resume"]
                .map(|name| MenuItem::new(name, "x"))
                .to_vec(),
            mentions: ["claude-code/opus", "card-002", "codex"]
                .map(|name| MenuItem::new(name, "x"))
                .to_vec(),
            ..Menus::default()
        };
        assert_eq!(names(&menus, "/mod", &ctx), ["/model", "/set-model"]);
        assert_eq!(names(&menus, "/DIR", &ctx), ["/add-dir"]);
        assert_eq!(names(&menus, "/adddir", &ctx), ["/add-dir"]);
        assert!(menus.rows("/points", &ctx, 20).is_none());
        assert!(menus.rows("/rsm", &ctx, 20).is_none());
        assert_eq!(names(&menus, "ask @opus", &ctx), ["@claude-code/opus"]);
        assert_eq!(names(&menus, "@002", &ctx), ["@card-002"]);
        assert_eq!(
            names(&menus, "@c", &ctx),
            ["@claude-code/opus", "@card-002", "@codex"]
        );

        let rows = menus.rows("/dir", &ctx, 20).unwrap();
        let bold: String = rows[0]
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(bold, "dir");
    }

    #[test]
    fn slash_menu_tab_completes_mid_prompt() {
        let theme = theme();
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let mut menus = menus();
        menus
            .commands
            .push(MenuItem::new("solo", "One agent does it").needing_argument());
        assert_eq!(
            menus.handle_key(key(KeyCode::Tab), "please /res"),
            MenuEvent::Complete("please /resume ".into())
        );
        assert_eq!(
            menus.handle_key(key(KeyCode::Enter), "then\n/mo"),
            MenuEvent::Complete("then\n/model ".into())
        );
        assert_eq!(names(&menus, "please /pre", &ctx), ["/preset"]);
        assert_eq!(
            menus.handle_key(key(KeyCode::Enter), "/sol"),
            MenuEvent::Complete("/solo ".into())
        );
        assert_eq!(
            menus.handle_key(key(KeyCode::Enter), "/res"),
            MenuEvent::Run("/resume".into())
        );
        assert!(!menus.is_open("a/res"));
        assert!(!menus.is_open("see /tmp/x"));
        assert!(!menus.is_open("please /res "));
    }

    #[test]
    fn mentions_show_their_kind_at_the_right() {
        let theme = theme();
        let ctx = RenderCtx::new(60, &theme, Clock::still(Instant::now()));
        let rows = menus().rows("ask @c", &ctx, 10).unwrap();
        let text = plain_text(&rows);
        assert!(text.starts_with("  @codex"), "{text}");
        assert!(rows[0].width() <= 60);
        assert!(text.lines().next().unwrap().ends_with("agent"), "{text}");
    }

    #[test]
    fn the_shortcuts_panel_lays_the_keymaps_hints_out_in_three_columns() {
        let theme = theme();
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let mut menus = menus();
        assert!(menus.rows("", &ctx, 10).is_none());
        menus.shortcuts_open = true;
        let rows_per_column = menus.shortcuts.len().div_ceil(3);
        for width in [80, 100, 120] {
            let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
            let rows = menus.rows("", &ctx, 10).unwrap();
            let text = plain_text(&rows);
            assert_eq!(rows.len(), rows_per_column, "{width}\n{text}");
            assert!(rows.iter().all(|row| row.width() <= usize::from(width)));
            for hint in &menus.shortcuts {
                assert!(text.contains(hint.as_str()), "{width}: {hint}\n{text}");
            }
            let first = plain_text(&rows[..1]);
            assert!(first.starts_with("  / for commands   "), "{first}");
            let third = &menus.shortcuts[2 * rows_per_column];
            assert!(first.trim_end().ends_with(third.as_str()), "{first}");
        }
        // Too narrow for three, the hints take two columns and stay whole.
        let ctx = RenderCtx::new(60, &theme, Clock::still(Instant::now()));
        let text = plain_text(&menus.rows("", &ctx, 10).unwrap());
        assert_eq!(text.lines().count(), menus.shortcuts.len().div_ceil(2));
        for hint in &menus.shortcuts {
            assert!(text.contains(hint.as_str()), "60: {hint}\n{text}");
        }
    }

    #[test]
    fn unselected_rows_are_dim_and_no_color_reverses_the_top_one() {
        let theme = theme();
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let rows = menus().rows("/", &ctx, 10).unwrap();
        assert_eq!(rows[1].spans[1].style, theme.style(Role::Dim));
        let plain = Theme::dark().with_level(crate::theme::ColorLevel::None);
        let ctx = RenderCtx::new(80, &plain, Clock::still(Instant::now()));
        let rows = menus().rows("/", &ctx, 10).unwrap();
        assert!(
            rows[0].spans[1]
                .style
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            !rows[1].spans[1]
                .style
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }
}
