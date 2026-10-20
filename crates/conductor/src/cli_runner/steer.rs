//! Something to tell a running agent: a message, the wrap-up, or the repeat note, and how its
//! delivery is recorded. Each delivery emits `Delivered` for a message and a fresh `Input`
//! holding only the new parts.

use argonauts_core::events::session::{
    Delivery, InputPart, InputPartKind, LaunchRecord, LimitEvent, QueuedMessage, SessionEvent,
    WrapUpVia,
};
use argonauts_core::{LimitDimension, WRAP_UP_MESSAGE};

use super::launch::{input_event, message_part, part};
use super::outbox::Outbox;
use crate::watchdogs::{NO_PROGRESS_WRAP_UP, REPEAT_NOTE};

/// Put between messages sent as one turn.
const BETWEEN: &str = "\n\n";

#[derive(Debug, Clone)]
pub struct Pending {
    pub part: InputPart,
    /// The message and how it's delivered, for `Delivered`.
    pub message: Option<(QueuedMessage, Delivery)>,
    /// Sent with the delivery, such as `WrapUpSent`.
    pub limit: Option<LimitEvent>,
}

impl Pending {
    pub fn message(message: &QueuedMessage, via: Delivery) -> Self {
        Self {
            part: message_part(message),
            message: Some((message.clone(), via)),
            limit: None,
        }
    }

    /// `dimension` is the limit at its soft stop, when the meter asked.
    pub fn wrap_up(dimension: Option<LimitDimension>, via: WrapUpVia) -> Self {
        Self {
            part: part(InputPartKind::WrapUp, "wrap-up", WRAP_UP_MESSAGE, None),
            message: None,
            limit: dimension.map(|dimension| LimitEvent::WrapUpSent { via, dimension }),
        }
    }

    /// The wrap-up for a later try whose calls changed nothing: run the checks, and report.
    pub fn check_and_report() -> Self {
        Self {
            part: part(InputPartKind::WrapUp, "wrap-up", NO_PROGRESS_WRAP_UP, None),
            message: None,
            limit: None,
        }
    }

    /// Asks a session that reports through the tool for the report it ended without.
    pub fn report_asked() -> Self {
        Self {
            part: part(
                InputPartKind::Message,
                "report asked",
                super::report_tool::ASK_FOR_IT,
                None,
            ),
            message: None,
            limit: None,
        }
    }

    pub fn note(call: String, times: u32) -> Self {
        Self {
            part: part(InputPartKind::Message, "repeat note", REPEAT_NOTE, None),
            message: None,
            limit: Some(LimitEvent::Nudged { call, times }),
        }
    }
}

/// The parts of items sent as one turn, a blank line after each but the last, so that their
/// concatenation is exactly the text sent.
pub fn joined_parts(items: &[Pending]) -> Vec<InputPart> {
    let last = items.len().saturating_sub(1);
    items
        .iter()
        .enumerate()
        .map(|(at, item)| {
            let mut part = item.part.clone();
            if at < last {
                part.text.push_str(BETWEEN);
            }
            part
        })
        .collect()
}

/// `Delivered` for each message and any limit event, then the `Input` that went out.
pub fn announce(
    out: &Outbox,
    items: &[Pending],
    parts: Vec<InputPart>,
    launch: LaunchRecord,
    queued: Vec<QueuedMessage>,
) {
    for item in items {
        if let Some((message, via)) = &item.message {
            out.session(SessionEvent::Delivered {
                message: message.id.clone(),
                via: *via,
            });
        }
        if let Some(limit) = &item.limit {
            out.session(SessionEvent::Limit(limit.clone()));
        }
    }
    out.session(input_event(launch, parts, queued));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_runner::launch::prompt_text;
    use crate::cli_runner::testing::queued;

    #[test]
    fn joined_parts_concatenate_to_what_is_sent() {
        let items = [
            Pending::message(&queued(1, "first", Delivery::Steer), Delivery::Steer),
            Pending::message(&queued(2, "second", Delivery::Steer), Delivery::Steer),
        ];
        let parts = joined_parts(&items);
        assert_eq!(prompt_text(&parts), "first\n\nsecond");
        assert_eq!(parts[1].text, "second");
        assert_eq!(joined_parts(&items[..1])[0].text, "first");
    }

    #[test]
    fn wrap_up_names_its_limit_only_when_the_meter_asked() {
        let asked = Pending::wrap_up(Some(LimitDimension::Spend), WrapUpVia::Input);
        assert_eq!(asked.part.text, WRAP_UP_MESSAGE);
        assert_eq!(
            asked.limit,
            Some(LimitEvent::WrapUpSent {
                via: WrapUpVia::Input,
                dimension: LimitDimension::Spend
            })
        );
        assert!(Pending::wrap_up(None, WrapUpVia::Input).limit.is_none());
    }
}
