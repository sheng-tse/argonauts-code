//! The mode line is the screen's bottom row, built from parts that give way whole on a narrow
//! terminal. Quotas, spend, and context sit at its right end when they fit. The mode and the
//! `←` hint never go.

use std::time::Instant;

use ratatui::text::{Line, Span};

use super::StatusPart;
use crate::cell::Tone;
use crate::render::RenderCtx;
use crate::text::{
    KEEP, Part, fit_parts, join_parts, sanitize_line, spans_width, truncate_spans, width_of,
};
use crate::theme::Role;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum ApprovalMode {
    #[default]
    Supervised,
    Shadow,
    Auto,
    /// Nothing waits for you.
    FullAuto,
}

impl ApprovalMode {
    pub const ALL: [ApprovalMode; 4] = [
        ApprovalMode::Supervised,
        ApprovalMode::Shadow,
        ApprovalMode::Auto,
        ApprovalMode::FullAuto,
    ];

    /// Shift+Tab's cycle: supervised, auto, full auto. Shadow is a
    /// setting; from it, Shift+Tab goes on to auto.
    pub fn next(self) -> Self {
        match self {
            ApprovalMode::Supervised | ApprovalMode::Shadow => ApprovalMode::Auto,
            ApprovalMode::Auto => ApprovalMode::FullAuto,
            ApprovalMode::FullAuto => ApprovalMode::Supervised,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ApprovalMode::Supervised => "supervised",
            ApprovalMode::Shadow => "shadow",
            ApprovalMode::Auto => "auto",
            ApprovalMode::FullAuto => "full auto",
        }
    }

    pub fn asks_first(self) -> bool {
        matches!(self, ApprovalMode::Supervised | ApprovalMode::Shadow)
    }

    pub fn role(self) -> Role {
        match self {
            ApprovalMode::Supervised => Role::ModeSupervised,
            ApprovalMode::Shadow => Role::ModeShadow,
            ApprovalMode::Auto => Role::ModeAuto,
            ApprovalMode::FullAuto => Role::ModeFullAuto,
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
    /// Takes the whole line.
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
    /// Quotas, spend, and context, at the right end before any notice.
    pub status: &'a [StatusPart],
}

/// Columns kept free between the mode line's parts and what sits at its right end.
const NOTICE_GAP: usize = 2;

/// The lowest gives way first on a narrow terminal. Every status part ranks lower, so quotas
/// and spend go before the mode's own hints, except the chips that never go.
mod rank {
    pub const MANAGE: u16 = 1000;
    pub const FOOTER: u16 = 1001;
    pub const CYCLE: u16 = 1002;
    pub const NOTICE: u16 = 1003;
}

impl ModeLine {
    pub fn line(&self, ctx: &RenderCtx<'_>, state: ModeState<'_>) -> Line<'static> {
        let theme = ctx.theme;
        let dim = theme.style(Role::Dim);
        let width = usize::from(ctx.width);
        let lead = vec![Span::raw("  ")];

        // A card that has the keys stands in for the input, so the input's vim mode isn't
        // shown while it does.
        let vim = self.vim.filter(|_| !state.card_focused);
        if let Some(hint) = state.hint {
            let mut parts = Vec::new();
            if let Some(vim) = vim {
                parts.push(Part::new(vec![Span::styled(vim.label(), dim)], KEEP));
            }
            let hint = sanitize_line(hint).into_owned();
            parts.push(Part::joined(
                Span::raw(" "),
                vec![Span::styled(hint, dim)],
                KEEP,
            ));
            return two_sided(lead, &parts, &status_parts(state.status, ctx), width);
        }

        let dot = || Span::styled(" · ", dim);
        let mut parts = Vec::new();
        if let Some(vim) = vim {
            parts.push(Part::new(vec![Span::styled(vim.label(), dim)], KEEP));
        }
        let status = status_parts(state.status, ctx);
        // Claude Code's mode line says only this in bash mode, so Esc and Enter mean one thing.
        if state.shell && !state.card_focused {
            parts.push(Part::joined(
                Span::raw(" "),
                vec![Span::styled("! for shell mode", theme.style(Role::Shell))],
                KEEP,
            ));
            return self.with_notice(lead, parts, status, ctx, None);
        }
        let glyph = if self.approval.asks_first() {
            theme.glyphs.mode_ask
        } else {
            theme.glyphs.mode_go
        };
        parts.push(Part::joined(
            Span::raw(" "),
            vec![Span::styled(
                format!("{glyph} {} mode on", self.approval.label()),
                theme.style(self.approval.role()),
            )],
            KEEP,
        ));
        // A focused card holds every key but Shift+Tab and the quit keys, and its own key line
        // names Shift+Tab, so a hint here would name a key that does nothing until it lets go.
        if state.card_focused {
            return two_sided(lead, &parts, &status, width);
        }
        // Claude Code leaves this off for its default mode, which hides how to leave it.
        parts.push(Part::joined(
            Span::raw(" "),
            vec![Span::styled("(shift+tab to cycle)", dim)],
            rank::CYCLE,
        ));
        if state.panel_focused {
            parts.push(Part::joined(
                dot(),
                vec![Span::styled(
                    "enter to open · x to stop · esc to go back",
                    dim,
                )],
                KEEP,
            ));
            return two_sided(lead, &parts, &status, width);
        }
        // Esc in INSERT or VISUAL only goes back to NORMAL, so its hint waits until then.
        if self.footer_hints && !matches!(self.vim, Some(VimMode::Insert | VimMode::Visual)) {
            let footer = if state.working {
                "esc to interrupt"
            } else {
                "? for shortcuts"
            };
            parts.push(Part::joined(
                dot(),
                vec![Span::styled(footer, dim)],
                rank::FOOTER,
            ));
        }
        if !state.typing
            && let Some(arrow) = self.arrow_hint(ctx, state.pinned_cards)
        {
            parts.push(Part::joined(dot(), vec![arrow], KEEP));
        }
        if state.panel {
            parts.push(Part::joined(
                dot(),
                vec![Span::styled("↓ to manage", dim)],
                rank::MANAGE,
            ));
        }

        self.with_notice(lead, parts, status, ctx, state.enter_sends)
    }

    /// The notice, or else what Enter sends, at the right end after the status.
    fn with_notice(
        &self,
        lead: Vec<Span<'static>>,
        parts: Vec<Part>,
        status: Vec<Part>,
        ctx: &RenderCtx<'_>,
        enter_sends: Option<&str>,
    ) -> Line<'static> {
        let theme = ctx.theme;
        let dim = theme.style(Role::Dim);
        let width = usize::from(ctx.width);
        let notice = self
            .notice
            .as_ref()
            .filter(|notice| notice.until > ctx.clock.now)
            .map(|notice| (notice.text.as_str(), theme.style(notice.tone.role())))
            .or(enter_sends.map(|text| (text, dim)))
            .map(|(text, style)| Span::styled(sanitize_line(text).trim().to_owned(), style))
            .filter(|span| span.width() > 0);
        let mut right = status;
        if let Some(notice) = notice {
            // A notice wider than what the kept parts leave is cut to fit, never dropped, since
            // an error there may be the only word on why Enter did nothing.
            let kept_width = |parts: &[Part]| -> usize {
                let kept: Vec<bool> = parts.iter().map(|part| part.rank == KEEP).collect();
                width_of(parts, &kept)
            };
            let kept_right = kept_width(&right);
            let joiner = if kept_right > 0 { NOTICE_GAP } else { 0 };
            let room = width.saturating_sub(spans_width(&lead));
            let cap =
                room.saturating_sub(kept_width(&parts) + NOTICE_GAP + kept_right + joiner + 1);
            let notice = truncate_spans(&[notice], cap);
            if spans_width(&notice) > 0 {
                right.push(Part::joined(
                    Span::raw(" ".repeat(NOTICE_GAP)),
                    notice,
                    rank::NOTICE,
                ));
            }
        }
        two_sided(lead, &parts, &right, width)
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
}

/// `left` after `lead`, and `right` against the right end with one column after it. Parts on
/// either side give way by rank until both fit with a gap between them.
fn two_sided(
    lead: Vec<Span<'static>>,
    left: &[Part],
    right: &[Part],
    width: usize,
) -> Line<'static> {
    let room = width.saturating_sub(spans_width(&lead));
    let mut shown_left = vec![true; left.len()];
    let mut shown_right = vec![true; right.len()];
    loop {
        let right_width = width_of(right, &shown_right);
        let ends = if right_width > 0 {
            NOTICE_GAP + right_width + 1
        } else {
            0
        };
        if width_of(left, &shown_left) + ends <= room {
            break;
        }
        let next = left
            .iter()
            .zip(&shown_left)
            .chain(right.iter().zip(&shown_right))
            .enumerate()
            .filter(|(_, (part, shown))| **shown && part.rank != KEEP)
            .min_by_key(|(i, (part, _))| (part.rank, std::cmp::Reverse(*i)))
            .map(|(i, _)| i);
        match next {
            Some(i) if i < left.len() => shown_left[i] = false,
            Some(i) => shown_right[i - left.len()] = false,
            None => break,
        }
    }
    let mut spans = lead;
    spans.extend(join_parts(left, &shown_left));
    let ends = join_parts(right, &shown_right);
    if !ends.is_empty() {
        let used = spans_width(&spans) + spans_width(&ends) + 1;
        let pad = width.saturating_sub(used).max(NOTICE_GAP);
        spans.push(Span::raw(" ".repeat(pad)));
        spans.extend(ends);
    }
    crate::text::truncate_line(&Line::from(spans), width)
}

fn status_parts(parts: &[StatusPart], ctx: &RenderCtx<'_>) -> Vec<Part> {
    let theme = ctx.theme;
    let dim = theme.style(Role::Dim);
    parts
        .iter()
        .filter_map(|part| {
            let text = sanitize_line(&part.text).trim().to_owned();
            (!text.is_empty()).then(|| {
                Part::joined(
                    Span::styled(" · ", dim),
                    vec![Span::styled(text, theme.style(part.tone.role()))],
                    part.rank,
                )
            })
        })
        .collect()
}

/// The status parts alone in one row, as they'd fit with nothing else on it.
pub fn status_line(parts: &[StatusPart], ctx: &RenderCtx<'_>) -> Option<Line<'static>> {
    let parts = status_parts(parts, ctx);
    (!parts.is_empty()).then(|| fit_parts(vec![Span::raw("  ")], &parts, usize::from(ctx.width)))
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
        mode.approval = ApprovalMode::Shadow;
        assert_eq!(
            text(mode.line(&ctx, idle())),
            "  ǁ shadow mode on (shift+tab to cycle) · ? for shortcuts"
        );
        mode.approval = ApprovalMode::Auto;
        let line = mode.line(&ctx, idle());
        assert_eq!(
            text(line.clone()),
            "  ⏵⏵ auto mode on (shift+tab to cycle) · ? for shortcuts"
        );
        assert_eq!(line.spans[1].style, theme.style(Role::ModeAuto));
        assert_eq!(line.spans[3].style, theme.style(Role::Dim));
        mode.approval = ApprovalMode::FullAuto;
        let line = mode.line(&ctx, idle());
        assert_eq!(
            text(line.clone()),
            "  ⏵⏵ full auto mode on (shift+tab to cycle) · ? for shortcuts"
        );
        assert_eq!(line.spans[1].style, theme.style(Role::ModeFullAuto));
        mode.approval = ApprovalMode::Supervised;
        let line = mode.line(&ctx, idle());
        assert_eq!(line.spans[1].style, theme.style(Role::ModeSupervised));
    }

    /// The cycle: supervised, auto, full auto. Shadow is a setting, and Shift+Tab goes
    /// on from it to auto.
    #[test]
    fn shift_tab_goes_from_auto_to_full_auto_and_back_round() {
        let cycle: Vec<ApprovalMode> =
            std::iter::successors(Some(ApprovalMode::Supervised), |mode| Some(mode.next()))
                .take(4)
                .collect();
        assert_eq!(
            cycle,
            [
                ApprovalMode::Supervised,
                ApprovalMode::Auto,
                ApprovalMode::FullAuto,
                ApprovalMode::Supervised,
            ]
        );
        assert_eq!(ApprovalMode::Shadow.next(), ApprovalMode::Auto);
        assert!(!ApprovalMode::FullAuto.asks_first());
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
        assert!(text(mode.line(&ctx, working)).contains("esc to interrupt"));
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
        assert_eq!(str_width(&row), 79);
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
            approval: ApprovalMode::Shadow,
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
            "  ǁ shadow mode on (shift+tab to cycle) · esc to interrupt · ← 3 agents · ↓ to manage"
        );
        assert_eq!(
            at(80),
            "  ǁ shadow mode on (shift+tab to cycle) · esc to interrupt · ← 3 agents"
        );
        assert_eq!(
            at(60),
            "  ǁ shadow mode on (shift+tab to cycle) · ← 3 agents"
        );
        assert_eq!(at(40), "  ǁ shadow mode on · ← 3 agents");
    }

    #[test]
    fn quotas_and_spend_sit_at_the_right_end_when_they_fit() {
        let theme = theme();
        let mode = ModeLine::default();
        let status = vec![
            StatusPart::quota("Claude 5h 71% used", 71, Tone::Dim),
            StatusPart::quota("Codex 5h 38% used", 38, Tone::Dim),
            StatusPart::spend("$0.00"),
        ];
        let state = ModeState {
            status: &status,
            ..idle()
        };
        let at = |width| {
            let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
            text(mode.line(&ctx, state))
        };
        let wide = at(140);
        assert!(
            wide.starts_with("  ǁ supervised mode on (shift+tab to cycle) · ? for shortcuts  "),
            "{wide:?}"
        );
        assert!(
            wide.ends_with("Claude 5h 71% used · Codex 5h 38% used · $0.00"),
            "{wide:?}"
        );
        assert_eq!(str_width(&wide), 139);
        // The least used quota goes first, then the other, and the mode's own hints last.
        assert!(
            at(100).ends_with("  Claude 5h 71% used · $0.00"),
            "{:?}",
            at(100)
        );
        assert!(at(80).ends_with("  $0.00"), "{:?}", at(80));
        assert_eq!(
            at(64),
            "  ǁ supervised mode on (shift+tab to cycle) · ? for shortcuts"
        );
        assert_eq!(at(30), "  ǁ supervised mode on");
    }

    #[test]
    fn an_alert_chip_stays_and_the_mode_hints_give_way_to_it() {
        let theme = theme();
        let mode = ModeLine::default();
        let status = vec![
            StatusPart::quota("Codex 5h 91% used", 91, Tone::Warning),
            StatusPart::spend("$0.46"),
        ];
        let state = ModeState {
            status: &status,
            ..idle()
        };
        let ctx = RenderCtx::new(60, &theme, Clock::still(Instant::now()));
        let line = mode.line(&ctx, state);
        let row = text(line.clone());
        assert!(row.starts_with("  ǁ supervised mode on"), "{row:?}");
        assert!(row.ends_with("  Codex 5h 91% used"), "{row:?}");
        assert!(!row.contains("$0.46"), "{row:?}");
        assert_eq!(line.spans.last().unwrap().style, theme.style(Role::Warning));
    }

    #[test]
    fn a_notice_goes_after_the_status_at_the_right_end() {
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
        let status = vec![StatusPart::spend("$0.21")];
        let state = ModeState {
            status: &status,
            ..idle()
        };
        let ctx = RenderCtx::new(100, &theme, Clock::still(now));
        let row = text(mode.line(&ctx, state));
        assert!(row.ends_with("  $0.21  Saved"), "{row:?}");
        assert_eq!(str_width(&row), 99);
    }

    #[test]
    fn the_status_line_drops_chips_by_rank() {
        let theme = theme();
        let parts = vec![
            StatusPart::quota("Claude 5h 42% used", 42, Tone::Dim),
            StatusPart::quota("Codex 5h 91% used", 91, Tone::Warning),
            StatusPart::quota("agy 12% used", 12, Tone::Dim),
            StatusPart::spend("$0.46"),
            StatusPart::model("opus"),
            StatusPart::new(" ", Tone::Dim),
        ];
        let at = |width| {
            let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
            status_line(&parts, &ctx).map(text)
        };
        assert_eq!(
            at(100).unwrap(),
            "  Claude 5h 42% used · Codex 5h 91% used · agy 12% used · $0.46 · opus"
        );
        assert_eq!(
            at(60).unwrap(),
            "  Claude 5h 42% used · Codex 5h 91% used · $0.46 · opus"
        );
        assert_eq!(at(40).unwrap(), "  Codex 5h 91% used · $0.46 · opus");
        assert_eq!(at(30).unwrap(), "  Codex 5h 91% used · opus");
        assert_eq!(at(22).unwrap(), "  Codex 5h 91% used");
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        let line = status_line(&parts, &ctx).unwrap();
        assert_eq!(line.spans[3].style, theme.style(Role::Warning));
        assert_eq!(status_line(&[], &ctx), None);
    }

    #[test]
    fn stale_readings_go_first_and_every_open_quota_before_the_spend() {
        let theme = theme();
        let parts = vec![
            StatusPart::model("claude-code/opus"),
            StatusPart::quota("Claude 5h 71% used", 71, Tone::Dim),
            StatusPart::quota("Codex 5h 91% used", 91, Tone::Warning),
            StatusPart::no_quota("OpenRouter $0.06"),
            StatusPart::stale("Gemini 5h 30% used", 30),
            StatusPart::spend("$0.21"),
        ];
        let at = |width| {
            let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
            status_line(&parts, &ctx).map(text).unwrap()
        };
        assert_eq!(
            at(120),
            "  claude-code/opus · Claude 5h 71% used · Codex 5h 91% used · OpenRouter $0.06 · Gemini 5h 30% used · $0.21"
        );
        assert_eq!(
            at(100),
            "  claude-code/opus · Claude 5h 71% used · Codex 5h 91% used · OpenRouter $0.06 · $0.21"
        );
        assert_eq!(
            at(80),
            "  claude-code/opus · Claude 5h 71% used · Codex 5h 91% used · $0.21"
        );
        assert_eq!(at(60), "  claude-code/opus · Codex 5h 91% used · $0.21");
        assert_eq!(at(40), "  claude-code/opus · Codex 5h 91% used");
        assert_eq!(at(30), "  Codex 5h 91% used");
    }
}
