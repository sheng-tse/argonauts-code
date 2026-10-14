//! The mode line is the screen's bottom row: the approval mode with its hints, the parts the
//! bottom line has, and any notice at the right end. What gives way on a narrow terminal, and
//! in what order, is [`line`](super::line)'s to decide. The mode never goes, and neither does
//! the `←` hint.

use std::time::Instant;

use ratatui::text::{Line, Span};

use super::line::{self, BottomLine, Give, Joiner, ModePiece, ModePieces};
use crate::cell::Tone;
use crate::render::RenderCtx;
use crate::text::sanitize_line;
use crate::theme::Role;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum ApprovalMode {
    #[default]
    Supervised,
    AutoPreview,
    Auto,
    /// Nothing waits for you.
    Autopilot,
}

impl ApprovalMode {
    pub const ALL: [ApprovalMode; 4] = [
        ApprovalMode::Supervised,
        ApprovalMode::AutoPreview,
        ApprovalMode::Auto,
        ApprovalMode::Autopilot,
    ];

    /// Shift+Tab's cycle: supervised, auto, full auto. Shadow is a
    /// setting; from it, Shift+Tab goes on to auto.
    pub fn next(self) -> Self {
        match self {
            ApprovalMode::Supervised | ApprovalMode::AutoPreview => ApprovalMode::Auto,
            ApprovalMode::Auto => ApprovalMode::Autopilot,
            ApprovalMode::Autopilot => ApprovalMode::Supervised,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ApprovalMode::Supervised => "supervised",
            ApprovalMode::AutoPreview => "auto preview",
            ApprovalMode::Auto => "auto",
            ApprovalMode::Autopilot => "autopilot",
        }
    }

    /// What the mode line says: "auto mode on", and "autopilot on" without the extra word.
    pub fn banner(self) -> String {
        match self {
            ApprovalMode::Autopilot => "autopilot on".to_owned(),
            mode => format!("{} mode on", mode.label()),
        }
    }

    pub fn asks_first(self) -> bool {
        matches!(self, ApprovalMode::Supervised | ApprovalMode::AutoPreview)
    }

    pub fn role(self) -> Role {
        match self {
            ApprovalMode::Supervised => Role::ModeSupervised,
            ApprovalMode::AutoPreview => Role::ModeShadow,
            ApprovalMode::Auto => Role::ModeAuto,
            ApprovalMode::Autopilot => Role::ModeAutopilot,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VimMode {
    Insert,
    Normal,
    Visual,
}

impl VimMode {
    pub fn label(self) -> &'static str {
        match self {
            VimMode::Insert => "-- INSERT --",
            VimMode::Normal => "-- NORMAL --",
            VimMode::Visual => "-- VISUAL --",
        }
    }

    /// The label on a terminal too narrow for it.
    pub fn short_label(self) -> &'static str {
        match self {
            VimMode::Insert => "-- I --",
            VimMode::Normal => "-- N --",
            VimMode::Visual => "-- V --",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeLine {
    pub vim: Option<VimMode>,
    pub approval: ApprovalMode,
    pub background_agents: usize,
    /// Cards that wait on you somewhere else, such as in another run.
    pub needs_you: usize,
    /// Hidden while `-- INSERT --` shows, since Esc there only leaves INSERT.
    pub footer_hints: bool,
    pub notice: Option<ModeNotice>,
}

impl Default for ModeLine {
    fn default() -> Self {
        Self {
            vim: None,
            approval: ApprovalMode::default(),
            background_agents: 0,
            needs_you: 0,
            footer_hints: true,
            notice: None,
        }
    }
}

/// Unlike a hint, a notice sits at the right end and leaves the mode in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeNotice {
    pub text: String,
    pub tone: Tone,
    pub until: Instant,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ModeState<'a> {
    /// Takes the mode's place.
    pub hint: Option<&'a str>,
    pub working: bool,
    /// The input has text, so ← moves the cursor rather than opening the crew view, and the
    /// `←` hint hides.
    pub typing: bool,
    pub pinned_cards: usize,
    /// The agent panel has rows.
    pub panel: bool,
    pub panel_focused: bool,
    /// A card has the keys.
    pub card_focused: bool,
    /// The input is in shell mode, which takes the approval mode's place.
    pub shell: bool,
    /// Shown in the notice slot while no notice is up.
    pub enter_sends: Option<&'a str>,
    /// The line's parts and each CLI's plan usage. `None` draws the mode alone.
    pub line: Option<&'a BottomLine>,
}

impl ModeLine {
    /// The bottom row: the mode with its hints, the parts the line has, and any notice at the
    /// right end, in the fullest layout `ctx.width` has room for.
    pub fn line(&self, ctx: &RenderCtx<'_>, state: ModeState<'_>) -> Line<'static> {
        let (pieces, notice) = self.pieces(ctx, state);
        let plain = BottomLine::default();
        line::lay_out(state.line.unwrap_or(&plain), &pieces, notice, ctx)
    }

    /// The mode's own part, and what sits in the notice slot.
    pub(super) fn pieces(
        &self,
        ctx: &RenderCtx<'_>,
        state: ModeState<'_>,
    ) -> (ModePieces, Option<Span<'static>>) {
        let theme = ctx.theme;
        let dim = theme.style(Role::Dim);
        let words = |joiner, text: &str, style, gives| ModePiece::Words {
            joiner,
            span: Span::styled(text.to_owned(), style),
            gives,
            short: None,
        };
        let mut pieces = Vec::new();
        // A card that has the keys stands in for the input, so the input's vim mode isn't
        // shown while it does.
        if let Some(vim) = self.vim.filter(|_| !state.card_focused) {
            pieces.push(ModePiece::Words {
                joiner: Joiner::Space,
                span: Span::styled(vim.label(), dim),
                gives: None,
                short: Some(Span::styled(vim.short_label(), dim)),
            });
        }
        if let Some(hint) = state.hint {
            pieces.push(words(Joiner::Space, &sanitize_line(hint), dim, None));
            return (ModePieces(pieces), None);
        }
        // Claude Code's mode line says only this in bash mode, so Esc and Enter mean one thing.
        if state.shell && !state.card_focused {
            let shell = theme.style(Role::Shell);
            pieces.push(words(Joiner::Space, "! for shell mode", shell, None));
            return (ModePieces(pieces), self.notice(ctx, None));
        }
        pieces.push(ModePiece::Mode {
            sign: if self.approval.asks_first() {
                theme.glyphs.mode_ask
            } else {
                theme.glyphs.mode_go
            },
            word: self.approval.label(),
            banner: self.approval.banner(),
            style: theme.style(self.approval.role()),
        });
        // A focused card holds every key but Shift+Tab and the quit keys, and its own key line
        // names Shift+Tab, so a hint here would name a key that does nothing until it lets go.
        if state.card_focused {
            return (ModePieces(pieces), None);
        }
        // Claude Code leaves this off for its default mode, which hides how to leave it.
        pieces.push(words(
            Joiner::Space,
            "(shift+tab to cycle)",
            dim,
            Some(Give::CycleHint),
        ));
        if state.panel_focused {
            let keys = "enter to open · x to stop · esc to go back";
            pieces.push(words(Joiner::Dot, keys, dim, None));
            return (ModePieces(pieces), None);
        }
        // Esc in INSERT or VISUAL only goes back to NORMAL, so its hint waits until then.
        if self.footer_hints && !matches!(self.vim, Some(VimMode::Insert | VimMode::Visual)) {
            let footer = if state.working {
                "esc to interrupt"
            } else {
                "? for shortcuts"
            };
            pieces.push(words(Joiner::Dot, footer, dim, Some(Give::FooterHint)));
        }
        if !state.typing
            && let Some(arrow) = self.arrow_hint(ctx, state.pinned_cards)
        {
            pieces.push(ModePiece::Words {
                joiner: Joiner::Dot,
                short: Some(self.short_arrow_hint(ctx, state.pinned_cards)),
                span: arrow,
                gives: None,
            });
        }
        if state.panel {
            pieces.push(words(
                Joiner::Dot,
                "↓ to manage",
                dim,
                Some(Give::ManageHint),
            ));
        }
        (ModePieces(pieces), self.notice(ctx, state.enter_sends))
    }

    /// The notice, or else what Enter sends, for the right end after the line's parts.
    fn notice(&self, ctx: &RenderCtx<'_>, enter_sends: Option<&str>) -> Option<Span<'static>> {
        let theme = ctx.theme;
        self.notice
            .as_ref()
            .filter(|notice| notice.until > ctx.clock.now)
            .map(|notice| (notice.text.as_str(), theme.style(notice.tone.role())))
            .or(enter_sends.map(|text| (text, theme.style(Role::Dim))))
            .map(|(text, style)| Span::styled(sanitize_line(text).trim().to_owned(), style))
            .filter(|span| span.width() > 0)
    }

    /// Claude Code's space after the arrow, which some fonts draw two cells wide, keeps it off
    /// the count.
    fn arrow_hint(&self, ctx: &RenderCtx<'_>, pinned_cards: usize) -> Option<Span<'static>> {
        let theme = ctx.theme;
        let arrow = theme.glyphs.agents;
        let waiting = self.needs_you + pinned_cards;
        if waiting > 0 {
            return Some(Span::styled(
                format!("{arrow} {waiting} needs you"),
                theme.style(Role::Warning),
            ));
        }
        let agents = self.background_agents;
        (agents > 0).then(|| {
            let noun = if agents == 1 { "agent" } else { "agents" };
            Span::styled(format!("{arrow} {agents} {noun}"), theme.style(Role::Dim))
        })
    }

    /// [`Self::arrow_hint`] on a terminal too narrow for its words: `← 1!`, `← 2`.
    fn short_arrow_hint(&self, ctx: &RenderCtx<'_>, pinned_cards: usize) -> Span<'static> {
        let theme = ctx.theme;
        let arrow = theme.glyphs.agents;
        let waiting = self.needs_you + pinned_cards;
        if waiting > 0 {
            return Span::styled(format!("{arrow} {waiting}!"), theme.style(Role::Warning));
        }
        Span::styled(
            format!("{arrow} {}", self.background_agents),
            theme.style(Role::Dim),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::render::Clock;
    use crate::text::{plain_text, str_width};
    use crate::theme::{Glyphs, Theme};

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    fn text(line: Line<'static>) -> String {
        plain_text(&[line])
    }

    fn idle() -> ModeState<'static> {
        ModeState::default()
    }

    #[test]
    fn each_mode_has_its_glyph_words_and_color() {
        let theme = theme();
        let ctx = RenderCtx::new(100, &theme, Clock::still(Instant::now()));
        let mut mode = ModeLine::default();
        assert_eq!(
            text(mode.line(&ctx, idle())),
            "  ǁ supervised mode on (shift+tab to cycle) · ? for shortcuts"
        );
        mode.approval = ApprovalMode::AutoPreview;
        assert_eq!(
            text(mode.line(&ctx, idle())),
            "  ǁ auto preview mode on (shift+tab to cycle) · ? for shortcuts"
        );
        mode.approval = ApprovalMode::Auto;
        let line = mode.line(&ctx, idle());
        assert_eq!(
            text(line.clone()),
            "  ⏵⏵ auto mode on (shift+tab to cycle) · ? for shortcuts"
        );
        assert_eq!(line.spans[1].style, theme.style(Role::ModeAuto));
        assert_eq!(line.spans[3].style, theme.style(Role::Dim));
        mode.approval = ApprovalMode::Autopilot;
        let line = mode.line(&ctx, idle());
        assert_eq!(
            text(line.clone()),
            "  ⏵⏵ autopilot on (shift+tab to cycle) · ? for shortcuts"
        );
        assert_eq!(line.spans[1].style, theme.style(Role::ModeAutopilot));
        mode.approval = ApprovalMode::Supervised;
        let line = mode.line(&ctx, idle());
        assert_eq!(line.spans[1].style, theme.style(Role::ModeSupervised));
    }

    /// Shift+Tab's cycle: supervised, auto, full auto. Shadow is a setting, and Shift+Tab goes
    /// on from it to auto.
    #[test]
    fn shift_tab_goes_from_auto_to_autopilot_and_back_round() {
        let cycle: Vec<ApprovalMode> =
            std::iter::successors(Some(ApprovalMode::Supervised), |mode| Some(mode.next()))
                .take(4)
                .collect();
        assert_eq!(
            cycle,
            [
                ApprovalMode::Supervised,
                ApprovalMode::Auto,
                ApprovalMode::Autopilot,
                ApprovalMode::Supervised,
            ]
        );
        assert_eq!(ApprovalMode::AutoPreview.next(), ApprovalMode::Auto);
        assert!(!ApprovalMode::Autopilot.asks_first());
    }

    /// A card that has the keys stands in for the input, so neither the input's vim mode nor a
    /// hint for its keys shows; the card's own key line names Shift+Tab.
    #[test]
    fn a_card_with_the_keys_leaves_the_vim_mode_off() {
        let theme = theme();
        let ctx = RenderCtx::new(100, &theme, Clock::still(Instant::now()));
        let mode = ModeLine {
            vim: Some(VimMode::Insert),
            ..ModeLine::default()
        };
        let card = ModeState {
            card_focused: true,
            ..idle()
        };
        assert_eq!(text(mode.line(&ctx, card)), "  ǁ supervised mode on");
        assert!(text(mode.line(&ctx, idle())).starts_with("  -- INSERT -- ǁ supervised"));
    }

    /// As Claude Code's bash mode reads, `-- INSERT -- ! for shell mode`, with a notice still at
    /// the right end.
    #[test]
    fn shell_mode_says_only_that() {
        let theme = theme();
        let now = Instant::now();
        let ctx = RenderCtx::new(100, &theme, Clock::still(now));
        let mut mode = ModeLine {
            background_agents: 3,
            vim: Some(VimMode::Insert),
            ..ModeLine::default()
        };
        let shell = ModeState {
            shell: true,
            working: true,
            ..idle()
        };
        let line = mode.line(&ctx, shell);
        assert_eq!(text(line.clone()), "  -- INSERT -- ! for shell mode");
        let words = line
            .spans
            .iter()
            .find(|span| span.content == "! for shell mode")
            .expect("the words");
        assert_eq!(words.style, theme.style(Role::Shell));

        mode.vim = None;
        mode.notice = Some(ModeNotice {
            text: "Copied".into(),
            tone: Tone::Success,
            until: now + Duration::from_secs(2),
        });
        let line = text(mode.line(&ctx, shell));
        assert!(line.starts_with("  ! for shell mode  "), "{line}");
        assert!(line.ends_with("Copied"), "{line}");
    }

    #[test]
    fn hints_sit_inline_and_hide_under_insert() {
        let theme = theme();
        let ctx = RenderCtx::new(100, &theme, Clock::still(Instant::now()));
        let mut mode = ModeLine {
            background_agents: 3,
            ..ModeLine::default()
        };
        let working = ModeState {
            working: true,
            panel: true,
            ..idle()
        };
        assert_eq!(
            text(mode.line(&ctx, working)),
            "  ǁ supervised mode on (shift+tab to cycle) · esc to interrupt · ← 3 agents · ↓ to manage"
        );
        mode.vim = Some(VimMode::Insert);
        assert_eq!(
            text(mode.line(&ctx, working)),
            "  -- INSERT -- ǁ supervised mode on (shift+tab to cycle) · ← 3 agents · ↓ to manage"
        );
        mode.vim = Some(VimMode::Normal);
        let wide = RenderCtx::new(120, &theme, Clock::still(Instant::now()));
        assert!(text(mode.line(&wide, working)).contains("esc to interrupt"));
        mode.background_agents = 1;
        mode.vim = None;
        assert!(text(mode.line(&ctx, idle())).ends_with("← 1 agent"));
    }

    #[test]
    fn the_arrow_hint_says_when_cards_wait_and_hides_while_typing() {
        let theme = theme();
        let ctx = RenderCtx::new(100, &theme, Clock::still(Instant::now()));
        let mode = ModeLine {
            background_agents: 3,
            footer_hints: false,
            ..ModeLine::default()
        };
        let pinned = ModeState {
            pinned_cards: 1,
            ..idle()
        };
        let line = mode.line(&ctx, pinned);
        assert_eq!(
            text(line.clone()),
            "  ǁ supervised mode on (shift+tab to cycle) · ← 1 needs you"
        );
        assert_eq!(line.spans.last().unwrap().style, theme.style(Role::Warning));
        let typing = ModeState {
            typing: true,
            ..idle()
        };
        assert_eq!(
            text(mode.line(&ctx, typing)),
            "  ǁ supervised mode on (shift+tab to cycle)"
        );
    }

    #[test]
    fn a_focused_card_leaves_the_mode_alone() {
        let theme = theme();
        let ctx = RenderCtx::new(100, &theme, Clock::still(Instant::now()));
        let mode = ModeLine {
            approval: ApprovalMode::Auto,
            ..ModeLine::default()
        };
        let state = ModeState {
            card_focused: true,
            working: true,
            panel: true,
            ..idle()
        };
        assert_eq!(text(mode.line(&ctx, state)), "  ⏵⏵ auto mode on");
    }

    #[test]
    fn a_hint_replaces_the_mode() {
        let theme = theme();
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let mode = ModeLine {
            vim: Some(VimMode::Normal),
            ..ModeLine::default()
        };
        let state = ModeState {
            hint: Some("Press Ctrl-C again to exit"),
            ..idle()
        };
        assert_eq!(
            text(mode.line(&ctx, state)),
            "  -- NORMAL -- Press Ctrl-C again to exit"
        );
    }

    #[test]
    fn a_notice_sits_at_the_right_end_and_keeps_the_mode() {
        let theme = theme();
        let now = Instant::now();
        let ctx = RenderCtx::new(80, &theme, Clock::still(now));
        let mode = ModeLine {
            approval: ApprovalMode::Auto,
            notice: Some(ModeNotice {
                text: "auto mode unavailable for this model".into(),
                tone: Tone::Warning,
                until: now + Duration::from_secs(7),
            }),
            ..ModeLine::default()
        };
        let line = mode.line(&ctx, idle());
        let row = text(line.clone());
        assert!(row.starts_with("  ⏵⏵ auto mode on"), "{row:?}");
        assert!(
            row.ends_with("auto mode unavailable for this model"),
            "{row:?}"
        );
        assert_eq!(str_width(&row), 80);
        assert_eq!(line.spans.last().unwrap().style, theme.style(Role::Warning));
        let later = RenderCtx::new(80, &theme, Clock::still(now + Duration::from_secs(8)));
        assert!(!text(mode.line(&later, idle())).contains("unavailable"));
    }

    #[test]
    fn a_notice_too_wide_is_cut_never_dropped() {
        let theme = theme();
        let now = Instant::now();
        let ctx = RenderCtx::new(50, &theme, Clock::still(now));
        let mode = ModeLine {
            notice: Some(ModeNotice {
                text: "/effort: \"turbo\" isn't a level; use low, medium, high, xhigh, max".into(),
                tone: Tone::Error,
                until: now + Duration::from_secs(7),
            }),
            ..ModeLine::default()
        };
        let row = text(mode.line(&ctx, idle()));
        assert!(row.contains("/effort: \"turbo\""), "{row:?}");
        assert!(row.ends_with('…'), "{row:?}");
        assert!(str_width(&row) <= 50, "{row:?}");
    }

    #[test]
    fn narrow_terminals_drop_whole_parts_in_order() {
        let theme = theme();
        let mode = ModeLine {
            approval: ApprovalMode::AutoPreview,
            background_agents: 3,
            ..ModeLine::default()
        };
        let state = ModeState {
            working: true,
            panel: true,
            ..idle()
        };
        let at = |width| {
            let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
            text(mode.line(&ctx, state))
        };
        assert_eq!(
            at(100),
            "  ǁ auto preview mode on (shift+tab to cycle) · esc to interrupt · ← 3 agents · ↓ to manage"
        );
        // The footer's hint first, then how to change the mode, then the panel's.
        assert_eq!(
            at(80),
            "  ǁ auto preview mode on (shift+tab to cycle) · ← 3 agents · ↓ to manage"
        );
        assert_eq!(
            at(60),
            "  ǁ auto preview mode on · ← 3 agents · ↓ to manage"
        );
        assert_eq!(at(40), "  ǁ auto preview mode on · ← 3 agents");
        // The mode loses its words before the hint that says something waits.
        assert_eq!(at(30), "  ǁ auto preview · ← 3 agents");
        assert_eq!(at(20), "  ǁ · ← 3 agents");
    }

    /// The parts the line has sit against the right edge, and the mode's own hints are the
    /// first things a narrow terminal takes.
    #[test]
    fn the_lines_parts_sit_at_the_right_end_and_the_hints_go_first() {
        use crate::bottom::line::{AddedPart, LinePart};

        let theme = theme();
        let mode = ModeLine::default();
        let mut line = BottomLine::default();
        line.right.push(LinePart::Added(AddedPart::words(
            "spend",
            "$0.21",
            Tone::Dim,
        )));
        let state = ModeState {
            line: Some(&line),
            ..idle()
        };
        let at = |width| {
            let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
            text(mode.line(&ctx, state))
        };
        let wide = at(100);
        assert!(
            wide.starts_with("  ǁ supervised mode on (shift+tab to cycle) · ? for shortcuts  "),
            "{wide:?}"
        );
        assert!(wide.ends_with("  $0.21"), "{wide:?}");
        assert_eq!(str_width(&wide), 100, "the right side ends at the edge");
        assert_eq!(
            at(60),
            format!(
                "  ǁ supervised mode on (shift+tab to cycle){}$0.21",
                " ".repeat(12)
            )
        );
        assert_eq!(
            at(40),
            format!("  ǁ supervised mode on{}$0.21", " ".repeat(13))
        );
        // A part you added goes before the mode loses a word.
        assert_eq!(at(26), "  ǁ supervised mode on");
        assert_eq!(at(20), "  ǁ supervised");
        assert_eq!(at(10), "  ǁ");
    }

    #[test]
    fn a_notice_goes_after_the_lines_parts_at_the_right_end() {
        use crate::bottom::line::{AddedPart, LinePart};

        let theme = theme();
        let now = Instant::now();
        let mode = ModeLine {
            notice: Some(ModeNotice {
                text: "Saved".into(),
                tone: Tone::Success,
                until: now + Duration::from_secs(5),
            }),
            ..ModeLine::default()
        };
        let mut line = BottomLine::default();
        line.right.push(LinePart::Added(AddedPart::words(
            "spend",
            "$0.21",
            Tone::Dim,
        )));
        let state = ModeState {
            line: Some(&line),
            ..idle()
        };
        let ctx = RenderCtx::new(100, &theme, Clock::still(now));
        let row = text(mode.line(&ctx, state));
        assert!(row.ends_with("  $0.21  Saved"), "{row:?}");
        assert_eq!(str_width(&row), 100);
        // On a narrow terminal the part goes and the notice stays.
        let narrow = RenderCtx::new(32, &theme, Clock::still(now));
        let row = text(mode.line(&narrow, state));
        assert_eq!(row, "  ǁ supervised mode on     Saved");
    }
}
