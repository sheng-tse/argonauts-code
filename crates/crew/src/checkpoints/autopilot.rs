//! Full auto: nothing waits for you. A checkpoint your rules and the approver couldn't approve
//! takes the option that ends that part safely and keeps its work, one a hard limit stopped
//! does what its `at_hard_limit` says, and a question takes the crew's recommended answer. The
//! transcript says what was chosen and why, in place of a card. A run never ends here, and never
//! waits on a goal or a plan: once another round can't change one, a goal goes ahead as the lead
//! wrote it and a plan runs as a checked solo run. Only what you set to wait for you waits: a
//! hard limit set to park, or a plan or merge your settings keep for you at the terminal.

use argonauts_core::events::RunEventKind;
use argonauts_core::events::checkpoint::{
    AnswerAction, AnswerOption, CheckpointDecided, CheckpointEvent, CheckpointKind,
    CheckpointOpened, CheckpointPacket,
};
use argonauts_core::events::goal::{LET_THE_LEAD_ANSWER, LET_THE_LEAD_DECIDE};
use argonauts_core::events::run::Party;
use argonauts_core::policy::{HardLimit, Violation};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::{ApprovalMode, AtHardLimit, Presence};
use argonauts_core::text::{clause_case, sentence_case};
use argonauts_core::{Note, NoteLevel};

use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::integrate::in_the_way::MERGE_LATER;
use crate::state::RunState;
use crate::state::checkpoints::CheckpointRecord;
use crate::workflow::change;

use super::{configured, lock, only_yours, own_settings};

/// What ends a part safely and keeps its work, the first one a card offers. Deferring a card
/// lets the run go on without it, keeping the branch leaves the merge to you later, and sending
/// back runs another round or declines a change. Abandoning is never one: it ends the run.
const SAFE_ENDINGS: &[AnswerAction] = &[
    AnswerAction::Defer,
    AnswerAction::KeepBranch,
    AnswerAction::KeepReplacement,
    AnswerAction::LetContinue,
    AnswerAction::SendBack,
    AnswerAction::Stop,
    AnswerAction::Wait,
    AnswerAction::Cancel,
];

/// Why a card would have waited for you.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    /// Your rules or the approver turned it down with reasons, which another round can answer.
    Rejected,
    /// Nothing could decide; another round would end the same way.
    Undecided,
    /// A hard limit stopped it, and the checkpoint's `at_hard_limit` says what that does.
    HardLimit(AtHardLimit),
    /// Your settings keep it for you at the terminal (`presence = required`).
    Kept,
}

/// Whether your settings keep a checkpoint of `kind` for you at the terminal, which full auto
/// leaves as you set it: `presence = required` holds every plan and merge.
pub fn kept_for_you(settings: &Settings, kind: CheckpointKind) -> bool {
    settings.approvals.presence == Presence::Required
        && matches!(kind, CheckpointKind::Plan | CheckpointKind::Merge)
}

/// What an agent's question gets when the lead's answer was turned down and no one is at the
/// terminal. Never a goal's answer, since it speaks to the agent.
pub const OWN_ANSWER: &str = "No one is at the terminal: the run is on autopilot. Go with your \
                              best judgment, and say in your report what you assumed.";

/// Why a question you hadn't answered went from you to the lead.
pub const LEFT_TO_THE_LEAD: &str = "left to the main agent on autopilot";

/// Whether nothing at a checkpoint of `kind` waits for you in this run: Shift+Tab chose full
/// auto, or without a choice your settings did. A kind with no settings of its own follows the
/// mode all your checkpoints share.
pub fn applies(state: &RunState, settings: &Settings, kind: CheckpointKind) -> bool {
    chosen(state.run.approval_mode, settings, kind)
}

pub fn applies_now(host: &Host, kind: CheckpointKind) -> bool {
    host.read(|state| applies(state, host.settings(), kind))
}

/// [`applies`] for the mode Shift+Tab chose, `None` when it chose none.
pub fn chosen(mode: Option<ApprovalMode>, settings: &Settings, kind: CheckpointKind) -> bool {
    let mode = mode.unwrap_or_else(|| {
        own_settings(settings, kind).map_or_else(|| settings.approvals.mode(), |own| own.mode)
    });
    mode == ApprovalMode::Autopilot
}

/// A plan stops for you only in supervised mode. In auto, one your rules and
/// the judge weighed and didn't approve is settled the way autopilot settles it: another round
/// when they gave reasons, else a checked solo run. A hard limit still comes to you, and so
/// does a plan nothing weighed.
pub fn settles_a_plan_in_auto(opened: &CheckpointOpened, weighed: bool, at_a_limit: bool) -> bool {
    settled_in_auto(opened) && weighed && !at_a_limit
}

/// The option full auto takes on this card, with the text an answer needs: the recommended
/// answer to a question, else its first, else the safest ending offered. Your own files in the
/// way of a merge wait for you to move them, so that merge waits for later. A goal or plan goes
/// back for another round only when it was `Rejected`, since nothing else would change. An
/// agent's question gets the lead's answer only once the approver approved it, so here it's
/// told to use its best judgment. A hard limit does what its checkpoint's `at_hard_limit` says
/// when the card offers that, and otherwise ends safely. A goal or plan with nothing safe to
/// take keeps the work going: the goal as the lead wrote it, the plan as a checked solo run,
/// never abandoned. `None` leaves the card to you, which only parking at a hard limit and a card
/// your settings keep for you do.
pub fn choice(opened: &CheckpointOpened, held: Held) -> Option<(AnswerOption, Option<String>)> {
    let options = &opened.options;
    let offered = |action: &AnswerAction| options.iter().find(|option| option.action == *action);
    let chosen = |value: &str| {
        options.iter().find(
            |option| matches!(&option.action, AnswerAction::Choose { value: v } if v == value),
        )
    };
    let answer = if only_yours(&opened.packet) {
        chosen(MERGE_LATER)
    } else if matches!(opened.kind, CheckpointKind::Question | CheckpointKind::Goal) {
        // An answer, never "let the lead answer" or "let the lead decide", which hand your part
        // over: the go-ahead card offers one beside its approval.
        let answers: Vec<&AnswerOption> = options
            .iter()
            .filter(|option| {
                matches!(&option.action, AnswerAction::Choose { value }
                    if value != LET_THE_LEAD_ANSWER && value != LET_THE_LEAD_DECIDE)
            })
            .collect();
        answers
            .iter()
            .copied()
            .find(|option| option.recommended)
            .or_else(|| answers.first().copied())
    } else {
        // A handoff's recommended agent takes the card on; another needs your say.
        options.iter().find(|option| {
            option.recommended && matches!(option.action, AnswerAction::Candidate { .. })
        })
    };
    if let Some(answer) = answer {
        return Some((answer.clone(), None));
    }
    let wanted: &[AnswerAction] = match held {
        Held::HardLimit(AtHardLimit::Park) => return None,
        Held::HardLimit(AtHardLimit::SendBack) => {
            &[AnswerAction::SendBack, AnswerAction::RequestChanges]
        }
        Held::HardLimit(AtHardLimit::OpenPullRequest) => &[AnswerAction::PullRequest],
        Held::HardLimit(AtHardLimit::Stop) | Held::Rejected | Held::Undecided | Held::Kept => &[],
    };
    if let Some(wanted) = wanted.iter().find_map(offered) {
        return Some((wanted.clone(), None));
    }
    // An agent's question takes an answer only once a model of another company checked it,
    // so one nothing could check goes as "use your best judgment", below.
    let question = opened.kind == CheckpointKind::Question;
    let another_round = held == Held::Rejected
        || !matches!(opened.kind, CheckpointKind::Goal | CheckpointKind::Plan);
    let ending = SAFE_ENDINGS
        .iter()
        .filter(|action| another_round || **action != AnswerAction::SendBack)
        .find_map(offered);
    if let Some(ending) = ending {
        return Some((ending.clone(), None));
    }
    let keep_going = match (opened.kind, held) {
        (_, Held::Kept) => None,
        (CheckpointKind::Goal, _) => offered(&AnswerAction::Approve),
        (CheckpointKind::Plan, _) => offered(&AnswerAction::SwitchToSolo),
        _ => None,
    };
    if let Some(option) = keep_going {
        return Some((option.clone(), None));
    }
    offered(&AnswerAction::Answer)
        .filter(|_| question)
        .map(|option| (option.clone(), Some(OWN_ANSWER.to_owned())))
}

/// What a hard limit does at this checkpoint in full auto: its `at_hard_limit`, except that it
/// sends back at most once per checkpoint, and never for a limit another round can't change.
/// Either way it stops instead, since another round would only end the same way.
pub(super) fn at_hard_limit(
    host: &Host,
    record: &CheckpointRecord,
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

/// Whether full auto already sent an earlier checkpoint like `opened` back at a hard limit.
fn sent_back_at_a_hard_limit(state: &RunState, opened: &CheckpointOpened) -> bool {
    state.checkpoints.by_id.values().any(|earlier| {
        earlier.opened.id != opened.id
            && earlier.opened.kind == opened.kind
            && earlier.opened.card == opened.card
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

/// Decides an open checkpoint in your place with [`choice`], and says so in the transcript,
/// with `why` it didn't go ahead on its own when that's known. False when the card offers
/// nothing full auto may take, so it waits for you after all, and one line says why. The caller
/// holds the desk's deciding lock.
pub(super) fn decide(
    host: &Host,
    record: &CheckpointRecord,
    held: Held,
    why: Option<String>,
    limits: &[Violation],
) -> EngineResult<bool> {
    let opened = &record.opened;
    let Some((option, text)) = choice(opened, held) else {
        let why = why
            .as_deref()
            .map_or("nothing it may take settles it", first_sentence);
        // Parking was your call, so the line says so.
        let yours = if held == Held::HardLimit(AtHardLimit::Park) {
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
    note(
        host,
        opened,
        ending_line(opened, label(&option, text.as_deref()), why.as_deref()),
    )?;
    // Sent back at a hard limit, the agents hear which limit stopped them.
    let text = text.or_else(|| {
        let at_limit = held == Held::HardLimit(AtHardLimit::SendBack) && sends_back(&option.action);
        at_limit.then(|| limit_note(limits))
    });
    let decided = CheckpointDecided {
        id: opened.id.clone(),
        action: option.action,
        text,
        by: Party::Autopilot,
        evidence_sha256: opened.evidence_sha256.clone(),
        shadow: None,
        cards: Vec::new(),
    };
    host.record(
        opened.card.as_ref(),
        None,
        RunEventKind::Checkpoint(CheckpointEvent::Decided(decided.clone())),
    )?;
    change::on_decided(host, opened, &decided);
    host.shared().desk.wake(&decided);
    Ok(true)
}

/// What the agents are told when full auto sends their work back at a hard limit.
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

fn note(host: &Host, opened: &CheckpointOpened, text: String) -> EngineResult<()> {
    host.record(
        opened.card.as_ref(),
        None,
        RunEventKind::Note(Note {
            level: NoteLevel::Warn,
            text,
        }),
    )?;
    Ok(())
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
fn settled_in_auto(opened: &CheckpointOpened) -> bool {
    opened.kind == CheckpointKind::Plan && opened.mode == ApprovalMode::Auto
}

/// The transcript's line for what full auto chose in your place, and why it couldn't go ahead
/// on its own.
fn ending_line(opened: &CheckpointOpened, label: &str, why: Option<&str>) -> String {
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

/// Says that your rules alone decided a checkpoint in full auto, since no model could check it:
/// which card, why none could, and what fixes that when the policy says, in plain words:
/// `"Run this workflow?": Antigravity CLI worked on this, so it can't approve it; your rules
/// decided. Pick another approver in /approvals.` A vendor's passing error comes with no fix.
pub(super) fn rules_alone(host: &Host, opened: &CheckpointOpened, why: &str) -> EngineResult<()> {
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

/// Every part full auto ended in your place, and every question or approval you left with
/// Esc, one line each as the transcript said it, for the run's closing answer to list.
pub fn endings(state: &RunState) -> Vec<String> {
    let mut decided: Vec<&CheckpointRecord> = state
        .checkpoints
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

/// Whether the policy weighed this checkpoint in full auto: it opened there, or was asked
/// again once full auto came on.
pub(super) fn asked_in_autopilot(host: &Host, record: &CheckpointRecord) -> bool {
    record.opened.mode == ApprovalMode::Autopilot
        || lock(&host.shared().desk.autopilot).contains(&record.opened.id)
}

/// A goal question you hadn't answered when full auto came on. The lead answers it instead,
/// from the goal step that asked.
fn yours_to_the_lead(record: &CheckpointRecord) -> bool {
    record.opened.kind == CheckpointKind::Goal
        && matches!(&record.opened.packet, CheckpointPacket::Goal(goal) if !goal.questions.is_empty())
}

/// Settles every open checkpoint that waits for you, once full auto applies to it. One only you
/// could answer takes its safe option, a goal question goes to the lead, and one the policy
/// weighed outside full auto, or never weighed because `approvals.ask_you_about` kept it yours,
/// is weighed in it. One the policy is still deciding is left to it, and one it already weighed
/// in full auto, or full auto kept for you, stays yours.
pub fn settle_open(host: &Host) -> EngineResult<()> {
    let desk = &host.shared().desk;
    let _deciding = lock(&desk.deciding);
    let waiting: Vec<CheckpointRecord> = host.read(|state| {
        state
            .checkpoints
            .open()
            .filter(|record| applies(state, host.settings(), record.opened.kind))
            .cloned()
            .collect()
    });
    for record in waiting {
        if only_yours(&record.opened.packet) {
            let why = super::why_asked_of(&record);
            decide(host, &record, Held::Undecided, why, &[])?;
        } else if asked_in_autopilot(host, &record) {
            continue;
        } else if yours_to_the_lead(&record) {
            super::withdraw_locked(host, &record.opened.id, LEFT_TO_THE_LEAD)?;
        } else {
            super::ask_again(host, &record)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::checkpoint::{CheckpointPacket, FreeText};
    use argonauts_core::names::CheckpointId;
    use argonauts_core::settings::approvals::DecideWith;

    use super::*;
    use crate::checkpoints::options;

    fn opened(kind: CheckpointKind, options: Vec<AnswerOption>) -> CheckpointOpened {
        CheckpointOpened {
            id: CheckpointId::numbered(1),
            kind,
            title: "A task".to_owned(),
            card: None,
            packet: CheckpointPacket::WorkflowPromotion(serde_json::json!({})),
            evidence_sha256: String::new(),
            mode: ApprovalMode::Autopilot,
            decide_with: DecideWith::Rules,
            options,
            free_text: FreeText::None,
            needs_you: false,
        }
    }

    fn taken(kind: CheckpointKind, held: Held) -> Option<AnswerAction> {
        let card = opened(kind, options::standard(kind));
        choice(&card, held).map(|(option, _)| option.action)
    }

    #[test]
    fn each_kind_ends_safely_keeps_the_work_and_never_abandons_the_run() {
        let undecided = |kind| taken(kind, Held::Undecided);
        assert_eq!(
            undecided(CheckpointKind::Merge),
            Some(AnswerAction::KeepBranch)
        );
        assert_eq!(
            undecided(CheckpointKind::Escalation),
            Some(AnswerAction::Defer)
        );
        assert_eq!(undecided(CheckpointKind::Budget), Some(AnswerAction::Stop));
        assert_eq!(
            undecided(CheckpointKind::ChangeRequest),
            Some(AnswerAction::SendBack)
        );
        assert_eq!(
            undecided(CheckpointKind::VendorDown),
            Some(AnswerAction::Wait)
        );
        assert_eq!(
            undecided(CheckpointKind::TakeOverReturn),
            Some(AnswerAction::LetContinue)
        );
        // Another round wouldn't change a goal or a plan, so the work goes on: the goal as the
        // lead wrote it, the plan as a checked solo run. Rejected, it goes back for another
        // round first.
        assert_eq!(undecided(CheckpointKind::Goal), Some(AnswerAction::Approve));
        assert_eq!(
            undecided(CheckpointKind::Plan),
            Some(AnswerAction::SwitchToSolo)
        );
        assert_eq!(
            taken(CheckpointKind::Goal, Held::Rejected),
            Some(AnswerAction::SendBack)
        );
        assert_eq!(
            taken(CheckpointKind::Plan, Held::Rejected),
            Some(AnswerAction::SendBack)
        );
        for kind in [
            CheckpointKind::Goal,
            CheckpointKind::Plan,
            CheckpointKind::Merge,
        ] {
            for held in [Held::Rejected, Held::Undecided, Held::Kept] {
                assert_ne!(taken(kind, held), Some(AnswerAction::Abandon), "{kind:?}");
            }
        }
        // A plan your settings keep for you at the terminal waits for you, as you set it.
        assert_eq!(taken(CheckpointKind::Plan, Held::Kept), None);
        assert_eq!(
            taken(CheckpointKind::Merge, Held::Kept),
            Some(AnswerAction::KeepBranch)
        );
    }

    /// The coordinator's plan broke the rules on its last try, so the card offers no approval
    /// and no other round: full auto runs the work solo instead of asking.
    #[test]
    fn a_plan_with_nothing_to_approve_runs_solo() {
        let blocked = crate::divide::plan_checkpoint::answer_options(true).unwrap_or_default();
        let without_rounds: Vec<AnswerOption> = blocked
            .into_iter()
            .filter(|option| option.action != AnswerAction::SendBack)
            .collect();
        let card = opened(CheckpointKind::Plan, without_rounds);
        for held in [
            Held::Rejected,
            Held::Undecided,
            Held::HardLimit(AtHardLimit::Stop),
        ] {
            let taken = choice(&card, held).map(|(option, _)| option.action);
            assert_eq!(taken, Some(AnswerAction::SwitchToSolo), "{held:?}");
        }
        assert_eq!(choice(&card, Held::HardLimit(AtHardLimit::Park)), None);
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
            choice(&opened(CheckpointKind::Question, answers), Held::Undecided).expect("an answer");
        assert_eq!(option.label, "trace");
        assert_eq!(text, None);

        let own = options::standard(CheckpointKind::Question);
        let (option, text) =
            choice(&opened(CheckpointKind::Question, own), Held::Undecided).expect("an answer");
        assert_eq!(option.action, AnswerAction::Answer);
        assert!(text.is_some_and(|text| text.contains("autopilot")));
    }

    /// Held back from the approver's say, whether it turned the answer down or couldn't check
    /// it, an agent's question never gets the unchecked answer: it's told to use its judgment.
    #[test]
    fn an_agents_question_never_gets_an_answer_no_one_checked() {
        let checked = options::lead_answer("codex");
        let card = opened(CheckpointKind::Question, checked);
        for held in [
            Held::Undecided,
            Held::Rejected,
            Held::HardLimit(AtHardLimit::Stop),
        ] {
            let (option, text) = choice(&card, held).expect("an answer");
            assert_eq!(option.action, AnswerAction::Answer, "{held:?}");
            assert_eq!(text.as_deref(), Some(OWN_ANSWER), "{held:?}");
        }
    }

    /// A hard limit does what the checkpoint's `at_hard_limit` says when the card offers it,
    /// else ends safely as when nothing could decide.
    #[test]
    fn a_hard_limit_does_what_its_checkpoint_says() {
        use AtHardLimit::{OpenPullRequest, Park, SendBack, Stop};
        let at = |kind, limit| taken(kind, Held::HardLimit(limit));
        assert_eq!(
            at(CheckpointKind::Merge, Stop),
            Some(AnswerAction::KeepBranch)
        );
        assert_eq!(
            at(CheckpointKind::Merge, OpenPullRequest),
            Some(AnswerAction::PullRequest)
        );
        assert_eq!(
            at(CheckpointKind::Merge, SendBack),
            Some(AnswerAction::RequestChanges)
        );
        assert_eq!(at(CheckpointKind::Merge, Park), None);
        assert_eq!(
            at(CheckpointKind::Plan, SendBack),
            Some(AnswerAction::SendBack)
        );
        assert_eq!(
            at(CheckpointKind::Plan, Stop),
            Some(AnswerAction::SwitchToSolo),
            "a plan has nothing to stop, so it runs solo"
        );
        assert_eq!(at(CheckpointKind::Plan, Park), None);
        assert_eq!(at(CheckpointKind::Budget, Stop), Some(AnswerAction::Stop));
        assert_eq!(at(CheckpointKind::Budget, Park), None);
        // A card that doesn't offer the one you picked ends safely instead.
        assert_eq!(
            at(CheckpointKind::Escalation, OpenPullRequest),
            Some(AnswerAction::Defer)
        );
        for kind in [
            CheckpointKind::Goal,
            CheckpointKind::Plan,
            CheckpointKind::Merge,
        ] {
            for limit in [Stop, SendBack, OpenPullRequest, Park] {
                assert_ne!(at(kind, limit), Some(AnswerAction::Abandon), "{kind:?}");
            }
        }
    }

    /// A goal question with nothing to pick parks for you: the agents' own-answer text would be
    /// written into the goal.
    #[test]
    fn a_goal_question_never_takes_the_agents_own_answer() {
        let own: Vec<AnswerOption> = options::question(&[])
            .into_iter()
            .filter(|option| option.label == options::TYPE_MY_OWN_ANSWER)
            .collect();
        assert_eq!(own.len(), 1);
        assert_eq!(
            choice(&opened(CheckpointKind::Goal, own), Held::Undecided),
            None
        );
    }

    /// The line names the card and the agents as people read them, and points to /approvals
    /// only when the policy says picking another approver is the fix, never to an id or a
    /// setting key.
    #[test]
    fn rules_alone_says_why_in_plain_words() {
        // As the approver's independence check says it when the only agent from another company
        // wrote the work.
        let why = "Antigravity CLI worked on this, so it can't approve it. Pick another approver \
                   in /approvals";
        assert_eq!(
            rules_alone_line("Run this workflow?", why),
            "\"Run this workflow?\": Antigravity CLI worked on this, so it can't approve it; your \
             rules decided. Pick another approver in /approvals."
        );
        // A vendor's passing error names the approver, and asks nothing of you.
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
