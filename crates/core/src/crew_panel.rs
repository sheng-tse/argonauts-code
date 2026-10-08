//! The crew panel: every agent, conversation, and CLI session, in tabs by what each waits on,
//! grouped by repo and then run, with the highlighted row's details beside or under the list.

use std::cmp::Reverse;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::names::{ChatId, CheckpointId, SessionKey, WorkerRef};
use crate::vendor_sessions::VendorSessionRef;
use crate::views::ContextUse;
use crate::{CardId, RunId, Spend};

/// Tab and Shift+Tab cycle them in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrewTab {
    NeedsYou,
    Working,
    Ready,
    Done,
    #[default]
    All,
}

impl CrewTab {
    pub const ORDER: [CrewTab; 5] = [
        CrewTab::NeedsYou,
        CrewTab::Working,
        CrewTab::Ready,
        CrewTab::Done,
        CrewTab::All,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CrewTab::NeedsYou => "Needs you",
            CrewTab::Working => "Working",
            CrewTab::Ready => "Ready",
            CrewTab::Done => "Done",
            CrewTab::All => "All",
        }
    }

    /// Tab.
    pub fn next(self) -> Self {
        Self::ORDER[(self.index() + 1) % Self::ORDER.len()]
    }

    /// Shift+Tab.
    pub fn previous(self) -> Self {
        Self::ORDER[(self.index() + Self::ORDER.len() - 1) % Self::ORDER.len()]
    }

    fn index(self) -> usize {
        Self::ORDER.iter().position(|tab| *tab == self).unwrap_or(0)
    }

    pub fn holds(self, state: CrewState) -> bool {
        match self {
            CrewTab::All => true,
            CrewTab::NeedsYou => state == CrewState::NeedsYou,
            CrewTab::Working => matches!(state, CrewState::Working | CrewState::Waiting),
            CrewTab::Ready => state == CrewState::Ready,
            CrewTab::Done => matches!(
                state,
                CrewState::Done | CrewState::Stopped | CrewState::Failed
            ),
        }
    }
}

/// Where a row's work stands, which picks its glyph and its tab. In this order within a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrewState {
    /// Waiting on your answer or approval.
    NeedsYou,
    Working,
    /// Waiting on other work, such as a card it depends on or a usage reset.
    Waiting,
    /// Its turn is over and it takes more: a conversation, a CLI session, or a parked run.
    Ready,
    Done,
    Stopped,
    Failed,
}

/// One row of the panel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrewRow {
    /// What → or Enter opens, and x stops.
    pub target: CrewTarget,
    pub state: CrewState,
    /// The card's title, the run's goal, or the conversation's first prompt.
    pub title: String,
    /// An agent's name in its run, such as `build:add-due`. The list names the row by it, since
    /// the run's heading already says what the work is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The tries of its seat this row stands for, once a retry folded the earlier ones into
    /// it. 0 and 1 both mean it's the only one.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub tries: u32,
    /// With `~` for home.
    pub repo: String,
    /// The run whose group it's in. `None` for conversations and CLI sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<CrewRun>,
    /// The harness and model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<WorkerRef>,
    /// Its latest message, or what it waits for, on one line.
    pub latest: String,
    #[serde(with = "time::serde::rfc3339")]
    pub updated: OffsetDateTime,
    #[serde(default)]
    pub details: CrewDetails,
}

/// A run, as its group's heading names it: `✓ add-due-dates · merged 13m ago · ~$0.23`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrewRun {
    pub id: RunId,
    pub title: String,
    /// The run's own name, such as `add-due-dates`. The heading shows it in place of the title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Where the run stands. A heading without it shows the name alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stands: Option<RunStands>,
}

impl CrewRun {
    pub fn new(id: RunId, title: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            name: None,
            stands: None,
        }
    }
}

/// Where a run stands, for its heading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunStands {
    /// Picks the heading's glyph.
    pub state: CrewState,
    /// In a word or two, such as `merged` or `needs you`.
    pub word: String,
    /// When it got there. The heading says how long ago for a run that ended.
    #[serde(with = "time::serde::rfc3339")]
    pub since: OffsetDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spent: Option<Spend>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// One session as a try of its seat, for [`fold_tries`].
#[derive(Debug, Clone, PartialEq)]
pub struct SeatTry {
    /// What its tries share: its label without the `-2` a later one gets to stay unique.
    pub seat: String,
    pub started: OffsetDateTime,
    /// `None` while it runs.
    pub ended: Option<OffsetDateTime>,
    pub row: CrewRow,
}

/// The seat a label names: the label itself, without the `-2` that keeps a later session's
/// label unique in its run.
pub fn seat_of(label: &str) -> &str {
    match label.rsplit_once('-') {
        Some((seat, n)) if !seat.is_empty() && n.parse::<u32>().is_ok_and(|n| n >= 2) => seat,
        _ => label,
    }
}

/// One row per seat: a session that starts after the last one of its seat ended is a retry,
/// and takes that one's place under the seat's first label, with the count of tries so far. Sessions of a seat that run side
/// by side, such as two reviewers, each keep their row. `tries` come in the order they started.
pub fn fold_tries(tries: Vec<SeatTry>) -> Vec<CrewRow> {
    let mut seats: Vec<SeatTry> = Vec::new();
    for mut next in tries {
        let earlier = seats.iter_mut().rev().find(|seat| {
            seat.seat == next.seat && seat.ended.is_some_and(|ended| ended <= next.started)
        });
        match earlier {
            Some(seat) => {
                next.row.tries = seat.row.tries.max(1) + 1;
                // The seat keeps its first try's label, which has no `-2` to tell it apart.
                next.row.label = seat.row.label.take().or(next.row.label);
                *seat = next;
            }
            None => seats.push(next),
        }
    }
    seats
        .into_iter()
        .map(|seat| {
            let mut row = seat.row;
            if let Some(words) = tries_words(row.state, row.tries) {
                row.latest = match row.state {
                    CrewState::Working | CrewState::Waiting => {
                        format!("{words} · {}", row.latest)
                    }
                    _ => words,
                };
            }
            row
        })
        .collect()
}

/// What a folded row says of its tries, such as `accepted on its 6th try`. A row that asks you
/// something keeps its question, and so does one that stands for a single try.
fn tries_words(state: CrewState, tries: u32) -> Option<String> {
    if tries < 2 {
        return None;
    }
    let nth = ordinal(tries);
    Some(match state {
        CrewState::NeedsYou | CrewState::Ready => return None,
        CrewState::Working | CrewState::Waiting => format!("{nth} try"),
        CrewState::Done => format!("accepted on its {nth} try"),
        CrewState::Stopped => format!("stopped on its {nth} try"),
        CrewState::Failed => format!("failed on its {nth} try"),
    })
}

/// `1st`, `2nd`, `3rd`, `4th`, `11th`, `22nd`.
fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CrewTarget {
    /// One agent's session in a run.
    Agent {
        run: RunId,
        session: SessionKey,
    },
    /// A card that waits on you before any agent works on it.
    Checkpoint {
        run: RunId,
        checkpoint: CheckpointId,
    },
    /// A whole run, such as one in another repo.
    Run {
        run: RunId,
    },
    Conversation {
        chat: ChatId,
    },
    VendorSession {
        session: VendorSessionRef,
    },
}

/// The details pane. The row itself gives the title, state, repo, run, and model.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CrewDetails {
    /// In full.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card: Option<CardId>,
    /// Tokens and cost.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spent: Option<Spend>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextUse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// What it was asked to do.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

/// The panel's rows, from the screen's own sessions and the controller's answer.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CrewPanel {
    /// This screen's repo, with `~` for home. Its group comes first.
    pub repo: String,
    pub rows: Vec<CrewRow>,
}

/// One repo's rows, by run.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoGroup<'a> {
    pub repo: &'a str,
    pub runs: Vec<RunGroup<'a>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunGroup<'a> {
    /// `None` holds the conversations and CLI sessions.
    pub run: Option<&'a CrewRun>,
    pub rows: Vec<&'a CrewRow>,
}

impl CrewPanel {
    /// The count each tab's label shows.
    pub fn count(&self, tab: CrewTab) -> usize {
        self.rows.iter().filter(|row| tab.holds(row.state)).count()
    }

    /// The rows `tab` shows. This repo comes first, then the others with the most urgent row
    /// first, then the newest. Runs and rows within them sort the same way, and a repo's
    /// conversations and CLI sessions come after its runs, so they stay put when a run ends.
    pub fn groups(&self, tab: CrewTab) -> Vec<RepoGroup<'_>> {
        let mut repos: Vec<RepoGroup<'_>> = Vec::new();
        for row in self.rows.iter().filter(|row| tab.holds(row.state)) {
            let at = match repos.iter().position(|group| group.repo == row.repo) {
                Some(at) => at,
                None => {
                    repos.push(RepoGroup {
                        repo: &row.repo,
                        runs: Vec::new(),
                    });
                    repos.len() - 1
                }
            };
            let runs = &mut repos[at].runs;
            let id = row.run.as_ref().map(|run| &run.id);
            match runs
                .iter_mut()
                .find(|group| group.run.map(|run| &run.id) == id)
            {
                Some(group) => group.rows.push(row),
                None => runs.push(RunGroup {
                    run: row.run.as_ref(),
                    rows: vec![row],
                }),
            }
        }
        for repo in &mut repos {
            for group in &mut repo.runs {
                group.rows.sort_by_key(|row| urgency(row));
            }
            repo.runs.sort_by_key(|group| {
                (
                    group.run.is_none(),
                    group.rows.first().map(|row| urgency(row)),
                )
            });
        }
        repos.sort_by_key(|group| {
            let first = group
                .runs
                .iter()
                .filter_map(|run| run.rows.first())
                .map(|row| urgency(row))
                .min();
            (group.repo != self.repo, first)
        });
        repos
    }
}

fn urgency(row: &CrewRow) -> (CrewState, Reverse<OffsetDateTime>) {
    (row.state, Reverse(row.updated))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn at(minute: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_790_000_000 + minute * 60).unwrap()
    }

    fn row(repo: &str, run: Option<&str>, state: CrewState, minute: i64, title: &str) -> CrewRow {
        CrewRow {
            target: CrewTarget::Conversation {
                chat: ChatId::numbered(1),
            },
            state,
            title: title.into(),
            label: None,
            tries: 0,
            repo: repo.into(),
            run: run.map(|id| CrewRun::new(RunId::new(id).unwrap(), format!("Goal of {id}"))),
            worker: None,
            latest: String::new(),
            updated: at(minute),
            details: CrewDetails::default(),
        }
    }

    #[test]
    fn tabs_cycle_and_hold_their_states() {
        assert_eq!(CrewTab::All.next(), CrewTab::NeedsYou);
        assert_eq!(CrewTab::NeedsYou.previous(), CrewTab::All);
        assert_eq!(CrewTab::Working.next(), CrewTab::Ready);
        let labels: Vec<&str> = CrewTab::ORDER.iter().map(|tab| tab.label()).collect();
        assert_eq!(labels, ["Needs you", "Working", "Ready", "Done", "All"]);
        assert!(CrewTab::Working.holds(CrewState::Waiting));
        assert!(CrewTab::Done.holds(CrewState::Failed));
        assert!(!CrewTab::Ready.holds(CrewState::Done));
        assert_eq!(CrewTab::default(), CrewTab::All);
    }

    #[test]
    fn rows_group_by_repo_then_run_most_urgent_first() {
        let panel = CrewPanel {
            repo: "~/app".into(),
            rows: vec![
                row(
                    "~/other",
                    Some("run-b"),
                    CrewState::NeedsYou,
                    9,
                    "other needs you",
                ),
                row("~/app", None, CrewState::Ready, 8, "a chat"),
                row("~/app", Some("run-a"), CrewState::Done, 7, "done task"),
                row("~/app", Some("run-a"), CrewState::Working, 1, "older work"),
                row("~/app", Some("run-a"), CrewState::Working, 5, "newer work"),
                row("~/app", None, CrewState::Done, 2, "old session"),
            ],
        };
        assert_eq!(panel.count(CrewTab::All), 6);
        assert_eq!(panel.count(CrewTab::Working), 2);
        assert_eq!(panel.count(CrewTab::NeedsYou), 1);

        let groups = panel.groups(CrewTab::All);
        let shape: Vec<(&str, Option<&str>, Vec<&str>)> = groups
            .iter()
            .flat_map(|repo| {
                repo.runs.iter().map(|group| {
                    (
                        repo.repo,
                        group.run.map(|run| run.id.as_str()),
                        group.rows.iter().map(|row| row.title.as_str()).collect(),
                    )
                })
            })
            .collect();
        assert_eq!(
            shape,
            [
                (
                    "~/app",
                    Some("run-a"),
                    vec!["newer work", "older work", "done task"]
                ),
                ("~/app", None, vec!["a chat", "old session"]),
                ("~/other", Some("run-b"), vec!["other needs you"]),
            ]
        );
        let done = panel.groups(CrewTab::Done);
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].runs.len(), 2);
    }

    #[test]
    fn conversations_stay_under_a_finished_run() {
        let panel = CrewPanel {
            repo: "~/app".into(),
            rows: vec![
                row("~/app", None, CrewState::Ready, 8, "a chat"),
                row("~/app", Some("run-a"), CrewState::Done, 7, "done task"),
            ],
        };
        let groups = panel.groups(CrewTab::All);
        let runs: Vec<Option<&str>> = groups[0]
            .runs
            .iter()
            .map(|group| group.run.map(|run| run.id.as_str()))
            .collect();
        assert_eq!(runs, [Some("run-a"), None]);
    }

    #[test]
    fn rows_and_targets_round_trip() {
        let value = json!({
            "repo": "~/app",
            "rows": [{
                "target": {"kind": "vendor_session",
                    "session": {"vendor": "codex", "id": "019a2f"}},
                "state": "ready",
                "title": "Explain the input handling",
                "repo": "~/app",
                "worker": "codex/gpt-6.1-sol",
                "latest": "It reads keys in one loop.",
                "updated": "2026-09-21T14:13:20Z",
                "details": {"branch": "main", "prompt": "Explain the input handling",
                    "context": {"used": 1200, "cap": 200000}}
            }, {
                "target": {"kind": "agent", "run": "run-a", "session": "s-001"},
                "state": "needs_you",
                "title": "Add the login form",
                "repo": "~/app",
                "run": {"id": "run-a", "title": "Login"},
                "latest": "Waiting for your approval of the plan",
                "updated": "2026-09-21T14:13:20Z",
                "details": {"card": "task-002", "spent": {"input_tokens": 10, "output_tokens": 2,
                    "cached_tokens": 0, "cost_usd": 0.5, "cost_partial": false,
                    "cost_estimated": false}}
            }]
        });
        let panel: CrewPanel = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&panel).unwrap(), value);
        for target in [
            json!({"kind": "checkpoint", "run": "run-a", "checkpoint": "cp-001"}),
            json!({"kind": "run", "run": "run-a"}),
            json!({"kind": "conversation", "chat": "chat-001"}),
        ] {
            let parsed: CrewTarget = serde_json::from_value(target.clone()).unwrap();
            assert_eq!(serde_json::to_value(&parsed).unwrap(), target);
        }
    }

    fn seat_try(label: &str, state: CrewState, started: i64, ended: Option<i64>) -> SeatTry {
        let mut row = row("~/app", Some("run-a"), state, started, label);
        row.label = Some(label.to_owned());
        row.latest = "Drafting the plan".to_owned();
        SeatTry {
            seat: seat_of(label).to_owned(),
            started: at(started),
            ended: ended.map(at),
            row,
        }
    }

    #[test]
    fn a_seat_s_retries_fold_into_its_latest_row() {
        let rows = fold_tries(vec![
            seat_try("plan:orchestrator", CrewState::Failed, 0, Some(1)),
            seat_try("plan:orchestrator-2", CrewState::Failed, 1, Some(2)),
            seat_try("plan:orchestrator-3", CrewState::Done, 2, Some(3)),
            seat_try("build:add-due", CrewState::Done, 3, Some(4)),
        ]);
        let shown: Vec<(Option<&str>, u32, &str)> = rows
            .iter()
            .map(|row| (row.label.as_deref(), row.tries, row.latest.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                (Some("plan:orchestrator"), 3, "accepted on its 3rd try"),
                (Some("build:add-due"), 0, "Drafting the plan"),
            ]
        );
    }

    #[test]
    fn a_retry_still_running_says_which_try_it_is() {
        let rows = fold_tries(vec![
            seat_try("build:flag", CrewState::Failed, 0, Some(1)),
            seat_try("build:flag", CrewState::Working, 2, None),
        ]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].latest, "2nd try · Drafting the plan");

        let rows = fold_tries(vec![
            seat_try("build:flag", CrewState::Failed, 0, Some(1)),
            seat_try("build:flag", CrewState::Failed, 2, Some(3)),
        ]);
        assert_eq!(rows[0].latest, "failed on its 2nd try");
    }

    #[test]
    fn agents_of_a_seat_that_run_side_by_side_keep_their_rows() {
        let rows = fold_tries(vec![
            seat_try("review:flag", CrewState::Working, 0, None),
            seat_try("review:flag-2", CrewState::Done, 0, Some(2)),
        ]);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.tries == 0));
    }

    #[test]
    fn a_seat_is_its_label_without_the_number_that_keeps_it_unique() {
        assert_eq!(seat_of("plan:orchestrator-2"), "plan:orchestrator");
        assert_eq!(seat_of("plan:orchestrator"), "plan:orchestrator");
        assert_eq!(seat_of("build:add-due"), "build:add-due");
        assert_eq!(seat_of("fix:issue-1"), "fix:issue-1");
        assert_eq!(seat_of("-2"), "-2");
        assert_eq!(
            [1, 2, 3, 4, 11, 12, 13, 21, 22, 103, 111].map(ordinal),
            [
                "1st", "2nd", "3rd", "4th", "11th", "12th", "13th", "21st", "22nd", "103rd",
                "111th"
            ]
        );
    }
}
