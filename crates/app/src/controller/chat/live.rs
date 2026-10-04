//! What your conversation's Claude Code session takes while it works, as Claude Code's own
//! screen does: a message you send goes in at once and the model reads it
//! after the running tool call, send now hands it over at once, ↑ takes back what the model
//! hasn't read, and the session draws You should know above the input for a screen that joins.

use argonauts_core::screen::{Drawn, ScreenChange, ScreenRequest};
use argonauts_core::settings::ui::HeadsUp;
use argonauts_core::worker::SessionControl;
use argonauts_core::{AgentEvent, MessageStage};
use tokio::sync::mpsc;

/// What the session's `init` lists when it takes messages mid-turn with send now.
const SEND_NOW: &str = "interrupt_send_now_v1";
/// What it lists when a screen can join it.
const SCREEN: &str = "ui_surface_v1";
/// The plugin that draws above the input, listed when you've turned it on in Claude Code.
const HEADS_UP_PLUGIN: &str = "cc-plugin-you-should-know";

/// The running turn's way in.
pub(super) struct Live {
    control: mpsc::Sender<SessionControl>,
    /// The turn's answer is the CLI's own text. A turn whose answer has a schema, such as the
    /// estimate, would answer your later message instead of the one it was asked.
    plain_reply: bool,
    /// The session's `init` listed send now, so what you send joins it.
    send_now: bool,
    /// Your messages in the session that the model hasn't read yet, oldest first.
    joining: Vec<Joining>,
    screen: Screen,
    /// The last drawing, whose buttons a press names.
    drawn: Option<Drawn>,
}

struct Joining {
    uuid: String,
    text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Off,
    Joined,
    /// A reply didn't read, so nothing more is asked of this session.
    Broken,
}

/// What a message's progress means for the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Heard {
    /// The model read it: it shows in the conversation now.
    Read(String),
    /// Back into the input.
    TakenBack(String),
    /// Too late to take back.
    AlreadyRead,
    /// It never went, so it waits for the next turn.
    NotSent(String),
}

impl Live {
    pub(super) fn new(control: mpsc::Sender<SessionControl>, plain_reply: bool) -> Self {
        Self {
            control,
            plain_reply,
            send_now: false,
            joining: Vec::new(),
            screen: Screen::Off,
            drawn: None,
        }
    }

    /// The session's first line: whether it takes messages mid-turn, and whether to join it
    /// as a screen for You should know. False when it isn't joined, so nothing of an earlier
    /// session's stays above the input.
    pub(super) fn on_init(
        &mut self,
        capabilities: &[String],
        plugins: &[String],
        heads_up: HeadsUp,
    ) -> bool {
        let lists = |name: &str| capabilities.iter().any(|known| known == name);
        self.send_now = self.plain_reply && lists(SEND_NOW);
        let drawn_above = plugins
            .iter()
            .any(|plugin| plugin.split('@').next() == Some(HEADS_UP_PLUGIN));
        let join = lists(SCREEN)
            && drawn_above
            && heads_up == HeadsUp::WhenEnabled
            && self.screen == Screen::Off;
        if join
            && self.ask(SessionControl::Screen(ScreenRequest::Attach))
            && self.ask(SessionControl::Screen(ScreenRequest::Render))
        {
            self.screen = Screen::Joined;
        }
        self.screen == Screen::Joined
    }

    /// Whether what you send now joins the running turn.
    pub(super) fn takes_messages(&self) -> bool {
        self.send_now
    }

    /// The event as the screen gets it: the `init` of a turn that takes no message mid-turn
    /// doesn't offer send now there either.
    pub(super) fn shown(&self, mut event: AgentEvent) -> AgentEvent {
        if let AgentEvent::Init { capabilities, .. } = &mut event
            && !self.send_now
        {
            capabilities.retain(|known| known != SEND_NOW);
        }
        event
    }

    /// Sends your message into the session with a uuid of its own. False when the session
    /// can't take it, and it waits for the next turn instead.
    pub(super) fn send(&mut self, text: String) -> bool {
        if !self.send_now {
            return false;
        }
        let uuid = uuid::Uuid::new_v4().to_string();
        let sent = self.ask(SessionControl::Send {
            text: text.clone(),
            uuid: uuid.clone(),
        });
        if sent {
            self.joining.push(Joining { uuid, text });
        }
        sent
    }

    /// Ctrl+Enter: what waits goes to the model now. False when nothing waits.
    pub(super) fn send_now(&mut self) -> bool {
        let Some(last) = self.joining.last() else {
            return false;
        };
        let uuid = last.uuid.clone();
        self.ask(SessionControl::SendNow { uuid })
    }

    /// ↑: asks for every message the model hasn't read back. Each comes back with its
    /// [`Heard::TakenBack`].
    pub(super) fn take_back(&mut self) -> bool {
        let uuids: Vec<String> = self.joining.iter().map(|j| j.uuid.clone()).collect();
        let mut asked = false;
        for uuid in uuids {
            asked |= self.ask(SessionControl::TakeBack { uuid });
        }
        asked
    }

    /// What waits when the turn ends with no process left to read it.
    pub(super) fn unread(&mut self) -> Vec<String> {
        self.joining.drain(..).map(|joining| joining.text).collect()
    }

    pub(super) fn on_message(&mut self, uuid: &str, state: MessageStage) -> Option<Heard> {
        let at = self.joining.iter().position(|joining| joining.uuid == uuid);
        match state {
            MessageStage::Started => at.map(|at| Heard::Read(self.joining.remove(at).text)),
            MessageStage::TakenBack => at.map(|at| Heard::TakenBack(self.joining.remove(at).text)),
            MessageStage::NotSent => at.map(|at| Heard::NotSent(self.joining.remove(at).text)),
            MessageStage::AlreadyRead => Some(Heard::AlreadyRead),
            MessageStage::Queued
            | MessageStage::Completed
            | MessageStage::SendingNow
            | MessageStage::NothingWaiting => None,
        }
    }

    /// What the session said about the screen. True when it no longer reads, which is said
    /// once.
    pub(super) fn on_screen(&mut self, change: &ScreenChange) -> bool {
        if self.screen != Screen::Joined {
            return false;
        }
        match change {
            ScreenChange::Attached { ok: true } => false,
            ScreenChange::Attached { ok: false } => {
                self.screen = Screen::Off;
                false
            }
            ScreenChange::Stale => {
                self.ask(SessionControl::Screen(ScreenRequest::Render));
                false
            }
            ScreenChange::Drawn { drawn } => {
                self.drawn = Some(drawn.clone());
                false
            }
            ScreenChange::Unreadable { .. } => {
                self.screen = Screen::Broken;
                true
            }
        }
    }

    /// Presses a button of the last drawing. False when this session never drew it.
    pub(super) fn press(&mut self, plugin: &str, handle: u64) -> bool {
        let known = self.drawn.as_ref().is_some_and(|drawn| {
            drawn
                .buttons
                .iter()
                .any(|button| button.plugin == plugin && button.handle == handle)
        });
        known
            && self.ask(SessionControl::Screen(ScreenRequest::Press {
                plugin: plugin.to_owned(),
                handle,
            }))
    }

    /// A full or closed channel means the session is on its way out.
    fn ask(&self, control: SessionControl) -> bool {
        self.control.try_send(control).is_ok()
    }
}

/// Events the screen draws for the moment and the journal leaves out: a heads-up, a message's
/// progress, and what a mod shows only while it's there.
pub(super) fn is_passing(event: &AgentEvent) -> bool {
    use argonauts_core::ModOutputKind;
    match event {
        AgentEvent::TextDelta { .. }
        | AgentEvent::ScreenChanged { .. }
        | AgentEvent::MessageState { .. }
        | AgentEvent::CommandsChanged { .. }
        | AgentEvent::ModelsReported { .. }
        | AgentEvent::BackgroundTasks { .. } => true,
        AgentEvent::ModOutput { kind, .. } => matches!(
            kind,
            ModOutputKind::Toast
                | ModOutputKind::Status
                | ModOutputKind::Notice
                | ModOutputKind::Urgent
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::screen::DrawnButton;

    use super::*;

    fn live() -> (Live, mpsc::Receiver<SessionControl>) {
        let (control, steering) = mpsc::channel(16);
        (Live::new(control, true), steering)
    }

    fn caps(names: &[&str]) -> Vec<String> {
        names.iter().map(|&name| name.to_owned()).collect()
    }

    #[test]
    fn messages_join_only_where_init_lists_send_now() {
        let (mut live, mut sent) = live();
        assert!(!live.send("too early".into()));
        live.on_init(&caps(&[SEND_NOW]), &[], HeadsUp::WhenEnabled);
        assert!(live.send("also check the changelog".into()));
        let Ok(SessionControl::Send { text, uuid }) = sent.try_recv() else {
            panic!("nothing sent");
        };
        assert_eq!(text, "also check the changelog");
        assert!(live.send_now());
        assert_eq!(
            sent.try_recv().ok(),
            Some(SessionControl::SendNow { uuid: uuid.clone() })
        );
        assert_eq!(
            live.on_message(&uuid, MessageStage::Started),
            Some(Heard::Read("also check the changelog".into()))
        );
        // Nothing waits now.
        assert!(!live.send_now());
    }

    #[test]
    fn taking_back_asks_for_each_unread_message() {
        let (mut live, mut sent) = live();
        live.on_init(&caps(&[SEND_NOW]), &[], HeadsUp::WhenEnabled);
        live.send("one".into());
        live.send("two".into());
        let uuids: Vec<String> = std::iter::from_fn(|| match sent.try_recv() {
            Ok(SessionControl::Send { uuid, .. }) => Some(uuid),
            _ => None,
        })
        .collect();
        assert!(live.take_back());
        assert!(matches!(
            sent.try_recv(),
            Ok(SessionControl::TakeBack { .. })
        ));
        assert_eq!(
            live.on_message(&uuids[0], MessageStage::TakenBack),
            Some(Heard::TakenBack("one".into()))
        );
        assert_eq!(
            live.on_message(&uuids[1], MessageStage::AlreadyRead),
            Some(Heard::AlreadyRead)
        );
    }

    #[test]
    fn the_screen_joins_only_when_the_mod_is_on_and_allowed() {
        let all = caps(&[SEND_NOW, SCREEN]);
        let plugins = caps(&["cc-plugin-you-should-know@builtin"]);
        let (mut off, mut sent) = live();
        off.on_init(&all, &plugins, HeadsUp::Off);
        assert!(sent.try_recv().is_err(), "ui.heads_up = off");
        let (mut without, mut sent) = live();
        without.on_init(&all, &[], HeadsUp::WhenEnabled);
        assert!(sent.try_recv().is_err(), "the mod isn't on");
        let (mut old, mut sent) = live();
        old.on_init(&caps(&[SEND_NOW]), &plugins, HeadsUp::WhenEnabled);
        assert!(sent.try_recv().is_err(), "no ui_surface_v1");

        let (mut live, mut sent) = live();
        live.on_init(&all, &plugins, HeadsUp::WhenEnabled);
        assert_eq!(
            sent.try_recv().ok(),
            Some(SessionControl::Screen(ScreenRequest::Attach))
        );
        assert_eq!(
            sent.try_recv().ok(),
            Some(SessionControl::Screen(ScreenRequest::Render))
        );
        live.on_screen(&ScreenChange::Stale);
        assert_eq!(
            sent.try_recv().ok(),
            Some(SessionControl::Screen(ScreenRequest::Render))
        );
        let drawn = Drawn {
            buttons: vec![DrawnButton {
                label: "Dismiss".into(),
                plugin: HEADS_UP_PLUGIN.into(),
                handle: 7,
            }],
            ..Drawn::default()
        };
        live.on_screen(&ScreenChange::Drawn { drawn });
        assert!(!live.press(HEADS_UP_PLUGIN, 8), "a handle it never drew");
        assert!(live.press(HEADS_UP_PLUGIN, 7));
        assert_eq!(
            sent.try_recv().ok(),
            Some(SessionControl::Screen(ScreenRequest::Press {
                plugin: HEADS_UP_PLUGIN.into(),
                handle: 7
            }))
        );
        assert!(live.on_screen(&ScreenChange::Unreadable { why: "x".into() }));
        assert!(
            !live.on_screen(&ScreenChange::Stale),
            "said once, then quiet"
        );
        assert!(sent.try_recv().is_err());
    }
}
