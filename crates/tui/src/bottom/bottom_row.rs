//! The bottom line is the screen's bottom row: the approval mode with its hints, the parts the
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

/// The bottom line while a paste typed out as keys is held back, as Claude Code's says while a
/// paste comes in.
pub const PASTING: &str = "Pasting…";

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

    /// Shift+Tab's cycle: supervised, auto, autopilot. Auto preview is a
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

    /// What the bottom line says: "auto mode on", and "autopilot on" without the extra word.
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
            ApprovalMode::AutoPreview => Role::ModeAutoPreview,
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
    /// What the bottom line says of the mode, as Claude Code draws it: nothing in NORMAL, and
    /// the whole label at every width.
    pub fn label(self) -> Option<&'static str> {
        match self {
            VimMode::Insert => Some("-- INSERT --"),
            VimMode::Normal => None,
            VimMode::Visual => Some("-- VISUAL --"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BottomRow {
    pub vim: Option<VimMode>,
    pub approval: ApprovalMode,
    pub background_agents: usize,
    /// Questions that wait on you somewhere else, such as in another run.
    pub needs_you: usize,
    /// Hidden while `-- INSERT --` shows, since Esc there only leaves INSERT.
    pub footer_hints: bool,
    pub notice: Option<ModeNotice>,
    /// The pull request a run opened last, such as `PR #12`, at the right end when nothing
    /// else is.
    pub pull_request: Option<String>,
    /// `ui.pr_footer`: whether the pull request shows.
    pub pr_footer: bool,
}

impl Default for BottomRow {
    fn default() -> Self {
        Self {
            vim: None,
            approval: ApprovalMode::default(),
            background_agents: 0,
            needs_you: 0,
            footer_hints: true,
            notice: None,
            pull_request: None,
            pr_footer: true,
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
    /// The turn is over and its work waits on what runs in the background, as the dim line
    /// above the input says. Esc stops nothing then, and the line keeps to the mode and the
    /// panel's key, as Claude Code's does.
    pub waiting: bool,
    /// The input has text, so ← moves the cursor rather than opening the agent panel, and the
    /// `←` hint hides.
    pub typing: bool,
    pub pinned_questions: usize,
    /// The agent panel has rows.
    pub panel: bool,
    pub panel_focused: bool,
    /// The keys of what is picked, such as a council message, after the mode in place of the
    /// other hints.
    pub picked_keys: Option<&'a str>,
    /// What Enter does on the run's row picked in the panel, such as `enter to answer`.
    pub row_enter: Option<&'a str>,
    /// A question has the keys.
    pub question_focused: bool,
    /// The input is in shell mode, which takes the approval mode's place.
    pub shell: bool,
    /// Shown in the notice slot while no notice is up.
    pub enter_sends: Option<&'a str>,
    /// The line's parts and each CLI's plan usage. `None` draws the mode alone.
    pub line: Option<&'a BottomLine>,
}

impl BottomRow {
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
            short_when: Give::ShortPieces,
        };
        let mut pieces = Vec::new();
        // A hint such as "Press Ctrl-C again to exit" or "Pasting…" takes the whole line, the
        // vim label too, as Claude Code's footer does.
        if let Some(hint) = state.hint {
            pieces.push(words(Joiner::Space, &sanitize_line(hint), dim, None));
            return (ModePieces(pieces), None);
        }
        // A question that has the keys stands in for the input, so the input's vim mode isn't
        // shown while it does.
        if let Some(label) = self
            .vim
            .filter(|_| !state.question_focused)
            .and_then(VimMode::label)
        {
            pieces.push(words(Joiner::Space, label, dim, None));
        }
        // Claude Code's bottom line says only this in bash mode, so Esc and Enter mean one thing.
        if state.shell && !state.question_focused {
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
        // A focused question holds every key but Shift+Tab and the quit keys, and its own key line
        // names Shift+Tab, so a hint here would name a key that does nothing until it lets go.
        if state.question_focused {
            return (ModePieces(pieces), None);
        }
        // On a run's row, what Enter does stays longest: the way back goes first, then how to
        // pick another.
        if let Some(enter) = state.row_enter.filter(|_| state.panel_focused) {
            let arrow = "↓";
            let arrows = format!(
                "↑ {arrow}{}to pick another",
                crate::theme::glyphs::gap_after(arrow)
            );
            pieces.push(words(Joiner::Dot, &arrows, dim, Some(Give::CycleHint)));
            pieces.push(words(Joiner::Dot, enter, dim, None));
            let back = "esc to go back to the input";
            pieces.push(words(Joiner::Dot, back, dim, Some(Give::FooterHint)));
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
        if let Some(keys) = state.picked_keys {
            for key in keys.split(" · ") {
                pieces.push(ModePiece::Words {
                    joiner: Joiner::Dot,
                    span: Span::styled(key.to_owned(), dim),
                    gives: None,
                    short: short_key(key).map(|short| Span::styled(short, dim)),
                    short_when: Give::ShortKeys,
                });
            }
            return (ModePieces(pieces), None);
        }
        // Esc in INSERT or VISUAL only goes back to NORMAL, so its hint waits until then.
        if self.footer_hints
            && !state.waiting
            && !matches!(self.vim, Some(VimMode::Insert | VimMode::Visual))
        {
            let footer = if state.working {
                "esc to interrupt"
            } else {
                "? for shortcuts"
            };
            pieces.push(words(Joiner::Dot, footer, dim, Some(Give::FooterHint)));
        }
        if !state.typing
            && let Some(arrow) = self.arrow_hint(ctx, state.pinned_questions, state.waiting)
        {
            pieces.push(ModePiece::Words {
                joiner: Joiner::Dot,
                short: Some(self.short_arrow_hint(ctx, state.pinned_questions)),
                span: arrow,
                gives: None,
                short_when: Give::ShortPieces,
            });
        }
        if state.panel {
            pieces.push(words(
                Joiner::Dot,
                "↓  to manage",
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
            .or(self
                .pull_request
                .as_deref()
                .filter(|_| self.pr_footer)
                .map(|text| (text, theme.style(Role::Dim))))
            .map(|(text, style)| Span::styled(sanitize_line(text).trim().to_owned(), style))
            .filter(|span| span.width() > 0)
    }

    /// Claude Code's space after the arrow, which some fonts draw two cells wide, keeps it off
    /// the count.
    /// What waits on you always shows; the agents at work leave it while the work only waits.
    fn arrow_hint(
        &self,
        ctx: &RenderCtx<'_>,
        pinned_questions: usize,
        work_waits: bool,
    ) -> Option<Span<'static>> {
        let theme = ctx.theme;
        let arrow = theme.glyphs.agents;
        let gap = crate::theme::glyphs::gap_after(arrow);
        let waiting = self.needs_you + pinned_questions;
        if waiting > 0 {
            return Some(Span::styled(
                format!("{arrow}{gap}{waiting} needs you"),
                theme.style(Role::Warn),
            ));
        }
        let agents = self.background_agents;
        (agents > 0 && !work_waits).then(|| {
            let noun = if agents == 1 { "agent" } else { "agents" };
            Span::styled(
                format!(
                    "{arrow}{}{agents} {noun}",
                    crate::theme::glyphs::gap_after(arrow)
                ),
                theme.style(Role::Dim),
            )
        })
    }

    /// [`Self::arrow_hint`] on a terminal too narrow for its words: `← 1!`, `← 2`.
    fn short_arrow_hint(&self, ctx: &RenderCtx<'_>, pinned_questions: usize) -> Span<'static> {
        let theme = ctx.theme;
        let arrow = theme.glyphs.agents;
        let gap = crate::theme::glyphs::gap_after(arrow);
        let waiting = self.needs_you + pinned_questions;
        if waiting > 0 {
            return Span::styled(format!("{arrow}{gap}{waiting}!"), theme.style(Role::Warn));
        }
        Span::styled(
            format!(
                "{arrow}{}{}",
                crate::theme::glyphs::gap_after(arrow),
                self.background_agents
            ),
            theme.style(Role::Dim),
        )
    }
}

/// A picked message's or task's key in fewer words, for a narrow line: `↑ ↓  to pick`,
/// `enter to open`, `esc to go back`. `None` for a key with no shorter way to say it.
fn short_key(key: &str) -> Option<String> {
    if let Some(pick) = key.strip_suffix(" another") {
        return Some(pick.to_owned());
    }
    if key.starts_with("enter to open ") {
        return Some("enter to open".to_owned());
    }
    key.strip_suffix(" to the input").map(str::to_owned)
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
        let mut mode = BottomRow::default();
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

    /// Shift+Tab's cycle: supervised, auto, autopilot. Auto preview is a setting, and Shift+Tab
    /// goes on from it to auto.
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

    /// A question that has the keys stands in for the input, so neither the input's vim mode nor a
    /// hint for its keys shows; the question's own key line names Shift+Tab.
    #[test]
    fn a_question_with_the_keys_leaves_the_vim_mode_off() {
        let theme = theme();
        let ctx = RenderCtx::new(100, &theme, Clock::still(Instant::now()));
        let mode = BottomRow {
            vim: Some(VimMode::Insert),
            ..BottomRow::default()
        };
        let answering = ModeState {
            question_focused: true,
            ..idle()
        };
        assert_eq!(text(mode.line(&ctx, answering)), "  ǁ supervised mode on");
        assert!(text(mode.line(&ctx, idle())).starts_with("  -- INSERT -- ǁ supervised"));
    }

    /// As Claude Code's bash mode reads, `-- INSERT -- ! for shell mode`, with a notice still at
    /// the right end.
    #[test]
    fn shell_mode_says_only_that() {
        let theme = theme();
        let now = Instant::now();
        let ctx = RenderCtx::new(100, &theme, Clock::still(now));
        let mut mode = BottomRow {
            background_agents: 3,
            vim: Some(VimMode::Insert),
            ..BottomRow::default()
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
        let mut mode = BottomRow {
            background_agents: 3,
            ..BottomRow::default()
        };
        let working = ModeState {
            working: true,
            panel: true,
            ..idle()
        };
        assert_eq!(
            text(mode.line(&ctx, working)),
            "  ǁ supervised mode on (shift+tab to cycle) · esc to interrupt · ←  3 agents · ↓  to manage"
        );
        mode.vim = Some(VimMode::Insert);
        assert_eq!(
            text(mode.line(&ctx, working)),
            "  -- INSERT -- ǁ supervised mode on (shift+tab to cycle) · ←  3 agents · ↓  to manage"
        );
        mode.vim = Some(VimMode::Normal);
        let wide = RenderCtx::new(120, &theme, Clock::still(Instant::now()));
        assert!(text(mode.line(&wide, working)).contains("esc to interrupt"));
        mode.background_agents = 1;
        mode.vim = None;
        assert!(text(mode.line(&ctx, idle())).ends_with("←  1 agent"));
    }

    /// A turn over with its workflows going: Esc stops nothing and the shortcuts wait, so the
    /// line keeps to the mode and the panel's key, as Claude Code's does. What waits on you
    /// still shows.
    #[test]
    fn while_the_work_waits_the_line_keeps_to_the_mode_and_the_panel() {
        let theme = theme();
        let ctx = RenderCtx::new(118, &theme, Clock::still(Instant::now()));
        let mut mode = BottomRow {
            approval: ApprovalMode::Auto,
            background_agents: 1,
            ..BottomRow::default()
        };
        let waiting = ModeState {
            waiting: true,
            panel: true,
            ..idle()
        };
        assert_eq!(
            text(mode.line(&ctx, waiting)),
            "  ⏵⏵ auto mode on (shift+tab to cycle) · ↓  to manage"
        );
        mode.needs_you = 1;
        assert_eq!(
            text(mode.line(&ctx, waiting)),
            "  ⏵⏵ auto mode on (shift+tab to cycle) · ←  1 needs you · ↓  to manage"
        );
    }

    #[test]
    fn the_arrow_hint_says_when_questions_wait_and_hides_while_typing() {
        let theme = theme();
        let ctx = RenderCtx::new(100, &theme, Clock::still(Instant::now()));
        let mode = BottomRow {
            background_agents: 3,
            footer_hints: false,
            ..BottomRow::default()
        };
        let pinned = ModeState {
            pinned_questions: 1,
            ..idle()
        };
        let line = mode.line(&ctx, pinned);
        assert_eq!(
            text(line.clone()),
            "  ǁ supervised mode on (shift+tab to cycle) · ←  1 needs you"
        );
        assert_eq!(line.spans.last().unwrap().style, theme.style(Role::Warn));
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
    fn a_focused_question_leaves_the_mode_alone() {
        let theme = theme();
        let ctx = RenderCtx::new(100, &theme, Clock::still(Instant::now()));
        let mode = BottomRow {
            approval: ApprovalMode::Auto,
            ..BottomRow::default()
        };
        let state = ModeState {
            question_focused: true,
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
        let mode = BottomRow {
            vim: Some(VimMode::Normal),
            ..BottomRow::default()
        };
        let state = ModeState {
            hint: Some("Press Ctrl-C again to exit"),
            ..idle()
        };
        assert_eq!(text(mode.line(&ctx, state)), "  Press Ctrl-C again to exit");
        // The vim label goes too, as in Claude Code.
        for vim in [VimMode::Insert, VimMode::Visual] {
            let mode = BottomRow {
                vim: Some(vim),
                ..BottomRow::default()
            };
            for hint in ["Press Ctrl-C again to exit", PASTING] {
                let state = ModeState {
                    hint: Some(hint),
                    ..idle()
                };
                assert_eq!(text(mode.line(&ctx, state)), format!("  {hint}"));
            }
        }
    }

    /// Claude Code draws `-- INSERT --` and `-- VISUAL --` dim at the start of the line under
    /// the prompt, whole at any width, and nothing at all in NORMAL.
    #[test]
    fn the_vim_label_reads_as_claude_codes() {
        let theme = theme();
        let still = Clock::still(Instant::now());
        let mut mode = BottomRow {
            vim: Some(VimMode::Normal),
            ..BottomRow::default()
        };
        let wide = RenderCtx::new(100, &theme, still);
        assert_eq!(
            text(mode.line(&wide, idle())),
            "  ǁ supervised mode on (shift+tab to cycle) · ? for shortcuts"
        );
        for (vim, label) in [
            (VimMode::Insert, "-- INSERT --"),
            (VimMode::Visual, "-- VISUAL --"),
        ] {
            mode.vim = Some(vim);
            for width in [100, 30] {
                let line = mode.line(&RenderCtx::new(width, &theme, still), idle());
                assert!(
                    text(line.clone()).starts_with(&format!("  {label} ǁ")),
                    "{width}"
                );
                let span = line.spans.iter().find(|span| span.content == label);
                assert_eq!(span.map(|span| span.style), Some(theme.style(Role::Dim)));
            }
        }
    }

    #[test]
    fn a_notice_sits_at_the_right_end_and_keeps_the_mode() {
        let theme = theme();
        let now = Instant::now();
        let ctx = RenderCtx::new(80, &theme, Clock::still(now));
        let mode = BottomRow {
            approval: ApprovalMode::Auto,
            notice: Some(ModeNotice {
                text: "auto mode unavailable for this model".into(),
                tone: Tone::Warning,
                until: now + Duration::from_secs(7),
            }),
            ..BottomRow::default()
        };
        let line = mode.line(&ctx, idle());
        let row = text(line.clone());
        assert!(row.starts_with("  ⏵⏵ auto mode on"), "{row:?}");
        assert!(
            row.ends_with("auto mode unavailable for this model"),
            "{row:?}"
        );
        assert_eq!(str_width(&row), 80);
        assert_eq!(line.spans.last().unwrap().style, theme.style(Role::Warn));
        let later = RenderCtx::new(80, &theme, Clock::still(now + Duration::from_secs(8)));
        assert!(!text(mode.line(&later, idle())).contains("unavailable"));
    }

    /// A pull request a run opened sits at the right end, dim, under any notice, and
    /// `ui.pr_footer = false` hides it.
    #[test]
    fn the_pull_request_sits_at_the_right_end_unless_turned_off() {
        let theme = theme();
        let now = Instant::now();
        let ctx = RenderCtx::new(80, &theme, Clock::still(now));
        let mut mode = BottomRow {
            pull_request: Some("PR #12".into()),
            ..BottomRow::default()
        };
        let line = mode.line(&ctx, idle());
        let row = text(line.clone());
        assert!(row.ends_with("PR #12"), "{row:?}");
        assert_eq!(str_width(&row), 80);
        assert_eq!(line.spans.last().unwrap().style, theme.style(Role::Dim));
        mode.notice = Some(ModeNotice {
            text: "Copied".into(),
            tone: Tone::Success,
            until: now + Duration::from_secs(7),
        });
        assert!(text(mode.line(&ctx, idle())).ends_with("Copied"));
        mode.notice = None;
        mode.pr_footer = false;
        assert!(!text(mode.line(&ctx, idle())).contains("PR #12"));
    }

    #[test]
    fn a_notice_too_wide_is_cut_never_dropped() {
        let theme = theme();
        let now = Instant::now();
        let ctx = RenderCtx::new(50, &theme, Clock::still(now));
        let mode = BottomRow {
            notice: Some(ModeNotice {
                text: "/effort: \"turbo\" isn't a level; use low, medium, high, xhigh, max".into(),
                tone: Tone::Error,
                until: now + Duration::from_secs(7),
            }),
            ..BottomRow::default()
        };
        let row = text(mode.line(&ctx, idle()));
        assert!(row.contains("/effort: \"turbo\""), "{row:?}");
        assert!(row.ends_with('…'), "{row:?}");
        assert!(str_width(&row) <= 50, "{row:?}");
    }

    #[test]
    fn narrow_terminals_drop_whole_parts_in_order() {
        let theme = theme();
        let mode = BottomRow {
            approval: ApprovalMode::AutoPreview,
            background_agents: 3,
            ..BottomRow::default()
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
            "  ǁ auto preview mode on (shift+tab to cycle) · esc to interrupt · ←  3 agents · ↓  to manage"
        );
        // The footer's hint first, then how to change the mode, then the panel's.
        assert_eq!(
            at(80),
            "  ǁ auto preview mode on (shift+tab to cycle) · ←  3 agents · ↓  to manage"
        );
        assert_eq!(
            at(60),
            "  ǁ auto preview mode on · ←  3 agents · ↓  to manage"
        );
        assert_eq!(at(40), "  ǁ auto preview mode on · ←  3 agents");
        // The mode loses its words before the hint that says something waits.
        assert_eq!(at(30), "  ǁ auto preview · ←  3 agents");
        assert_eq!(at(20), "  ǁ · ←  3 agents");
    }

    /// The parts the line has sit against the right edge, and the mode's own hints are the
    /// first things a narrow terminal takes.
    #[test]
    fn the_lines_parts_sit_at_the_right_end_and_the_hints_go_first() {
        use crate::bottom::line::{AddedPart, LinePart};

        let theme = theme();
        let mode = BottomRow::default();
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
        let mode = BottomRow {
            notice: Some(ModeNotice {
                text: "Saved".into(),
                tone: Tone::Success,
                until: now + Duration::from_secs(5),
            }),
            ..BottomRow::default()
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
