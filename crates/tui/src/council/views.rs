//! The council's screens of their own: a thread, and a seat's own session. Each keeps a line you
//! type into at the bottom; what you write goes to every seat as a reply in the thread, or to the
//! one seat in private. In a seat's session ↑ picks a private message of yours and `s` shares it
//! with every seat, and `o` carries the session on in its CLI.

use argonauts_core::RunId;
use argonauts_core::app::{UiCommand, ViewData, ViewRequest};
use argonauts_core::cli_sessions::CliSessionRef;
use argonauts_core::events::council::CouncilAction;
use argonauts_core::names::{MessageId, SeatId, SessionKey};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::faces::Faces;
use super::session_view::{SeatSession, draw_lines};
use super::thread::Thread;
use crate::theme::Role;
use crate::views::{View, ViewCtx, ViewOutcome};

/// Something you say to the council, or to one seat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CouncilSay {
    /// To every seat, as a reply when `reply_to` names a message. The seats it `mentions`
    /// answer first.
    ToCouncil {
        text: String,
        reply_to: Option<MessageId>,
        mentions: Vec<SeatId>,
    },
    /// To one seat, in its own session, never forwarded.
    Private {
        seat: SeatId,
        run: RunId,
        text: String,
    },
    /// A private message of yours, shared with every seat.
    Share { message: MessageId },
    /// The seat carries on in its own CLI and sits out the talk meanwhile. `None` for a
    /// session whose CLI keeps no id to open it by.
    CarryOn {
        seat: SeatId,
        run: RunId,
        session: Option<CliSessionRef>,
    },
}

impl CouncilSay {
    /// The command that carries it to the controller, the one place a council's request turns
    /// into one. A seat whose session has no id to open it by can't be carried on from here,
    /// so it gets `None`.
    pub fn command(&self) -> Option<UiCommand> {
        let (run, action) = match self.clone() {
            CouncilSay::ToCouncil {
                text,
                reply_to,
                mentions,
            } => {
                let action = CouncilAction::Write {
                    text,
                    reply_to,
                    mentions,
                    points: Vec::new(),
                };
                (None, action)
            }
            CouncilSay::Private { seat, run, text } => {
                (Some(run), CouncilAction::Private { seat, text })
            }
            CouncilSay::Share { message } => (None, CouncilAction::Share { message }),
            CouncilSay::CarryOn { seat, run, session } => {
                let session = session?;
                return Some(UiCommand::CarryOnSeat { run, seat, session });
            }
        };
        Some(UiCommand::Council { run, action })
    }

    /// Sent as its command, or for carrying a seat on, how to do it yourself.
    fn outcome(&self, carry_on: &str) -> ViewOutcome {
        match self.command() {
            Some(command) => ViewOutcome::Send(command),
            None => ViewOutcome::Notice(format!("Run {carry_on} to carry it on in its CLI")),
        }
    }
}

/// The line you type into, at the bottom of a council screen.
#[derive(Debug, Clone, Default)]
struct Draft {
    text: String,
}

impl Draft {
    /// Takes a key that edits the line; false for any other.
    fn edit(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.text.push(ch);
                true
            }
            KeyCode::Backspace => {
                self.text.pop();
                true
            }
            _ => false,
        }
    }

    /// Writes what you typed over the input's placeholder, on the row `input` from the top.
    fn draw(&self, lines: &mut [Line<'static>], input: usize, ctx: &ViewCtx<'_>) {
        if self.text.is_empty() {
            return;
        }
        if let Some(line) = lines.get_mut(input) {
            let prompt = line
                .spans
                .first()
                .cloned()
                .unwrap_or_else(|| Span::raw("❯"));
            *line = Line::from(vec![
                prompt,
                Span::raw(" "),
                Span::styled(self.text.clone(), ctx.theme.style(Role::Text)),
            ]);
        }
    }
}

/// `t` on a message with replies: its thread alone.
#[derive(Debug)]
pub struct ThreadView {
    thread: Thread,
    faces: Faces,
    root: MessageId,
    draft: Draft,
}

impl ThreadView {
    pub fn new(thread: Thread, faces: Faces, root: MessageId) -> Self {
        Self {
            thread,
            faces,
            root,
            draft: Draft::default(),
        }
    }

    /// What you typed, as your reply to the thread's first message. `None` while the line is
    /// empty.
    fn reply(&mut self) -> Option<CouncilSay> {
        if self.draft.text.trim().is_empty() {
            return None;
        }
        let text = std::mem::take(&mut self.draft.text);
        Some(CouncilSay::ToCouncil {
            mentions: self.faces.mentioned(&text),
            text,
            reply_to: Some(self.root.clone()),
        })
    }
}

impl View for ThreadView {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let height = usize::from(area.height);
        let mut lines = self
            .thread
            .lines(usize::from(area.width), height, &self.faces, ctx.theme);
        self.draft.draw(&mut lines, height.saturating_sub(3), ctx);
        draw_lines(&lines, area, buf);
    }

    fn handle_key(&mut self, key: KeyEvent, _ctx: &ViewCtx<'_>) -> ViewOutcome {
        match key.code {
            KeyCode::Esc => ViewOutcome::Close,
            KeyCode::Enter => match self.reply() {
                Some(say) => say.outcome(""),
                None => ViewOutcome::Ignored,
            },
            _ if self.draft.edit(key) => ViewOutcome::Consumed,
            _ => ViewOutcome::Ignored,
        }
    }
}

/// A seat's own session: what it read, and a line only that seat sees.
#[derive(Debug)]
pub struct SeatView {
    session: SeatSession,
    faces: Faces,
    /// Where a private message goes: the seat, its run and its session.
    target: Option<(SeatId, RunId, SessionKey)>,
    draft: Draft,
    /// `o` on an empty line waits for a second `o`, so a message can still start with one.
    carry_on_asked: bool,
    /// The seat's key, asked for each time the view opens: the letters it reads the seats by.
    key: Option<ViewRequest>,
}

impl SeatView {
    /// Picks the next private message of yours: the newest first, then each older one, or
    /// `newer` back toward the newest and off it. False when there is none to pick.
    fn pick(&mut self, newer: bool) -> bool {
        let order = self.session.shareable_newest_first();
        let at = self
            .session
            .picked
            .and_then(|picked| order.iter().position(|&index| index == picked));
        let next = match (at, newer) {
            (None, false) => order.first().copied(),
            (None, true) => return false,
            (Some(at), false) => order.get(at + 1).or(order.get(at)).copied(),
            (Some(0), true) => None,
            (Some(at), true) => order.get(at - 1).copied(),
        };
        if next.is_none() && at.is_none() {
            return false;
        }
        self.session.picked = next;
        true
    }

    fn seat(&self) -> Option<&SeatId> {
        self.target.as_ref().map(|(seat, _, _)| seat)
    }

    pub fn new(
        session: SeatSession,
        faces: Faces,
        target: Option<(SeatId, RunId, SessionKey)>,
    ) -> Self {
        Self {
            session,
            faces,
            target,
            draft: Draft::default(),
            carry_on_asked: false,
            key: None,
        }
    }

    /// Asks for the key at the session's head, the letters its seat reads the seats by, when
    /// the view opens.
    pub fn asking_key(mut self, request: ViewRequest) -> Self {
        self.key = Some(request);
        self
    }
}

impl View for SeatView {
    fn asks_on_open(&self) -> Option<ViewRequest> {
        self.key.clone()
    }

    /// The key it asked for: the letter its seat calls itself by and the one it reads each
    /// other seat by, which only you see.
    fn accept(&mut self, data: &ViewData) -> bool {
        let ViewData::SeatKey {
            run,
            talk,
            reader,
            letters,
        } = data
        else {
            return false;
        };
        let asked = matches!(
            &self.key,
            Some(ViewRequest::SeatKey { run: r, talk: t, reader: who, .. })
                if r == run && t == talk && who == reader
        );
        if asked {
            self.session.only_you = super::session_view::key_line(letters, reader);
        }
        asked
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let height = usize::from(area.height);
        let mut lines = self
            .session
            .lines(usize::from(area.width), height, &self.faces, ctx.theme);
        self.draft.draw(&mut lines, height.saturating_sub(3), ctx);
        draw_lines(&lines, area, buf);
    }

    fn handle_key(&mut self, key: KeyEvent, _ctx: &ViewCtx<'_>) -> ViewOutcome {
        self.key(key)
    }
}

impl SeatView {
    fn key(&mut self, key: KeyEvent) -> ViewOutcome {
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL);
        let empty = self.draft.text.is_empty();
        let picked = self.session.picked_message().cloned();
        let carry_on = self.session.command.clone();
        if std::mem::take(&mut self.carry_on_asked) {
            match (key.code, self.seat()) {
                (KeyCode::Char('o'), Some(_)) if plain => {
                    let Some((seat, run, _)) = self.target.clone() else {
                        return ViewOutcome::Consumed;
                    };
                    let session = self.session.carried.clone();
                    return CouncilSay::CarryOn { seat, run, session }.outcome(&carry_on);
                }
                (KeyCode::Esc | KeyCode::Backspace, _) => return ViewOutcome::Consumed,
                // It was the first letter of a message.
                _ => self.draft.text.push('o'),
            }
            return self.key(key);
        }
        match key.code {
            KeyCode::Esc if picked.is_some() => {
                self.session.picked = None;
                ViewOutcome::Consumed
            }
            KeyCode::Esc => ViewOutcome::Close,
            KeyCode::Up | KeyCode::Down if plain && empty => {
                if self.pick(key.code == KeyCode::Down) {
                    ViewOutcome::Consumed
                } else {
                    ViewOutcome::Ignored
                }
            }
            KeyCode::Char('s') if plain => match picked {
                Some(message) => {
                    self.session.picked = None;
                    CouncilSay::Share { message }.outcome(&carry_on)
                }
                None => self.typed(key),
            },
            // Only on an empty line, and only once you press it twice: carrying on takes the
            // seat out of the talk, and a message may start with an "o".
            KeyCode::Char('o')
                if plain
                    && empty
                    && picked.is_none()
                    && !carry_on.is_empty()
                    && self.seat().is_some() =>
            {
                self.carry_on_asked = true;
                ViewOutcome::Notice(format!(
                    "Press o again to carry it on with {carry_on} · what you type there reaches \
                     only this seat, and an edit there gives its task a fresh session"
                ))
            }
            KeyCode::Enter if !self.draft.text.trim().is_empty() => {
                let Some((seat, run, _)) = self.target.clone() else {
                    return ViewOutcome::Notice("This seat's session has ended".to_owned());
                };
                let say = CouncilSay::Private {
                    seat,
                    run,
                    text: std::mem::take(&mut self.draft.text),
                };
                say.outcome(&carry_on)
            }
            _ => self.typed(key),
        }
    }

    /// A key for the line you type into; typing puts down what was picked.
    fn typed(&mut self, key: KeyEvent) -> ViewOutcome {
        if self.draft.edit(key) {
            self.session.picked = None;
            ViewOutcome::Consumed
        } else {
            ViewOutcome::Ignored
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::super::story;
    use super::*;
    use crate::state::AppState;
    use crate::theme::Theme;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(view: &mut dyn View, ctx: &ViewCtx<'_>, text: &str) {
        for ch in text.chars() {
            assert!(matches!(
                view.handle_key(key(KeyCode::Char(ch)), ctx),
                ViewOutcome::Consumed
            ));
        }
    }

    #[test]
    fn what_you_write_in_a_seats_session_goes_to_that_seat_alone() {
        let theme = Theme::dark();
        let state = AppState::default();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        let run = RunId::new("run-rate-limit").expect("a run id");
        let mut view = SeatView::new(
            story::luna_session(),
            story::faces(),
            Some((story::luna(), run.clone(), SessionKey::numbered(2))),
        );
        typed(&mut view, &ctx, "Is /v1/status called with a key?");
        let mut buf = Buffer::empty(Rect::new(0, 0, 120, 38));
        view.render(Rect::new(0, 0, 120, 38), &mut buf, &ctx);
        let input: String = (0..120).map(|x| buf[(x, 35)].symbol().to_owned()).collect();
        assert!(
            input.starts_with("❯ Is /v1/status called with a key?"),
            "{input}"
        );
        match view.handle_key(key(KeyCode::Enter), &ctx) {
            ViewOutcome::Send(UiCommand::Council {
                run: sent,
                action: CouncilAction::Private { seat, text },
            }) => {
                assert_eq!((sent, seat), (Some(run), story::luna()));
                assert_eq!(text, "Is /v1/status called with a key?");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            view.handle_key(key(KeyCode::Esc), &ctx),
            ViewOutcome::Close
        ));
    }

    /// A seat's view asks for its key each time it opens, and takes the key of its own seat
    /// alone: the letter the seat calls itself by and the one it reads each other seat by.
    #[test]
    fn a_seats_view_asks_for_its_key_and_shows_it_to_you() {
        use argonauts_core::app::{ViewData, ViewRequest};
        use argonauts_core::names::{SeatLetter, WorkflowId};
        use argonauts_core::workflow::StepRef;

        let run = RunId::new("run-rate-limit").expect("a run id");
        let talk = StepRef {
            workflow: WorkflowId::numbered(1),
            phase: 1,
            path: vec![0],
        };
        let seats = story::all();
        let request = ViewRequest::SeatKey {
            run: run.clone(),
            talk: talk.clone(),
            seats: seats.clone(),
            reader: story::luna(),
        };
        let mut session = story::luna_session();
        session.only_you = Vec::new();
        let mut view = SeatView::new(
            session,
            story::faces(),
            Some((story::luna(), run.clone(), SessionKey::numbered(2))),
        )
        .asking_key(request.clone());
        assert_eq!(view.asks_on_open(), Some(request));
        let letter = |c| SeatLetter::new(c).expect("a letter of the pool");
        let letters: Vec<_> = seats
            .iter()
            .cloned()
            .zip([letter('T'), letter('H'), letter('B'), letter('X')])
            .collect();
        let key = |reader| ViewData::SeatKey {
            run: run.clone(),
            talk: talk.clone(),
            reader,
            letters: letters.clone(),
        };
        assert!(!view.accept(&key(story::flash())), "another seat's key");
        assert!(view.accept(&key(story::luna())));
        assert_eq!(
            view.session.only_you,
            super::super::session_view::key_line(&letters, &story::luna())
        );
    }

    fn luna_view() -> SeatView {
        let run = RunId::new("run-rate-limit").expect("a run id");
        SeatView::new(
            story::luna_session(),
            story::faces(),
            Some((story::luna(), run, SessionKey::numbered(2))),
        )
    }

    fn row(view: &mut SeatView, ctx: &ViewCtx<'_>, y: u16) -> (String, Buffer) {
        let mut buf = Buffer::empty(Rect::new(0, 0, 120, 38));
        view.render(Rect::new(0, 0, 120, 38), &mut buf, ctx);
        let text = (0..120).map(|x| buf[(x, y)].symbol().to_owned()).collect();
        (text, buf)
    }

    #[test]
    fn up_picks_a_private_message_of_yours_and_s_shares_it() {
        let theme = Theme::dark();
        let state = AppState::default();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        let mut view = luna_view();
        assert!(matches!(
            view.handle_key(key(KeyCode::Up), &ctx),
            ViewOutcome::Consumed
        ));
        assert_eq!(
            view.session.picked_message(),
            Some(&MessageId::numbered(12))
        );
        let (keys, buf) = row(&mut view, &ctx, 37);
        assert!(keys.starts_with(&format!("  {}", super::super::session_view::SHARE_KEYS)));
        let y = (0..38)
            .find(|&y| buf[(2, y)].symbol() == "F" && buf[(11, y)].symbol() == "u")
            .expect("the private turn");
        assert_eq!(buf[(0, y)].style().fg, theme.style(Role::Accent).fg);
        assert_eq!(buf[(4, y)].style().bg, theme.style(Role::Selection).bg);

        // It is the only one, so ↑ keeps it and ↓ puts it down.
        view.handle_key(key(KeyCode::Up), &ctx);
        assert_eq!(
            view.session.picked,
            view.session.shareable_newest_first().first().copied()
        );
        view.handle_key(key(KeyCode::Down), &ctx);
        assert_eq!(view.session.picked, None);
        assert!(matches!(
            view.handle_key(key(KeyCode::Down), &ctx),
            ViewOutcome::Ignored
        ));

        view.handle_key(key(KeyCode::Up), &ctx);
        assert!(matches!(
            view.handle_key(key(KeyCode::Char('s')), &ctx),
            ViewOutcome::Send(UiCommand::Council {
                run: None,
                action: CouncilAction::Share { .. },
            })
        ));
        assert_eq!(view.session.picked, None, "sharing puts it down");
        assert!(view.draft.text.is_empty());

        // Esc puts a picked message down before it leaves the session.
        view.handle_key(key(KeyCode::Up), &ctx);
        assert!(matches!(
            view.handle_key(key(KeyCode::Esc), &ctx),
            ViewOutcome::Consumed
        ));
        assert!(matches!(
            view.handle_key(key(KeyCode::Esc), &ctx),
            ViewOutcome::Close
        ));
    }

    #[test]
    fn o_carries_the_session_on_only_from_an_empty_line() {
        let theme = Theme::dark();
        let state = AppState::default();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        let mut view = luna_view();
        let notice = |outcome: ViewOutcome| match outcome {
            ViewOutcome::Notice(notice) => notice,
            other => panic!("{other:?}"),
        };
        let asked = notice(view.handle_key(key(KeyCode::Char('o')), &ctx));
        assert_eq!(
            asked,
            "Press o again to carry it on with codex resume 0a1b2c3d-5e6f-4a7b-8c9d-0e1f2a3b4c5d · \
             what you type there reaches only this seat, and an edit there gives its task a \
             fresh session"
        );
        let carried = notice(view.handle_key(key(KeyCode::Char('o')), &ctx));
        assert!(
            carried.contains("codex resume 0a1b2c3d-5e6f-4a7b-8c9d-0e1f2a3b4c5d"),
            "{carried}"
        );

        // Any other key after the first o makes it the message's first letter.
        view.handle_key(key(KeyCode::Char('o')), &ctx);
        typed(&mut view, &ctx, "k, so");
        assert_eq!(view.draft.text, "ok, so");
        let (input, _) = row(&mut view, &ctx, 35);
        assert!(input.starts_with("❯ ok, so"), "{input}");

        // Esc after it asks nothing more and keeps the line empty.
        let mut view = luna_view();
        view.handle_key(key(KeyCode::Char('o')), &ctx);
        assert!(matches!(
            view.handle_key(key(KeyCode::Esc), &ctx),
            ViewOutcome::Consumed
        ));
        assert!(view.draft.text.is_empty());
        assert!(!view.carry_on_asked);

        let mut ended = SeatView::new(story::luna_session(), story::faces(), None);
        typed(&mut ended, &ctx, "o");
        assert_eq!(
            ended.draft.text, "o",
            "a session that ended carries nothing on"
        );

        // A session with its CLI's id carries the seat on there, and it sits out meanwhile.
        let mut session = story::luna_session();
        let codex = CliSessionRef {
            agent_id: argonauts_core::names::AgentId("codex".into()),
            id: "0a1b2c3d-0000-4000".into(),
        };
        session.carried = Some(codex.clone());
        let run = RunId::new("run-rate-limit").expect("a run id");
        let target = Some((story::luna(), run.clone(), SessionKey::numbered(2)));
        let mut view = SeatView::new(session, story::faces(), target);
        view.handle_key(key(KeyCode::Char('o')), &ctx);
        assert!(matches!(
            view.handle_key(key(KeyCode::Char('o')), &ctx),
            ViewOutcome::Send(UiCommand::CarryOnSeat { run: sent, seat, session })
                if sent == run && seat == story::luna() && session == codex
        ));
    }

    #[test]
    fn a_reply_in_a_thread_goes_to_every_seat() {
        let theme = Theme::dark();
        let state = AppState::default();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        let mut view = ThreadView::new(story::thread(), story::faces(), MessageId::numbered(4));
        typed(&mut view, &ctx, "Keep it.");
        assert!(matches!(
            view.handle_key(key(KeyCode::Enter), &ctx),
            ViewOutcome::Send(UiCommand::Council {
                run: None,
                action: CouncilAction::Write { text, reply_to: Some(_), .. },
            }) if text == "Keep it."
        ));
        typed(&mut view, &ctx, "@5.6 Luna, keep it?");
        assert_eq!(
            view.reply(),
            Some(CouncilSay::ToCouncil {
                text: "@5.6 Luna, keep it?".to_owned(),
                reply_to: Some(MessageId::numbered(4)),
                mentions: vec![story::luna()],
            })
        );
        assert_eq!(view.reply(), None);
    }
}
