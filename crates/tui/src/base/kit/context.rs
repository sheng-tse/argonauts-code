//! The context mod: how full a session's context window is, by the CLI's categories in
//! Claude Code's names and colours, with the point where it compacts or rotates marked on the
//! bar. A blank row parts the bar from its legend, and the legend is a table: each name, three
//! blanks or more, its amount, two blanks, its share, the entries six blanks apart, so the
//! columns line up row to row.
//!
//! ```text
//! ╭──────────────────────────────────────────────────────────────────────────────────╮
//! │ ◆ context                                 32.3k of 1M · compacts at 967k    3%  │
//! │ ██████▋                                                                  ▏█████ │
//! │                                                                                 │
//! │ ▌ System prompt             4.4k   0.4%      ▌ System tools   19.4k   1.9%      │
//! │ ▌ Messages                    10  <0.1%      ▌ Free space      935k  93.5%      │
//! ```

use ratatui::style::{Color, Style};
use ratatui::text::Span;

use super::bars::{self, Stack};
use super::legend::{self, Counted, Entry};
use super::words::spoken;
use super::{Badge, Drawn, Form, Frame, Header, Kit, Level, Levels, Toggle, spread};
use crate::base::format::{self, UNKNOWN, tokens, tokens_beside};
use crate::base::text::{spans_width, truncate_spans};
use crate::base::theme::{Brightness, Role, Theme, downgrade};

/// Claude Code's /context categories in its order, each with the colour name its reply gives
/// it. A category it adds later keeps the name and colour its reply gives.
pub const CLAUDE_CODE_PARTS: [(&str, &str); 7] = [
    ("System prompt", "promptBorder"),
    ("System tools", "inactive"),
    ("MCP tools", "cyan_FOR_SUBAGENTS_ONLY"),
    ("MCP server instructions", "green_FOR_SUBAGENTS_ONLY"),
    ("Memory files", "claude"),
    ("Skills", "warning"),
    ("Messages", "purple_FOR_SUBAGENTS_ONLY"),
];

/// Legend entries a row at most.
const LEGEND_COLUMNS: usize = 3;
/// Blanks a legend row keeps from the frame's border beyond the frame's own, as a column of
/// numbers keeps from its neighbour.
const LEGEND_EDGE: usize = legend::BEFORE_NUMBERS - 1;
/// Items in each line a screen reader hears.
const SPOKEN_ITEMS: usize = 4;
/// Pills in the folded line.
const LINE_PILLS: usize = 5;
/// The roles a card lends to the parts that draw in their CLI's own colours, in order, so the
/// shared bar paints each part as its CLI does.
const PART_SLOTS: [Role; 10] = [
    Role::CatSystem,
    Role::CatTools,
    Role::CatMcp,
    Role::CatAgents,
    Role::CatMemory,
    Role::CatSkills,
    Role::CatMessages,
    Role::CatBrief,
    Role::CatOther,
    Role::TokensCached,
];

/// How one of Claude Code's colours draws, by the name its reply gives a category: in a dark
/// terminal as Claude Code's own /context paints it, and in a light one the theme's own shade.
/// A name this version doesn't know draws dim.
pub fn claude_code_paint(kit: &Kit<'_>, color: &str) -> Style {
    let (dark, light, role): (u32, Option<u32>, Role) = match color {
        "promptBorder" => (0x888888, Some(0x707070), Role::Rule),
        "inactive" => (0x999999, Some(0x707070), Role::Dim),
        "green_FOR_SUBAGENTS_ONLY" | "success" => (0x17a34a, Some(0x2f9e7a), Role::Success),
        "warning" => (0xffc109, Some(0x9a6f12), Role::Warn),
        "purple_FOR_SUBAGENTS_ONLY" => (0x827dbd, Some(0x5769f7), Role::Question),
        "permission" | "suggestion" => (0xb1b9f9, Some(0x5769f7), Role::Question),
        "cyan_FOR_SUBAGENTS_ONLY" => (0x0a91b3, None, Role::AgentCyan),
        "claude" => (0xd77757, None, Role::CliClaude),
        "blue_FOR_SUBAGENTS_ONLY" => return kit.fg(Role::AgentBlue),
        "red_FOR_SUBAGENTS_ONLY" | "error" => return kit.fg(Role::Danger),
        "yellow_FOR_SUBAGENTS_ONLY" => return kit.fg(Role::AgentYellow),
        "orange_FOR_SUBAGENTS_ONLY" => return kit.fg(Role::AgentOrange),
        "pink_FOR_SUBAGENTS_ONLY" => return kit.fg(Role::AgentPink),
        _ => return kit.fg(Role::Dim),
    };
    let hex = match (kit.theme.brightness, light) {
        (Brightness::Dark, _) => dark,
        (Brightness::Light, Some(light)) => light,
        (Brightness::Light, None) => return kit.fg(role),
    };
    painted(kit.theme, role, hex)
}

/// How Claude Code's /context paints its free cells and the room it holds back for compacting:
/// grey in a dark terminal, and the theme's faint and dim in a light one.
pub fn claude_code_free(kit: &Kit<'_>, held: bool) -> Style {
    let role = if held { Role::Dim } else { Role::Faint };
    match kit.theme.brightness {
        Brightness::Dark => painted(kit.theme, role, 0x999999),
        Brightness::Light => kit.fg(role),
    }
}

fn painted(theme: &Theme, role: Role, hex: u32) -> Style {
    let rgb = Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8);
    downgrade(role, Style::new().fg(rgb), theme.level)
}

/// A share of the window as Claude Code's /context writes it, in tenths of a percent, and
/// nothing where there is no window.
pub fn share_of_window(tokens: u64, window: u64) -> String {
    if window == 0 {
        return String::new();
    }
    format::Figures::ClaudeCode.share(tokens, window)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    /// The CLI compacts there on its own.
    Compacts,
    /// Argonauts Code starts a fresh session there (`sessions.rotate_at`).
    Rotates,
}

/// Where something happens to the session, drawn as the bar's tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limit {
    pub at: u64,
    pub kind: LimitKind,
}

/// How a part draws: in a role of the theme, or in its CLI's own colour by the name the CLI
/// gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Paint {
    Role(Role),
    Named(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPart {
    pub name: String,
    pub paint: Paint,
    /// None before the first reading.
    pub tokens: Option<u64>,
}

impl ContextPart {
    pub fn new(name: impl Into<String>, role: Role, tokens: Option<u64>) -> Self {
        Self {
            name: name.into(),
            paint: Paint::Role(role),
            tokens,
        }
    }

    /// A part in its CLI's own colour, such as Claude Code's `promptBorder`.
    pub fn named(name: impl Into<String>, color: impl Into<String>, tokens: Option<u64>) -> Self {
        Self {
            name: name.into(),
            paint: Paint::Named(color.into()),
            tokens,
        }
    }
}

/// Where the numbers come from before the session's CLI has counted it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Estimate {
    /// What the last fresh session on the same CLI and model held at its start.
    LastSession,
    /// The CLI's own prompt and tools as recorded for its version, and what this machine
    /// adds to them.
    Recorded,
}

/// Before the CLI's own count: where the numbers come from, and whether the CLI is starting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BeforeCount {
    pub from: Estimate,
    /// The CLI is starting, from your first message until its count comes.
    pub loading: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContextCard {
    /// Whose context, when it isn't the conversation's own: `main · Opus 5.5 on Claude Code`.
    pub subject: Option<String>,
    /// Tokens in use. None before the first reading.
    pub used: Option<u64>,
    pub window: Option<u64>,
    pub limit: Option<Limit>,
    /// The categories in bar order. Free is what's left of the window.
    pub parts: Vec<ContextPart>,
    /// The parts are Argonauts Code's own count, for a CLI that reports no breakdown.
    pub estimated: bool,
    /// How full it is is worked out from the session's totals, so it and the free space are
    /// marked as estimates too.
    pub used_estimated: bool,
    /// Every number is an estimate until the CLI's own count.
    pub before: Option<BeforeCount>,
    /// The CLI is compacting now; the bar keeps its last reading.
    pub compacting: bool,
    /// The reading is old.
    pub stale: bool,
    /// The key that shows or folds it, for the folded line, such as `ctrl+x c`.
    pub key: Option<String>,
    /// Bar rows: one above the input, two on the Context tab.
    pub bar_rows: usize,
    pub levels: Levels,
}

impl ContextCard {
    /// A Claude Code session's card, its parts in /context's order. `tokens` holds each
    /// category's count, or None before a reading.
    pub fn claude_code(tokens: Option<[u64; 7]>) -> Self {
        let parts = CLAUDE_CODE_PARTS
            .iter()
            .enumerate()
            .map(|(k, &(name, color))| ContextPart::named(name, color, tokens.map(|t| t[k])))
            .collect();
        Self::with_parts(parts, false)
    }

    /// For a CLI that reports no breakdown: what Argonauts Code sent it, and the rest of the
    /// conversation by its own count, both marked as estimates.
    pub fn estimate(brief: Option<u64>, conversation: Option<u64>) -> Self {
        let parts = vec![
            ContextPart::new("brief", Role::CatBrief, brief),
            ContextPart::new("conversation", Role::CatOther, conversation),
        ];
        Self::with_parts(parts, true)
    }

    /// A card with `parts`, before any reading.
    pub fn with_parts(parts: Vec<ContextPart>, estimated: bool) -> Self {
        Self {
            subject: None,
            used: None,
            window: None,
            limit: None,
            parts,
            estimated,
            used_estimated: false,
            before: None,
            compacting: false,
            stale: false,
            key: None,
            bar_rows: 1,
            levels: Levels::CONTEXT,
        }
    }

    /// The reading, if both numbers are in.
    fn reading(&self) -> Option<(u64, u64)> {
        Some((self.used?, self.window?)).filter(|&(_, window)| window > 0)
    }

    /// The level, against the tick where there is one, since that's where something happens.
    pub fn level(&self) -> Level {
        let Some((used, window)) = self.reading() else {
            return Level::Ok;
        };
        let against = self.limit.map_or(window, |limit| limit.at.max(1));
        self.levels.level(used as f64 / against as f64 * 100.0)
    }

    /// What's left of the window, the room held back for compacting left out.
    fn free(&self) -> Option<u64> {
        let held = |window: u64| match self.limit {
            Some(Limit {
                at,
                kind: LimitKind::Compacts,
            }) => window.saturating_sub(at),
            _ => 0,
        };
        self.reading()
            .map(|(used, window)| window.saturating_sub(used).saturating_sub(held(window)))
    }

    /// `~` where every number is an estimate.
    fn mark(&self, estimate: bool) -> &'static str {
        if estimate || self.before.is_some() {
            "~"
        } else {
            ""
        }
    }

    fn amount(&self, tokens_count: Option<u64>, estimate: bool) -> String {
        match tokens_count {
            None => String::from(UNKNOWN),
            Some(n) => format!("{}{}", self.mark(estimate), self.sized(n)),
        }
    }

    /// A count in this window's divisor, so the parts of a 64k window add up to 64k.
    fn sized(&self, count: u64) -> String {
        match self.window {
            Some(window) => tokens_beside(count, window),
            None => tokens(count),
        }
    }

    fn badge(&self) -> Badge {
        match self.reading() {
            None => Badge::pending(),
            Some(_) if self.before.is_some_and(|before| before.loading) => Badge {
                pending: true,
                ..Badge::words("loading", Level::Ok)
            },
            Some(_) if self.compacting => Badge::words("compacting", Level::Warn),
            Some((used, window)) => {
                let mut badge = Badge::percent(used as f64 / window as f64, self.level());
                if self.stale || self.before.is_some() {
                    badge.text = format!("~{}", badge.text);
                }
                badge
            }
        }
    }

    /// ` of 1M · compacts at 967k`, and before the CLI's count where the numbers come from.
    fn tail(&self) -> String {
        let window = self
            .window
            .map_or_else(|| String::from(UNKNOWN), format::window);
        let mut tail = format!(" of {window}");
        if let Some(limit) = self.limit {
            let verb = match limit.kind {
                LimitKind::Compacts => "compacts",
                LimitKind::Rotates => "rotates",
            };
            let mark = self.mark(false);
            tail.push_str(&format!(" · {verb} at {mark}{}", self.sized(limit.at)));
        }
        match self.before.map(|before| before.from) {
            Some(Estimate::LastSession) => tail.push_str(" · from your last session"),
            Some(Estimate::Recorded) => tail.push_str(" · estimated"),
            None => {}
        }
        tail
    }

    pub fn header(&self, kit: &Kit<'_>) -> Header {
        let amount = match self.reading() {
            None => String::from(UNKNOWN),
            Some((used, _)) => self.amount(Some(used), self.stale || self.used_estimated),
        };
        Header {
            subject: self.subject.clone(),
            amount: Some(amount),
            tail: vec![kit.dim(self.tail())],
            badge: Some(self.badge()),
            ..Header::new("context", Role::Accent)
        }
    }

    /// The theme the bar and legend draw with: the kit's, with each part that draws in its
    /// CLI's own colour lent a role of its own. Each part's role, in order.
    fn palette(&self, kit: &Kit<'_>) -> (Theme, Vec<Role>) {
        let mut theme = kit.theme.clone();
        let mut slots = PART_SLOTS.iter();
        let roles = self
            .parts
            .iter()
            .map(|part| match (&part.paint, slots.next()) {
                (Paint::Role(role), _) => *role,
                (Paint::Named(color), Some(&slot)) => {
                    theme.set(slot, claude_code_paint(kit, color));
                    slot
                }
                (Paint::Named(_), None) => Role::CatOther,
            })
            .collect();
        (theme, roles)
    }

    /// The parts and free, as the bar draws them, each part in `roles`. Before a reading the
    /// bar is all track, with the tick already where it will be.
    fn stack_in(&self, roles: &[Role]) -> Stack {
        let window = self.window.filter(|&window| window > 0);
        let mut parts: Vec<(u64, Role)> = self
            .parts
            .iter()
            .zip(roles)
            .filter_map(|(part, role)| Some((part.tokens?, *role)))
            .filter(|_| self.reading().is_some())
            .collect();
        if let Some((used, window)) = self.reading() {
            parts.push((window.saturating_sub(used), Role::CatFree));
        }
        let limit = self
            .limit
            .map(|limit| limit.at)
            .filter(|_| window.is_some());
        // Past the window the parts squeeze into it, so the tick stays where it is.
        Stack {
            parts,
            total: window.unwrap_or(1),
            tick: limit,
            held_from: limit,
            track: Role::CatFree,
        }
    }

    /// Each part's role where it draws in one of the theme's, and `other` where it draws in
    /// its CLI's own colour, for words that need no colour.
    fn own_roles(&self) -> Vec<Role> {
        self.parts
            .iter()
            .map(|part| match &part.paint {
                Paint::Role(role) => *role,
                Paint::Named(_) => Role::CatOther,
            })
            .collect()
    }

    /// The legend's entries in `roles`: each part, then the free space.
    fn entries_in(&self, roles: &[Role]) -> Vec<Entry> {
        let window = self.reading().map(|(_, window)| window);
        let share = |n: Option<u64>| match (n, window) {
            (Some(n), Some(w)) => share_of_window(n, w),
            _ => String::new(),
        };
        let mut entries: Vec<Entry> = self
            .parts
            .iter()
            .zip(roles)
            .map(|(part, role)| {
                let tokens_count = part.tokens.filter(|_| window.is_some());
                Entry::new(
                    *role,
                    part.name.clone(),
                    self.amount(tokens_count, self.estimated),
                )
                .shared(share(tokens_count))
            })
            .collect();
        let free = self.free();
        entries.push(
            Entry::new(
                Role::CatFree,
                "Free space",
                self.amount(free, self.used_estimated),
            )
            .shared(share(free)),
        );
        entries
    }

    /// The legend's entries: each part, then the free space.
    pub fn legend_entries(&self) -> Vec<Entry> {
        self.entries_in(&self.own_roles())
    }

    /// The rows inside the frame: the header, the bar, and in the full form a blank row and
    /// the legend.
    pub fn body(&self, kit: &Kit<'_>, inner: usize, form: Form) -> Drawn {
        let (theme, roles) = self.palette(kit);
        let painted = Kit::new(&theme);
        let mut body = Drawn::new();
        body.push(self.header(kit).draw(kit, inner));
        let bar = bars::stacked(&painted, &self.stack_in(&roles), inner);
        body.push_meter(bar, 0, inner, self.bar_rows.max(1));
        if form == Form::Full {
            body.push(Vec::new());
            let entries = self.entries_in(&roles);
            let width = inner.saturating_sub(LEGEND_EDGE);
            for row in
                legend::legend_counted(&painted, &entries, width, LEGEND_COLUMNS, Counted::WIDEST)
            {
                body.push(row);
            }
        }
        body
    }

    /// The full or compact form inside `frame`.
    pub fn card(&self, kit: &Kit<'_>, form: Form, frame: Frame) -> Drawn {
        frame.draw(kit, self.body(kit, frame.inner(), form))
    }

    /// The mod above the input, in `form`, on a screen `screen` cells wide.
    pub fn draw(&self, kit: &Kit<'_>, form: Form, screen: usize) -> Drawn {
        let mut out = Drawn::new();
        match form {
            Form::Full | Form::Compact => return self.card(kit, form, Frame::band(screen)),
            Form::Line => out.push(self.line(kit, screen, Some(Toggle::Folded))),
            Form::Chip => out.push(self.chip(kit)),
        }
        out
    }

    /// One row, `width` cells: `▸ ◆ context   ▰▱▱▱▱ 21% · 212k of 1M …   ctrl+x c  [+]`. When
    /// it's too narrow the details are cut first, then the key goes, then the subject is cut;
    /// the meter and its number stay.
    pub fn line(&self, kit: &Kit<'_>, width: usize, toggle: Option<Toggle>) -> Vec<Span<'static>> {
        let level = self.level();
        let title = vec![
            Span::raw(" "),
            kit.dim(format!("{} ", kit.glyphs.folded)),
            kit.span(kit.glyphs.the_mod, Role::Accent),
            Span::raw(" "),
            kit.strong("context"),
        ];
        let subject: Vec<Span<'static>> = self
            .subject
            .iter()
            .map(|subject| kit.dim(format!(" · {subject}")))
            .collect();
        let mut meter = vec![Span::raw("   ")];
        let mut details = Vec::new();
        match self.reading() {
            Some((used, window)) => {
                let ratio = used as f64 / window as f64;
                meter.extend(bars::pills(kit, ratio, LINE_PILLS, level.fill_role()));
                meter.push(Span::raw(" "));
                meter.push(Span::styled(
                    self.badge().text,
                    kit.bold(level.strong_role()),
                ));
                details.push(kit.dim(format!(" · {}", self.sized(used))));
                details
                    .push(kit.span(format!(" of {}", format::window(window)), Role::Denominator));
                if let Some((_, rest)) = self.tail().split_once(" · ") {
                    details.push(kit.dim(format!(" · {rest}")));
                }
            }
            None => {
                meter.extend(bars::pills(kit, 0.0, LINE_PILLS, Role::BarFill));
                meter.push(kit.dim(format!(" {UNKNOWN}")));
            }
        }
        let mut toggle_spans = Vec::new();
        if let Some(toggle) = toggle {
            toggle_spans.push(kit.dim(toggle.text()));
            toggle_spans.push(Span::raw(" "));
        }
        let mut keyed = Vec::new();
        if let Some(key) = &self.key {
            keyed.push(kit.dim(key.clone()));
            keyed.push(Span::raw("  "));
        }
        keyed.extend(toggle_spans.iter().cloned());

        let head = [title.clone(), subject.clone(), meter.clone()].concat();
        let room_beside = |right: &[Span<'static>]| width.saturating_sub(spans_width(right) + 2);
        // The details, cut to what's left after the head, or gone when only a stub would fit.
        let with_details = |room: usize| {
            let rest = room.saturating_sub(spans_width(&head));
            let cut = truncate_spans(&details, rest);
            if spans_width(&cut) < spans_width(&details) && rest < 6 {
                head.clone()
            } else {
                [head.clone(), cut].concat()
            }
        };
        let (left, right) = if spans_width(&head) <= room_beside(&keyed) {
            (with_details(room_beside(&keyed)), keyed)
        } else if spans_width(&head) <= room_beside(&toggle_spans) {
            (with_details(room_beside(&toggle_spans)), toggle_spans)
        } else {
            let room = width.saturating_sub(spans_width(&toggle_spans) + 2);
            let subject_room = room.saturating_sub(spans_width(&title) + spans_width(&meter));
            let left = [title, truncate_spans(&subject, subject_room), meter].concat();
            (truncate_spans(&left, room), toggle_spans)
        };
        spread(left, right, width)
    }

    /// On the bottom line: `ctx 21%`, the number dim until it passes warn.
    pub fn chip(&self, kit: &Kit<'_>) -> Vec<Span<'static>> {
        let text = match self.reading() {
            None => String::from(UNKNOWN),
            Some(_) => self.badge().text,
        };
        let role = if self.stale {
            Role::Faint
        } else {
            self.level().number_role()
        };
        vec![kit.dim("ctx "), kit.span(text, role)]
    }

    /// The mod as a screen reader hears it.
    pub fn words(&self) -> Vec<String> {
        let Some((used, window)) = self.reading() else {
            return vec![String::from("context: no reading yet")];
        };
        let mut head = format!("{} of {}", self.sized(used), format::window(window));
        if self.compacting {
            head.push_str(", compacting");
        } else {
            head.push_str(&format!(", {}", self.badge().text));
            if let Some(word) = self.level().word() {
                head.push_str(&format!(", {word}"));
            }
        }
        if let Some((_, rest)) = self.tail().split_once(" · ") {
            head.push_str(&format!(", {rest}"));
        }
        if self.stale {
            head.push_str(", not current");
        }
        let mut lines = spoken("context", &[head], 1);
        let items: Vec<String> = self
            .legend_entries()
            .into_iter()
            .map(|entry| {
                let share = if entry.share.is_empty() {
                    String::new()
                } else {
                    format!(" {}", entry.share)
                };
                format!("{} {}{share}", entry.name, entry.amount)
            })
            .collect();
        lines.extend(spoken("context", &items, SPOKEN_ITEMS));
        lines
    }
}
