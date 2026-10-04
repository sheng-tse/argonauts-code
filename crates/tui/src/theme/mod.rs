//! Colors by role, so themes, light terminals, and terminals with fewer colors can swap them.
//!
//! Both palettes use the values Claude Code sends in its dark and light themes. The accent is
//! ours, a warm gold. Where Claude Code gave nothing to measure (the light error and auto mode
//! colors), the light palette picks a color that reads on white.

pub mod color;
pub mod glyphs;
mod rainbow;

use ratatui::style::{Color, Modifier, Style};

pub use color::{Brightness, ColorEnv, ColorLevel, downgrade, nearest_ansi16, nearest_ansi256};
pub use glyphs::Glyphs;

/// What a piece of text is, independent of the color a theme gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Normal text, in the terminal's own foreground.
    Text,
    /// The bullet of assistant text and your sent messages, brighter than normal text.
    Bright,
    Dim,
    Faint,
    Rule,
    Accent,
    /// The brighter band that sweeps across the working verb.
    Shimmer,
    Success,
    Error,
    Warning,
    Code,
    Card,
    /// The label on the composer's top rule.
    Label,
    ModeSupervised,
    ModeShadow,
    ModeAuto,
    /// Shell mode: the input's `!`, its rules, and the mode line's words.
    Shell,
    /// Background of a removed diff row.
    RemovedRow,
    /// Background of the changed words inside a removed row.
    RemovedWord,
    /// A removed row's line number and `-`.
    RemovedMarker,
    /// Background of an added diff row.
    AddedRow,
    /// Background of the changed words inside an added row.
    AddedWord,
    /// An added row's line number and `+`.
    AddedMarker,
    /// The code inside a removed or added diff row.
    DiffText,
    /// The band behind your own messages.
    UserBand,
    /// Text selected with the mouse in the fullscreen app.
    Selection,
    BarTrack,
    BarFill,
    AgentRed,
    AgentBlue,
    AgentGreen,
    AgentYellow,
    AgentPurple,
    AgentOrange,
    AgentPink,
    AgentCyan,
    SyntaxKeyword,
    SyntaxString,
    /// Commands and function names, cyan in shell snippets.
    SyntaxCommand,
    SyntaxComment,
    SyntaxNumber,
    SyntaxType,
    SyntaxConstant,
}

impl Role {
    pub const COUNT: usize = 43;

    pub const ALL: [Role; Role::COUNT] = [
        Role::Text,
        Role::Bright,
        Role::Dim,
        Role::Faint,
        Role::Rule,
        Role::Accent,
        Role::Shimmer,
        Role::Success,
        Role::Error,
        Role::Warning,
        Role::Code,
        Role::Card,
        Role::Label,
        Role::ModeSupervised,
        Role::ModeShadow,
        Role::ModeAuto,
        Role::Shell,
        Role::RemovedRow,
        Role::RemovedWord,
        Role::RemovedMarker,
        Role::AddedRow,
        Role::AddedWord,
        Role::AddedMarker,
        Role::DiffText,
        Role::UserBand,
        Role::Selection,
        Role::BarTrack,
        Role::BarFill,
        Role::AgentRed,
        Role::AgentBlue,
        Role::AgentGreen,
        Role::AgentYellow,
        Role::AgentPurple,
        Role::AgentOrange,
        Role::AgentPink,
        Role::AgentCyan,
        Role::SyntaxKeyword,
        Role::SyntaxString,
        Role::SyntaxCommand,
        Role::SyntaxComment,
        Role::SyntaxNumber,
        Role::SyntaxType,
        Role::SyntaxConstant,
    ];

    /// The name a config file uses for this role, such as `accent` or `removed-row`.
    pub fn name(self) -> &'static str {
        match self {
            Role::Text => "text",
            Role::Bright => "bright",
            Role::Dim => "dim",
            Role::Faint => "faint",
            Role::Rule => "rule",
            Role::Accent => "accent",
            Role::Shimmer => "shimmer",
            Role::Success => "success",
            Role::Error => "error",
            Role::Warning => "warning",
            Role::Code => "code",
            Role::Card => "card",
            Role::Label => "label",
            Role::ModeSupervised => "mode-supervised",
            Role::ModeShadow => "mode-shadow",
            Role::ModeAuto => "mode-auto",
            Role::Shell => "shell",
            Role::RemovedRow => "removed-row",
            Role::RemovedWord => "removed-word",
            Role::RemovedMarker => "removed-marker",
            Role::AddedRow => "added-row",
            Role::AddedWord => "added-word",
            Role::AddedMarker => "added-marker",
            Role::DiffText => "diff-text",
            Role::UserBand => "user-band",
            Role::Selection => "selection",
            Role::BarTrack => "bar-track",
            Role::BarFill => "bar-fill",
            Role::AgentRed => "agent-red",
            Role::AgentBlue => "agent-blue",
            Role::AgentGreen => "agent-green",
            Role::AgentYellow => "agent-yellow",
            Role::AgentPurple => "agent-purple",
            Role::AgentOrange => "agent-orange",
            Role::AgentPink => "agent-pink",
            Role::AgentCyan => "agent-cyan",
            Role::SyntaxKeyword => "syntax-keyword",
            Role::SyntaxString => "syntax-string",
            Role::SyntaxCommand => "syntax-command",
            Role::SyntaxComment => "syntax-comment",
            Role::SyntaxNumber => "syntax-number",
            Role::SyntaxType => "syntax-type",
            Role::SyntaxConstant => "syntax-constant",
        }
    }

    pub fn from_name(name: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|role| role.name() == name)
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// One of eight colors that tell agents apart by name, as Claude Code colors its subagents.
/// Never used on a bullet, whose color is the state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentColor {
    Red,
    Blue,
    Green,
    Yellow,
    Purple,
    Orange,
    Pink,
    Cyan,
}

impl AgentColor {
    /// The order colors are handed out in. Red, yellow, and green come last, since they echo
    /// the error, warning, and success colors.
    pub const ORDER: [AgentColor; 8] = [
        AgentColor::Blue,
        AgentColor::Purple,
        AgentColor::Cyan,
        AgentColor::Orange,
        AgentColor::Pink,
        AgentColor::Green,
        AgentColor::Yellow,
        AgentColor::Red,
    ];

    /// The color for the `n`th agent of a run, counting from zero. After eight they repeat.
    pub fn nth(n: usize) -> Self {
        Self::ORDER[n % Self::ORDER.len()]
    }

    pub fn role(self) -> Role {
        match self {
            AgentColor::Red => Role::AgentRed,
            AgentColor::Blue => Role::AgentBlue,
            AgentColor::Green => Role::AgentGreen,
            AgentColor::Yellow => Role::AgentYellow,
            AgentColor::Purple => Role::AgentPurple,
            AgentColor::Orange => Role::AgentOrange,
            AgentColor::Pink => Role::AgentPink,
            AgentColor::Cyan => Role::AgentCyan,
        }
    }
}

/// Hands out agent colors in the order agents first appear, so each keeps its color for the run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentColors {
    assigned: Vec<(String, AgentColor)>,
}

impl AgentColors {
    /// The agent's color, giving it the next free one the first time it is asked for.
    pub fn color_for(&mut self, agent: &str) -> AgentColor {
        if let Some(color) = self.get(agent) {
            return color;
        }
        let color = AgentColor::nth(self.assigned.len());
        self.assigned.push((agent.to_owned(), color));
        color
    }

    pub fn get(&self, agent: &str) -> Option<AgentColor> {
        self.assigned
            .iter()
            .find(|(name, _)| name == agent)
            .map(|(_, color)| *color)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    styles: [Style; Role::COUNT],
    pub glyphs: Glyphs,
    pub level: ColorLevel,
    pub brightness: Brightness,
    /// Hold animations still: no spinner, shimmer, blinking, or turning glyphs.
    pub reduced_motion: bool,
}

impl Theme {
    pub fn dark() -> Self {
        Self::from_fn(Brightness::Dark, dark_style)
    }

    pub fn light() -> Self {
        Self::from_fn(Brightness::Light, light_style)
    }

    pub fn for_brightness(brightness: Brightness) -> Self {
        match brightness {
            Brightness::Dark => Self::dark(),
            Brightness::Light => Self::light(),
        }
    }

    /// The theme this terminal can show, from `NO_COLOR`, the color depth, and the background.
    pub fn detect(env: &ColorEnv) -> Self {
        let brightness = Brightness::detect(env).unwrap_or_default();
        Self::for_brightness(brightness).with_level(ColorLevel::detect(env))
    }

    /// This theme fitted to a terminal that shows `level` colors.
    pub fn with_level(mut self, level: ColorLevel) -> Self {
        for role in Role::ALL {
            let style = self.styles[role.index()];
            self.styles[role.index()] = downgrade(role, style, level);
        }
        self.level = level;
        self
    }

    pub fn style(&self, role: Role) -> Style {
        self.styles[role.index()]
    }

    pub fn set(&mut self, role: Role, style: Style) {
        self.styles[role.index()] = style;
    }

    /// An agent's name style: its own color for the run, or normal text without one.
    pub fn agent_style(&self, color: Option<AgentColor>) -> Style {
        color.map_or(Style::new(), |color| self.style(color.role()))
    }

    /// Without colors, words and weights carry what color would.
    pub fn has_color(&self) -> bool {
        self.level != ColorLevel::None
    }

    /// The shimmer becomes a lighter shade of the new accent, and the bar fill follows it.
    pub fn with_accent(mut self, (r, g, b): (u8, u8, u8)) -> Self {
        let lighten = |c: u8| c.saturating_add(((255 - u16::from(c)) * 2 / 5) as u8);
        self.set(Role::Accent, Style::new().fg(Color::Rgb(r, g, b)));
        self.set(
            Role::Shimmer,
            Style::new().fg(Color::Rgb(lighten(r), lighten(g), lighten(b))),
        );
        self.set(Role::BarFill, Style::new().bg(Color::Rgb(r, g, b)));
        let level = self.level;
        self.with_level(level)
    }

    fn from_fn(brightness: Brightness, style: fn(Role) -> Style) -> Self {
        Self {
            styles: Role::ALL.map(style),
            glyphs: Glyphs::UNICODE,
            level: ColorLevel::TrueColor,
            brightness,
            reduced_motion: false,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

const fn fg(hex: u32) -> Style {
    Style::new().fg(rgb(hex))
}

const fn bg(hex: u32) -> Style {
    Style::new().bg(rgb(hex))
}

fn dark_style(role: Role) -> Style {
    // Claude Code's dark theme, from live captures.
    match role {
        Role::Text => Style::new(),
        Role::Bright => fg(0xffffff),
        Role::Dim => fg(0x999999),
        Role::Faint => fg(0x505050),
        Role::Rule => fg(0x888888),
        // A warm gold, for the golden fleece, with a lighter gold sweeping across the verb.
        Role::Accent => fg(0xe6a23c),
        Role::Shimmer => fg(0xf7d488),
        Role::Success => fg(0x4eba65),
        Role::Error => fg(0xff6b80),
        Role::Warning => fg(0xffc107),
        Role::Code => fg(0xb1b9f9),
        Role::Card => fg(0xb1b9f9),
        Role::Label => fg(0xaf87ff),
        Role::ModeSupervised => fg(0x999999),
        Role::ModeShadow => fg(0x48968c),
        Role::ModeAuto => fg(0xffc107),
        // Claude Code's bash mode pink.
        Role::Shell => fg(0xfd5db1),
        Role::RemovedRow => bg(0x3d0100),
        Role::RemovedWord => bg(0x5c0200),
        Role::RemovedMarker => fg(0xdc5a5a),
        Role::AddedRow => bg(0x022800),
        Role::AddedWord => bg(0x044700),
        Role::AddedMarker => fg(0x50c850),
        Role::DiffText => fg(0xf8f8f2),
        Role::UserBand => bg(0x373737),
        Role::Selection => bg(0x264f78),
        Role::BarTrack => bg(0x383945),
        Role::BarFill => bg(0xe6a23c),
        // Lighter shades of the eight, so names read on a dark background.
        Role::AgentRed => fg(0xf87171),
        Role::AgentBlue => fg(0x60a5fa),
        Role::AgentGreen => fg(0x4ade80),
        Role::AgentYellow => fg(0xfacc15),
        Role::AgentPurple => fg(0xc084fc),
        Role::AgentOrange => fg(0xfb923c),
        Role::AgentPink => fg(0xf472b6),
        Role::AgentCyan => fg(0x22d3ee),
        Role::SyntaxKeyword => fg(0xc678dd),
        Role::SyntaxString => fg(0xe37676),
        Role::SyntaxCommand => fg(0x99d9f9),
        Role::SyntaxComment => fg(0x999999).add_modifier(Modifier::ITALIC),
        Role::SyntaxNumber => fg(0xd19a66),
        Role::SyntaxType => fg(0xe5c07b),
        Role::SyntaxConstant => fg(0xd19a66),
    }
}

fn light_style(role: Role) -> Style {
    // Claude Code's light theme, with our own picks where nothing was measured.
    match role {
        Role::Text => Style::new(),
        Role::Bright => fg(0x000000),
        Role::Dim => fg(0x666666),
        Role::Faint => fg(0xafafaf),
        Role::Rule => fg(0x999999),
        // A darker gold, which reads on white.
        Role::Accent => fg(0xb7791f),
        Role::Shimmer => fg(0xd69e2e),
        Role::Success => fg(0x2c7a39),
        Role::Error => fg(0xcf222e),
        Role::Warning => fg(0x966c1e),
        Role::Code => fg(0x5769f7),
        Role::Card => fg(0x5769f7),
        Role::Label => fg(0x8700ff),
        Role::ModeSupervised => fg(0x666666),
        Role::ModeShadow => fg(0x006666),
        Role::ModeAuto => fg(0x966c1e),
        Role::Shell => fg(0xff0087),
        Role::RemovedRow => bg(0xffdcdc),
        Role::RemovedWord => bg(0xffc7c7),
        Role::RemovedMarker => fg(0xcf222e),
        Role::AddedRow => bg(0xdcffdc),
        Role::AddedWord => bg(0xb2ffb2),
        Role::AddedMarker => fg(0x248a3d),
        Role::DiffText => fg(0x333333),
        Role::UserBand => bg(0xf0f0f0),
        Role::Selection => bg(0xadd6ff),
        Role::BarTrack => bg(0xd8dbe2),
        Role::BarFill => bg(0xb7791f),
        // Claude Code's subagent colors.
        Role::AgentRed => fg(0xdc2626),
        Role::AgentBlue => fg(0x2563eb),
        Role::AgentGreen => fg(0x16a34a),
        Role::AgentYellow => fg(0xca8a04),
        Role::AgentPurple => fg(0x9333ea),
        Role::AgentOrange => fg(0xea580c),
        Role::AgentPink => fg(0xdb2777),
        Role::AgentCyan => fg(0x0891b2),
        Role::SyntaxKeyword => fg(0x8250df),
        Role::SyntaxString => fg(0xc2410c),
        Role::SyntaxCommand => fg(0x0e7490),
        Role::SyntaxComment => fg(0x666666).add_modifier(Modifier::ITALIC),
        Role::SyntaxNumber => fg(0xb45309),
        Role::SyntaxType => fg(0x9a6700),
        Role::SyntaxConstant => fg(0xb45309),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_index_in_declaration_order() {
        for (i, role) in Role::ALL.into_iter().enumerate() {
            assert_eq!(role.index(), i, "{role:?}");
        }
    }

    #[test]
    fn role_names_round_trip() {
        for role in Role::ALL {
            assert_eq!(Role::from_name(role.name()), Some(role));
        }
    }

    #[test]
    fn dark_palette_uses_the_reference_values() {
        let theme = Theme::dark();
        let fg = |role| theme.style(role).fg;
        let bg = |role| theme.style(role).bg;
        assert_eq!(fg(Role::Dim), Some(rgb(0x999999)));
        assert_eq!(fg(Role::Bright), Some(rgb(0xffffff)));
        assert_eq!(fg(Role::Faint), Some(rgb(0x505050)));
        assert_eq!(fg(Role::Rule), Some(rgb(0x888888)));
        assert_eq!(fg(Role::Card), Some(rgb(0xb1b9f9)));
        assert_eq!(bg(Role::RemovedRow), Some(rgb(0x3d0100)));
        assert_eq!(bg(Role::AddedRow), Some(rgb(0x022800)));
        assert_eq!(bg(Role::UserBand), Some(rgb(0x373737)));
        assert_eq!(theme.style(Role::Text), Style::new());
    }

    #[test]
    fn every_role_has_a_style_in_both_palettes() {
        for role in Role::ALL {
            for theme in [Theme::dark(), Theme::light()] {
                let style = theme.style(role);
                let colored = style.fg.is_some() || style.bg.is_some();
                assert!(colored || role == Role::Text, "{role:?} has no color");
            }
        }
    }

    #[test]
    fn agents_keep_their_color_for_the_run() {
        let mut colors = AgentColors::default();
        let builder = colors.color_for("builder");
        let reviewer = colors.color_for("reviewer");
        assert_ne!(builder, reviewer);
        assert_eq!(colors.color_for("builder"), builder);
        assert_eq!(colors.get("reviewer"), Some(reviewer));
        assert_eq!(colors.get("nobody"), None);
        let all: std::collections::HashSet<_> = (0..8).map(AgentColor::nth).collect();
        assert_eq!(all.len(), 8);
    }
}
