//! What an owner answers in its report. The report schema has no field for replies, so an owner
//! writes each on its own line of `summary` or `concerns`, as the delivered message asks:
//! `answer m-004: <answer>`, `accept m-007: <why>`, or `reject m-007: <why>`.

use std::sync::LazyLock;

use regex::Regex;

use argonauts_core::names::MessageId;
use argonauts_core::steps::card::CardReport;

static REPLY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^\s*[-*]?\s*`?(answer|accept|approve|reject|object)\s+(m-\d{3,})`?\s*[:,-]?\s*(.*)$",
    )
    .expect("a valid pattern")
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Answer {
        to: MessageId,
        text: String,
    },
    /// One owner's vote on a change request.
    Vote {
        on: MessageId,
        accept: bool,
        reason: String,
    },
}

/// The replies in a report, in order, the first per message only.
pub fn in_report(report: &CardReport) -> Vec<Reply> {
    let lines = report
        .summary
        .lines()
        .chain(report.concerns.iter().flat_map(|concern| concern.lines()));
    let mut replies: Vec<Reply> = Vec::new();
    for line in lines {
        let Some(reply) = parse(line) else {
            continue;
        };
        if !replies.iter().any(|known| about(known) == about(&reply)) {
            replies.push(reply);
        }
    }
    replies
}

fn parse(line: &str) -> Option<Reply> {
    let found = REPLY.captures(line)?;
    let verb = found.get(1)?.as_str().to_ascii_lowercase();
    let id = MessageId::parse(found.get(2)?.as_str())?;
    let rest = found
        .get(3)
        .map(|rest| rest.as_str().trim().trim_end_matches('`').trim().to_owned())
        .unwrap_or_default();
    Some(match verb.as_str() {
        "answer" => Reply::Answer { to: id, text: rest },
        "accept" | "approve" => Reply::Vote {
            on: id,
            accept: true,
            reason: rest,
        },
        _ => Reply::Vote {
            on: id,
            accept: false,
            reason: rest,
        },
    })
}

fn about(reply: &Reply) -> &MessageId {
    match reply {
        Reply::Answer { to, .. } => to,
        Reply::Vote { on, .. } => on,
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::steps::card::ReportStatus;

    use super::*;

    #[test]
    fn replies_are_read_from_summary_and_concerns() {
        let report = CardReport {
            status: ReportStatus::Done,
            summary: "Built the store.\nanswer m-004: Seconds, as an integer.".to_owned(),
            files_changed: Vec::new(),
            tests_run: Vec::new(),
            concerns: vec![
                "- Reject m-007: the interface is frozen".to_owned(),
                "accept m-007: changed my mind".to_owned(),
                "`approve m-009`".to_owned(),
                "The answer m-004 is above".to_owned(),
            ],
            questions: Vec::new(),
            change_requests: Vec::new(),
            memory_proposals: Vec::new(),
        };
        assert_eq!(
            in_report(&report),
            [
                Reply::Answer {
                    to: MessageId::numbered(4),
                    text: "Seconds, as an integer.".to_owned()
                },
                Reply::Vote {
                    on: MessageId::numbered(7),
                    accept: false,
                    reason: "the interface is frozen".to_owned()
                },
                Reply::Vote {
                    on: MessageId::numbered(9),
                    accept: true,
                    reason: String::new()
                },
            ]
        );
    }
}
