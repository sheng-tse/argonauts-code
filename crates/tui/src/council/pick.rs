//! Picking a message or a task with shift+↑, newest first, as Messages lets you press on one:
//! ↑ and ↓ pick another, `r` replies to it, `t` opens its thread, Enter opens what a task is
//! (its agents, its workflow or its seat's session), and Esc goes back to the input. Only the
//! picked task says "enter to open", and the picked one gets a ring.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::reply::ReplyBar;
use super::views::CouncilSay;

/// What the input knows of a council on this screen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CouncilInput {
    /// A council sat down here, so shift+↑ picks its newest message or task.
    pub sits: bool,
    /// It still talks, so what you write goes to its seats. Once it agrees or stops, a
    /// message goes as any other until the talk goes on.
    pub talking: bool,
    /// The bottom line's words while a message or task is picked.
    pub picking: Option<String>,
    /// Its seats, first in the `@` list.
    pub faces: crate::council::Faces,
    /// The message picked, whose printed bubble the full screen rings.
    pub picked: Option<argonauts_core::names::MessageId>,
    /// Approve with edits for the council's plan, for its approval's question to open.
    pub edits: Option<crate::council::edits::PlanEdits>,
    /// The task whose workflow view is opening, for that view's head.
    pub opening: Option<crate::council::task_header::TaskHeader>,
}

impl CouncilInput {
    /// What you send from the input while the council talks: your message to every seat, as
    /// a reply when `replying` names a message, with the seats it names answering first.
    /// `None` when it doesn't talk.
    pub fn say(&self, text: &str, replying: Option<&ReplyBar>) -> Option<CouncilSay> {
        self.talking.then(|| CouncilSay::ToCouncil {
            text: text.trim().to_owned(),
            reply_to: replying.map(|bar| bar.message.clone()),
            mentions: self.faces.mentioned(text),
        })
    }
}

/// The bottom line's words while a message or task is picked: what Enter opens, such as "its
/// agents", whether `t` opens a thread, and Esc.
pub fn pick_keys(opens: Option<&str>, thread: bool) -> String {
    let mut keys = vec!["↑ ↓  to pick another".to_owned()];
    if let Some(opens) = opens {
        keys.push(format!("enter to open {opens}"));
    }
    if thread {
        keys.push("t to open its thread".to_owned());
    }
    keys.push("r to reply".to_owned());
    keys.push("esc to go back".to_owned());
    keys.join(" · ")
}

/// Shift+↑, which starts picking.
pub fn starts_picking(key: &KeyEvent) -> bool {
    key.code == KeyCode::Up && key.modifiers == KeyModifiers::SHIFT
}

/// What a key did to what was picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickKey<T> {
    Moved,
    Reply(T),
    Thread(T),
    Open(T),
    /// Back to the input, nothing picked.
    Back,
    Ignored,
}

/// The messages and tasks you can pick, newest first, and which is picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker<T> {
    newest_first: Vec<T>,
    at: usize,
}

impl<T: Clone> Picker<T> {
    /// Picks the newest of `newest_first`; `None` when there is nothing to pick.
    pub fn start(newest_first: Vec<T>) -> Option<Self> {
        (!newest_first.is_empty()).then_some(Self {
            newest_first,
            at: 0,
        })
    }

    pub fn picked(&self) -> &T {
        &self.newest_first[self.at]
    }

    /// The things to pick changed, as when one more came: the one picked stays picked
    /// while it is still there.
    pub fn refresh(&mut self, newest_first: Vec<T>)
    where
        T: PartialEq,
    {
        if newest_first.is_empty() {
            return;
        }
        let picked = self.picked().clone();
        self.at = newest_first
            .iter()
            .position(|one| *one == picked)
            .unwrap_or(0);
        self.newest_first = newest_first;
    }

    pub fn key(&mut self, key: KeyEvent) -> PickKey<T> {
        match key.code {
            KeyCode::Up => {
                self.at = (self.at + 1).min(self.newest_first.len() - 1);
                PickKey::Moved
            }
            KeyCode::Down if self.at == 0 => PickKey::Back,
            KeyCode::Down => {
                self.at -= 1;
                PickKey::Moved
            }
            KeyCode::Char('r') => PickKey::Reply(self.picked().clone()),
            KeyCode::Char('t') => PickKey::Thread(self.picked().clone()),
            KeyCode::Enter => PickKey::Open(self.picked().clone()),
            KeyCode::Esc => PickKey::Back,
            _ => PickKey::Ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::names::MessageId;

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn shift_up_picks_the_newest_and_up_goes_further_back() {
        let ids: Vec<MessageId> = (1..=3).rev().map(MessageId::numbered).collect();
        let mut picker = Picker::start(ids).expect("something to pick");
        assert_eq!(picker.picked(), &MessageId::numbered(3));
        assert_eq!(picker.key(key(KeyCode::Up)), PickKey::Moved);
        assert_eq!(picker.picked(), &MessageId::numbered(2));
        assert_eq!(
            picker.key(key(KeyCode::Char('r'))),
            PickKey::Reply(MessageId::numbered(2))
        );
        assert_eq!(
            picker.key(key(KeyCode::Char('t'))),
            PickKey::Thread(MessageId::numbered(2))
        );
        picker.key(key(KeyCode::Up));
        picker.key(key(KeyCode::Up));
        assert_eq!(picker.picked(), &MessageId::numbered(1));
        picker.key(key(KeyCode::Down));
        picker.key(key(KeyCode::Down));
        assert_eq!(picker.key(key(KeyCode::Down)), PickKey::Back);
        assert!(Picker::<MessageId>::start(Vec::new()).is_none());
    }

    #[test]
    fn the_bottom_line_says_what_enter_opens() {
        assert_eq!(
            pick_keys(Some("its agents"), false),
            "↑ ↓  to pick another · enter to open its agents · r to reply · esc to go back"
        );
        assert_eq!(
            pick_keys(None, true),
            "↑ ↓  to pick another · t to open its thread · r to reply · esc to go back"
        );
    }
}
