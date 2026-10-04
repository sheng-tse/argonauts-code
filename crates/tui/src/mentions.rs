//! The `@` list: workers in the run, cards, roles, follow numbers, and `all`, from the run on
//! screen. A name that matches more than one session asks which.

use argonauts_core::app::UiCommand;
use argonauts_core::events::card::CardState;
use argonauts_core::input::{MentionTarget, ParsedInput, parse_input};
use argonauts_core::names::{RoleName, WorkerRef};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::bottom::MenuItem;
use crate::state::{AppState, RunState, SessionState};
use crate::text::{sanitize_line, truncate_line};
use crate::theme::Role;
use crate::views::{View, ViewCtx, ViewOutcome, list};

/// Everyone you can mention in the attached run, in the list's order: workers, cards, roles,
/// follow numbers, then `all`. Empty with no run on screen, since a mention needs one.
pub fn candidates(state: &AppState) -> Vec<MenuItem> {
    let Some(run) = state.attached_run() else {
        return Vec::new();
    };
    let live: Vec<&SessionState> = run.sessions_in_order().filter(|s| s.is_running()).collect();
    let mut items = Vec::new();

    let mut workers: Vec<&str> = Vec::new();
    for session in &live {
        if !workers.contains(&session.worker.worker.as_str()) {
            workers.push(&session.worker.worker);
        }
    }
    for worker in workers {
        let sessions: Vec<&&SessionState> =
            live.iter().filter(|s| s.worker.worker == worker).collect();
        let description = match sessions.as_slice() {
            [one] => session_summary(one),
            many => format!(
                "{} sessions: {}",
                many.len(),
                many.iter()
                    .map(|s| session_place(s))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        items.push(MenuItem::new(worker, description).with_kind("agent"));
    }

    for card in run.cards.values().filter(|card| !finished(card.state)) {
        items.push(MenuItem::new(card.id.as_str(), card.title.clone()).with_kind("card"));
    }

    let mut roles: Vec<RoleName> = Vec::new();
    for session in &live {
        if !roles.contains(&session.role) {
            roles.push(session.role);
        }
    }
    for role in roles {
        let who: Vec<String> = live
            .iter()
            .filter(|s| s.role == role)
            .map(|s| worker_place(s))
            .collect();
        items.push(MenuItem::new(role.as_str(), who.join(", ")).with_kind("role"));
    }

    let mut numbered: Vec<&&SessionState> = live.iter().filter(|s| s.follow.is_some()).collect();
    numbered.sort_by_key(|s| s.follow);
    for session in numbered {
        if let Some(key) = session.follow {
            items.push(MenuItem::new(key.to_string(), worker_place(session)).with_kind("follow"));
        }
    }

    items.push(MenuItem::new("all", "Every agent in this run").with_kind("run"));
    items
}

fn finished(state: Option<CardState>) -> bool {
    matches!(state, Some(CardState::Integrated | CardState::Cancelled))
}

/// `owner on card-002`, or the role alone.
fn session_place(session: &SessionState) -> String {
    match &session.card {
        Some(card) => format!("{} on {card}", session.role.as_str()),
        None => session.role.as_str().to_owned(),
    }
}

/// `opus · owner on card-002`.
fn session_summary(session: &SessionState) -> String {
    match &session.worker.model {
        Some(model) => format!("{model} · {}", session_place(session)),
        None => session_place(session),
    }
}

/// `codex on card-003`, or the worker alone.
fn worker_place(session: &SessionState) -> String {
    match &session.card {
        Some(card) => format!("{} on {card}", session.worker),
        None => session.worker.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Resolved {
    One(MentionTarget),
    /// Several sessions answer to the name; a picker asks which.
    Ambiguous(Vec<(String, MentionTarget)>),
    Unknown,
}

/// `name` as typed after the `@`.
pub fn resolve(name: &str, state: &AppState) -> Resolved {
    match parse_input(&format!("@{name}")) {
        Ok(ParsedInput::Mention { target, .. }) => resolve_target(&target, state),
        _ => Resolved::Unknown,
    }
}

/// A worker name matches every session of that worker, so with more than one it's ambiguous:
/// the picker offers each session and all of them. A card or follow number must be in the run.
/// A role or a worker nobody runs yet is fine, since the message waits for its next turn.
pub fn resolve_target(target: &MentionTarget, state: &AppState) -> Resolved {
    let Some(run) = state.attached_run() else {
        return Resolved::Unknown;
    };
    match target {
        MentionTarget::Worker(worker) => resolve_worker(target, worker, run),
        MentionTarget::Card(card) => {
            let known = run.cards.contains_key(card)
                || run.sessions.values().any(|s| s.card.as_ref() == Some(card));
            if known {
                Resolved::One(target.clone())
            } else {
                Resolved::Unknown
            }
        }
        MentionTarget::Number(key) => {
            if run.sessions.values().any(|s| s.follow == Some(*key)) {
                Resolved::One(target.clone())
            } else {
                Resolved::Unknown
            }
        }
        MentionTarget::Role(_) | MentionTarget::All => Resolved::One(target.clone()),
    }
}

fn resolve_worker(target: &MentionTarget, worker: &WorkerRef, run: &RunState) -> Resolved {
    let sessions: Vec<&SessionState> = run
        .sessions_in_order()
        .filter(|s| s.is_running() && answers_to(worker, &s.worker))
        .collect();
    if sessions.len() < 2 {
        return Resolved::One(target.clone());
    }
    let mut choices = vec![(
        format!("All {} {worker} sessions", sessions.len()),
        target.clone(),
    )];
    for session in sessions {
        if let Some(target) = session_target(session) {
            choices.push((
                format!("{} · {}", session.worker, session_place(session)),
                target,
            ));
        }
    }
    if choices.len() < 2 {
        return Resolved::One(target.clone());
    }
    Resolved::Ambiguous(choices)
}

fn answers_to(name: &WorkerRef, worker: &WorkerRef) -> bool {
    name.worker == worker.worker && (name.model.is_none() || name.model == worker.model)
}

/// A target that reaches this one session: its follow key, or the card it owns.
fn session_target(session: &SessionState) -> Option<MentionTarget> {
    if let Some(key) = session.follow {
        return Some(MentionTarget::Number(key));
    }
    let card = session
        .card
        .clone()
        .filter(|_| session.role == RoleName::Owner)?;
    Some(MentionTarget::Card(card))
}

/// Asks which session a mention meant: numbered rows with a `❯` pointer, as a card asks.
#[derive(Debug, Clone, PartialEq)]
pub struct MentionPicker {
    /// As typed after the `@`.
    pub name: String,
    pub text: String,
    pub choices: Vec<(String, MentionTarget)>,
    pub selected: usize,
}

impl MentionPicker {
    pub fn new(
        name: impl Into<String>,
        text: impl Into<String>,
        choices: Vec<(String, MentionTarget)>,
    ) -> Self {
        Self {
            name: name.into(),
            text: text.into(),
            choices,
            selected: 0,
        }
    }

    pub fn lines(&self, ctx: &ViewCtx<'_>, width: u16) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let dim = theme.style(Role::Dim);
        let name = sanitize_line(&self.name).into_owned();
        let text = sanitize_line(&self.text).into_owned();
        let mut lines = vec![
            Line::styled(
                format!("@{name} matches more than one agent"),
                theme.style(Role::Card),
            ),
            Line::styled(format!("Who gets \"{text}\"?"), dim),
            Line::default(),
        ];
        for (index, (label, _)) in self.choices.iter().enumerate() {
            let pointer = if index == self.selected {
                theme.glyphs.prompt
            } else {
                " "
            };
            let style = if index == self.selected {
                theme.style(Role::Card)
            } else {
                theme.style(Role::Text)
            };
            let label = sanitize_line(label).into_owned();
            lines.push(Line::from(vec![
                Span::styled(format!("{pointer} {}. ", index + 1), style),
                Span::styled(label, style),
            ]));
        }
        lines.push(Line::default());
        lines.push(Line::styled("Enter to send · Esc to cancel", dim));
        lines
            .iter()
            .map(|line| truncate_line(line, usize::from(width)))
            .collect()
    }

    fn send(&self, index: usize) -> ViewOutcome {
        match self.choices.get(index) {
            Some((_, target)) => ViewOutcome::Send(UiCommand::Mention {
                target: target.clone(),
                text: self.text.clone(),
            }),
            None => ViewOutcome::Consumed,
        }
    }
}

impl View for MentionPicker {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        for (row, line) in self.lines(ctx, area.width).iter().enumerate() {
            let Ok(row) = u16::try_from(row) else { break };
            if row >= area.height {
                break;
            }
            buf.set_line(area.x, area.y + row, line, area.width);
        }
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let count = self.choices.len();
        if count == 0 {
            return ViewOutcome::Close;
        }
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.checked_sub(1).unwrap_or(count - 1);
                ViewOutcome::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1) % count;
                ViewOutcome::Consumed
            }
            KeyCode::Home => {
                self.selected = 0;
                ViewOutcome::Consumed
            }
            KeyCode::End => {
                self.selected = count - 1;
                ViewOutcome::Consumed
            }
            KeyCode::Enter => self.send(self.selected),
            KeyCode::Char(c @ '1'..='9') => {
                let index = usize::from(c as u8 - b'1');
                if index < count {
                    self.selected = index;
                    self.send(index)
                } else {
                    ViewOutcome::Consumed
                }
            }
            KeyCode::Esc => ViewOutcome::Close,
            _ => ViewOutcome::Ignored,
        }
    }
}

/// A run on screen with three live agents, two of them codex, for tests here and in the input
/// router.
#[cfg(test)]
pub(crate) mod fixture {
    use argonauts_core::app::AppMode;
    use argonauts_core::events::card::CardState;
    use argonauts_core::names::{RoleName, SessionKey, WorkerRef};
    use argonauts_core::{CardId, RunId};
    use time::OffsetDateTime;

    use crate::state::{AppState, CardRow, Doing, RunState, SessionState, SessionStatus};

    pub fn run_id() -> RunId {
        RunId::new("run-20260930-101500-3fa2").unwrap()
    }

    pub fn session(
        n: u32,
        worker: &str,
        model: Option<&str>,
        role: RoleName,
        card: Option<&str>,
        follow: Option<u8>,
    ) -> SessionState {
        SessionState {
            key: SessionKey::numbered(n),
            follow,
            worker: WorkerRef::new(worker, model.map(str::to_owned)),
            model_assumed: false,
            vendor: worker.to_owned(),
            role,
            step: None,
            card: card.map(|card| CardId::new(card).unwrap()),
            workflow_step: None,
            label: None,
            status: SessionStatus::Working,
            started: OffsetDateTime::UNIX_EPOCH,
            ended: None,
            last_event: OffsetDateTime::UNIX_EPOCH,
            tokens_in: 0,
            tokens_out: 0,
            cost_usd: None,
            context: None,
            context_window: None,
            rotations: 0,
            todos: Vec::new(),
            tools: Vec::new(),
            messages: Vec::new(),
            streaming: String::new(),
            diffs: Vec::new(),
            subagents: Vec::new(),
            workflows: Vec::new(),
            inputs: Vec::new(),
            queued: Vec::new(),
            retry: None,
            standing_in_for: None,
            end: None,
            unknown: Vec::new(),
            doing: Doing::Nothing,
            stalled: false,
            vendor_session: None,
        }
    }

    fn card(id: &str, title: &str, state: CardState) -> CardRow {
        CardRow {
            id: CardId::new(id).unwrap(),
            title: title.to_owned(),
            state: Some(state),
            owner: None,
            attempt: 1,
            diff: None,
            standing_in_for: None,
            debt: Vec::new(),
            checkpoints: 0,
            rotations: 0,
            last_gate: None,
            reviews: 0,
            waiting_on_vendor: None,
            item: None,
        }
    }

    pub fn working_state() -> AppState {
        let mut run = RunState::new(run_id());
        let sessions = [
            session(
                1,
                "claude-code",
                Some("opus"),
                RoleName::Owner,
                Some("card-002"),
                Some(1),
            ),
            session(2, "codex", None, RoleName::Owner, Some("card-003"), Some(2)),
            session(
                3,
                "codex",
                None,
                RoleName::Reviewer,
                Some("card-002"),
                Some(3),
            ),
        ];
        let mut done = session(4, "agy", None, RoleName::Critic, None, None);
        done.status = SessionStatus::Done;
        for session in sessions.into_iter().chain([done]) {
            run.session_order.push(session.key.clone());
            run.sessions.insert(session.key.clone(), session);
        }
        for card in [
            card("card-002", "Add the JSON writer", CardState::Running),
            card("card-003", "Fix the flaky export test", CardState::InReview),
            card("card-001", "Set up the crate", CardState::Integrated),
        ] {
            run.cards.insert(card.id.clone(), card);
        }
        let mut state = AppState::default();
        state.attached = Some(run_id());
        state.mode = Some(AppMode::Working { run: run_id() });
        state.runs.insert(run_id(), run);
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ambiguous_name_picker_takes_vim_keys_when_they_are_on() {
        let mut state = fixture::working_state();
        let Resolved::Ambiguous(choices) = resolve("codex", &state) else {
            panic!("codex answers to two sessions");
        };
        let last = choices.len() - 1;
        let theme = crate::theme::Theme::dark();
        let press = |picker: &mut MentionPicker, c: char, state: &AppState| {
            let ctx = ViewCtx {
                theme: &theme,
                state,
                now: std::time::Instant::now(),
            };
            picker.handle_key(KeyEvent::from(KeyCode::Char(c)), &ctx)
        };

        // `j` and `k` always moved here; `h`, `l`, `g`, and `G` are new with vim keys.
        state.ui = list::keys_state(false).ui;
        let mut picker = MentionPicker::new("codex", "rebase first", choices.clone());
        press(&mut picker, 'j', &state);
        assert_eq!(picker.selected, 1);
        for c in ['h', 'l', 'g', 'G'] {
            assert!(
                matches!(press(&mut picker, c, &state), ViewOutcome::Ignored),
                "{c}"
            );
        }

        state.ui = list::keys_state(true).ui;
        let mut picker = MentionPicker::new("codex", "rebase first", choices);
        press(&mut picker, 'G', &state);
        assert_eq!(picker.selected, last);
        press(&mut picker, 'g', &state);
        assert_eq!(picker.selected, 0);
        press(&mut picker, 'j', &state);
        assert!(matches!(
            press(&mut picker, 'l', &state),
            ViewOutcome::Send(UiCommand::Mention {
                target: MentionTarget::Number(2),
                ..
            })
        ));
        assert!(matches!(
            press(&mut picker, 'h', &state),
            ViewOutcome::Close
        ));
    }

    #[test]
    fn mention_candidates_from_run() {
        let state = fixture::working_state();
        let rows: Vec<(String, String, String)> = candidates(&state)
            .into_iter()
            .map(|item| (item.name, item.description, item.kind.unwrap_or_default()))
            .collect();
        let row = |name: &str, description: &str, kind: &str| {
            (name.to_owned(), description.to_owned(), kind.to_owned())
        };
        assert_eq!(
            rows,
            [
                row("claude-code", "opus · owner on card-002", "agent"),
                row(
                    "codex",
                    "2 sessions: owner on card-003, reviewer on card-002",
                    "agent"
                ),
                row("card-002", "Add the JSON writer", "card"),
                row("card-003", "Fix the flaky export test", "card"),
                row(
                    "owner",
                    "claude-code/opus on card-002, codex on card-003",
                    "role"
                ),
                row("reviewer", "codex on card-002", "role"),
                row("1", "claude-code/opus on card-002", "follow"),
                row("2", "codex on card-003", "follow"),
                row("3", "codex on card-002", "follow"),
                row("all", "Every agent in this run", "run"),
            ]
        );
        assert!(candidates(&AppState::default()).is_empty());
    }

    #[test]
    fn ambiguous_mention_lists_sessions() {
        let state = fixture::working_state();
        let codex = WorkerRef::new("codex", None);
        let Resolved::Ambiguous(choices) = resolve("codex", &state) else {
            panic!("codex runs twice");
        };
        assert_eq!(
            choices,
            [
                (
                    "All 2 codex sessions".to_owned(),
                    MentionTarget::Worker(codex)
                ),
                (
                    "codex · owner on card-003".to_owned(),
                    MentionTarget::Number(2)
                ),
                (
                    "codex · reviewer on card-002".to_owned(),
                    MentionTarget::Number(3)
                ),
            ]
        );
        assert_eq!(
            resolve("claude-code", &state),
            Resolved::One(MentionTarget::Worker(WorkerRef::new("claude-code", None)))
        );
        assert_eq!(resolve("9", &state), Resolved::Unknown);
        assert_eq!(resolve("card-404", &state), Resolved::Unknown);
        assert!(matches!(resolve("reviewer", &state), Resolved::One(_)));

        let theme = crate::theme::Theme::dark();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: std::time::Instant::now(),
        };
        let mut picker = MentionPicker::new("codex", "rebase first", choices);
        let shown: Vec<String> = picker
            .lines(&ctx, 60)
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(shown[0], "@codex matches more than one agent");
        assert!(shown[3].ends_with("1. All 2 codex sessions"), "{shown:?}");
        assert!(
            shown[5].ends_with("3. codex · reviewer on card-002"),
            "{shown:?}"
        );
        picker.handle_key(KeyEvent::from(KeyCode::Down), &ctx);
        let ViewOutcome::Send(command) = picker.handle_key(KeyEvent::from(KeyCode::Enter), &ctx)
        else {
            panic!("Enter sends");
        };
        assert_eq!(
            command,
            UiCommand::Mention {
                target: MentionTarget::Number(2),
                text: "rebase first".into(),
            }
        );
        assert!(matches!(
            picker.handle_key(KeyEvent::from(KeyCode::Char('3')), &ctx),
            ViewOutcome::Send(UiCommand::Mention {
                target: MentionTarget::Number(3),
                ..
            })
        ));
        assert!(matches!(
            picker.handle_key(KeyEvent::from(KeyCode::Esc), &ctx),
            ViewOutcome::Close
        ));
    }
}
