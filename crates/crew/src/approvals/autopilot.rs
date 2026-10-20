//! Autopilot: nothing waits for you. An approval your rules and the judge couldn't approve takes
//! the option that ends that part safely and keeps its work, one a hard limit stopped does what its
//! `at_hard_limit` says, and a question takes the crew's recommended answer. The transcript says
//! what was chosen and why, in place of a question. A run never ends here, and never waits on a
//! goal or a plan: once another round can't change one, a goal goes ahead as the main agent wrote
//! it and a plan runs as a checked Captain run. Only what you set to wait for you waits: a hard
//! limit set to wait, or a plan or merge your settings keep for you at the terminal.

use argonauts_core::events::RunEventKind;
use argonauts_core::events::approval::{
    AnswerAction, AnswerOption, ApprovalDecided, ApprovalEvent, ApprovalKind, ApprovalOpened,
    ApprovalPacket,
};
use argonauts_core::events::goal::{LET_THE_MAIN_AGENT_ANSWER, LET_THE_MAIN_AGENT_DECIDE};
use argonauts_core::events::run::Party;
use argonauts_core::events::shared::{AnsweredForYou, BlackboardEvent, BlackboardKind};
use argonauts_core::events::task::TaskState;
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::policy::{HardLimit, StuckFacts, Violation};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::{ApprovalMode, AtHardLimit, Presence};
use argonauts_core::text::{clause_case, sentence_case};
use argonauts_core::{Note, NoteLevel};

use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::merge::in_the_way::MERGE_LATER;
use crate::state::RunState;
use crate::state::approvals::ApprovalRecord;
use crate::workflow::change;

use super::{configured, lock, only_yours, own_settings};

/// What ends a part safely and keeps its work, the first one a question offers. Deferring a
/// question lets the run go on without it, keeping the branch leaves the merge to you later, and
/// sending back runs another round or declines a change. Abandoning is never one: it ends the run.
const SAFE_ENDINGS: &[AnswerAction] = &[
    AnswerAction::Defer,
    AnswerAction::KeepBranch,
    AnswerAction::KeepFallback,
    AnswerAction::LetContinue,
    AnswerAction::SendBack,
    AnswerAction::Stop,
    AnswerAction::Wait,
    AnswerAction::Cancel,
];

/// Why a question would have waited for you.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    /// Your rules or the judge turned it down with reasons, which another round can answer.
    Rejected,
    /// Nothing could decide; another round would end the same way.
    Undecided,
    /// A hard limit stopped it, and the approval's `at_hard_limit` says what that does.
    HardLimit(AtHardLimit),
    /// Your settings keep it for you at the terminal (`presence = required`).
    Kept,
}

/// Whether your settings keep an approval of `kind` for you at the terminal, which autopilot
/// leaves as you set it: `presence = required` holds every plan and merge.
pub fn kept_for_you(settings: &Settings, kind: ApprovalKind) -> bool {
    settings.approvals.presence == Presence::Required
        && matches!(kind, ApprovalKind::Plan | ApprovalKind::Merge)
}

/// What an agent's question gets when the main agent's answer was turned down and no one is at the
/// terminal. Never a goal's answer, since it speaks to the agent.
pub const OWN_ANSWER: &str = "No one is at the terminal: the run is on autopilot. Go with your \
                              best judgment, and say in your report what you assumed.";

/// What an agent reads about a question its finished report asked on autopilot.
pub const OWN_CALL: &str = "Nobody is asked on autopilot. What you built stands as your call, \
                            and the run's closing answer lists it for the person to review.";

/// Why a question you hadn't answered went from you to the main agent.
pub const LEFT_TO_THE_MAIN_AGENT: &str = "left to the main agent on autopilot";

/// What autopilot's line calls the answer that sends a task that keeps failing to work again:
/// to another agent, or to the same one where no other may take it.
const ANOTHER_TRY: &str = "Give it another try";

/// Whether nothing at an approval of `kind` waits for you in this run: Shift+Tab chose
/// autopilot, or without a choice your settings did. A kind with no settings of its own follows the
/// mode all your approvals share.
pub fn applies(state: &RunState, settings: &Settings, kind: ApprovalKind) -> bool {
    chosen(state.run.approval_mode, settings, kind)
}

pub fn applies_now(host: &Host, kind: ApprovalKind) -> bool {
    host.read(|state| applies(state, host.settings(), kind))
}

/// [`applies`] for the mode Shift+Tab chose, `None` when it chose none.
pub fn chosen(mode: Option<ApprovalMode>, settings: &Settings, kind: ApprovalKind) -> bool {
    let mode = mode.unwrap_or_else(|| {
        own_settings(settings, kind).map_or_else(|| settings.approvals.mode(), |own| own.mode)
    });
    mode == ApprovalMode::Autopilot
}

/// A plan stops for you only in supervised mode. In auto, one your rules and
/// the judge weighed and didn't approve is settled the way autopilot settles it: another round
/// when they gave reasons, else a checked Captain run. A hard limit still comes to you, and so
/// does a plan nothing weighed.
pub fn settles_a_plan_in_auto(opened: &ApprovalOpened, weighed: bool, at_a_limit: bool) -> bool {
    settled_in_auto(opened) && weighed && !at_a_limit
}

/// The option autopilot takes on this question, with the text an answer needs: the recommended
/// answer to a question, else its first, else the safest ending offered. Your own files in the way
/// of a merge wait for you to move them, so that merge waits for later. An agent that is down
/// has each of its tasks settled on its own. A goal or plan goes back
/// for another round only when it was `Rejected`, since nothing else would change. An agent's
/// question gets the main agent's answer only once the judge approved it, so here it's told to use
/// its best judgment. A hard limit does what its approval's `at_hard_limit` says when the question
/// offers that, and otherwise ends safely. A goal or plan with nothing safe to take keeps the work
/// going: the goal as the main agent wrote it, the plan as a checked Captain run, never abandoned.
/// `None` leaves the question to you, which only waiting at a hard limit and a question your
/// settings keep for you do.
pub fn choice(opened: &ApprovalOpened, held: Held) -> Option<(AnswerOption, Option<String>)> {
    let options = &opened.options;
    let offered = |action: &AnswerAction| options.iter().find(|option| option.action == *action);
    let chosen = |value: &str| {
        options.iter().find(
            |option| matches!(&option.action, AnswerAction::Choose { value: v } if v == value),
        )
    };
    let answer = if only_yours(&opened.packet) {
        chosen(MERGE_LATER)
    } else if matches!(
        opened.kind,
        ApprovalKind::Question | ApprovalKind::Understand
    ) {
        // An answer, never "let the main agent answer" or "let the main agent decide", which hand
        // your part over: the go-ahead question offers one beside its approval.
        let answers: Vec<&AnswerOption> = options
            .iter()
            .filter(|option| {
                matches!(&option.action, AnswerAction::Choose { value }
                    if value != LET_THE_MAIN_AGENT_ANSWER && value != LET_THE_MAIN_AGENT_DECIDE)
            })
            .collect();
        answers
            .iter()
            .copied()
            .find(|option| option.recommended)
            .or_else(|| answers.first().copied())
    } else if opened.kind == ApprovalKind::AgentDown {
        // How to choose is a question for a person. Each paused task is settled on its own:
        // the agent recommended for it takes it, or it waits for its own and tries again.
        offered(&AnswerAction::PerTask)
    } else {
        // A fallback's recommended agent takes the task on; another needs your say.
        options.iter().find(|option| {
            option.recommended && matches!(option.action, AnswerAction::Candidate { .. })
        })
    };
    if let Some(answer) = answer {
        return Some((answer.clone(), None));
    }
    let wanted: &[AnswerAction] = match held {
        Held::HardLimit(AtHardLimit::Wait) => return None,
        Held::HardLimit(AtHardLimit::SendBack) => {
            &[AnswerAction::SendBack, AnswerAction::RequestChanges]
        }
        Held::HardLimit(AtHardLimit::OpenPullRequest) => &[AnswerAction::PullRequest],
        Held::HardLimit(AtHardLimit::Stop) | Held::Rejected | Held::Undecided | Held::Kept => &[],
    };
    if let Some(wanted) = wanted.iter().find_map(offered) {
        return Some((wanted.clone(), None));
    }
    // An agent's question takes an answer only once a model of another provider checked it,
    // so one nothing could check goes as "use your best judgment", below.
    let question = opened.kind == ApprovalKind::Question;
    let another_round = held == Held::Rejected
        || !matches!(opened.kind, ApprovalKind::Understand | ApprovalKind::Plan);
    let ending = SAFE_ENDINGS
        .iter()
        .filter(|action| another_round || **action != AnswerAction::SendBack)
        .find_map(offered);
    if let Some(ending) = ending {
        return Some((ending.clone(), None));
    }
    let keep_going = match (opened.kind, held) {
        (_, Held::Kept) => None,
        (ApprovalKind::Understand, _) => offered(&AnswerAction::Approve),
        (ApprovalKind::Plan, _) => offered(&AnswerAction::SwitchToCaptain),
        _ => None,
    };
    if let Some(option) = keep_going {
        return Some((option.clone(), None));
    }
    offered(&AnswerAction::Answer)
        .filter(|_| question)
        .map(|option| (option.clone(), Some(OWN_ANSWER.to_owned())))
}

/// What a hard limit does at this approval in autopilot: its `at_hard_limit`, except that it
/// sends back at most once per approval, and never for a limit another round can't change.
/// Either way it stops instead, since another round would only end the same way.
pub(super) fn at_hard_limit(
    host: &Host,
    record: &ApprovalRecord,
    violations: &[Violation],
) -> AtHardLimit {
    let set = configured(host.settings(), record.opened.kind).at_hard_limit;
    if set != AtHardLimit::SendBack {
        return set;
    }
    let another_round_may_clear = violations
        .iter()
        .all(|violation| another_round_may_clear(violation.limit));
    let sent_back = host.read(|state| sent_back_at_a_hard_limit(state, &record.opened));
    if another_round_may_clear && !sent_back {
        AtHardLimit::SendBack
    } else {
        AtHardLimit::Stop
    }
}

/// Whether the agents' next round could clear `limit`. The caps and ceilings hold whatever they
/// change, as do a second send-back and what the run's own setup decides.
fn another_round_may_clear(limit: HardLimit) -> bool {
    !matches!(
        limit,
        HardLimit::AutoMergeCap
            | HardLimit::Ceiling
            | HardLimit::SentBackTwice
            | HardLimit::HeadMoved
            | HardLimit::SelfApproval
            | HardLimit::NoSandbox
            | HardLimit::NoGoalHash
    )
}

/// Whether autopilot already sent an earlier approval like `opened` back at a hard limit.
fn sent_back_at_a_hard_limit(state: &RunState, opened: &ApprovalOpened) -> bool {
    state.approvals.by_id.values().any(|earlier| {
        earlier.opened.id != opened.id
            && earlier.opened.kind == opened.kind
            && earlier.opened.task == opened.task
            && earlier.decided.as_ref().is_some_and(|decided| {
                decided.by == Party::Autopilot && sends_back(&decided.action)
            })
            && earlier
                .policy_check()
                .is_some_and(|check| !check.violations.is_empty())
    })
}

fn sends_back(action: &AnswerAction) -> bool {
    matches!(
        action,
        AnswerAction::SendBack | AnswerAction::RequestChanges
    )
}

/// Decides an open approval in your place with [`choice`], and says so in the transcript,
/// with `why` it didn't go ahead on its own when that's known. False when the question offers
/// nothing autopilot may take, so it waits for you after all, and one line says why. The caller
/// holds the desk's deciding lock.
pub(super) fn decide(
    host: &Host,
    record: &ApprovalRecord,
    held: Held,
    why: Option<String>,
    limits: &[Violation],
) -> EngineResult<bool> {
    let opened = &record.opened;
    let again = another_try(host, opened, held);
    let tries_again = again.is_some();
    let chosen = again.or_else(|| choice(opened, held));
    let Some((option, text)) = chosen else {
        let why = why
            .as_deref()
            .map_or("nothing it may take settles it", first_sentence);
        // Waiting was your call, so the line says so.
        let yours = if held == Held::HardLimit(AtHardLimit::Wait) {
            ", as your settings say for a hard limit"
        } else {
            ""
        };
        note(
            host,
            opened,
            format!(
                "{} left \"{}\" to you{yours}, since {}.",
                if settled_in_auto(opened) {
                    "Auto mode"
                } else {
                    "Autopilot"
                },
                opened.title.trim(),
                clause_case(why)
            ),
        )?;
        return Ok(false);
    };
    // The option that gives a task another try is worded for handing it to someone else, and
    // the reason beside it is often that nobody else is left, so the line says what happens.
    let taken = if tries_again {
        ANOTHER_TRY
    } else {
        label(&option, text.as_deref())
    };
    // What autopilot settled is one quiet line; only what it left to you is a warning. A
    // worker's question says who asked and what follows, as one answered by a model does.
    let answered = (opened.kind == ApprovalKind::Question)
        .then(|| asked_by_a_worker(host, opened, taken, why.as_deref(), None))
        .flatten();
    let said = match answered {
        Some(answered) => RunEventKind::Blackboard(BlackboardEvent::AnsweredForYou(answered)),
        None => RunEventKind::Note(Note {
            level: NoteLevel::Info,
            text: ending_line(opened, taken, why.as_deref()),
        }),
    };
    host.record(opened.task.as_ref(), None, said)?;
    // Sent back at a hard limit, the agents hear which limit stopped them.
    let text = text.or_else(|| {
        let at_limit = held == Held::HardLimit(AtHardLimit::SendBack) && sends_back(&option.action);
        at_limit.then(|| limit_note(limits))
    });
    let decided = ApprovalDecided {
        id: opened.id.clone(),
        action: option.action,
        text,
        by: Party::Autopilot,
        evidence_sha256: opened.evidence_sha256.clone(),
        auto_preview: None,
        tasks: Vec::new(),
    };
    host.record(
        opened.task.as_ref(),
        None,
        RunEventKind::Approval(ApprovalEvent::Decided(decided.clone())),
    )?;
    change::on_decided(host, opened, &decided);
    host.shared().desk.wake(&decided);
    Ok(true)
}

/// A task that keeps failing isn't put off while something is left to try. Within
/// `approvals.rules.stuck.max_extra_tries` and the run's reserve, and never at a hard limit,
/// another agent takes it, or the same one tries once more where no other may, told what
/// stopped it. After that it takes the safe ending, which defers it with its work kept.
fn another_try(
    host: &Host,
    opened: &ApprovalOpened,
    held: Held,
) -> Option<(AnswerOption, Option<String>)> {
    if matches!(held, Held::HardLimit(_) | Held::Kept) {
        return None;
    }
    let ApprovalPacket::Stuck(stuck) = &opened.packet else {
        return None;
    };
    let facts = super::evidence_of(host, opened).ok()?.facts.stuck?;
    let allowed = host.settings().approvals.rules.stuck.max_extra_tries;
    if !may_try_again(&facts, allowed) {
        return None;
    }
    let option = opened
        .options
        .iter()
        .find(|option| option.action == AnswerAction::Fallback)?;
    let note = format!(
        "No one is at the terminal: the run is on autopilot and gave this task another try. \
         What stopped it: {}. Take another approach to that, and say in your report what you \
         assumed.",
        stuck.last_failure.trim().trim_end_matches('.')
    );
    Some((option.clone(), Some(note)))
}

/// Whether a stuck task may have one more try: within the tries your rules allow, and unless
/// the reserve is known not to pay for it. A task with no limit of its own has no known cost
/// for a try, and the run's own limit still stops it, so that one isn't put off unheard.
fn may_try_again(facts: &StuckFacts, allowed: u32) -> bool {
    facts.fits_reserve != Some(false) && facts.extra_tries < allowed
}

/// What the agents are told when autopilot sends their work back at a hard limit.
fn limit_note(limits: &[Violation]) -> String {
    let details: Vec<&str> = limits
        .iter()
        .map(|limit| limit.detail.trim().trim_end_matches('.'))
        .filter(|detail| !detail.is_empty())
        .collect();
    let which = if details.is_empty() {
        String::new()
    } else {
        format!(": {}", details.join("; "))
    };
    format!("Sent back at a hard limit{which}. Change the work so it stays within the limit.")
}

fn note(host: &Host, opened: &ApprovalOpened, text: String) -> EngineResult<()> {
    host.record(
        opened.task.as_ref(),
        None,
        RunEventKind::Note(Note {
            level: NoteLevel::Warn,
            text,
        }),
    )?;
    Ok(())
}

/// A worker's question answered in your place with one of its options, by autopilot's rule or
/// by the judge (`judge`): the question on the board, and whether its task had already gone on
/// without it.
pub(super) fn asked_by_a_worker(
    host: &Host,
    opened: &ApprovalOpened,
    taken: &str,
    why: Option<&str>,
    judge: Option<ModelRef>,
) -> Option<AnsweredForYou> {
    let ApprovalPacket::Question(packet) = &opened.packet else {
        return None;
    };
    let task = opened.task.as_ref()?;
    host.read(|state| {
        let board = &state.blackboard;
        let question = board.messages.iter().rev().find(|message| {
            message.kind == BlackboardKind::Question
                && message.text == packet.text
                && board.posted_from.get(&message.id) == Some(task)
        })?;
        let blocked = state
            .tasks
            .get(task)
            .is_some_and(|record| record.state == TaskState::Blocked);
        Some(AnsweredForYou {
            question: question.id.clone(),
            asker: crate::labels::task_label(state, task),
            answer: taken.to_owned(),
            role: judge.as_ref().map(|_| RoleName::Judge),
            by: judge,
            why: why
                .map(first_sentence)
                .filter(|why| !why.is_empty())
                .map(clause_case),
            finished: !blocked,
        })
    })
}

/// What the line calls the option taken: its label, or what the agent was told when no one
/// was there to answer.
fn label<'a>(option: &'a AnswerOption, text: Option<&str>) -> &'a str {
    if text == Some(OWN_ANSWER) {
        "Go with your best judgment"
    } else {
        &option.label
    }
}

/// In auto only a plan is settled in your place, and its lines say whose rule that is.
fn settled_in_auto(opened: &ApprovalOpened) -> bool {
    opened.kind == ApprovalKind::Plan && opened.mode == ApprovalMode::Auto
}

/// The transcript's line for what autopilot chose in your place, and why it couldn't go ahead
/// on its own.
fn ending_line(opened: &ApprovalOpened, label: &str, why: Option<&str>) -> String {
    let because = why
        .map(first_sentence)
        .filter(|why| !why.is_empty())
        .map(|why| format!(", since {}", clause_case(why)))
        .unwrap_or_default();
    let title = opened.title.trim();
    if settled_in_auto(opened) {
        return format!(
            "A plan waits for you only in supervised mode, so \"{title}\" took \"{label}\"\
             {because}."
        );
    }
    format!("Autopilot answered \"{title}\" with \"{label}\" instead of asking you{because}.")
}

/// Says that your rules alone decided an approval in autopilot, since no model could check it:
/// which question, why none could, and what fixes that when the policy says, in plain words:
/// `"Run this workflow?": Antigravity CLI worked on this, so it can't approve it; your rules
/// decided. Pick another judge in /approvals.` An agent's passing error comes with no fix.
pub(super) fn rules_alone(host: &Host, opened: &ApprovalOpened, why: &str) -> EngineResult<()> {
    note(host, opened, rules_alone_line(&opened.title, why))
}

fn rules_alone_line(title: &str, why: &str) -> String {
    let why = why.trim().trim_end_matches('.');
    let (clause, fix) = match why.split_once(". ") {
        Some((clause, fix)) => (clause, Some(fix)),
        None => (why, None),
    };
    let fix = fix
        .map(|fix| format!(" {}.", sentence_case(fix.trim_end_matches('.'))))
        .unwrap_or_default();
    format!(
        "\"{}\": {}; your rules decided.{fix}",
        title.trim(),
        sentence_case(clause)
    )
}

/// A reason's first sentence, without its period, so the transcript's line stays one line.
fn first_sentence(text: &str) -> &str {
    let text = text.trim();
    let end = text.find(". ").unwrap_or(text.len());
    text[..end].trim_end_matches('.')
}

/// Every part autopilot ended in your place, and every question or approval you left with
/// Esc, one line each as the transcript said it, for the run's closing answer to list.
pub fn endings(state: &RunState) -> Vec<String> {
    let mut decided: Vec<&ApprovalRecord> = state
        .approvals
        .by_id
        .values()
        .filter(|record| {
            record
                .decided
                .as_ref()
                .is_some_and(|decided| matches!(decided.by, Party::Autopilot | Party::Declined))
        })
        .collect();
    decided.sort_by_key(|record| record.since);
    decided
        .into_iter()
        .filter_map(|record| {
            let decided = record.decided.as_ref()?;
            let option = record
                .opened
                .options
                .iter()
                .find(|option| option.action == decided.action)?;
            // What you left with Esc says so in its own words.
            if decided.by == Party::Declined {
                return Some(super::decline::line(
                    &record.opened,
                    option,
                    decided.text.as_deref(),
                ));
            }
            let why = record.policy_check().and_then(|check| {
                check.reasons.first().cloned().or_else(|| {
                    check
                        .violations
                        .first()
                        .map(|first| format!("a hard limit, {}", first.detail.trim()))
                })
            });
            let label = label(option, decided.text.as_deref());
            Some(ending_line(&record.opened, label, why.as_deref()))
        })
        .collect()
}

/// Every approval autopilot ended in your place, oldest first, each as the closing answer
/// lists it: `"Merge this run?" took "Keep the branch", since the daily cap is spent`.
pub fn decided(state: &RunState) -> Vec<String> {
    let mut records: Vec<&ApprovalRecord> = state.approvals.by_id.values().collect();
    records.sort_by_key(|record| record.since);
    records
        .into_iter()
        .filter_map(|record| {
            let decided = record.decided.as_ref()?;
            if decided.by != Party::Autopilot {
                return None;
            }
            let option = record
                .opened
                .options
                .iter()
                .find(|option| option.action == decided.action)?;
            let because = record
                .policy_check()
                .and_then(|check| check.reasons.first())
                .map(|why| first_sentence(why.as_str()))
                .filter(|why| !why.is_empty())
                .map(|why| format!(", since {}", clause_case(why)))
                .unwrap_or_default();
            Some(format!(
                "\"{}\" took \"{}\"{because}",
                record.opened.title.trim(),
                label(option, decided.text.as_deref())
            ))
        })
        .collect()
}

/// Whether the policy weighed this approval in autopilot: it opened there, or was asked
/// again once autopilot came on.
pub(super) fn asked_in_autopilot(host: &Host, record: &ApprovalRecord) -> bool {
    record.opened.mode == ApprovalMode::Autopilot
        || lock(&host.shared().desk.autopilot).contains(&record.opened.id)
}

/// A goal question you hadn't answered when autopilot came on. The main agent answers it instead,
/// from the Understand step that asked.
fn yours_to_the_main_agent(record: &ApprovalRecord) -> bool {
    record.opened.kind == ApprovalKind::Understand
        && matches!(&record.opened.packet, ApprovalPacket::Understand(goal) if !goal.questions.is_empty())
}

/// Settles every open approval that waits for you, once autopilot applies to it. One only you
/// could answer takes its safe option, a goal question goes to the main agent, and one the policy
/// weighed outside autopilot, or never weighed because `approvals.ask_you_about` kept it yours,
/// is weighed in it. One the policy is still deciding is left to it, and one it already weighed
/// in autopilot, or autopilot kept for you, stays yours.
pub fn settle_open(host: &Host) -> EngineResult<()> {
    let desk = &host.shared().desk;
    let _deciding = lock(&desk.deciding);
    let waiting: Vec<ApprovalRecord> = host.read(|state| {
        state
            .approvals
            .open()
            .filter(|record| applies(state, host.settings(), record.opened.kind))
            .cloned()
            .collect()
    });
    for record in waiting {
        if only_yours(&record.opened.packet) {
            let why = super::why_asked_of(&record)
                .or_else(|| super::why_only_yours(&record.opened.packet));
            decide(host, &record, Held::Undecided, why, &[])?;
        } else if asked_in_autopilot(host, &record) {
            continue;
        } else if yours_to_the_main_agent(&record) {
            super::withdraw_locked(host, &record.opened.id, LEFT_TO_THE_MAIN_AGENT)?;
        } else {
            super::ask_again(host, &record)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::approval::{ApprovalPacket, FreeText};
    use argonauts_core::names::ApprovalId;
    use argonauts_core::settings::approvals::DecideWith;

    use super::*;
    use crate::approvals::options;

    /// A stuck task gets its extra try unless the reserve is known not to pay for it. One with
    /// no limit of its own has no known cost for a try, which is no reason to put it off.
    #[test]
    fn a_try_of_unknown_cost_is_still_given() {
        let facts = |fits_reserve, extra_tries| StuckFacts {
            fallback_mode: argonauts_core::settings::fallback::FallbackMode::Auto,
            providers_tried: vec!["anthropic".to_owned()],
            next_provider: Some("openai".to_owned()),
            next_name: Some("Codex".to_owned()),
            extra_tries,
            fits_reserve,
        };
        assert!(may_try_again(&facts(Some(true), 0), 1));
        assert!(may_try_again(&facts(None, 0), 1));
        assert!(!may_try_again(&facts(Some(false), 0), 1));
        assert!(!may_try_again(&facts(None, 1), 1));
        assert!(!may_try_again(&facts(Some(true), 0), 0));
    }

    fn opened(kind: ApprovalKind, options: Vec<AnswerOption>) -> ApprovalOpened {
        ApprovalOpened {
            id: ApprovalId::numbered(1),
            kind,
            title: "A task".to_owned(),
            task: None,
            packet: ApprovalPacket::WorkflowPromotion(serde_json::json!({})),
            evidence_sha256: String::new(),
            mode: ApprovalMode::Autopilot,
            decide_with: DecideWith::Rules,
            options,
            free_text: FreeText::None,
            needs_you: false,
        }
    }

    fn taken(kind: ApprovalKind, held: Held) -> Option<AnswerAction> {
        let question = opened(kind, options::standard(kind));
        choice(&question, held).map(|(option, _)| option.action)
    }

    #[test]
    fn each_kind_ends_safely_keeps_the_work_and_never_abandons_the_run() {
        let undecided = |kind| taken(kind, Held::Undecided);
        assert_eq!(
            undecided(ApprovalKind::Merge),
            Some(AnswerAction::KeepBranch)
        );
        assert_eq!(undecided(ApprovalKind::Stuck), Some(AnswerAction::Defer));
        assert_eq!(undecided(ApprovalKind::Limit), Some(AnswerAction::Stop));
        assert_eq!(
            undecided(ApprovalKind::ChangeRequest),
            Some(AnswerAction::SendBack)
        );
        assert_eq!(
            undecided(ApprovalKind::AgentDown),
            Some(AnswerAction::PerTask)
        );
        assert_eq!(
            undecided(ApprovalKind::TakeOverReturn),
            Some(AnswerAction::LetContinue)
        );
        // Another round wouldn't change a goal or a plan, so the work goes on: the goal as the main
        // agent wrote it, the plan as a checked Captain run. Rejected, it goes back for another
        // round first.
        assert_eq!(
            undecided(ApprovalKind::Understand),
            Some(AnswerAction::Approve)
        );
        assert_eq!(
            undecided(ApprovalKind::Plan),
            Some(AnswerAction::SwitchToCaptain)
        );
        assert_eq!(
            taken(ApprovalKind::Understand, Held::Rejected),
            Some(AnswerAction::SendBack)
        );
        assert_eq!(
            taken(ApprovalKind::Plan, Held::Rejected),
            Some(AnswerAction::SendBack)
        );
        for kind in [
            ApprovalKind::Understand,
            ApprovalKind::Plan,
            ApprovalKind::Merge,
        ] {
            for held in [Held::Rejected, Held::Undecided, Held::Kept] {
                assert_ne!(taken(kind, held), Some(AnswerAction::Abandon), "{kind:?}");
            }
        }
        // A plan your settings keep for you at the terminal waits for you, as you set it.
        assert_eq!(taken(ApprovalKind::Plan, Held::Kept), None);
        assert_eq!(
            taken(ApprovalKind::Merge, Held::Kept),
            Some(AnswerAction::KeepBranch)
        );
    }

    /// The orchestrator's plan broke the rules on its last try, so the question offers no approval
    /// and no other round: autopilot runs the work as Captain instead of asking.
    #[test]
    fn a_plan_with_nothing_to_approve_runs_captain() {
        let blocked = crate::divide::plan_approval::answer_options(true).unwrap_or_default();
        let without_rounds: Vec<AnswerOption> = blocked
            .into_iter()
            .filter(|option| option.action != AnswerAction::SendBack)
            .collect();
        let question = opened(ApprovalKind::Plan, without_rounds);
        for held in [
            Held::Rejected,
            Held::Undecided,
            Held::HardLimit(AtHardLimit::Stop),
        ] {
            let taken = choice(&question, held).map(|(option, _)| option.action);
            assert_eq!(taken, Some(AnswerAction::SwitchToCaptain), "{held:?}");
        }
        assert_eq!(choice(&question, Held::HardLimit(AtHardLimit::Wait)), None);
    }

    #[test]
    fn the_reason_is_one_clause() {
        assert_eq!(
            first_sentence("No model can judge this. Auto asks you instead."),
            "No model can judge this"
        );
        assert_eq!(
            first_sentence("a hard limit, v1.2 is old."),
            "a hard limit, v1.2 is old"
        );
    }

    #[test]
    fn a_question_takes_the_recommended_answer_or_says_no_one_is_there() {
        let mut answers = options::question(&["debug".to_owned(), "trace".to_owned()]);
        answers[1].recommended = true;
        let (option, text) =
            choice(&opened(ApprovalKind::Question, answers), Held::Undecided).expect("an answer");
        assert_eq!(option.label, "trace");
        assert_eq!(text, None);

        let own = options::standard(ApprovalKind::Question);
        let (option, text) =
            choice(&opened(ApprovalKind::Question, own), Held::Undecided).expect("an answer");
        assert_eq!(option.action, AnswerAction::Answer);
        assert!(text.is_some_and(|text| text.contains("autopilot")));
    }

    /// Held back from the judge's say, whether it turned the answer down or couldn't check
    /// it, an agent's question never gets the unchecked answer: it's told to use its judgment.
    #[test]
    fn an_agents_question_never_gets_an_answer_no_one_checked() {
        let checked = options::main_agent_answer("codex");
        let question = opened(ApprovalKind::Question, checked);
        for held in [
            Held::Undecided,
            Held::Rejected,
            Held::HardLimit(AtHardLimit::Stop),
        ] {
            let (option, text) = choice(&question, held).expect("an answer");
            assert_eq!(option.action, AnswerAction::Answer, "{held:?}");
            assert_eq!(text.as_deref(), Some(OWN_ANSWER), "{held:?}");
        }
    }

    /// A hard limit does what the approval's `at_hard_limit` says when the question offers it,
    /// else ends safely as when nothing could decide.
    #[test]
    fn a_hard_limit_does_what_its_approval_says() {
        use AtHardLimit::{OpenPullRequest, SendBack, Stop, Wait};
        let at = |kind, limit| taken(kind, Held::HardLimit(limit));
        assert_eq!(
            at(ApprovalKind::Merge, Stop),
            Some(AnswerAction::KeepBranch)
        );
        assert_eq!(
            at(ApprovalKind::Merge, OpenPullRequest),
            Some(AnswerAction::PullRequest)
        );
        assert_eq!(
            at(ApprovalKind::Merge, SendBack),
            Some(AnswerAction::RequestChanges)
        );
        assert_eq!(at(ApprovalKind::Merge, Wait), None);
        assert_eq!(
            at(ApprovalKind::Plan, SendBack),
            Some(AnswerAction::SendBack)
        );
        assert_eq!(
            at(ApprovalKind::Plan, Stop),
            Some(AnswerAction::SwitchToCaptain),
            "a plan has nothing to stop, so it runs as Captain"
        );
        assert_eq!(at(ApprovalKind::Plan, Wait), None);
        assert_eq!(at(ApprovalKind::Limit, Stop), Some(AnswerAction::Stop));
        assert_eq!(at(ApprovalKind::Limit, Wait), None);
        // A question that doesn't offer the one you picked ends safely instead.
        assert_eq!(
            at(ApprovalKind::Stuck, OpenPullRequest),
            Some(AnswerAction::Defer)
        );
        for kind in [
            ApprovalKind::Understand,
            ApprovalKind::Plan,
            ApprovalKind::Merge,
        ] {
            for limit in [Stop, SendBack, OpenPullRequest, Wait] {
                assert_ne!(at(kind, limit), Some(AnswerAction::Abandon), "{kind:?}");
            }
        }
    }

    /// A goal question with nothing to pick waits for you: the agents' own-answer text would be
    /// written into the goal.
    #[test]
    fn a_goal_question_never_takes_the_agents_own_answer() {
        let own: Vec<AnswerOption> = options::question(&[])
            .into_iter()
            .filter(|option| option.label == options::TYPE_MY_OWN_ANSWER)
            .collect();
        assert_eq!(own.len(), 1);
        assert_eq!(
            choice(&opened(ApprovalKind::Understand, own), Held::Undecided),
            None
        );
    }

    /// The line names the question and the agents as people read them, and points to /approvals
    /// only when the policy says picking another judge is the fix, never to an id or a
    /// setting key.
    #[test]
    fn rules_alone_says_why_in_plain_words() {
        // As the judge's independence check says it when the only agent from another provider
        // wrote the work.
        let why = "Antigravity CLI worked on this, so it can't approve it. Pick another judge \
                   in /approvals";
        assert_eq!(
            rules_alone_line("Run this workflow?", why),
            "\"Run this workflow?\": Antigravity CLI worked on this, so it can't approve it; your \
             rules decided. Pick another judge in /approvals."
        );
        // An agent's passing error names the judge, and asks nothing of you.
        let why = "Haiku on Claude Code couldn't check it, as its session ended early (its agent \
                   refused the sign-in) even when asked again, and no agent from another provider \
                   could instead";
        assert_eq!(
            rules_alone_line("Approve this plan?", why),
            "\"Approve this plan?\": Haiku on Claude Code couldn't check it, as its session ended \
             early (its agent refused the sign-in) even when asked again, and no agent from \
             another provider could instead; your rules decided."
        );
    }
}
