//! A quick look at the inline terminal and the bottom region on a real terminal. Typing chats
//! back, Ctrl+O opens the transcript overlay, Ctrl+G runs a stand-in editor, and Ctrl+C twice
//! quits.

use std::time::{Duration, Instant};

use argonauts_tui::app::{Flow, Host, StdoutWriter, Ui, UiAction, run};
use argonauts_tui::bottom::{Activity, AgentRow, AgentState, CardOption, CardPrompt, WorkingLine};
use argonauts_tui::cell::{Cell, HistoryCell};
use argonauts_tui::render::RenderCtx;
use argonauts_tui::text::{sanitize_line, wrap_text};
use argonauts_tui::theme::{ColorEnv, ColorLevel, Role, Theme};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

const AGENTS_AT: Duration = Duration::from_millis(2500);
const CARD_AT: Duration = Duration::from_millis(5000);

#[derive(Debug, Clone)]
struct Said {
    look: Look,
    text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Look {
    User,
    Answer,
    Dim,
    Answered,
}

impl Said {
    fn cell(look: Look, text: impl Into<String>) -> Cell {
        Cell::Custom(Box::new(Said {
            look,
            text: text.into(),
        }))
    }
}

impl HistoryCell for Said {
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let glyphs = &theme.glyphs;
        let width = usize::from(ctx.width);
        let text = sanitize_line(&self.text);
        let hang = [Span::raw("  ")];
        match self.look {
            Look::User => {
                let prompt = [Span::styled(
                    format!("{} ", glyphs.prompt),
                    theme.style(Role::Dim),
                )];
                wrap_text(&text, Style::new(), width, &prompt, &hang)
                    .into_iter()
                    .map(|line| line.style(theme.style(Role::UserBand)))
                    .collect()
            }
            Look::Answer => {
                let bullet = [Span::raw(format!("{} ", glyphs.bullet))];
                wrap_text(&text, Style::new(), width, &bullet, &hang)
            }
            Look::Dim => wrap_text(&text, theme.style(Role::Dim), width, &[], &[]),
            Look::Answered => {
                let diamond = [Span::styled(
                    format!("{} ", glyphs.card),
                    theme.style(Role::Warning),
                )];
                let bold = Style::new().add_modifier(Modifier::BOLD);
                wrap_text(&text, bold, width, &diamond, &hang)
            }
        }
    }
}

#[derive(Debug, Default)]
struct Smoke {
    started: Option<Instant>,
    played: usize,
}

impl Host<StdoutWriter> for Smoke {
    fn on_start(&mut self, ui: &mut Ui<StdoutWriter>, now: Instant) {
        self.started = Some(now);
        let model = ui.model_mut();
        model.title = "UI kit smoke test".into();
        model.bottom.composer.label = Some("solo".into());
        model.history.push(Said::cell(Look::User, "hi"));
        model.history.push(Said::cell(
            Look::Answer,
            "Hi! This is the UI kit's smoke test. Items like this one print once into \
             scrollback, and only the region below redraws.",
        ));
        model
            .history
            .push(Said::cell(Look::Dim, "✻ Sailed for 3s · done 10:42 AM"));

        let mut working = WorkingLine::new("Rowing", now);
        working.activity = Activity::Thinking { since: now };
        working.tokens_down = 3_600;
        working.last_progress = Some(now);
        model.bottom.working = Some(working);
    }

    fn next_deadline(&self) -> Option<Instant> {
        let started = self.started?;
        match self.played {
            0 => Some(started + AGENTS_AT),
            1 => Some(started + CARD_AT),
            _ => None,
        }
    }

    fn on_deadline(&mut self, ui: &mut Ui<StdoutWriter>, now: Instant) -> Flow {
        let model = ui.model_mut();
        match self.played {
            0 => {
                let mut rows = Vec::new();
                for (name, task, done) in [("claude-code", "card-002", 2), ("codex", "card-003", 1)]
                {
                    let mut row = AgentRow::new(name, task, AgentState::Working);
                    row.steps = Some((done, 5));
                    row.started_at = Some(now);
                    row.tokens_down = 12_900 * u64::from(done);
                    rows.push(row);
                }
                let mut waiting = AgentRow::new("agy", "review", AgentState::Waiting);
                waiting.note = Some("waiting".into());
                rows.push(waiting);
                model.bottom.agents.rows = rows;
                model.bottom.mode.background_agents = 3;
                if let Some(working) = &mut model.bottom.working {
                    working.details = vec!["build".into(), "3 agents".into()];
                    working.last_progress = Some(now);
                }
            }
            1 => {
                let mut card = CardPrompt::new(
                    1,
                    "Merge card-002 into main?",
                    "Merge card-002?",
                    vec![
                        CardOption::irreversible("Merge"),
                        CardOption::new("Open a pull request"),
                        CardOption::new("Keep the branch"),
                    ],
                );
                card.detail = vec!["3 files · +48 -6 · tests passed".into()];
                card.allow_note = true;
                model.bottom.card = Some(card);
            }
            _ => {}
        }
        self.played += 1;
        Flow::Continue
    }

    fn on_action(&mut self, ui: &mut Ui<StdoutWriter>, action: UiAction, _now: Instant) -> Flow {
        match action {
            UiAction::Quit => return Flow::Quit,
            UiAction::Submit { shown, .. } => {
                let history = &mut ui.model_mut().history;
                history.push(Said::cell(Look::User, shown));
                history.push(Said::cell(
                    Look::Answer,
                    "Noted. This smoke test only echoes.",
                ));
            }
            UiAction::Answer { option, note, .. } => {
                let note = note.map(|note| format!(" · {note}")).unwrap_or_default();
                let model = ui.model_mut();
                model.history.push(Said::cell(
                    Look::Answered,
                    format!("Answered option {}{note}", option + 1),
                ));
                model.bottom.working = None;
            }
            UiAction::Interrupt => {
                let model = ui.model_mut();
                model.bottom.working = None;
                model
                    .history
                    .push(Said::cell(Look::Dim, "  ⎿  Interrupted"));
            }
            UiAction::EditDraft => {
                let child = ui.with_restored(|| {
                    std::process::Command::new("sh")
                        .args([
                            "-c",
                            "printf 'editor stand-in has the terminal\\n'; sleep 1",
                        ])
                        .status()
                });
                let note = match child {
                    Ok(Ok(status)) if status.success() => "  ⎿  The editor stand-in finished",
                    _ => "  ⎿  The editor stand-in didn't run",
                };
                ui.model_mut().history.push(Said::cell(Look::Dim, note));
            }
            _ => {}
        }
        Flow::Continue
    }
}

fn main() {
    let env = ColorEnv::from_process();
    let mut theme = Theme::detect(&env);
    if std::env::args().any(|arg| arg == "--no-color") {
        theme = theme.with_level(ColorLevel::None);
    }
    if let Err(error) = run(&mut Smoke::default(), theme) {
        eprintln!("live_smoke: {error}");
        std::process::exit(1);
    }
}
