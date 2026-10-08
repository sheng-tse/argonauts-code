//! The context widget: how full a session's context window is, by category, with the point
//! where it compacts or rotates marked on the bar.
//!
//! ```text
//! ╭──────────────────────────────────────────────────────────────────╮ [-]
//! │ ◆ context                  212k of 1M · compacts at 950k  [21%] │
//! │ ███████████████████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░│░░░ │
//! │ ▌ system prompt  3.4k 0.3%     ▌ tools           12k   1%     │
//! ```

use ratatui::text::Span;

use super::bars::{self, Stack};
use super::bodies;
use super::legend::Entry;
use super::words::spoken;
use super::{Badge, Drawn, Form, Frame, Header, Kit, Level, Levels, Toggle, spread};
use crate::format::{UNKNOWN, percent, tokens};
use crate::text::{spans_width, truncate_spans};
use crate::theme::Role;

/// Claude Code's /context categories, in its order. Anything it adds later goes to `other`.
pub const CLAUDE_CODE_PARTS: [(&str, Role); 7] = [
    ("system prompt", Role::CatSystem),
    ("tools", Role::CatTools),
    ("mcp tools", Role::CatMcp),
    ("agents", Role::CatAgents),
    ("memory files", Role::CatMemory),
    ("skills", Role::CatSkills),
    ("messages", Role::CatMessages),
];

/// Legend columns at most, the example's three to a row.
const LEGEND_COLUMNS: usize = 3;
/// Items in each line a screen reader hears.
const SPOKEN_ITEMS: usize = 4;
/// Pills in the folded line.
const LINE_PILLS: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    /// The CLI compacts there on its own.
    Compacts,
    /// Argonauts starts a fresh session there (`sessions.rotate_at`).
    Rotates,
}

/// Where something happens to the session, drawn as the bar's tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limit {
    pub at: u64,
    pub kind: LimitKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPart {
    pub name: String,
    pub role: Role,
    /// None before the first reading.
    pub tokens: Option<u64>,
}

impl ContextPart {
    pub fn new(name: impl Into<String>, role: Role, tokens: Option<u64>) -> Self {
        Self {
            name: name.into(),
            role,
            tokens,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContextCard {
    /// Whose context, when it isn't the chat's own: `lead · Opus 5.5 on Claude Code`.
    pub subject: Option<String>,
    /// Tokens in use, by the CLI's own count. None before the first reading.
    pub used: Option<u64>,
    pub window: Option<u64>,
    pub limit: Option<Limit>,
    /// The categories in bar order. Free is what's left of the window.
    pub parts: Vec<ContextPart>,
    /// The parts are Argonauts' estimate, for a CLI that reports no breakdown.
    pub estimated: bool,
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
            .map(|(k, &(name, role))| ContextPart::new(name, role, tokens.map(|t| t[k])))
            .collect();
        Self::with_parts(parts, false)
    }

    /// For a CLI that reports no breakdown: what Argonauts sent it, and the rest of the
    /// conversation by its own count, both marked as estimates.
    pub fn estimate(brief: Option<u64>, conversation: Option<u64>) -> Self {
        let parts = vec![
            ContextPart::new("brief", Role::CatBrief, brief),
            ContextPart::new("conversation", Role::CatOther, conversation),
        ];
        Self::with_parts(parts, true)
    }

    fn with_parts(parts: Vec<ContextPart>, estimated: bool) -> Self {
        Self {
            subject: None,
            used: None,
            window: None,
            limit: None,
            parts,
            estimated,
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

    fn free(&self) -> Option<u64> {
        self.reading()
            .map(|(used, window)| window.saturating_sub(used))
    }

    fn amount(&self, tokens_count: Option<u64>) -> String {
        match tokens_count {
            None => String::from(UNKNOWN),
            Some(n) if self.estimated => format!("~{}", tokens(n)),
            Some(n) => tokens(n),
        }
    }

    fn badge(&self) -> Badge {
        match self.reading() {
            None => Badge::pending(),
            Some(_) if self.compacting => Badge::words("compacting", Level::Warn),
            Some((used, window)) => {
                let mut badge = Badge::percent(used as f64 / window as f64, self.level());
                if self.stale {
                    badge.text = format!("~{}", badge.text);
                }
                badge
            }
        }
    }

    /// ` of 1M · compacts at 950k`.
    fn tail(&self) -> String {
        let window = self.window.map_or_else(|| String::from(UNKNOWN), tokens);
        let mut tail = format!(" of {window}");
        if let Some(limit) = self.limit {
            let verb = match limit.kind {
                LimitKind::Compacts => "compacts",
                LimitKind::Rotates => "rotates",
            };
            tail.push_str(&format!(" · {verb} at {}", tokens(limit.at)));
        }
        tail
    }

    pub fn header(&self, kit: &Kit<'_>) -> Header {
        // The amount is the CLI's own count, never an estimate.
        let amount = match self.reading() {
            None => String::from(UNKNOWN),
            Some((used, _)) if self.stale => format!("~{}", tokens(used)),
            Some((used, _)) => tokens(used),
        };
        Header {
            subject: self.subject.clone(),
            amount: Some(amount),
            tail: vec![kit.dim(self.tail())],
            badge: Some(self.badge()),
            ..Header::new("context", Role::Accent)
        }
    }

    /// The parts and free, as the bar draws them. Before a reading the bar is all track, with
    /// the tick already where it will be.
    pub fn stack(&self) -> Stack {
        let window = self.window.filter(|&window| window > 0);
        let mut parts: Vec<(u64, Role)> = self
            .parts
            .iter()
            .filter_map(|part| Some((part.tokens?, part.role)))
            .filter(|_| self.reading().is_some())
            .collect();
        if let Some(free) = self.free() {
            parts.push((free, Role::CatFree));
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

    pub fn legend_entries(&self) -> Vec<Entry> {
        let window = self.reading().map(|(_, window)| window);
        let share = |n: Option<u64>| match (n, window) {
            (Some(n), Some(w)) => percent(n as f64 / w as f64),
            _ => String::new(),
        };
        let mut entries: Vec<Entry> = self
            .parts
            .iter()
            .map(|part| {
                let tokens_count = part.tokens.filter(|_| window.is_some());
                Entry {
                    role: part.role,
                    name: part.name.clone(),
                    amount: self.amount(tokens_count),
                    share: share(tokens_count),
                }
            })
            .collect();
        entries.push(Entry {
            role: Role::CatFree,
            name: String::from("free"),
            amount: self.free().map_or_else(|| String::from(UNKNOWN), tokens),
            share: String::new(),
        });
        entries
    }

    /// The rows inside the frame: the header, the bar, and in the full form the legend.
    pub fn body(&self, kit: &Kit<'_>, inner: usize, form: Form) -> Drawn {
        let mut body = Drawn::new();
        body.push(self.header(kit).draw(kit, inner));
        let entries = match form {
            Form::Full => self.legend_entries(),
            _ => Vec::new(),
        };
        body.append(bodies::stack(
            kit,
            &self.stack(),
            &entries,
            inner,
            self.bar_rows,
            LEGEND_COLUMNS,
        ));
        body
    }

    /// The full or compact form inside `frame`.
    pub fn card(&self, kit: &Kit<'_>, form: Form, frame: Frame) -> Drawn {
        frame.draw(kit, self.body(kit, frame.inner(), form))
    }

    /// The widget above the input, in `form`, on a screen `screen` cells wide.
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
            kit.span(kit.glyphs.widget, Role::Accent),
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
                details.push(kit.dim(format!(" · {}", tokens(used))));
                details.push(kit.span(format!(" of {}", tokens(window)), Role::Denominator));
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

    /// On the mode line: `ctx 21%`, the number dim until it passes warn.
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

    /// The widget as a screen reader hears it.
    pub fn words(&self) -> Vec<String> {
        let Some((used, window)) = self.reading() else {
            return vec![String::from("context: no reading yet")];
        };
        let mut head = format!("{} of {}", tokens(used), tokens(window));
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
