//! What a list read out in plain mode offers on the next line, where the full screen has keys:
//! `/rewind`'s checkpoints by number, `s` after `/status` for the narrator's summary, `l` after
//! a workflow for the library, and the library's files by number, with `c`, `C`, or `d` before
//! it to copy one, copy it to the repo, or diff it with the built-in. `a` after the default
//! agent's `/effort` flips All hands, which the slider does with Tab. Any other line drops the
//! offer and means what it always does.

use argonauts_core::CardId;
use argonauts_core::app::{UiCommand, ViewData, ViewRequest};
use argonauts_core::input::{SlashCommand, WorkflowAction};
use argonauts_core::views::WorkflowFile;

use crate::text::sanitize_line;

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Offer {
    Checkpoints(Vec<(CardId, u32)>),
    Summary,
    /// The run's workflow was read out; `l` reads the library.
    Workflow,
    Library(Vec<WorkflowFile>),
    /// The default agent's effort was read out with All hands, which is on when `true`.
    AllHands(bool),
}

/// What a line taken by an offer comes to.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Taken {
    pub lines: Vec<String>,
    pub command: Option<UiCommand>,
}

impl Offer {
    pub fn for_view(data: &ViewData) -> Option<Self> {
        match data {
            ViewData::Checkpoints { checkpoints, .. } if !checkpoints.is_empty() => {
                Some(Self::Checkpoints(
                    checkpoints
                        .iter()
                        .map(|row| (row.card.clone(), row.n))
                        .collect(),
                ))
            }
            ViewData::Status { summary } if summary.run.is_some() => Some(Self::Summary),
            ViewData::Workflow { .. } => Some(Self::Workflow),
            ViewData::WorkflowLibrary {
                workflows,
                refresh: false,
            } if !workflows.is_empty() => Some(Self::Library(workflows.clone())),
            ViewData::Effort { slider } => slider.all_hands.map(|toggle| Self::AllHands(toggle.on)),
            _ => None,
        }
    }

    /// The line read out after the list.
    pub fn hint(&self) -> &'static str {
        match self {
            Self::Checkpoints(_) => {
                "rewind: type a checkpoint's number to go back to it. A confirm card comes first."
            }
            Self::Summary => "status: type s for the narrator's summary.",
            Self::Workflow => "workflow: type l for the workflow library.",
            Self::Library(_) => {
                "workflows: type a number to read one, or c, C, or d and a number to copy it, \
                 copy it to the repo, or diff it with the built-in."
            }
            Self::AllHands(false) => "all hands: type a to turn it on.",
            Self::AllHands(true) => "all hands: type a to turn it off.",
        }
    }

    /// `None` when `line` isn't an answer to this offer.
    pub fn take(&self, line: &str) -> Option<Taken> {
        let line = line.trim();
        match self {
            Self::Summary => (line == "s").then(|| Taken {
                command: Some(UiCommand::Summary),
                ..Taken::default()
            }),
            Self::AllHands(on) => (line == "a").then(|| Taken {
                command: Some(UiCommand::Slash {
                    command: SlashCommand::Effort {
                        role: None,
                        level: None,
                        all_hands: Some(!on),
                    },
                }),
                ..Taken::default()
            }),
            Self::Workflow => (line == "l").then(|| Taken {
                command: Some(UiCommand::Request {
                    view: ViewRequest::WorkflowLibrary,
                }),
                ..Taken::default()
            }),
            Self::Checkpoints(rows) => {
                let (card, checkpoint) = rows.get(number(line)?.checked_sub(1)?)?.clone();
                Some(Taken {
                    command: Some(UiCommand::Rewind { card, checkpoint }),
                    ..Taken::default()
                })
            }
            Self::Library(rows) => {
                let (action, rest) = match line.split_once(' ') {
                    Some(("c", rest)) => (Some(WorkflowAction::Copy), rest),
                    Some(("C", rest)) => (Some(WorkflowAction::CopyToProject), rest),
                    Some(("d", rest)) => (Some(WorkflowAction::Diff), rest),
                    _ => (None, line),
                };
                let row = rows.get(number(rest.trim())?.checked_sub(1)?)?;
                Some(match action {
                    Some(action) => Taken {
                        command: Some(UiCommand::WorkflowFile {
                            name: row.name.clone(),
                            action,
                        }),
                        ..Taken::default()
                    },
                    None => Taken {
                        lines: row
                            .text
                            .lines()
                            .map(|text| {
                                format!("workflow {}: {}", row.name, sanitize_line(text).trim_end())
                            })
                            .collect(),
                        command: None,
                    },
                })
            }
        }
    }
}

fn number(text: &str) -> Option<usize> {
    if text.is_empty() || !text.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use argonauts_core::RunId;
    use argonauts_core::api::StatusSummary;
    use argonauts_core::names::WorkerRef;
    use argonauts_core::views::{CheckpointRow, WorkflowOrigin};
    use time::OffsetDateTime;

    use super::*;

    fn checkpoints() -> ViewData {
        let row = |n| CheckpointRow {
            card: CardId::new("card-001").unwrap(),
            n,
            at: OffsetDateTime::UNIX_EPOCH,
            commit: "0123456".to_owned(),
            from: WorkerRef::new("claude-code", None),
            to: vec![WorkerRef::new("codex", None)],
            reason: "usage limit".to_owned(),
            reverted_here: false,
        };
        ViewData::Checkpoints {
            run: RunId::new("run-01").unwrap(),
            card: None,
            checkpoints: vec![row(1), row(2)],
        }
    }

    #[test]
    fn a_number_picks_a_checkpoint_and_anything_else_is_left_alone() {
        let offer = Offer::for_view(&checkpoints()).unwrap();
        let taken = offer.take(" 2 ").unwrap();
        assert!(matches!(
            taken.command,
            Some(UiCommand::Rewind { checkpoint: 2, .. })
        ));
        for other in ["3", "0", "2 please", "s", ""] {
            assert!(offer.take(other).is_none(), "{other}");
        }
    }

    /// The default agent's effort offers All hands by `a`, which asks to flip it, where the
    /// slider would take Tab.
    #[test]
    fn a_after_the_effort_flips_all_hands() {
        use argonauts_core::model_picker::EffortSlider;

        let view = |on: bool| {
            let worker: WorkerRef = "claude-code/haiku".parse().unwrap();
            let slider = EffortSlider::without_levels(None, worker, "Haiku 4.5");
            ViewData::Effort {
                slider: slider.with_all_hands(on),
            }
        };
        for on in [false, true] {
            let offer = Offer::for_view(&view(on)).unwrap();
            assert!(
                offer.hint().contains("type a to turn it"),
                "{}",
                offer.hint()
            );
            assert_eq!(
                offer.take(" a ").unwrap().command,
                Some(UiCommand::Slash {
                    command: SlashCommand::Effort {
                        role: None,
                        level: None,
                        all_hands: Some(!on),
                    }
                })
            );
            for other in ["A", "all", "s", ""] {
                assert!(offer.take(other).is_none(), "{other}");
            }
        }
        let worker: WorkerRef = "claude-code/haiku".parse().unwrap();
        let without = ViewData::Effort {
            slider: EffortSlider::without_levels(
                Some(argonauts_core::names::RoleName::Reviewer),
                worker,
                "Haiku 4.5",
            ),
        };
        assert_eq!(Offer::for_view(&without), None);
    }

    #[test]
    fn s_after_a_status_asks_the_narrator() {
        let summary = StatusSummary {
            run: Some(RunId::new("run-01").unwrap()),
            ..StatusSummary::default()
        };
        let offer = Offer::for_view(&ViewData::Status { summary }).unwrap();
        assert_eq!(offer.take("s").unwrap().command, Some(UiCommand::Summary));
        assert!(offer.take("so what").is_none());
        let none = ViewData::Status {
            summary: StatusSummary::default(),
        };
        assert!(Offer::for_view(&none).is_none());
    }

    #[test]
    fn the_library_reads_copies_and_diffs_by_number() {
        let file = WorkflowFile {
            name: "solo".to_owned(),
            origin: WorkflowOrigin::BuiltIn,
            source: "built in".to_owned(),
            description: String::new(),
            text: "name: solo\nphases: []".to_owned(),
            edited: None,
            in_use: true,
        };
        let offer = Offer::for_view(&ViewData::WorkflowLibrary {
            workflows: vec![file],
            refresh: false,
        })
        .unwrap();
        assert_eq!(
            offer.take("1").unwrap().lines,
            ["workflow solo: name: solo", "workflow solo: phases: []"]
        );
        let copy = |line: &str| match offer.take(line).and_then(|taken| taken.command) {
            Some(UiCommand::WorkflowFile { action, .. }) => Some(action),
            _ => None,
        };
        assert_eq!(copy("c 1"), Some(WorkflowAction::Copy));
        assert_eq!(copy("C 1"), Some(WorkflowAction::CopyToProject));
        assert_eq!(copy("d 1"), Some(WorkflowAction::Diff));
        assert_eq!(copy("x 1"), None);
        assert!(offer.take("c 2").is_none());
    }
}
