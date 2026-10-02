use std::time::{Duration, Instant};

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::format;
use crate::render::{Clock, RenderCtx};
use crate::text::{KEEP, Part, fit_parts, sanitize_line};
use crate::theme::{ColorLevel, Role, Theme};

/// How long each spinner frame shows, except at the two ends of its sweep.
pub const SPINNER_PERIOD: Duration = Duration::from_millis(120);

/// How long the spinner holds at `·` and at `✽`, the two ends of its sweep, so a full cycle
/// takes about two seconds, as Claude Code's does.
pub const SPINNER_HOLD: Duration = Duration::from_millis(450);

/// The part in parentheses joins after this long.
pub const STATUS_AFTER: Duration = Duration::from_secs(1);

/// How long without an event before the line starts to fade toward the warning color. Claude
/// Code sends a heartbeat every 30 s, so a longer quiet means the stream stalled.
pub const STALL_AFTER: Duration = Duration::from_secs(30);

/// How long the fade takes on terminals that can blend colors.
const STALL_FADE: Duration = Duration::from_secs(2);

/// The band's width in cells, and the steps it rests off the verb between sweeps.
const SHIMMER_WIDTH: usize = 3;
const SHIMMER_PAUSE: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingLine {
    /// Without the ellipsis.
    pub verb: String,
    pub started_at: Instant,
    pub tokens_down: u64,
    pub activity: Activity,
    pub details: Vec<String>,
    /// When this is a few seconds old, the line fades toward the warning color to show the
    /// stream stalled.
    pub last_progress: Option<Instant>,
    /// For chat turns, as in Claude Code and Codex. A crew run keeps its line, since the live
    /// blocks carry the streaming text.
    pub hide_while_text_streams: bool,
    /// Starts the parentheses with the elapsed time. `Pausing… (esc again to stop now)`
    /// leaves it out and shows its words at once.
    pub timed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Activity {
    #[default]
    None,
    Thinking {
        since: Instant,
    },
    RunningTool {
        since: Instant,
    },
    /// Between thinking spells.
    Thought {
        lasted: Duration,
    },
    Text(String),
}

impl Activity {
    pub fn wording(&self, now: Instant) -> Option<String> {
        match self {
            Activity::None => None,
            Activity::Thinking { since } => {
                let secs = now.saturating_duration_since(*since).as_secs();
                let words = match secs {
                    0..10 => "thinking",
                    10..20 => "still thinking",
                    20..30 => "thinking more",
                    30..45 => "thinking some more",
                    _ => "deep in thought",
                };
                Some(words.to_owned())
            }
            Activity::RunningTool { since } => {
                let elapsed = now.saturating_duration_since(*since);
                Some(format!("running tool for {}", format::duration(elapsed)))
            }
            Activity::Thought { lasted } => {
                Some(format!("thought for {}", format::duration(*lasted)))
            }
            Activity::Text(text) => {
                let text = sanitize_line(text);
                (!text.trim().is_empty()).then(|| text.into_owned())
            }
        }
    }

    fn since(&self) -> Option<Instant> {
        match self {
            Activity::Thinking { since } | Activity::RunningTool { since } => Some(*since),
            Activity::None | Activity::Text(_) | Activity::Thought { .. } => None,
        }
    }
}

impl WorkingLine {
    pub fn new(verb: impl Into<String>, started_at: Instant) -> Self {
        Self {
            verb: verb.into(),
            started_at,
            tokens_down: 0,
            activity: Activity::None,
            details: Vec::new(),
            last_progress: None,
            hide_while_text_streams: false,
            timed: true,
        }
    }

    pub fn shows(&self, text_streaming: bool) -> bool {
        let busy_elsewhere = matches!(
            self.activity,
            Activity::Thinking { .. } | Activity::RunningTool { .. }
        );
        !(self.hide_while_text_streams && text_streaming && !busy_elsewhere)
    }

    /// On a narrow terminal whole parts give way from the right, so a number is never cut.
    pub fn line(&self, ctx: &RenderCtx<'_>) -> Line<'static> {
        let theme = ctx.theme;
        let glyphs = &theme.glyphs;
        let clock = ctx.clock;
        let still = theme.reduced_motion || glyphs.spinner.is_empty();
        let glyph = if still {
            glyphs.turn_end
        } else {
            let (frame, _) = spinner_frame(clock.since(clock.start), glyphs.spinner.len());
            glyphs.spinner[frame]
        };

        let stall = self.stall(clock.now);
        let lead = lead_style(theme, stall);
        let mut spans = vec![Span::styled(format!("{glyph} "), lead)];

        let verb = format!("{}{}", sanitize_line(&self.verb), glyphs.ellipsis);
        if still || stall > 0.0 {
            spans.push(Span::styled(verb, lead));
        } else {
            let step = clock.frame(SPINNER_PERIOD, usize::MAX);
            spans.extend(shimmer(&verb, step, lead, theme.style(Role::Shimmer)));
        }

        let dim = theme.style(Role::Dim);
        let mut parts = vec![Part::new(spans, KEEP)];
        if !self.timed || clock.now.saturating_duration_since(self.started_at) >= STATUS_AFTER {
            // The rightmost part gives way first and the elapsed time last.
            let words = self.parts(clock.now, theme);
            let count = words.len();
            for (i, word) in words.into_iter().enumerate() {
                let joiner = if i == 0 { " (" } else { " · " };
                let rank = (count - i) as u16;
                parts.push(Part::joined(
                    Span::styled(joiner, dim),
                    vec![Span::styled(word, dim)],
                    rank.saturating_sub(1),
                ));
            }
        }
        // One column stays free for the closing parenthesis.
        let width = usize::from(ctx.width);
        let mut line = fit_parts(Vec::new(), &parts, width.saturating_sub(1));
        if line.spans.len() > parts[0].spans.len() {
            line.spans.push(Span::styled(")", dim));
        }
        line
    }

    pub fn next_change(&self, clock: &Clock, theme: &Theme) -> Option<Instant> {
        let now = clock.now;
        let mut next = next_second(self.started_at, now);
        if let Some(since) = self.activity.since() {
            next = next.min(next_second(since, now));
        }
        if let Some(progress) = self.last_progress {
            let stalls_at = progress + STALL_AFTER;
            if stalls_at > now {
                next = next.min(stalls_at);
            }
        }
        if !theme.reduced_motion {
            // The shimmer steps every period, and the spinner holds at the ends of its sweep.
            let frames = theme.glyphs.spinner.len();
            let (_, left) = spinner_frame(clock.since(clock.start), frames);
            next = next
                .min(clock.next_frame_at(SPINNER_PERIOD))
                .min(now + left);
        }
        Some(next)
    }

    fn parts(&self, now: Instant, theme: &Theme) -> Vec<String> {
        let mut parts = Vec::new();
        if self.timed {
            parts.push(format::duration(
                now.saturating_duration_since(self.started_at),
            ));
        }
        if self.tokens_down > 0 {
            parts.push(format!(
                "{} {} tokens",
                theme.glyphs.tokens,
                format::tokens(self.tokens_down)
            ));
        }
        parts.extend(self.activity.wording(now));
        parts.extend(
            self.details
                .iter()
                .map(|detail| sanitize_line(detail).trim().to_owned())
                .filter(|detail| !detail.is_empty()),
        );
        parts
    }

    /// How far the line has faded toward the warning color, fully at 1 and above.
    fn stall(&self, now: Instant) -> f32 {
        let Some(progress) = self.last_progress else {
            return 0.0;
        };
        let quiet = now.saturating_duration_since(progress);
        if quiet < STALL_AFTER {
            return 0.0;
        }
        (quiet - STALL_AFTER).as_secs_f32() / STALL_FADE.as_secs_f32()
    }
}

/// The frame at `elapsed` and how long until the next. The first frame and the middle one,
/// the two ends of the sweep, hold longer.
pub fn spinner_frame(elapsed: Duration, frames: usize) -> (usize, Duration) {
    if frames == 0 {
        return (0, SPINNER_PERIOD);
    }
    let len = |i: usize| {
        if i == 0 || i == frames / 2 {
            SPINNER_HOLD
        } else {
            SPINNER_PERIOD
        }
    };
    let cycle: u128 = (0..frames).map(|i| len(i).as_nanos()).sum();
    let mut into = elapsed.as_nanos() % cycle;
    for i in 0..frames {
        let this = len(i).as_nanos();
        if into < this {
            let left = u64::try_from(this - into).unwrap_or(u64::MAX);
            return (i, Duration::from_nanos(left));
        }
        into -= this;
    }
    (0, SPINNER_PERIOD)
}

/// Truecolor blends over the fade. Other levels, and reduced motion, switch at once.
fn lead_style(theme: &Theme, stall: f32) -> Style {
    let accent = theme.style(Role::Accent);
    if stall <= 0.0 {
        return accent;
    }
    let warning = theme.style(Role::Warning);
    let blend = theme.level == ColorLevel::TrueColor && !theme.reduced_motion;
    match (accent.fg, warning.fg) {
        (Some(Color::Rgb(ar, ag, ab)), Some(Color::Rgb(wr, wg, wb))) if blend => {
            let t = stall.min(1.0);
            let mix =
                |a: u8, w: u8| (f32::from(a) + (f32::from(w) - f32::from(a)) * t).round() as u8;
            accent.fg(Color::Rgb(mix(ar, wr), mix(ag, wg), mix(ab, wb)))
        }
        _ => warning,
    }
}

fn shimmer(text: &str, step: usize, base: Style, bright: Style) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let cycle = chars.len() + SHIMMER_WIDTH + SHIMMER_PAUSE;
    let center = (step % cycle) as isize - SHIMMER_WIDTH as isize / 2 - 1;
    let lit = |i: usize| (i as isize - center).unsigned_abs() <= SHIMMER_WIDTH / 2;

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_lit = false;
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && lit(i) != run_lit {
            spans.push(Span::styled(
                std::mem::take(&mut run),
                if run_lit { bright } else { base },
            ));
        }
        run_lit = lit(i);
        run.push(*c);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, if run_lit { bright } else { base }));
    }
    spans
}

fn next_second(start: Instant, now: Instant) -> Instant {
    let whole = now.saturating_duration_since(start).as_secs();
    start + Duration::from_secs(whole + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::plain_text;
    use crate::theme::Glyphs;

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    fn text(line: &WorkingLine, theme: &Theme, start: Instant, now: Instant, width: u16) -> String {
        let ctx = RenderCtx::new(width, theme, Clock::new(start, now));
        plain_text(&[line.line(&ctx)])
    }

    #[test]
    fn reads_like_the_reference() {
        let theme = theme();
        let t0 = Instant::now();
        let mut line = WorkingLine::new("Rowing", t0);
        line.tokens_down = 48_200;
        line.activity = Activity::Thinking { since: t0 };
        line.details = vec!["3 agents".into(), "$0.21".into()];
        let now = t0 + Duration::from_secs(252);
        assert_eq!(
            text(&line, &theme, t0, now, 100),
            "✻ Rowing… (4m 12s · ↓ 48.2k tokens · deep in thought · 3 agents · $0.21)"
        );
        assert_eq!(
            text(&line, &theme, t0, now, 60),
            "✻ Rowing… (4m 12s · ↓ 48.2k tokens · deep in thought)"
        );
        assert_eq!(
            text(&line, &theme, t0, now, 40),
            "✻ Rowing… (4m 12s · ↓ 48.2k tokens)"
        );
        assert_eq!(text(&line, &theme, t0, now, 30), "✻ Rowing… (4m 12s)");
        assert_eq!(text(&line, &theme, t0, now, 12), "✻ Rowing…");
    }

    #[test]
    fn the_verb_shows_alone_for_the_first_second() {
        let theme = theme();
        let t0 = Instant::now();
        let mut line = WorkingLine::new("Rowing", t0);
        line.activity = Activity::Thinking { since: t0 };
        let at = |ms| text(&line, &theme, t0, t0 + Duration::from_millis(ms), 80);
        assert_eq!(at(500), "✢ Rowing…");
        assert_eq!(at(1_000), "✼ Rowing… (1s · thinking)");
        line.activity = Activity::Thought {
            lasted: Duration::from_secs(2),
        };
        assert_eq!(
            text(&line, &theme, t0, t0 + Duration::from_secs(3), 80),
            "✼ Rowing… (3s · thought for 2s)"
        );
    }

    #[test]
    fn a_chat_turn_steps_aside_while_text_streams() {
        let t0 = Instant::now();
        let mut line = WorkingLine::new("Rowing", t0);
        assert!(line.shows(true), "a run keeps its line");
        line.hide_while_text_streams = true;
        assert!(line.shows(false));
        assert!(!line.shows(true));
        line.activity = Activity::RunningTool { since: t0 };
        assert!(line.shows(true), "tool calls bring it back");
        line.activity = Activity::Thinking { since: t0 };
        assert!(line.shows(true), "so does thinking");
    }

    #[test]
    fn thinking_reads_differently_as_time_passes() {
        let t0 = Instant::now();
        let thinking = Activity::Thinking { since: t0 };
        let at = |secs| thinking.wording(t0 + Duration::from_secs(secs)).unwrap();
        assert_eq!(at(0), "thinking");
        assert_eq!(at(9), "thinking");
        assert_eq!(at(10), "still thinking");
        assert_eq!(at(20), "thinking more");
        assert_eq!(at(30), "thinking some more");
        assert_eq!(at(44), "thinking some more");
        assert_eq!(at(45), "deep in thought");
        let tool = Activity::RunningTool { since: t0 };
        assert_eq!(
            tool.wording(t0 + Duration::from_secs(3)).unwrap(),
            "running tool for 3s"
        );
        assert_eq!(Activity::None.wording(t0), None);
    }

    #[test]
    fn tokens_show_only_once_counted() {
        let theme = theme();
        let t0 = Instant::now();
        let line = WorkingLine::new("Charting", t0);
        assert_eq!(
            text(&line, &theme, t0, t0 + Duration::from_secs(3), 80),
            "✼ Charting… (3s)"
        );
    }

    #[test]
    fn the_spinner_turns_and_reduced_motion_holds_still() {
        let mut theme = theme();
        let t0 = Instant::now();
        let line = WorkingLine::new("Rowing", t0);
        let glyph_at = |theme: &Theme, ms| {
            text(&line, theme, t0, t0 + Duration::from_millis(ms), 80)
                .chars()
                .next()
                .unwrap()
        };
        assert_eq!(glyph_at(&theme, 0), '∙');
        assert_eq!(glyph_at(&theme, 400), '∙');
        assert_eq!(glyph_at(&theme, 500), '✢');
        assert_eq!(glyph_at(&theme, 600), '✲');
        assert_eq!(glyph_at(&theme, 950), '✼');
        assert_eq!(glyph_at(&theme, 1_350), '✼');
        assert_eq!(glyph_at(&theme, 1_400), '✻');
        assert_eq!(glyph_at(&theme, 1_860), '∙');
        theme.reduced_motion = true;
        assert_eq!(glyph_at(&theme, 130), '✻');
    }

    #[test]
    fn the_shimmer_sweeps_across_the_verb() {
        let theme = theme();
        let t0 = Instant::now();
        let line = WorkingLine::new("Rowing", t0);
        let shimmer = theme.style(Role::Shimmer);
        let lit_at = |step: u32| -> String {
            let ctx = RenderCtx::new(80, &theme, Clock::new(t0, t0 + SPINNER_PERIOD * step));
            line.line(&ctx)
                .spans
                .iter()
                .filter(|span| span.style == shimmer)
                .map(|span| span.content.to_string())
                .collect()
        };
        assert_eq!(lit_at(0), "");
        assert_eq!(lit_at(2), "Ro");
        assert_eq!(lit_at(4), "owi");
        assert_eq!(lit_at(8), "g…");
        assert_eq!(lit_at(12), "");
    }

    #[test]
    fn a_stall_fades_toward_the_warning_color() {
        let theme = theme();
        let t0 = Instant::now();
        let mut line = WorkingLine::new("Rowing", t0);
        line.last_progress = Some(t0);
        let glyph_fg = |theme: &Theme, secs: f32| {
            let now = t0 + Duration::from_secs_f32(secs);
            let ctx = RenderCtx::new(80, theme, Clock::new(t0, now));
            line.line(&ctx).spans[0].style.fg
        };
        let accent = theme.style(Role::Accent).fg;
        let warning = theme.style(Role::Warning).fg;
        assert_eq!(glyph_fg(&theme, 29.9), accent);
        let halfway = glyph_fg(&theme, 31.0);
        assert!(halfway != accent && halfway != warning, "{halfway:?}");
        assert_eq!(glyph_fg(&theme, 32.0), warning);

        let ansi = Theme::dark().with_level(ColorLevel::Ansi256);
        let accent = ansi.style(Role::Accent).fg;
        let warning = ansi.style(Role::Warning).fg;
        assert_eq!(glyph_fg(&ansi, 29.9), accent);
        assert_eq!(glyph_fg(&ansi, 30.1), warning);
    }

    #[test]
    fn next_change_follows_the_spinner_or_the_seconds() {
        let t0 = Instant::now();
        let line = WorkingLine::new("Rowing", t0);
        let clock = Clock::new(t0, t0 + Duration::from_millis(1_050));
        let mut theme = theme();
        assert_eq!(
            line.next_change(&clock, &theme),
            Some(t0 + Duration::from_millis(1_080))
        );
        theme.reduced_motion = true;
        assert_eq!(
            line.next_change(&clock, &theme),
            Some(t0 + Duration::from_secs(2))
        );
    }

    #[test]
    fn an_untimed_line_shows_its_words_at_once() {
        let theme = theme();
        let t0 = Instant::now();
        let mut line = WorkingLine::new("Pausing", t0);
        line.timed = false;
        line.details = vec!["esc again to stop now".into()];
        assert_eq!(
            text(&line, &theme, t0, t0, 80),
            "∙ Pausing… (esc again to stop now)"
        );
    }
}
