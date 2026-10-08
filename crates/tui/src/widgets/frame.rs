//! The thin rounded box a widget draws, with its `[-]` toggle outside the top right corner.

use ratatui::text::Span;

use super::{Drawn, Kit, fit};
use crate::text::marks::decorative;
use crate::theme::Role;

/// The toggle beside a widget that folds: `[-]` while open, `[+]` folded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    Open,
    Folded,
}

impl Toggle {
    pub fn text(self) -> &'static str {
        match self {
            Toggle::Open => "[-]",
            Toggle::Folded => "[+]",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// Columns before the left border.
    pub left: usize,
    /// From border to border.
    pub width: usize,
    pub toggle: Option<Toggle>,
    /// The widget has the keys, so its border takes the card color.
    pub active: bool,
}

/// Columns kept right of a band widget for its toggle, as Claude Code keeps them.
const TOGGLE_ROOM: usize = 5;

impl Frame {
    /// A widget above the input: one column in, its toggle outside the right border.
    pub fn band(screen: usize) -> Self {
        Self {
            left: 1,
            width: screen.saturating_sub(1 + TOGGLE_ROOM),
            toggle: Some(Toggle::Open),
            active: false,
        }
    }

    /// A card on an Overview tab, the screen's width less a column each side.
    pub fn view(screen: usize) -> Self {
        Self {
            left: 1,
            width: screen.saturating_sub(2),
            toggle: None,
            active: false,
        }
    }

    /// A card printed once into the transcript, under its `●` row. It never changes, so it
    /// has no toggle.
    pub fn printed(screen: usize) -> Self {
        Self {
            left: 2,
            width: screen.saturating_sub(4),
            toggle: None,
            active: false,
        }
    }

    /// The cells inside, between a border and a space on each side.
    pub fn inner(&self) -> usize {
        self.width.saturating_sub(4)
    }

    /// `body` inside the frame. Rows are padded or cut to the inner width.
    pub fn draw(&self, kit: &Kit<'_>, body: Drawn) -> Drawn {
        let g = kit.glyphs;
        let border = kit.style(if self.active {
            Role::Card
        } else {
            Role::WidgetBorder
        });
        let across = g.across.repeat(self.width.saturating_sub(2));
        let mut top = vec![decorative(
            format!("{}{across}{}", g.top_left, g.top_right),
            border,
        )];
        if let Some(toggle) = self.toggle {
            top.push(Span::raw(" "));
            top.push(kit.dim(toggle.text()));
        }
        let mut out = Drawn::new();
        out.push(top);
        let inner = self.inner();
        let mut rows = Drawn::new();
        for line in body.lines {
            let mut row = vec![decorative(format!("{} ", g.down), border)];
            row.extend(fit(line.spans, inner));
            row.push(decorative(format!(" {}", g.down), border));
            rows.push(row);
        }
        // The body starts after the border and its space.
        rows.meters = body.meters;
        for area in &mut rows.meters {
            area.x += 2;
        }
        out.append(rows);
        out.push(vec![decorative(
            format!("{}{across}{}", g.bottom_left, g.bottom_right),
            border,
        )]);
        out.indent(self.left);
        out
    }
}
