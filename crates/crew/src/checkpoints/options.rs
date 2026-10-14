//! Each checkpoint kind's options, in order, with their keys. The screen draws what the engine
//! emits and nothing else, so the table lives here and only here.

use argonauts_core::events::checkpoint::{AnswerAction, AnswerOption, CheckpointKind};
use argonauts_core::events::goal::{GoalQuestion, LET_THE_LEAD_ANSWER, LET_THE_LEAD_DECIDE};
use argonauts_core::events::plan::JudgingResult;
use argonauts_core::events::vendor::HandoffPlan;
use argonauts_core::limits::{Candidate, CostEstimate};
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::protocol::JudgingMethod;

pub(crate) const TYPE_MY_OWN_ANSWER: &str = "Type my own answer";

fn option(key: char, label: &str, action: AnswerAction) -> AnswerOption {
    AnswerOption {
        key,
        label: label.to_owned(),
        action,
        recommended: false,
        detail: None,
    }
}

/// Answers take `a`, `b`, `c` in order. Past `z` the number keys still pick.
fn letter(index: usize) -> char {
    char::from_u32(u32::from('a') + index as u32)
        .filter(char::is_ascii_lowercase)
        .unwrap_or('z')
}

/// The fixed options of a kind whose options don't depend on its content. A question or a
/// handoff built from its content uses [`goal_question`] or [`handoff`]; here a question gets
/// only its own-answer line and a handoff only the options every handoff card has.
pub fn standard(kind: CheckpointKind) -> Vec<AnswerOption> {
    use AnswerAction as A;
    match kind {
        // Choose-only: your own words go in the chat, after Esc sets the card aside.
        CheckpointKind::Goal => vec![
            option('y', "Yes, run it", A::Approve),
            option(
                'l',
                LET_THE_LEAD_ANSWER,
                A::Choose {
                    value: LET_THE_LEAD_ANSWER.to_owned(),
                },
            ),
            option('b', "Send back for another round", A::SendBack),
            option('d', "Show the goal", A::ShowDiff),
            option('x', "No, stop here", A::Abandon),
        ],
        CheckpointKind::Plan => vec![
            option('y', "Approve the plan", A::Approve),
            option('e', "Approve with edits", A::ApproveWithEdits),
            option('b', "Send back", A::SendBack),
            option('s', "Run it solo", A::SwitchToSolo),
            option('x', "Abandon", A::Abandon),
            option('d', "Show the plan", A::ShowDiff),
        ],
        CheckpointKind::Merge => vec![
            option('y', "Merge", A::Merge),
            option('p', "Open a pull request", A::PullRequest),
            option('b', "Request changes on some tasks", A::RequestChanges),
            option('k', "Keep the branch", A::KeepBranch),
            option('x', "Abandon the run", A::Abandon),
            option('d', "Show the diff", A::ShowDiff),
        ],
        CheckpointKind::Handoff => handoff_tail(true),
        CheckpointKind::VendorDown => vec![
            option('a', "One agent for every paused task", A::OneForAll),
            option('e', "Choose per task", A::PerTask),
            option('w', "Wait for the agent", A::Wait),
        ],
        CheckpointKind::Escalation => vec![
            option('r', "Retry with a note", A::Retry),
            option('h', "Hand it off", A::HandOff),
            option('s', "Split", A::Split),
            option('t', "Take over myself", A::TakeOver),
            option('f', "Defer it", A::Defer),
            option('x', "Cancel the task", A::Cancel),
            option('d', "Show the details", A::ShowDiff),
        ],
        CheckpointKind::Budget => vec![
            option('m', "More limit", A::MoreBudget),
            option('h', "Hand it off", A::HandOff),
            option('t', "Take over myself", A::TakeOver),
            option('x', "Stop and ask me", A::Stop),
            option('d', "Show the details", A::ShowDiff),
        ],
        // Declining sends it back, and a note added with Tab carries your reason.
        CheckpointKind::ChangeRequest => vec![
            option('y', "Accept the change", A::Approve),
            option('n', "Decline it", A::SendBack),
            option('d', "Show the details", A::ShowDiff),
        ],
        CheckpointKind::GoalChange => vec![
            option('y', "Accept the change", A::Approve),
            option('n', "Keep the goal", A::SendBack),
            option('d', "Show the details", A::ShowDiff),
        ],
        CheckpointKind::WorkflowPromotion => vec![
            option('y', "Promote the workflow", A::Approve),
            option('n', "Keep it a candidate", A::SendBack),
            option('d', "Show the details", A::ShowDiff),
        ],
        CheckpointKind::TakeOverReturn => vec![
            option('c', "Let the worker continue", A::LetContinue),
            option('t', "Keep it mine", A::TakeOver),
            option('d', "Show the diff", A::ShowDiff),
        ],
        CheckpointKind::Question => vec![option('a', TYPE_MY_OWN_ANSWER, A::Answer)],
        CheckpointKind::OriginalReturned => original_returned_options(),
    }
}

/// One goal question: its answers `a`, `b`, `c`, the recommended one first and marked, then
/// "Let the lead decide this and the rest" on the next letter. No answer is typed.
pub fn goal_question(question: &GoalQuestion) -> Vec<AnswerOption> {
    let mut answers: Vec<_> = question.options.iter().collect();
    // Stable, so the others keep the order the differ gave them.
    answers.sort_by_key(|answer| !answer.recommended);
    let mut options: Vec<AnswerOption> = answers
        .iter()
        .enumerate()
        .map(|(index, answer)| AnswerOption {
            key: letter(index),
            label: answer.label.clone(),
            action: AnswerAction::Choose {
                value: answer.label.clone(),
            },
            recommended: answer.recommended,
            detail: answer.basis.clone(),
        })
        .collect();
    options.push(option(
        letter(options.len()),
        LET_THE_LEAD_DECIDE,
        AnswerAction::Choose {
            value: LET_THE_LEAD_DECIDE.to_owned(),
        },
    ));
    options
}

/// An owner's question to you: one option per offered answer, then "Type my own answer".
pub fn question(offered: &[String]) -> Vec<AnswerOption> {
    let mut options: Vec<AnswerOption> = offered
        .iter()
        .enumerate()
        .map(|(index, label)| {
            option(
                letter(index),
                label,
                AnswerAction::Choose {
                    value: label.clone(),
                },
            )
        })
        .collect();
    options.push(option(
        letter(options.len()),
        TYPE_MY_OWN_ANSWER,
        AnswerAction::Answer,
    ));
    options
}

/// An agent's question the lead answered in full auto, for the approver to check before the
/// answer goes: send it, recommended, or your own answer.
pub fn lead_answer(lead: &str) -> Vec<AnswerOption> {
    let mut send = option('y', &format!("Send {lead}'s answer"), AnswerAction::Approve);
    send.recommended = true;
    vec![send, option('a', TYPE_MY_OWN_ANSWER, AnswerAction::Answer)]
}

/// Picking the plan after a near tie or with `judging.method: human`: one option per plan,
/// `a`, `b`, `c`, the leader first and marked, each with its scores. Its value is the label.
pub fn plan_pick(judged: &JudgingResult) -> Vec<AnswerOption> {
    let mut plans: Vec<_> = judged.plans.iter().collect();
    // Stable, so plans with equal means keep their label order.
    plans.sort_by(|a, b| b.mean.total_cmp(&a.mean));
    let scored = judged.method != JudgingMethod::Human;
    plans
        .iter()
        .enumerate()
        .map(|(index, tally)| AnswerOption {
            key: letter(index),
            label: format!("Plan {}", tally.label),
            action: AnswerAction::Choose {
                value: tally.label.to_string(),
            },
            recommended: scored && index == 0,
            detail: scored.then(|| {
                format!(
                    "mean {:.1}, spread {:.1}, {} judge{}",
                    tally.mean,
                    tally.spread,
                    tally.counted,
                    if tally.counted == 1 { "" } else { "s" }
                )
            }),
        })
        .collect()
}

/// Why the handoff card opens, which decides its first option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffCause {
    UsageLimit,
    Outage,
    /// Starts with "I logged in, retry" (`r`).
    LoggedOut,
    /// Starts with "I topped up, retry" (`r`).
    BillingCap,
}

/// The handoff card: the retry when it applies, each candidate `a`, `b`, `c` with the
/// recommended one first, take over, wait, pick another agent, show the diff, remember.
pub fn handoff(plan: &HandoffPlan, cause: HandoffCause) -> Vec<AnswerOption> {
    let mut options = Vec::new();
    match cause {
        HandoffCause::LoggedOut => {
            options.push(option('r', "I logged in, retry", AnswerAction::Retry));
        }
        HandoffCause::BillingCap => {
            options.push(option('r', "I topped up, retry", AnswerAction::Retry));
        }
        HandoffCause::UsageLimit | HandoffCause::Outage => {}
    }
    // The planner lists candidates best first, so the first is the recommended one.
    options.extend(
        plan.candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| AnswerOption {
                key: letter(index),
                label: candidate.worker.to_string(),
                action: AnswerAction::Candidate {
                    worker: candidate.worker.clone(),
                },
                recommended: index == 0,
                detail: Some(candidate_detail(candidate)),
            }),
    );
    options.extend(handoff_tail(plan.resets_at.is_some()));
    options
}

/// The handoff card's options after the candidates. Waiting is for the reset when one is known,
/// else the card tries again after a while, as a sign-in error or an outage has no reset.
fn handoff_tail(resets: bool) -> Vec<AnswerOption> {
    let wait = if resets {
        "Wait until the reset"
    } else {
        "Wait and try again later"
    };
    vec![
        option('t', "Take over myself", AnswerAction::TakeOver),
        option('w', wait, AnswerAction::Wait),
        option('o', "Pick another agent", AnswerAction::OtherCandidates),
        option('d', "Show the diff", AnswerAction::ShowDiff),
        // A toggle the screen keeps; the candidate you then pick carries `remember`.
        option(
            'm',
            "Remember this pick",
            AnswerAction::Choose {
                value: "remember".to_owned(),
            },
        ),
    ]
}

fn candidate_detail(candidate: &Candidate) -> String {
    let cost = match candidate.cost {
        CostEstimate::Usd(usd) => format!("about ${usd:.2}"),
        CostEstimate::Quota(share) => format!("about {:.0}% of its window", share * 100.0),
        CostEstimate::Free => "free".to_owned(),
        CostEstimate::Unknown => "cost unknown".to_owned(),
    };
    let mut detail = format!("{} left · {cost}", candidate.quota_text);
    if let Some(record) = candidate.track_record {
        detail.push_str(&format!(" · passed {} of {}", record.passed, record.of));
    }
    if candidate.needs_confirm {
        detail.push_str(" · paid API");
    }
    if candidate.may_run_out {
        detail.push_str(" · may run out");
    }
    detail
}

/// The "original is back" card: review `r`, give back `o`, redo `u`, keep `y`, take
/// over `t`, show the diff `d`.
pub fn original_returned(original: &WorkerRef, replacement: &WorkerRef) -> Vec<AnswerOption> {
    let mut options = original_returned_options();
    for option in &mut options {
        option.detail = match option.action {
            AnswerAction::UpgradeReview | AnswerAction::HandBack | AnswerAction::Redo => {
                Some(original.to_string())
            }
            AnswerAction::KeepReplacement => Some(replacement.to_string()),
            _ => None,
        };
    }
    options
}

fn original_returned_options() -> Vec<AnswerOption> {
    use AnswerAction as A;
    vec![
        option('r', "Upgrade review", A::UpgradeReview),
        option('o', "Hand back", A::HandBack),
        option('u', "Redo", A::Redo),
        option('y', "Keep the fallback", A::KeepReplacement),
        option('t', "Take over myself", A::TakeOver),
        option('d', "Show the diff", A::ShowDiff),
    ]
}

/// The merge card while a card carries quality debt and its original is out: the merge
/// options, with Merge as "Merge anyway", then wait `w` and review with a candidate at least as
/// strong `a` when there is one, before Show the diff.
pub fn merge_with_debt(stronger: Option<&WorkerRef>) -> Vec<AnswerOption> {
    let mut options = standard(CheckpointKind::Merge);
    if let Some(merge) = options.first_mut() {
        merge.label = "Merge anyway".to_owned();
    }
    let show = options.pop();
    options.push(option('w', "Wait", AnswerAction::Wait));
    if let Some(stronger) = stronger {
        let mut upgrade = option('a', "Upgrade review", AnswerAction::UpgradeReview);
        upgrade.detail = Some(stronger.to_string());
        options.push(upgrade);
    }
    options.extend(show);
    options
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(options: &[AnswerOption]) -> String {
        options.iter().map(|option| option.key).collect()
    }

    /// Every kind's options in order with their letters.
    #[test]
    fn card_options_keep_their_order_and_keys() {
        let table = [
            (CheckpointKind::Goal, "ylbdx"),
            (CheckpointKind::Plan, "yebsxd"),
            (CheckpointKind::Merge, "ypbkxd"),
            (CheckpointKind::Handoff, "twodm"),
            (CheckpointKind::VendorDown, "aew"),
            (CheckpointKind::Escalation, "rhstfxd"),
            (CheckpointKind::Budget, "mhtxd"),
            (CheckpointKind::ChangeRequest, "ynd"),
            (CheckpointKind::GoalChange, "ynd"),
            (CheckpointKind::WorkflowPromotion, "ynd"),
            (CheckpointKind::TakeOverReturn, "ctd"),
            (CheckpointKind::Question, "a"),
            (CheckpointKind::OriginalReturned, "rouytd"),
        ];
        for (kind, expected) in table {
            assert_eq!(keys(&standard(kind)), expected, "{kind:?}");
        }
        let labels = |kind| {
            standard(kind)
                .into_iter()
                .map(|option| option.label)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            labels(CheckpointKind::Plan),
            [
                "Approve the plan",
                "Approve with edits",
                "Send back",
                "Run it solo",
                "Abandon",
                "Show the plan"
            ]
        );
        assert_eq!(
            labels(CheckpointKind::Merge),
            [
                "Merge",
                "Open a pull request",
                "Request changes on some tasks",
                "Keep the branch",
                "Abandon the run",
                "Show the diff"
            ]
        );
        assert_eq!(
            standard(CheckpointKind::Plan)[3].action,
            AnswerAction::SwitchToSolo
        );
        assert_eq!(
            standard(CheckpointKind::Merge)[0].action,
            AnswerAction::Merge
        );

        // Letters keep their meaning across cards.
        for kind in table.map(|(kind, _)| kind) {
            for option in standard(kind) {
                match option.key {
                    'd' => assert_eq!(option.action, AnswerAction::ShowDiff, "{kind:?}"),
                    't' => assert_eq!(option.action, AnswerAction::TakeOver, "{kind:?}"),
                    'w' => assert_eq!(option.action, AnswerAction::Wait, "{kind:?}"),
                    'e' if kind != CheckpointKind::VendorDown => {
                        assert_eq!(option.action, AnswerAction::ApproveWithEdits, "{kind:?}")
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn goal_question_puts_the_recommended_answer_first() {
        use argonauts_core::events::goal::ChoiceOption;
        use argonauts_core::steps::goal::DiffTag;
        let answer = |key, label: &str, recommended| ChoiceOption {
            key,
            label: label.to_owned(),
            recommended,
            basis: None,
        };
        let question = GoalQuestion {
            n: 1,
            of: 1,
            text: "Per user or per key?".to_owned(),
            options: vec![
                answer('a', "Per user", false),
                answer('b', "Per API key", true),
            ],
            tag: DiffTag::Scope,
            topic: None,
        };
        let options = goal_question(&question);
        assert_eq!(keys(&options), "abc");
        assert_eq!(options[0].label, "Per API key");
        assert!(options[0].recommended);
        assert!(!options[1].recommended);
        assert_eq!(options[2].label, LET_THE_LEAD_DECIDE);
        assert!(
            options
                .iter()
                .all(|option| option.action != AnswerAction::Answer)
        );
    }

    #[test]
    fn handoff_starts_with_the_retry_after_a_logout() {
        let plan: HandoffPlan = serde_json::from_value(serde_json::json!({
            "card": "task-001", "title": "t", "stopped": "claude-code/opus",
            "reason": "logged out", "resets_at": null,
            "work": {"files_changed": 0, "insertions": 0, "deletions": 0}, "gates_ran": false,
            "candidates": [], "mode": "ask"
        }))
        .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(keys(&handoff(&plan, HandoffCause::LoggedOut)), "rtwodm");
        assert_eq!(keys(&handoff(&plan, HandoffCause::UsageLimit)), "twodm");
    }

    /// Waiting is "until the reset" only when one is known; a sign-in error or an outage has
    /// none, so the card waits and tries again.
    #[test]
    fn a_handoff_waits_for_the_reset_only_when_one_is_known() {
        let plan = |resets_at: serde_json::Value| -> HandoffPlan {
            serde_json::from_value(serde_json::json!({
                "card": "task-001", "title": "t", "stopped": "claude-code/opus",
                "reason": "a sign-in error", "resets_at": resets_at,
                "work": {"files_changed": 0, "insertions": 0, "deletions": 0},
                "gates_ran": false, "candidates": [], "mode": "ask"
            }))
            .unwrap_or_else(|err| panic!("{err}"))
        };
        let wait = |plan: &HandoffPlan| {
            handoff(plan, HandoffCause::Outage)
                .into_iter()
                .find(|option| option.action == AnswerAction::Wait)
                .map(|option| option.label)
        };
        assert_eq!(
            wait(&plan(serde_json::Value::Null)).as_deref(),
            Some("Wait and try again later")
        );
        assert_eq!(
            wait(&plan(serde_json::json!("2026-10-04T18:00:00Z"))).as_deref(),
            Some("Wait until the reset")
        );
    }

    #[test]
    fn merge_with_debt_adds_wait_and_upgrade_before_the_diff() {
        let stronger = WorkerRef::new("codex", Some("gpt-5".to_owned()));
        assert_eq!(keys(&merge_with_debt(Some(&stronger))), "ypbkxwad");
        assert_eq!(keys(&merge_with_debt(None)), "ypbkxwd");
        assert_eq!(merge_with_debt(None)[0].label, "Merge anyway");
    }

    #[test]
    fn a_plan_pick_puts_the_leader_first() {
        use argonauts_core::events::plan::PlanTally;
        let tally = |label, mean| PlanTally {
            label,
            author: "codex".parse().unwrap(),
            mean,
            spread: 0.5,
            counted: 2,
            dropped: 0,
        };
        let mut judged = JudgingResult {
            method: JudgingMethod::Cross,
            plans: vec![tally(1, 7.1), tally(2, 7.4), tally(3, 7.4)],
            winner: None,
            margin: 0.0,
            near_tie: true,
        };
        let options = plan_pick(&judged);
        assert_eq!(keys(&options), "abc");
        assert_eq!(options[0].label, "Plan 2");
        assert!(options[0].recommended && !options[1].recommended);
        assert_eq!(
            options[2].action,
            AnswerAction::Choose {
                value: "1".to_owned()
            }
        );
        assert_eq!(
            options[0].detail.as_deref(),
            Some("mean 7.4, spread 0.5, 2 judges")
        );
        judged.method = JudgingMethod::Human;
        assert!(
            plan_pick(&judged)
                .iter()
                .all(|o| !o.recommended && o.detail.is_none())
        );
    }
}
