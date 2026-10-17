use std::time::{Duration, Instant};

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::format;
use crate::render::{Clock, RenderCtx};
use crate::text::{KEEP, Part, fit_parts, sanitize_line, truncate_line};
use crate::theme::{ColorLevel, Role, Theme};

/// How long each spinner frame shows, except at the two ends of its sweep.
pub const SPINNER_PERIOD: Duration = Duration::from_millis(120);

/// How long the spinner holds at `·` and at `✽`, the two ends of its sweep, so a full cycle
/// takes about two seconds, as Claude Code's does.
pub const SPINNER_HOLD: Duration = Duration::from_millis(450);

/// The part in parentheses joins after this long.
pub const STATUS_AFTER: Duration = Duration::from_secs(1);

/// How long the fade takes once Argonauts Code itself says the stream stalled.
const STALL_FADE: Duration = Duration::from_secs(2);

/// How long the stream may go without a token before the line starts toward red, and how long
/// one spell of thinking lasts before it starts toward amber. Claude Code uses the same ten
/// seconds for both.
pub const QUIET_AFTER: Duration = Duration::from_secs(10);

/// How long either fade takes, so the line is fully red twenty seconds after the last token.
pub const QUIET_FADE: Duration = Duration::from_secs(10);

/// The band's width in cells, and the steps it rests off the verb between sweeps.
const SHIMMER_WIDTH: usize = 3;
const SHIMMER_PAUSE: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingLine {
    /// Without the ellipsis.
    pub verb: String,
    /// The task in hand, from the CLI's own task list. It takes the verb's place.
    pub task: Option<String>,
    /// The first task still waiting on that list, drawn under the line as `⎿ Next: …`.
    pub next: Option<String>,
    /// With `ui.tips`, a tip for this stretch of work, drawn as `⎿ Tip: …` where no next task
    /// takes the place.
    pub tip: Option<&'static str>,
    pub started_at: Instant,
    pub tokens_down: u64,
    pub activity: Activity,
    /// Whether the parentheses say what the agent is doing. Several agents at once leave it
    /// out, since the words would jump from one to the next, while what they do between them
    /// still decides the line's color.
    pub says_activity: bool,
    /// The effort the model thinks with, as in `thinking with xhigh effort`.
    pub effort: Option<String>,
    pub details: Vec<String>,
    /// When the agent was last heard from, for a line that follows one. Ten seconds on with
    /// no tool running and no thinking, the line starts toward red, and the next token brings
    /// it back.
    pub heard_at: Option<Instant>,
    /// What the agent had sent when it was last heard from, so a sync that brings nothing new
    /// leaves `heard_at` alone.
    pub pulse: u64,
    /// When Argonauts Code said the stream stalled. The line goes red from then until the
    /// agent is heard from again.
    pub stalled_since: Option<Instant>,
    /// For conversation turns, as in Claude Code and Codex. A crew run keeps its line, since the
    /// live blocks carry the streaming text.
    pub hide_while_text_streams: bool,
    /// Starts the parentheses with the elapsed time. `Pausing… (esc again to stop now)`
    /// leaves it out and shows its words at once.
    pub timed: bool,
    /// Esc stops what this line shows: a conversation turn, or a run that's pausing. A run's agents
    /// keep going past Esc, so the bottom line makes no such offer for them.
    pub esc_stops: bool,
    /// The word this stretch of work was given, kept while `verb` says what the agent writes
    /// instead, such as a workflow's script.
    pub usual_verb: Option<String>,
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
    /// `effort` joins the thinking words, as in `thinking with xhigh effort`.
    pub fn wording(&self, now: Instant, effort: Option<&str>) -> Option<String> {
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
                let effort = effort.map(str::trim).filter(|effort| !effort.is_empty());
                Some(match effort {
                    Some(effort) => format!("{words} with {effort} effort"),
                    None => words.to_owned(),
                })
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
            task: None,
            next: None,
            tip: None,
            started_at,
            tokens_down: 0,
            activity: Activity::None,
            says_activity: true,
            effort: None,
            details: Vec::new(),
            heard_at: None,
            pulse: 0,
            stalled_since: None,
            hide_while_text_streams: false,
            timed: true,
            esc_stops: true,
            usual_verb: None,
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

        let fade = self.fade(clock.now);
        let style = verb_style(theme, fade);
        let mut spans = vec![Span::styled(format!("{glyph} "), style)];

        let words = self.task.as_deref().unwrap_or(&self.verb);
        let verb = format!("{}{}", sanitize_line(words).trim(), glyphs.ellipsis);
        if still || fade.is_some() {
            spans.push(Span::styled(verb, style));
        } else {
            let step = clock.frame(SPINNER_PERIOD, usize::MAX);
            spans.extend(shimmer(&verb, step, style, theme.style(Role::Shimmer)));
        }

        let dim = theme.style(Role::Dim);
        let mut parts = vec![Part::new(spans, KEEP)];
        if !self.timed || clock.now.saturating_duration_since(self.started_at) >= STATUS_AFTER {
            // The rightmost part gives way first and the elapsed time last.
            let words = self.parts(clock.now, theme);
            let count = words.len();
            // A long think colors its own words too, as Claude Code's does.
            let thinking = match fade {
                Some(Fade::LongThink(_)) => self.activity_words(clock.now),
                _ => None,
            };
            for (i, word) in words.into_iter().enumerate() {
                let joiner = if i == 0 { " (" } else { " · " };
                let rank = (count - i) as u16;
                let word_style = if thinking.as_deref() == Some(word.as_str()) {
                    style
                } else {
                    dim
                };
                parts.push(Part::joined(
                    Span::styled(joiner, dim),
                    vec![Span::styled(word, word_style)],
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

    /// The line, and under it the task that comes next when the CLI's list names one, or else
    /// the stretch's tip.
    pub fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let mut lines = vec![self.line(ctx)];
        lines.extend(self.next_line(ctx));
        lines
    }

    fn next_line(&self, ctx: &RenderCtx<'_>) -> Option<Line<'static>> {
        let next = self
            .next
            .as_deref()
            .map(|next| sanitize_line(next).trim().to_owned())
            .filter(|next| !next.is_empty());
        let words = match (next, self.tip) {
            (Some(next), _) => format!("Next: {next}"),
            (None, Some(tip)) => format!("Tip: {tip}"),
            (None, None) => return None,
        };
        let dim = ctx.theme.style(Role::Dim);
        let line = Line::from(vec![
            Span::styled(format!("  {} ", ctx.theme.glyphs.result), dim),
            Span::styled(words, dim),
        ]);
        Some(truncate_line(&line, usize::from(ctx.width)))
    }

    fn effort(&self) -> Option<&str> {
        self.effort.as_deref()
    }

    fn activity_words(&self, now: Instant) -> Option<String> {
        self.says_activity
            .then(|| self.activity.wording(now, self.effort()))
            .flatten()
    }

    pub fn next_change(&self, clock: &Clock, theme: &Theme) -> Option<Instant> {
        let now = clock.now;
        let mut next = next_second(self.started_at, now);
        if let Some(since) = self.activity.since() {
            next = next.min(next_second(since, now));
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
        parts.extend(self.activity_words(now));
        parts.extend(
            self.details
                .iter()
                .map(|detail| sanitize_line(detail).trim().to_owned())
                .filter(|detail| !detail.is_empty()),
        );
        parts
    }

    /// Which color the line is on its way to, and how far along it is.
    ///
    /// A stream Argonauts Code called stalled goes red whatever else is happening. Otherwise a
    /// running tool keeps the line as it is, a long spell of thinking takes it toward amber,
    /// and silence takes it toward red.
    fn fade(&self, now: Instant) -> Option<Fade> {
        if let Some(since) = self.stalled_since {
            let along = now.saturating_duration_since(since).as_secs_f32();
            return Some(Fade::Quiet(along / STALL_FADE.as_secs_f32()));
        }
        match self.activity {
            Activity::RunningTool { .. } => None,
            Activity::Thinking { since } => along_fade(since, now).map(Fade::LongThink),
            Activity::None | Activity::Thought { .. } | Activity::Text(_) => {
                along_fade(self.heard_at?, now).map(Fade::Quiet)
            }
        }
    }
}

/// A color the line fades toward, with how far it has come: 0 at the start, 1 once there.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Fade {
    /// No token for a while: toward red.
    Quiet(f32),
    /// One spell of thinking has run long: toward amber.
    LongThink(f32),
}

/// How far a fade that starts `QUIET_AFTER` past `since` has come, once it has started.
fn along_fade(since: Instant, now: Instant) -> Option<f32> {
    let past = now
        .saturating_duration_since(since)
        .checked_sub(QUIET_AFTER)?;
    (!past.is_zero()).then(|| (past.as_secs_f32() / QUIET_FADE.as_secs_f32()).min(1.0))
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

/// Truecolor blends over the fade. Other levels, and reduced motion, switch halfway.
fn verb_style(theme: &Theme, fade: Option<Fade>) -> Style {
    let accent = theme.style(Role::Accent);
    let (toward, along) = match fade {
        None => return accent,
        Some(Fade::Quiet(along)) => (theme.style(Role::Danger), along),
        Some(Fade::LongThink(along)) => (theme.style(Role::Warn), along),
    };
    let blend = theme.level == ColorLevel::TrueColor && !theme.reduced_motion;
    match (accent.fg, toward.fg) {
        (Some(Color::Rgb(ar, ag, ab)), Some(Color::Rgb(tr, tg, tb))) if blend => {
            let t = along.clamp(0.0, 1.0);
            let mix =
                |a: u8, to: u8| (f32::from(a) + (f32::from(to) - f32::from(a)) * t).round() as u8;
            accent.fg(Color::Rgb(mix(ar, tr), mix(ag, tg), mix(ab, tb)))
        }
        _ if along >= 0.5 => toward,
        _ => accent,
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
    fn a_conversation_turn_steps_aside_while_text_streams() {
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
        let at = |secs| {
            thinking
                .wording(t0 + Duration::from_secs(secs), None)
                .unwrap()
        };
        assert_eq!(at(0), "thinking");
        assert_eq!(at(9), "thinking");
        assert_eq!(at(10), "still thinking");
        assert_eq!(at(20), "thinking more");
        assert_eq!(at(30), "thinking some more");
        assert_eq!(at(44), "thinking some more");
        assert_eq!(at(45), "deep in thought");
        let tool = Activity::RunningTool { since: t0 };
        assert_eq!(
            tool.wording(t0 + Duration::from_secs(3), Some("high"))
                .unwrap(),
            "running tool for 3s"
        );
        assert_eq!(Activity::None.wording(t0, None), None);
        let with_effort = |secs| {
            thinking
                .wording(t0 + Duration::from_secs(secs), Some("xhigh"))
                .unwrap()
        };
        assert_eq!(with_effort(0), "thinking with xhigh effort");
        assert_eq!(with_effort(12), "still thinking with xhigh effort");
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

    fn glyph_fg(line: &WorkingLine, theme: &Theme, t0: Instant, secs: f32) -> Option<Color> {
        let now = t0 + Duration::from_secs_f32(secs);
        let ctx = RenderCtx::new(80, theme, Clock::new(t0, now));
        line.line(&ctx).spans[0].style.fg
    }

    #[test]
    fn a_reported_stall_fades_to_red() {
        let theme = theme();
        let t0 = Instant::now();
        let mut line = WorkingLine::new("Rowing", t0);
        let accent = theme.style(Role::Accent).fg;
        let red = theme.style(Role::Danger).fg;
        // A line that follows no agent never fades by itself.
        assert_eq!(glyph_fg(&line, &theme, t0, 600.0), accent);

        line.stalled_since = Some(t0 + Duration::from_secs(40));
        assert_eq!(glyph_fg(&line, &theme, t0, 40.0), accent);
        let halfway = glyph_fg(&line, &theme, t0, 41.0);
        assert!(halfway != accent && halfway != red, "{halfway:?}");
        assert_eq!(glyph_fg(&line, &theme, t0, 42.0), red);
        // Argonauts Code's own report counts through a tool call too.
        line.activity = Activity::RunningTool { since: t0 };
        assert_eq!(glyph_fg(&line, &theme, t0, 42.0), red);

        let ansi = Theme::dark().with_level(ColorLevel::Ansi256);
        assert_eq!(
            glyph_fg(&line, &ansi, t0, 42.0),
            ansi.style(Role::Danger).fg
        );
        line.stalled_since = None;
        assert_eq!(
            glyph_fg(&line, &ansi, t0, 600.0),
            ansi.style(Role::Accent).fg
        );
    }

    #[test]
    fn ten_seconds_without_a_token_starts_the_fade_to_red_and_twenty_ends_it() {
        let theme = theme();
        let t0 = Instant::now();
        let mut line = WorkingLine::new("Rowing", t0);
        line.heard_at = Some(t0 + Duration::from_secs(30));
        let accent = theme.style(Role::Accent).fg;
        let red = theme.style(Role::Danger).fg;
        assert_eq!(glyph_fg(&line, &theme, t0, 39.0), accent);
        assert_eq!(glyph_fg(&line, &theme, t0, 40.0), accent);
        let halfway = glyph_fg(&line, &theme, t0, 45.0);
        assert!(halfway != accent && halfway != red, "{halfway:?}");
        assert_eq!(glyph_fg(&line, &theme, t0, 50.0), red);
        assert_eq!(glyph_fg(&line, &theme, t0, 300.0), red);
        // The verb goes with the glyph, and holds still rather than shimmer.
        let ctx = RenderCtx::new(80, &theme, Clock::new(t0, t0 + Duration::from_secs(50)));
        let drawn = line.line(&ctx);
        assert_eq!(drawn.spans[1].content, "Rowing…");
        assert_eq!(drawn.spans[1].style.fg, red);

        // The next token brings the color back at once.
        line.heard_at = Some(t0 + Duration::from_secs(50));
        assert_eq!(glyph_fg(&line, &theme, t0, 50.0), accent);

        // A terminal that can't blend switches halfway.
        let ansi = Theme::dark().with_level(ColorLevel::Ansi256);
        line.heard_at = Some(t0);
        assert_eq!(
            glyph_fg(&line, &ansi, t0, 14.0),
            ansi.style(Role::Accent).fg
        );
        assert_eq!(
            glyph_fg(&line, &ansi, t0, 15.0),
            ansi.style(Role::Danger).fg
        );
    }

    #[test]
    fn a_running_tool_never_reads_as_quiet_and_a_long_think_goes_amber() {
        let theme = theme();
        let t0 = Instant::now();
        let mut line = WorkingLine::new("Rowing", t0);
        line.heard_at = Some(t0);
        let accent = theme.style(Role::Accent).fg;
        let amber = theme.style(Role::Warn).fg;

        line.activity = Activity::RunningTool { since: t0 };
        assert_eq!(glyph_fg(&line, &theme, t0, 600.0), accent);

        line.activity = Activity::Thinking { since: t0 };
        line.effort = Some("xhigh".into());
        assert_eq!(glyph_fg(&line, &theme, t0, 10.0), accent);
        let halfway = glyph_fg(&line, &theme, t0, 15.0);
        assert!(halfway != accent && halfway != amber, "{halfway:?}");
        assert_eq!(glyph_fg(&line, &theme, t0, 20.0), amber);
        assert_eq!(glyph_fg(&line, &theme, t0, 600.0), amber);
        // Its own words take the color with it; the time beside them stays dim.
        let ctx = RenderCtx::new(80, &theme, Clock::new(t0, t0 + Duration::from_secs(25)));
        let drawn = line.line(&ctx);
        let words = drawn
            .spans
            .iter()
            .find(|span| span.content == "thinking more with xhigh effort")
            .expect("the thinking words");
        assert_eq!(words.style.fg, amber);
        let time = drawn.spans.iter().find(|span| span.content == "25s");
        assert_eq!(time.unwrap().style, theme.style(Role::Dim));
    }

    #[test]
    fn the_task_in_hand_takes_the_verbs_place_and_the_next_one_sits_under_it() {
        let theme = theme();
        let t0 = Instant::now();
        let mut line = WorkingLine::new("Rowing", t0);
        line.task = Some("Add the due date column".into());
        line.next = Some("Run the tests".into());
        let ctx = RenderCtx::new(60, &theme, Clock::new(t0, t0 + Duration::from_secs(3)));
        assert_eq!(
            plain_text(&line.lines(&ctx)),
            "✼ Add the due date column… (3s)\n  ⎿ Next: Run the tests"
        );
        let next = &line.lines(&ctx)[1];
        assert!(
            next.spans
                .iter()
                .all(|span| span.style == theme.style(Role::Dim))
        );

        // With nothing waiting there's no second row, and with no task the verb is back.
        line.next = None;
        line.task = None;
        assert_eq!(plain_text(&line.lines(&ctx)), "✼ Rowing… (3s)");

        // A long task is cut at the screen's edge rather than wrapped.
        line.next = Some("x".repeat(100));
        let narrow = RenderCtx::new(20, &theme, Clock::new(t0, t0));
        let rows = line.lines(&narrow);
        assert_eq!(rows.len(), 2);
        assert!(crate::text::spans_width(&rows[1].spans) <= 20);
    }

    /// A stretch's tip sits where the next task would, dim, and a next task wins the row.
    #[test]
    fn a_tip_sits_under_the_line_unless_a_next_task_does() {
        let theme = theme();
        let t0 = Instant::now();
        let mut line = WorkingLine::new("Rowing", t0);
        line.tip = Some("Ctrl+O shows the whole transcript");
        let ctx = RenderCtx::new(60, &theme, Clock::new(t0, t0 + Duration::from_secs(3)));
        assert_eq!(
            plain_text(&line.lines(&ctx)),
            "✼ Rowing… (3s)\n  ⎿ Tip: Ctrl+O shows the whole transcript"
        );
        assert!(
            line.lines(&ctx)[1]
                .spans
                .iter()
                .all(|span| span.style == theme.style(Role::Dim))
        );
        line.next = Some("Run the tests".into());
        assert_eq!(
            plain_text(&line.lines(&ctx)),
            "✼ Rowing… (3s)\n  ⎿ Next: Run the tests"
        );
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
