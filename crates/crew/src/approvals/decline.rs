//! "Carry on without me", the last row of a question or an approval that waits on you: an
//! option is taken in your place at once, as `approvals.when_declined` says, and recorded as
//! yours to review.
//!
//! - `decide`: whoever asked decides and carries on. An agent's question goes back to the agent,
//!   which is told to say what it assumed. A goal's questions go to the main agent. A goal or a
//!   plan goes ahead as it was written, since the merge approval still comes. Anything else
//!   takes what ends that part safely and keeps its work, so nothing merges because you looked
//!   away.
//! - `stop`: that part ends, with its work kept.
//!
//! Either way the run's closing answer lists what was left and what was taken. The one question
//! nothing is taken on is the draft pull request's, whose yes posts on GitHub as you: it waits
//! for your answer.

use argonauts_core::base::names::ApprovalId;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::approval::{
    AnswerAction, AnswerOption, ApprovalDecided, ApprovalEvent, ApprovalKind, ApprovalOpened,
    LEFT_TO_THE_AGENT,
};
use argonauts_core::runs::events::goal::LET_THE_MAIN_AGENT_DECIDE;
use argonauts_core::runs::events::run::Party;
use argonauts_core::settings::asking::WhenDeclined;
use argonauts_core::{Note, NoteLevel};

use super::autopilot::{self, Held};
use super::lock;
use crate::engine::host::Host;
use crate::follow::draft;
use crate::workflow::change;

/// What an agent reads when its question was left and `approvals.when_declined` is `stop`.
pub const STOP_HERE: &str = "The user won't answer this and wants this part to end here. Stop, \
                             and report what is done and what is left.";

/// What ends a part and keeps its work, the first one a question or an approval offers.
/// Abandoning comes last, since it ends the whole run.
const ENDINGS: &[AnswerAction] = &[
    AnswerAction::Defer,
    AnswerAction::KeepBranch,
    AnswerAction::KeepFallback,
    AnswerAction::Stop,
    AnswerAction::Cancel,
    AnswerAction::Abandon,
];

/// Takes an option on the open approval `id` in your place. Refused when it is already
/// decided or withdrawn, or offers nothing that lets the work go on or end.
pub(super) fn left(host: &Host, id: &ApprovalId) -> Result<(), String> {
    let desk = &host.shared().desk;
    let _deciding = lock(&desk.deciding);
    let record = host
        .read(|state| state.approvals.by_id.get(id).cloned())
        .ok_or_else(|| format!("no approval {id}"))?;
    if record.decided.is_some() {
        return Err("already decided".to_owned());
    }
    if let Some(reason) = &record.withdrawn {
        return Err(format!("withdrawn: {reason}"));
    }
    let opened = &record.opened;
    if draft::asks(opened) {
        return Err("only your own answer posts on GitHub".to_owned());
    }
    // What the question said it would do when it opened, which its last row told you.
    let (option, text) = taken(opened, opened.when_declined)
        .ok_or_else(|| "nothing it offers lets the work go on without you".to_owned())?;
    let task = opened.task.as_ref();
    let line = Note {
        level: NoteLevel::Info,
        text: line(opened, &option, text.as_deref()),
    };
    host.record(task, None, RunEventKind::Note(line))
        .map_err(|err| err.to_string())?;
    let decided = ApprovalDecided {
        id: id.clone(),
        action: option.action,
        text,
        by: Party::Declined,
        evidence_sha256: opened.evidence_sha256.clone(),
        auto_preview: None,
        tasks: Vec::new(),
    };
    host.record(
        task,
        None,
        RunEventKind::Approval(ApprovalEvent::Decided(decided.clone())),
    )
    .map_err(|err| err.to_string())?;
    // Before waking the waiter, so it finds the change already recorded.
    change::on_decided(host, opened, &decided);
    desk.wake(&decided);
    Ok(())
}

/// The option taken in your place, with the text an answer carries.
pub fn taken(
    opened: &ApprovalOpened,
    when: WhenDeclined,
) -> Option<(AnswerOption, Option<String>)> {
    match when {
        WhenDeclined::Decide => carry_on(opened).or_else(|| ending(opened)),
        WhenDeclined::Stop => ending(opened),
    }
}

fn carry_on(opened: &ApprovalOpened) -> Option<(AnswerOption, Option<String>)> {
    let options = &opened.options;
    let offered = |action: AnswerAction| options.iter().find(|option| option.action == action);
    let chosen = |value: &str| {
        options.iter().find(
            |option| matches!(&option.action, AnswerAction::Choose { value: v } if v == value),
        )
    };
    // A goal's question goes to the main agent, with the rest of them. The go-ahead's "and
    // let the main agent answer" is more than carrying on without you asks for, so that one is
    // only approved.
    if let Some(option) = chosen(LET_THE_MAIN_AGENT_DECIDE) {
        return Some((option.clone(), None));
    }
    // The agent that asked decides for itself, and says what it assumed.
    if opened.kind == ApprovalKind::Question
        && let Some(option) = offered(AnswerAction::Answer)
    {
        return Some((option.clone(), Some(LEFT_TO_THE_AGENT.to_owned())));
    }
    // A goal or a plan goes ahead as written. What comes of it still has to pass its checks,
    // its reviews and the merge approval.
    if matches!(opened.kind, ApprovalKind::Understand | ApprovalKind::Plan)
        && let Some(option) = offered(AnswerAction::Approve)
    {
        return Some((option.clone(), None));
    }
    // The rest takes what autopilot would with no one to ask: the recommended agent for a
    // fallback, the safe ending for a merge, and so on.
    autopilot::choice(opened, Held::Undecided)
        .map(|(option, text)| (option, text.map(|_| LEFT_TO_THE_AGENT.to_owned())))
}

/// `stop`: the first ending the question offers. An agent's question offers none, so its agent
/// is told to stop and report.
fn ending(opened: &ApprovalOpened) -> Option<(AnswerOption, Option<String>)> {
    let offered = |action: &AnswerAction| {
        opened
            .options
            .iter()
            .find(|option| option.action == *action)
    };
    if let Some(option) = ENDINGS.iter().find_map(offered) {
        return Some((option.clone(), None));
    }
    offered(&AnswerAction::Answer).map(|option| (option.clone(), Some(STOP_HERE.to_owned())))
}

/// The transcript's line for what was taken, which the closing answer repeats.
pub fn line(opened: &ApprovalOpened, option: &AnswerOption, text: Option<&str>) -> String {
    let title = opened.title.trim();
    match text {
        Some(LEFT_TO_THE_AGENT) => {
            format!("You left \"{title}\" to the agent, which decides and says what it assumed.")
        }
        Some(STOP_HERE) => format!("You left \"{title}\", so its agent stops and reports."),
        _ => format!(
            "You left \"{title}\", so it took \"{}\" without you.",
            option.label.trim()
        ),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::runs::events::approval::{ApprovalPacket, FreeText};
    use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};

    use super::*;
    use crate::approvals::options;

    fn opened(kind: ApprovalKind, options: Vec<AnswerOption>) -> ApprovalOpened {
        ApprovalOpened {
            id: ApprovalId::numbered(1),
            kind,
            title: "Question from build:store".into(),
            task: None,
            packet: ApprovalPacket::WorkflowPromotion(serde_json::json!({})),
            evidence_sha256: "0".repeat(64),
            mode: ApprovalMode::Supervised,
            decide_with: DecideWith::Rules,
            when_declined: WhenDeclined::Decide,
            options,
            free_text: FreeText::None,
            needs_you: true,
        }
    }

    fn standard(kind: ApprovalKind) -> ApprovalOpened {
        opened(kind, options::standard(kind))
    }

    fn action(kind: ApprovalKind, when: WhenDeclined) -> Option<AnswerAction> {
        taken(&standard(kind), when).map(|(option, _)| option.action)
    }

    /// Carrying on without you hands a question back to the agent that asked, and it is told
    /// to say what it assumed. Nothing waits.
    #[test]
    fn a_question_left_goes_back_to_its_agent() {
        let question = standard(ApprovalKind::Question);
        let (option, text) = taken(&question, WhenDeclined::Decide).expect("taken");
        assert_eq!(option.action, AnswerAction::Answer);
        assert_eq!(text.as_deref(), Some(LEFT_TO_THE_AGENT));
        assert_eq!(
            line(&question, &option, text.as_deref()),
            "You left \"Question from build:store\" to the agent, which decides and says what \
             it assumed."
        );
        // With `stop`, its agent is told to end there.
        let (option, text) = taken(&question, WhenDeclined::Stop).expect("taken");
        assert_eq!(option.action, AnswerAction::Answer);
        assert_eq!(text.as_deref(), Some(STOP_HERE));
    }

    /// A goal's questions go to the main agent when the question offers that.
    #[test]
    fn a_goal_question_left_goes_to_the_main_agent() {
        let choose = |value: &str| AnswerOption {
            key: 'a',
            label: value.to_owned(),
            action: AnswerAction::Choose {
                value: value.to_owned(),
            },
            recommended: false,
            detail: None,
        };
        let goal = opened(
            ApprovalKind::Understand,
            vec![
                choose("Per API key"),
                choose("Per user"),
                choose(LET_THE_MAIN_AGENT_DECIDE),
            ],
        );
        let (option, text) = taken(&goal, WhenDeclined::Decide).expect("taken");
        assert_eq!(
            option.action,
            AnswerAction::Choose {
                value: LET_THE_MAIN_AGENT_DECIDE.to_owned()
            }
        );
        assert_eq!(text, None);
    }

    /// A goal or a plan goes ahead as written, and nothing merges because you looked away: a
    /// merge keeps its branch.
    #[test]
    fn an_approval_left_goes_on_and_a_merge_never_lands() {
        use ApprovalKind as Kind;
        let decide = |kind| action(kind, WhenDeclined::Decide);
        assert_eq!(decide(Kind::Understand), Some(AnswerAction::Approve));
        assert_eq!(decide(Kind::Plan), Some(AnswerAction::Approve));
        assert_eq!(decide(Kind::Merge), Some(AnswerAction::KeepBranch));
        for kind in [Kind::Merge, Kind::Limit, Kind::Stuck, Kind::Fallback] {
            let took = decide(kind).unwrap_or_else(|| panic!("{kind:?} waits"));
            assert!(
                !matches!(took, AnswerAction::Merge | AnswerAction::PullRequest),
                "{kind:?} took {took:?}"
            );
        }
    }

    /// `stop` ends that part with its work kept, and never approves.
    #[test]
    fn stop_ends_the_part_and_approves_nothing() {
        use ApprovalKind as Kind;
        let stop = |kind| action(kind, WhenDeclined::Stop);
        assert_eq!(stop(Kind::Merge), Some(AnswerAction::KeepBranch));
        for kind in [Kind::Understand, Kind::Plan, Kind::Merge, Kind::Stuck] {
            let took = stop(kind).unwrap_or_else(|| panic!("{kind:?} waits"));
            assert!(
                !matches!(took, AnswerAction::Approve | AnswerAction::Merge),
                "{kind:?} took {took:?}"
            );
        }
    }
}
