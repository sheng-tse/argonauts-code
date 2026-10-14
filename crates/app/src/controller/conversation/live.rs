//! What your conversation's Claude Code session takes while it works, as Claude Code's own
//! screen does: a message you send goes in at once and the model reads it
//! after the running tool call, send now hands it over at once, ↑ takes back what the model
//! hasn't read, and the session draws You should know above the input for a screen that joins.

use std::collections::BTreeSet;

use argonauts_core::events::context::ContextDetail;
use argonauts_core::events::session::{SubagentKind, SubagentStatus};
use argonauts_core::pictures::Picture;
use argonauts_core::screen::{Drawn, ScreenChange, ScreenRequest};
use argonauts_core::session_runner::SessionControl;
use argonauts_core::settings::ui::Notices;
use argonauts_core::{AgentEvent, MessageStage};
use tokio::sync::mpsc;

use super::Queued;

/// What the session's `init` lists when it takes messages mid-turn with send now.
const SEND_NOW: &str = "interrupt_send_now_v1";
/// What it lists when a screen can join it.
const SCREEN: &str = "ui_surface_v1";
/// The plugin that draws above the input, listed when you've turned it on in Claude Code.
const NOTICE_PLUGIN: &str = "cc-plugin-you-should-know";

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
    /// How many tasks the session's CLI runs in the background now, a command it left running
    /// among them.
    background: usize,
    /// The CLI's own subagents and workflows still going, by the id their updates carry.
    agents: BTreeSet<String>,
    /// The main agent's turn has its result, and it has started nothing since.
    idle: bool,
    /// You stopped a turn alone, so a session whose last word is that turn didn't fail.
    stopped_turn: bool,
}

struct Joining {
    uuid: String,
    /// What the session reads.
    text: String,
    /// The pictures its chips name, which go back with it if it never went.
    pictures: Vec<Picture>,
    /// For a message to one of the session's subagents: its name and your words. `text` adds
    /// how to pass them on, which the conversation never shows.
    for_subagent: Option<(String, String)>,
}

impl Joining {
    /// What the conversation shows and the prompt gets back: your own words.
    fn shown(&self) -> String {
        match &self.for_subagent {
            Some((name, words)) => format!("@{name} {words}"),
            None => self.text.clone(),
        }
    }

    /// The turn it waits for when it never went.
    fn queued(self) -> Queued {
        match self.for_subagent {
            Some((name, text)) => Queued::Subagent { name, text },
            None => Queued::Request(self.text, self.pictures),
        }
    }
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
    NotSent(Queued),
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
            background: 0,
            agents: BTreeSet::new(),
            idle: false,
            stopped_turn: false,
        }
    }

    /// The session's first line: whether it takes messages mid-turn, and whether to join it
    /// as a screen for You should know. False when it isn't joined, so nothing of an earlier
    /// session's stays above the input.
    pub(super) fn on_init(
        &mut self,
        capabilities: &[String],
        plugins: &[String],
        notices: Notices,
    ) -> bool {
        let lists = |name: &str| capabilities.iter().any(|known| known == name);
        self.send_now = self.plain_reply && lists(SEND_NOW);
        let drawn_above = plugins
            .iter()
            .any(|plugin| plugin.split('@').next() == Some(NOTICE_PLUGIN));
        let join = lists(SCREEN)
            && drawn_above
            && notices == Notices::WhenEnabled
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

    /// Sends your message into the session with a uuid of its own, and the pictures its chips
    /// name. False when the session can't take it, and it waits for the next turn instead.
    pub(super) fn send(&mut self, text: String, pictures: Vec<Picture>) -> bool {
        self.send_joining(text, pictures, None)
    }

    /// Sends `text`, your message for one of the session's subagents with how to pass it on,
    /// while the conversation shows only `@name` and your words.
    pub(super) fn send_for_subagent(&mut self, text: String, name: String, words: String) -> bool {
        self.send_joining(text, Vec::new(), Some((name, words)))
    }

    fn send_joining(
        &mut self,
        text: String,
        pictures: Vec<Picture>,
        for_subagent: Option<(String, String)>,
    ) -> bool {
        if !self.send_now {
            return false;
        }
        let uuid = uuid::Uuid::new_v4().to_string();
        let sent = self.ask(SessionControl::Send {
            text: text.clone(),
            uuid: uuid.clone(),
            pictures: pictures.clone(),
        });
        if sent {
            self.joining.push(Joining {
                uuid,
                text,
                pictures,
                for_subagent,
            });
        }
        sent
    }

    /// Follows what the session runs in the background and whether its main agent is at work,
    /// which decide what Esc stops.
    pub(super) fn on_agent(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::BackgroundTasks { running, .. } => self.background = running.len(),
            // A workflow reports itself, below.
            AgentEvent::Subagent { update } if update.kind != SubagentKind::Workflow => {
                let ended = matches!(update.status, SubagentStatus::Done | SubagentStatus::Failed);
                if ended {
                    self.agents.remove(&update.id);
                } else {
                    self.agents.insert(update.id.clone());
                }
            }
            AgentEvent::Workflow { workflow } if workflow.ended() => {
                self.agents.remove(&workflow.id);
            }
            AgentEvent::Workflow { workflow } => {
                self.agents.insert(workflow.id.clone());
            }
            AgentEvent::Result { .. } => self.idle = true,
            // Each turn of the CLI's opens with its init, and then the main agent speaks or
            // calls a tool.
            AgentEvent::Init { .. }
            | AgentEvent::TextDelta { parent: None, .. }
            | AgentEvent::ToolPending { parent: None, .. }
            | AgentEvent::ToolStarted { parent: None, .. } => self.idle = false,
            _ => {}
        }
    }

    /// Esc while the session's own subagents or workflows run: the main agent's turn stops
    /// and they go on, as on Claude Code's screen. False when none of them
    /// runs in the background or the session can't be asked, and then the whole session
    /// stops. A command left running is no agent: the list has no row to stop it from, so it
    /// goes with the session as before.
    pub(super) fn stop_turn_alone(&mut self) -> bool {
        if self.background == 0 || self.agents.is_empty() {
            return false;
        }
        // Only the background works, so there is no turn to stop and nothing to do.
        if self.idle {
            return true;
        }
        self.stopped_turn = self.ask(SessionControl::StopTurn);
        self.stopped_turn
    }

    /// You stopped a turn of this session alone.
    pub(super) fn stopped_turn(&self) -> bool {
        self.stopped_turn
    }

    /// Stops one of the session's own subagents. False when the session can't be asked now.
    pub(super) fn stop_subagent(&mut self, id: String) -> bool {
        self.ask(SessionControl::StopSubagent { id })
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
    pub(super) fn unread(&mut self) -> Vec<Queued> {
        self.joining.drain(..).map(Joining::queued).collect()
    }

    pub(super) fn on_message(&mut self, uuid: &str, state: MessageStage) -> Option<Heard> {
        let at = self.joining.iter().position(|joining| joining.uuid == uuid);
        match state {
            MessageStage::Started => at.map(|at| Heard::Read(self.joining.remove(at).shown())),
            MessageStage::TakenBack => {
                at.map(|at| Heard::TakenBack(self.joining.remove(at).shown()))
            }
            MessageStage::NotSent => at.map(|at| Heard::NotSent(self.joining.remove(at).queued())),
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
    pub(super) fn ask_context(&self, detail: ContextDetail) {
        self.ask(SessionControl::AskContext(detail));
    }

    pub(super) fn watch_context(&self, detail: Option<ContextDetail>) {
        self.ask(SessionControl::WatchContext(detail));
    }

    fn ask(&self, control: SessionControl) -> bool {
        self.control.try_send(control).is_ok()
    }
}

/// Events the screen draws for the moment and the journal leaves out: a notice, a message's
/// progress, a call still being written, and what a mod shows only while it's there.
pub(super) fn is_passing(event: &AgentEvent) -> bool {
    use argonauts_core::PluginOutputKind;
    match event {
        AgentEvent::TextDelta { .. }
        | AgentEvent::ToolPending { .. }
        | AgentEvent::ToolDropped { .. }
        | AgentEvent::ScreenChanged { .. }
        | AgentEvent::MessageState { .. }
        | AgentEvent::CommandsChanged { .. }
        | AgentEvent::ModelsReported { .. }
        | AgentEvent::ContextBreakdown { .. }
        | AgentEvent::Plan { .. }
        | AgentEvent::BackgroundTasks { .. } => true,
        AgentEvent::PluginOutput { kind, .. } => matches!(
            kind,
            PluginOutputKind::Toast
                | PluginOutputKind::Status
                | PluginOutputKind::Notice
                | PluginOutputKind::Urgent
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
        assert!(!live.send("too early".into(), Vec::new()));
        live.on_init(&caps(&[SEND_NOW]), &[], Notices::WhenEnabled);
        assert!(live.send("also check the changelog".into(), Vec::new()));
        let Ok(SessionControl::Send { text, uuid, .. }) = sent.try_recv() else {
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
    fn a_message_goes_in_with_its_pictures_and_keeps_them_if_it_never_went() {
        let (mut live, mut sent) = live();
        live.on_init(&caps(&[SEND_NOW]), &[], Notices::WhenEnabled);
        let shot = Picture {
            number: 2,
            path: "/tmp/shot.png".into(),
        };
        assert!(live.send("and this [Image #2]".into(), vec![shot.clone()]));
        let Ok(SessionControl::Send { pictures, .. }) = sent.try_recv() else {
            panic!("nothing sent");
        };
        assert_eq!(pictures, std::slice::from_ref(&shot));
        assert_eq!(
            live.unread(),
            [Queued::Request("and this [Image #2]".into(), vec![shot])]
        );
    }

    #[test]
    fn a_message_for_a_subagent_shows_your_words_and_waits_as_one() {
        let (mut live, mut sent) = live();
        live.on_init(&caps(&[SEND_NOW]), &[], Notices::WhenEnabled);
        assert!(live.send_for_subagent(
            "For your subagent \"Count slowly\": stop at 5\n\n(how to pass it on)".into(),
            "Count slowly".into(),
            "stop at 5".into(),
        ));
        let Ok(SessionControl::Send { uuid, .. }) = sent.try_recv() else {
            panic!("nothing sent");
        };
        assert_eq!(
            live.on_message(&uuid, MessageStage::Started),
            Some(Heard::Read("@Count slowly stop at 5".into()))
        );
        live.send_for_subagent("again".into(), "Count slowly".into(), "stop at 6".into());
        assert_eq!(
            live.unread(),
            [Queued::Subagent {
                name: "Count slowly".into(),
                text: "stop at 6".into(),
            }]
        );
    }

    #[test]
    fn taking_back_asks_for_each_unread_message() {
        let (mut live, mut sent) = live();
        live.on_init(&caps(&[SEND_NOW]), &[], Notices::WhenEnabled);
        live.send("one".into(), Vec::new());
        live.send("two".into(), Vec::new());
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
        off.on_init(&all, &plugins, Notices::Off);
        assert!(sent.try_recv().is_err(), "ui.notices = off");
        let (mut without, mut sent) = live();
        without.on_init(&all, &[], Notices::WhenEnabled);
        assert!(sent.try_recv().is_err(), "the mod isn't on");
        let (mut old, mut sent) = live();
        old.on_init(&caps(&[SEND_NOW]), &plugins, Notices::WhenEnabled);
        assert!(sent.try_recv().is_err(), "no ui_surface_v1");

        let (mut live, mut sent) = live();
        live.on_init(&all, &plugins, Notices::WhenEnabled);
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
                plugin: NOTICE_PLUGIN.into(),
                handle: 7,
            }],
            ..Drawn::default()
        };
        live.on_screen(&ScreenChange::Drawn { drawn });
        assert!(!live.press(NOTICE_PLUGIN, 8), "a handle it never drew");
        assert!(live.press(NOTICE_PLUGIN, 7));
        assert_eq!(
            sent.try_recv().ok(),
            Some(SessionControl::Screen(ScreenRequest::Press {
                plugin: NOTICE_PLUGIN.into(),
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
