//! `argonauts_start_workflow`: the main session hands work over, and the workflow in use
//! starts on it.
//!
//! Who decides to call it: the session's own judgment, by its standing lines, while a workflow
//! is in use. Here code decides whether the call may start one. With a workflow in use it may,
//! unless your words kept the request with the session. With solo in use it may only when your
//! own words asked for the crew, a checked copy, or other agents in roles, and those words go
//! through the same checks as an estimate's do, against your message as you typed it, so a word
//! the session picked out can't start anything.

use argonauts_core::events::run::{RunMode, TriageDecision};
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::{ForcedMode, SoloStyle};
use argonauts_core::steps::intake::{Instruction, InstructionKind};
use serde_json::Value;

use super::{Call, MainTool};
use crate::controller::chat::{ChatState, handover, keep, planned, run_request, standing};
use crate::controller::instructions::{self, Applied, Roster};
use crate::controller::recap::mode_name;
use crate::controller::{Ctx, Effect, ultra};

pub(in crate::controller::chat) const NAME: &str = "argonauts_start_workflow";

pub(super) fn tool() -> MainTool {
    MainTool {
        name: NAME,
        about: "`argonauts_start_workflow` hands work to the user's crew: Argonauts starts the \
                workflow they chose, other agents plan, build and review the work, and its \
                closing answer lands in this conversation.",
        offered,
        call,
    }
}

/// Always while a workflow is in use, since it is how work reaches it. In solo, when
/// `solo.start_workflow_tool` lets the session start one you asked for, or when ultracode on a
/// CLI with no workflow of its own sends real work to one on that same model.
fn offered(ctx: &Ctx, chat: &ChatState) -> bool {
    standing::workflow_for(ctx, chat).is_some()
        || ctx.settings().solo.start_workflow_tool
        || ultra::runs_on_the_same_model(ctx.settings())
}

fn call(ctx: &mut Ctx, chat: &mut ChatState, call: Call) -> Vec<Effect> {
    let Some(turn) = chat.turn.as_ref() else {
        return Vec::new();
    };
    if turn.run_began {
        call.answer("A workflow already started for this request. Stop here: it does the work.");
        return Vec::new();
    }
    let typed = turn.text.clone();
    // `/plan <task>`: the workflow plans, and the plan waits for you whatever the mode.
    let plan_first = turn.plan_first;
    let how_to_run: Vec<Instruction> = call
        .arguments
        .get("how_to_run")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|instruction| serde_json::from_value(instruction.clone()).ok())
        .collect();
    let applied = instructions::check(&how_to_run, &typed, ctx.settings(), &Roster::new(ctx), None);
    let in_use = standing::workflow_for(ctx, chat);
    // With ultracode on in solo on a CLI with no workflow of its own, real work goes to a
    // workflow on that same model, as Claude Code's own would.
    let same_model = in_use.is_none() && ultra::runs_on_the_same_model(ctx.settings());
    let mode = match leaves(in_use.is_some(), ctx.settings(), &applied) {
        Ok(mode) => mode,
        Err(_) if same_model => RunMode::Crew,
        Err(stays) => {
            call.answer(stays);
            return Vec::new();
        }
    };
    if let Some(turn) = chat.turn.as_mut() {
        turn.run_began = true;
    }
    let account = call.arguments.get("request").and_then(Value::as_str);
    let handed = handed_over(&typed, account.unwrap_or_default());
    let mut start = run_request(ctx, handed, mode, None, Some(call.turn.clone()));
    start.forced = false;
    start.triage = Some(TriageDecision {
        mode,
        escalate_to_crew: false,
        rule: TriageDecision::AS_YOU_ASKED.to_owned(),
        reason: String::new(),
        // A run works in a copy, so solo that starts one is checked.
        solo: (mode == RunMode::Solo).then_some(SoloStyle::Checked),
    });
    applied.apply_to(&mut start);
    // A setup you kept for this request alone holds for the workflow it starts, and so do the
    // steps you kept for it or for the whole conversation.
    start
        .request_settings
        .extend(super::super::talk::request_settings(chat, &call.turn));
    start.required_steps = super::super::talk::required_steps(chat, &call.turn);
    if same_model && mode == RunMode::Crew {
        start
            .request_settings
            .extend(ultra::same_model(ctx.settings()));
    }
    super::super::goal::carry(chat, &mut start);
    if plan_first {
        start = planned(start);
    }
    keep::offer(ctx, chat, &applied);
    let started = match in_use {
        Some(workflow) => format!("The {workflow} workflow started on this."),
        None if same_model => "A workflow on your own model started on this, since ultracode is \
                               on."
        .to_owned(),
        None => format!(
            "A {} run started on this, as the user asked.",
            mode_name(mode)
        ),
    };
    call.answer(format!(
        "{started} Say so in one line and stop: its agents do the work, the user watches it, \
         and its closing answer lands in this conversation."
    ));
    vec![handover::start(start)]
}

/// What the workflow is asked: your message as you typed it, and after it the session's own
/// account of the work when it adds something, marked as the session's. Your last message may
/// be no more than "yes, do that", and the agents that take the work haven't read the
/// conversation.
fn handed_over(typed: &str, account: &str) -> String {
    let (typed, account) = (typed.trim(), account.trim());
    if account.is_empty() || account == typed {
        return typed.to_owned();
    }
    format!("{typed}\n\nFrom the conversation, as the main agent put it: {account}")
}

/// The run a call starts, or what the session is told when the work stays with it.
/// `workflow_in_use` is false in solo, where only your words send work out.
fn leaves(
    workflow_in_use: bool,
    settings: &Settings,
    applied: &Applied,
) -> Result<RunMode, &'static str> {
    if !workflow_in_use {
        return asked_for(applied).ok_or("The user didn't ask for a workflow, so do it yourself.");
    }
    let asked = applied.asked;
    // "Just do it yourself" keeps one request with the session, whatever is in use.
    if asked.mode == Some(RunMode::Solo) && asked.solo != Some(SoloStyle::Checked) {
        return Err("The user asked you to do this one yourself, so do it here.");
    }
    Ok(match (asked.mode, settings.triage.force) {
        (Some(mode), _) => mode,
        // Maker-checker and best-of-n are one worker in a checked copy.
        (None, Some(ForcedMode::Solo)) => RunMode::Solo,
        (None, Some(ForcedMode::Crew) | None) => RunMode::Crew,
    })
}

/// With solo in use, the run your checked words ask for: the crew, a checked copy, or other
/// agents in roles, which a turn of this session can't give. Solo in your checkout is this turn
/// already, and a model, an effort, or a tightening alone changes nothing a run would add, so
/// none of them starts one.
fn asked_for(applied: &Applied) -> Option<RunMode> {
    use InstructionKind as Kind;
    let asked = applied.asked;
    if asked.mode == Some(RunMode::Crew) {
        return Some(RunMode::Crew);
    }
    let roles = applied.instructions.iter().any(|instruction| {
        instruction.followed()
            && matches!(
                instruction.kind,
                Kind::Lead
                    | Kind::Coordinate
                    | Kind::Build
                    | Kind::Review
                    | Kind::Judge
                    | Kind::Plan
                    | Kind::Check
                    | Kind::Crew
                    | Kind::Others
                    | Kind::Exclude
            )
    });
    (asked.solo == Some(SoloStyle::Checked) || roles).then_some(RunMode::Solo)
}

#[cfg(test)]
mod tests {
    use argonauts_core::steps::intake::InstructionScope;

    use super::*;
    use crate::controller::instructions::{Agent, AgentState};

    fn roster() -> Roster {
        let agent = |id: &str| Agent {
            id: id.to_owned(),
            name: id.to_owned(),
            family: id.to_owned(),
            state: AgentState::Ready,
            ..Agent::default()
        };
        Roster {
            agents: vec![
                agent("claude-code"),
                agent("codex"),
                agent("antigravity-cli"),
            ],
        }
    }

    fn said(kind: InstructionKind, says: &str, who: &str, value: &str) -> Instruction {
        Instruction {
            says: says.to_owned(),
            kind,
            who: who.to_owned(),
            model: String::new(),
            effort: String::new(),
            value: value.to_owned(),
            scope: InstructionScope::ThisRequest,
        }
    }

    /// A workflow that is one worker: `force` is how its file says so.
    fn one_worker(workflow: &str) -> Settings {
        let mut settings = Settings {
            workflow: workflow.to_owned(),
            ..Settings::default()
        };
        settings.triage.force = Some(ForcedMode::Solo);
        settings
    }

    fn solo() -> Settings {
        one_worker("solo")
    }

    fn checked(settings: &Settings, message: &str, list: &[Instruction]) -> Applied {
        instructions::check(list, message, settings, &roster(), None)
    }

    /// In solo only words that ask for the crew, a checked copy, or other agents in roles
    /// start anything: solo in your checkout, a model or an effort, and words you didn't write
    /// are this turn.
    #[test]
    fn in_solo_a_workflow_starts_only_for_what_this_turn_cannot_do() {
        use InstructionKind::{Crew, Effort, Mode, Review};
        let settings = solo();
        let asked = |message: &str, list: &[Instruction]| {
            leaves(false, &settings, &checked(&settings, message, list))
        };
        let review = said(Review, "have codex review this", "codex", "");
        assert_eq!(
            asked("Have codex review this.", &[review]),
            Ok(RunMode::Solo)
        );
        let list = [
            said(Crew, "use the crew", "", ""),
            said(Review, "codex reviews", "codex", ""),
        ];
        assert_eq!(
            asked("Use the crew, codex reviews.", &list),
            Ok(RunMode::Crew)
        );
        let own = said(Mode, "just do it solo", "", "solo");
        assert!(asked("Just do it solo.", &[own]).is_err());
        let picked = said(Mode, "the", "", "crew");
        assert!(asked("Fix the rounding in invoice/money.py", &[picked]).is_err());
        let effort = said(Effort, "max effort", "", "max");
        assert!(asked("Fix it on max effort.", &[effort]).is_err());
        assert!(asked("What does parse_due do?", &[]).is_err());
    }

    #[test]
    fn the_request_is_your_words_first_then_the_session_s_account() {
        assert_eq!(handed_over(" yes, do that ", ""), "yes, do that");
        assert_eq!(
            handed_over("add due dates", "add due dates"),
            "add due dates"
        );
        assert_eq!(
            handed_over(
                "yes, do that",
                "Add due dates to tasks, in the cli and the store."
            ),
            "yes, do that\n\nFrom the conversation, as the main agent put it: Add due dates to \
             tasks, in the cli and the store."
        );
    }

    /// With a workflow in use the session's call is enough, and your words still decide for
    /// one request.
    #[test]
    fn with_a_workflow_in_use_the_call_starts_it_unless_your_words_keep_it() {
        use InstructionKind::Mode;
        let council = Settings::default();
        assert_eq!(
            standing::workflow_in_use(&council).as_deref(),
            Some("council")
        );
        let asked = |settings: &Settings, message: &str, list: &[Instruction]| {
            let in_use = standing::workflow_in_use(settings).is_some();
            leaves(in_use, settings, &checked(settings, message, list))
        };
        assert_eq!(
            asked(&council, "Add due dates across the cli and the store.", &[]),
            Ok(RunMode::Crew)
        );
        let own = said(Mode, "just do it yourself", "", "solo");
        assert_eq!(
            asked(&council, "Just do it yourself: add due dates.", &[own]),
            Err("The user asked you to do this one yourself, so do it here.")
        );
        // Maker-checker is one worker in a checked copy, and "use the crew" still wins.
        let mut maker_checker = one_worker("maker-checker");
        maker_checker.solo.review = true;
        assert_eq!(
            asked(&maker_checker, "Add due dates.", &[]),
            Ok(RunMode::Solo)
        );
        let crew = said(Mode, "use the crew for this", "", "crew");
        assert_eq!(
            asked(
                &maker_checker,
                "Use the crew for this: add due dates.",
                &[crew]
            ),
            Ok(RunMode::Crew)
        );
        assert_eq!(
            standing::workflow_in_use(&maker_checker).as_deref(),
            Some("maker-checker")
        );
        assert_eq!(standing::workflow_in_use(&solo()), None);
    }
}
