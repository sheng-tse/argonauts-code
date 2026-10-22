//! `argonauts_start_workflow`: the main session hands work over, and the workflow in use
//! starts on it.
//!
//! Who decides to call it: the session's own judgment, by its standing lines, while a workflow
//! is in use. Here code decides whether the call may start one. With a workflow in use it may,
//! unless your words kept the request with the session. With Captain in use it may only when your
//! own words asked for the crew, a checked worktree, or other agents in roles, and those words go
//! through the same checks as an estimate's do, against your message as you typed it, so a word
//! the session picked out can't start anything.

use argonauts_core::events::run::{
    CaptainOrCrewDecision, InstructionOutcome, RunMode, handed_over,
};
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::{CaptainKind, ForcedMode};
use argonauts_core::steps::intake::{Instruction, InstructionKind};
use argonauts_core::text::sentence_case;
use argonauts_crew::ultra as ultra_engine;
use argonauts_runner::Loaded;
use serde_json::Value;

use super::{Call, MainTool};
use crate::controller::conversation::{
    ConversationState, fork, handover, keep, planned, run_news, run_request, standing,
};
use crate::controller::instructions::{self, Applied, Roster};
use crate::controller::recap::mode_name;
use crate::controller::{Ctx, Effect, crew_lines, ultra, workflows};

pub(in crate::controller::conversation) const NAME: &str = "argonauts_start_workflow";

pub(super) fn tool() -> MainTool {
    MainTool {
        name: NAME,
        about: "`argonauts_start_workflow` hands work to the user's crew: Argonauts Code starts the \
                workflow they chose, other agents plan, build and review the work, and its \
                closing answer lands in this conversation.",
        offered,
        call,
    }
}

/// Always while a workflow is in use, since it is how work reaches it. In Captain, when
/// `captain.start_workflow_tool` lets the session start one you asked for, or when ultracode on a
/// CLI with no workflow of its own sends real work to one on that same model.
fn offered(ctx: &Ctx, conversation: &ConversationState) -> bool {
    standing::workflow_for(ctx, conversation).is_some()
        || ctx.settings().captain.start_workflow_tool
        || ultra::runs_on_the_same_model(ctx.settings())
}

fn call(ctx: &mut Ctx, conversation: &mut ConversationState, call: Call) -> Vec<Effect> {
    let Some(turn) = conversation.turn.as_ref() else {
        return Vec::new();
    };
    if turn.run_began {
        call.answer("A workflow already started for this request. Stop here: it does the work.");
        return Vec::new();
    }
    let typed = turn.text.clone();
    // `/plan <task>`: the workflow plans, and the plan waits for you whatever the mode.
    let plan_first = turn.plan_first;
    let how_to_run = instructions_in(call.arguments.get("how_to_run"));
    let applied = instructions::check(&how_to_run, &typed, ctx.settings(), &Roster::new(ctx), None);
    let in_use = standing::workflow_for(ctx, conversation);
    // With ultracode on in Captain on a CLI with no workflow of its own, real work goes to a
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
    if let Some(turn) = conversation.turn.as_mut() {
        turn.run_began = true;
    }
    let account = call.arguments.get("request").and_then(Value::as_str);
    let handed = handed_over(&typed, account.unwrap_or_default());
    let mut start = run_request(ctx, handed, mode, None, Some(call.turn.clone()));
    start.forced = false;
    start.captain_or_crew = Some(CaptainOrCrewDecision {
        mode,
        move_to_crew: false,
        rule: CaptainOrCrewDecision::AS_YOU_ASKED.to_owned(),
        reason: String::new(),
        // A run works in a worktree, so Captain that starts one is checked.
        captain: (mode == RunMode::Captain).then_some(CaptainKind::Checked),
    });
    applied.apply_to(&mut start);
    // Captain has no crew workflow of its own, so the crew your words ask for runs the
    // orchestrator's, led by the main agent, which also builds unless your words named who
    // does. The models your words named come after, and win.
    let orchestrated = in_use.is_none() && !same_model && mode == RunMode::Crew;
    if orchestrated {
        let (before, after) = orchestrator_crew(ctx.settings(), &applied);
        start.request_settings.splice(0..0, before);
        start.request_settings.extend(after);
    }
    // A setup you kept for this request alone holds for the workflow it starts, and so do the
    // steps you kept for it or for the whole conversation.
    start
        .request_settings
        .extend(super::super::talk::request_settings(
            conversation,
            &call.turn,
        ));
    start.required_steps = super::super::talk::required_steps(conversation, &call.turn);
    if same_model && mode == RunMode::Crew {
        start
            .request_settings
            .extend(ultra::same_model(ctx.settings()));
        crate::telemetry::used(&ctx.env.paths, crate::telemetry::Use::Ultracode);
    }
    super::super::goal::carry(conversation, &mut start);
    if plan_first {
        start = planned(start);
    }
    // The crew shown is the one the run has, the models it adds around your words included.
    keep::offer_in_run(ctx, conversation, &applied, Some(&start.request_settings));
    let (what, began) = match &in_use {
        Some(workflow) => (
            format!("The {workflow} workflow"),
            format!("The {workflow} workflow started on this."),
        ),
        None if same_model => (
            "The workflow on your own model".to_owned(),
            "A workflow on your own model started on this, since ultracode is on.".to_owned(),
        ),
        None => (
            format!("The {} run", mode_name(mode)),
            format!(
                "A {} run started on this, as the user asked.",
                mode_name(mode)
            ),
        ),
    };
    let main = &ctx.settings().crew.main;
    let claude_codes_own = orchestrated && runs_as_claude_codes_own(ctx, &start.request_settings);
    let mut crew = match &in_use {
        Some(workflow) => Crew::asked(workflow, &applied),
        None if claude_codes_own => Crew::claude_codes_own(main, &applied),
        None if mode == RunMode::Crew => Crew::orchestrated(main, &applied),
        None => Crew::checked(main, &applied),
    };
    // Claude Code's own workflow plans in a session of its own, from the brief.
    let begins = crew_lines::with_request(ctx.settings(), &start.request_settings)
        .filter(|_| mode == RunMode::Crew && !claude_codes_own)
        .map(|settings| match (&in_use, &crew.plans) {
            (None, Some(plans)) => fork::start_on(ctx, conversation, &settings, plans),
            _ => fork::start(ctx, conversation, &settings),
        });
    if in_use.is_none() {
        crew.starts = begins
            .as_ref()
            .map(|begins| begins.words(|id| ctx.names.cli(id)));
    }
    let written_by = conversation
        .session
        .as_ref()
        .map_or(main, |session| &session.worker);
    let tokens = conversation.context.as_ref().map(|context| context.used);
    start.starts_from = begins
        .as_ref()
        .map(|begins| begins.starts_from(written_by, tokens, |id| ctx.names.cli(id)));
    let line = crew.line(|worker| ctx.names.worker(&ctx.names.running(worker)));
    let began = format!(
        "{began} Your whole reply to the user is this one line, word for word, with nothing \
         before or after it: {line}"
    );
    // The answer waits until the run is under way or has failed, so it tells the truth. An
    // orchestrator that copies this session hears it once the run is taken, so the copy holds
    // the answer rather than a call still waiting for one.
    let when = match &begins {
        Some(begins) if begins.forks() => run_news::Answer::WhenTaken,
        _ => run_news::Answer::WhenUnderWay,
    };
    run_news::hold(conversation, call.reply, what, began, when);
    vec![handover::start(start)]
}

/// The instructions a call passed: a list of them, as the tool asks, or one alone, either of
/// them also as JSON text, which a session sometimes sends in its place.
fn instructions_in(how_to_run: Option<&Value>) -> Vec<Instruction> {
    match how_to_run {
        Some(Value::Array(list)) => list
            .iter()
            .filter_map(|instruction| serde_json::from_value(instruction.clone()).ok())
            .collect(),
        Some(Value::Object(_)) => instructions_in(Some(&Value::Array(vec![
            how_to_run.cloned().unwrap_or_default(),
        ]))),
        Some(Value::String(text)) => match serde_json::from_str::<Value>(text) {
            Ok(value @ (Value::Array(_) | Value::Object(_))) => instructions_in(Some(&value)),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// Whether the engine will hand this crew run to Claude Code's own workflow, by the test it
/// makes itself: the workflow the run takes plans and hands out the work, and its orchestrator
/// (the first of the role's models that can run here) and every worker are on Claude Code,
/// with the request's own settings on top.
fn runs_as_claude_codes_own(ctx: &Ctx, request_settings: &[String]) -> bool {
    let Some(settings) = crew_lines::with_request(ctx.settings(), request_settings) else {
        return false;
    };
    let Ok(Loaded::Steps(file)) = workflows::library(ctx).load(&settings.workflow) else {
        return false;
    };
    ultra_engine::plans_and_delegates(&file)
        && ultra_engine::claude_codes_own(&settings, &ctx.services.workers.available()).is_some()
}

/// The run a call starts, or what the session is told when the work stays with it.
/// `workflow_in_use` is false in Captain, where only your words send work out.
fn leaves(
    workflow_in_use: bool,
    settings: &Settings,
    applied: &Applied,
) -> Result<RunMode, String> {
    if !workflow_in_use {
        return asked_for(applied).ok_or_else(|| why_not(applied));
    }
    let asked = applied.asked;
    // "Just do it yourself" keeps one request with the session, whatever is in use.
    if asked.mode == Some(RunMode::Captain) && asked.captain != Some(CaptainKind::Checked) {
        return Err("The user asked you to do this one yourself, so do it here.".to_owned());
    }
    Ok(match (asked.mode, settings.captain_or_crew.force) {
        (Some(mode), _) => mode,
        // Maker-checker and best-of-n are one worker in a checked worktree.
        (None, Some(ForcedMode::Captain)) => RunMode::Captain,
        (None, Some(ForcedMode::Crew) | None) => RunMode::Crew,
    })
}

/// With Captain in use, the run your checked words ask for: the crew, a checked worktree, or other
/// agents in roles, which a turn of this session can't give. Captain in your checkout is this turn
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
                Kind::MainAgent
                    | Kind::Orchestrate
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
    (asked.captain == Some(CaptainKind::Checked) || roles).then_some(RunMode::Captain)
}

/// The request settings that give a crew run from Captain the orchestrator's workflow: those that
/// go before your words', and those after. The main agent orchestrates, and builds unless your
/// words named who does, over the builders the check infers for reviewers you named.
fn orchestrator_crew(settings: &Settings, applied: &Applied) -> (Vec<String>, Vec<String>) {
    let mut before = vec!["workflow=\"orchestrator\"".to_owned()];
    if applied.main_agent.is_none() && applied.workers(RoleName::Orchestrator).is_empty() {
        before.push("crew.orchestrator=\"main\"".to_owned());
    }
    let mut after = Vec::new();
    if builders_asked(applied).is_empty() {
        let main = toml::Value::String(settings.crew.main.to_string());
        after.push(format!(
            "roles.{}.models=[{main}]",
            RoleName::Worker.as_str()
        ));
    }
    (before, after)
}

/// Who does what in the workflow a call started, as the session tells you.
#[derive(Debug, Clone, PartialEq)]
struct Crew {
    /// As the line names it: "Orchestrator", "Council".
    workflow: String,
    /// The reviewers your words named.
    reviewers: Vec<ModelRef>,
    /// Who plans, when your words or this call decided it.
    plans: Option<ModelRef>,
    /// Who builds, likewise.
    builds: Vec<ModelRef>,
    /// What the orchestrator starts from, said after its name: `carrying this conversation`.
    starts: Option<String>,
}

impl Crew {
    /// The workflow in use, with only the models your words named.
    fn asked(workflow: &str, applied: &Applied) -> Self {
        Self {
            workflow: sentence_case(workflow),
            reviewers: reviewers_asked(applied),
            plans: applied.main_agent.clone(),
            builds: builders_asked(applied),
            starts: None,
        }
    }

    /// The orchestrator's workflow a crew run from Captain takes: the main agent plans and builds
    /// unless your words named others.
    fn orchestrated(main: &ModelRef, applied: &Applied) -> Self {
        let plans = applied
            .main_agent
            .clone()
            .or_else(|| applied.workers(RoleName::Orchestrator).into_iter().next())
            .unwrap_or_else(|| main.clone());
        let mut builds = builders_asked(applied);
        if builds.is_empty() {
            builds.push(main.clone());
        }
        Self {
            workflow: "Orchestrator".to_owned(),
            reviewers: reviewers_asked(applied),
            plans: Some(plans),
            builds,
            starts: None,
        }
    }

    /// The orchestrator's workflow when Claude Code runs it as its own: the run is a Captain
    /// that Claude Code plans and builds, which its row names, with the same models doing the
    /// same work.
    fn claude_codes_own(main: &ModelRef, applied: &Applied) -> Self {
        Self {
            workflow: "Captain, as Claude Code's own".to_owned(),
            ..Self::orchestrated(main, applied)
        }
    }

    /// One worker in a checked worktree, as a review asked for in Captain starts.
    fn checked(main: &ModelRef, applied: &Applied) -> Self {
        Self {
            workflow: "Captain, in a checked worktree".to_owned(),
            reviewers: reviewers_asked(applied),
            plans: None,
            starts: None,
            builds: vec![
                applied
                    .captain_main_agent
                    .clone()
                    .unwrap_or_else(|| main.clone()),
            ],
        }
    }

    /// "Starting a workflow: Orchestrator, with **GPT 5.6 Luna on Codex** reviewing as you
    /// asked; **Haiku 5.5 on Claude Code** plans and builds." Once the line knows what the
    /// orchestrator starts from, it names the orchestrator first: "Starting a workflow:
    /// Orchestrator, **Opus 5.5 on Claude Code**, carrying this conversation; **GPT 6 Luna on
    /// Codex** reviews as you asked, and **Haiku 5.5 on Claude Code** builds."
    fn line(&self, name: impl Fn(&ModelRef) -> String) -> String {
        let names = |workers: &[ModelRef]| {
            let named: Vec<String> = workers
                .iter()
                .map(|worker| format!("**{}**", name(worker)))
                .collect();
            named.join(" and ")
        };
        let mut line = format!("Starting a workflow: {}", self.workflow);
        let one = |workers: &[ModelRef], singular: &'static str, plural: &'static str| {
            if workers.len() == 1 { singular } else { plural }
        };
        if let (Some(plans), Some(starts)) = (&self.plans, &self.starts) {
            line.push_str(&format!(
                ", {}, {starts}",
                names(std::slice::from_ref(plans))
            ));
            let mut doing = Vec::new();
            if !self.reviewers.is_empty() {
                doing.push(format!(
                    "{} {} as you asked",
                    names(&self.reviewers),
                    one(&self.reviewers, "reviews", "review")
                ));
            }
            if !self.builds.is_empty() {
                doing.push(format!(
                    "{} {}",
                    names(&self.builds),
                    one(&self.builds, "builds", "build")
                ));
            }
            if !doing.is_empty() {
                line.push_str("; ");
                line.push_str(&doing.join(", and "));
            }
            line.push('.');
            return line;
        }
        if !self.reviewers.is_empty() {
            line.push_str(&format!(
                ", with {} reviewing as you asked",
                names(&self.reviewers)
            ));
        }
        let build = one(&self.builds, "builds", "build");
        let doing = match (&self.plans, self.builds.as_slice()) {
            (Some(plans), [builds]) if plans == builds => Some(format!(
                "{} plans and builds",
                names(std::slice::from_ref(plans))
            )),
            (Some(plans), []) => Some(format!("{} plans", names(std::slice::from_ref(plans)))),
            (Some(plans), builds) => Some(format!(
                "{} plans and {} {build}",
                names(std::slice::from_ref(plans)),
                names(builds)
            )),
            (None, []) => None,
            (None, builds) => Some(format!("{} {build}", names(builds))),
        };
        if let Some(doing) = doing {
            line.push_str("; ");
            line.push_str(&doing);
        }
        line.push('.');
        line
    }
}

/// The reviewers your words named, when an instruction to review was followed.
fn reviewers_asked(applied: &Applied) -> Vec<ModelRef> {
    asked(applied, InstructionKind::Review, RoleName::Reviewer)
}

/// The builders your words named, never those the check inferred.
fn builders_asked(applied: &Applied) -> Vec<ModelRef> {
    asked(applied, InstructionKind::Build, RoleName::Worker)
}

/// Who fills `role`, when an instruction of `kind` was followed.
fn asked(applied: &Applied, kind: InstructionKind, role: RoleName) -> Vec<ModelRef> {
    let followed = applied
        .instructions
        .iter()
        .any(|instruction| instruction.kind == kind && instruction.followed());
    if followed {
        applied.workers(role)
    } else {
        Vec::new()
    }
}

/// Why no workflow started in Captain, instruction by instruction, so the session can correct its
/// call when your words did ask for one, and otherwise knows to do the work itself. The first
/// line alone is what the screen shows; the rest is for the session.
fn why_not(applied: &Applied) -> String {
    let mut text = String::from(
        "No workflow started.\nWith Captain, one starts only when the user's own words ask for \
         the crew or a workflow, a checked worktree, or other agents in roles.",
    );
    if applied.instructions.is_empty() {
        text.push_str(
            " The call passed no instruction in `how_to_run`, so nothing asked for one. If the \
             user did, call again with a list of their words for it, one instruction for each \
             thing they asked, the agents they named in roles included, `says` copied exactly, \
             such as [{\"says\": \"use a workflow\", \"kind\": \"mode\", \"value\": \"crew\"}, \
             {\"says\": \"with Codex as the reviewer\", \"kind\": \"review\", \"who\": \"codex\"}].",
        );
    } else {
        text.push_str(" What came of each instruction:");
        for instruction in &applied.instructions {
            let kind = serde_json::to_value(instruction.kind)
                .ok()
                .and_then(|kind| kind.as_str().map(str::to_owned))
                .unwrap_or_default();
            let outcome = match &instruction.outcome {
                InstructionOutcome::Followed { said, .. } if said.is_empty() => {
                    "followed, but it asks for nothing this turn can't do".to_owned()
                }
                InstructionOutcome::Followed { said, .. } => {
                    format!("followed ({said}), but it asks for nothing this turn can't do")
                }
                InstructionOutcome::NotFollowed { why } => format!("not followed: {why}"),
                InstructionOutcome::Dropped => {
                    "dropped: these words aren't in the user's message; copy them exactly"
                        .to_owned()
                }
            };
            text.push_str(&format!(
                "\n- \"{}\" ({kind}): {outcome}.",
                instruction.says
            ));
        }
        text.push_str(
            "\nIf the user's words do ask for a workflow, correct the call and make it again: \
             `who` takes the CLI or model the user named, such as \"codex\", never a role such \
             as \"reviewer\".",
        );
    }
    text.push_str(" If they don't, do the work yourself.");
    text
}

#[cfg(test)]
mod tests {
    use argonauts_core::steps::intake::InstructionScope;
    use serde_json::json;

    use super::*;
    use crate::controller::instructions::{Agent, AgentState};

    fn roster() -> Roster {
        let agent = |id: &str| Agent {
            id: id.to_owned(),
            name: id.to_owned(),
            provider: id.to_owned(),
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
        settings.captain_or_crew.force = Some(ForcedMode::Captain);
        settings
    }

    fn captain() -> Settings {
        one_worker("captain")
    }

    fn checked(settings: &Settings, message: &str, list: &[Instruction]) -> Applied {
        instructions::check(list, message, settings, &roster(), None)
    }

    /// In Captain only words that ask for the crew, a checked worktree, or other agents in roles
    /// start anything: Captain in your checkout, a model or an effort, and words you didn't write
    /// are this turn.
    #[test]
    fn in_captain_a_workflow_starts_only_for_what_this_turn_cannot_do() {
        use InstructionKind::{Crew, Effort, Mode, Review};
        let settings = captain();
        let asked = |message: &str, list: &[Instruction]| {
            leaves(false, &settings, &checked(&settings, message, list))
        };
        let review = said(Review, "have codex review this", "codex", "");
        assert_eq!(
            asked("Have codex review this.", &[review]),
            Ok(RunMode::Captain)
        );
        let list = [
            said(Crew, "use the crew", "", ""),
            said(Review, "codex reviews", "codex", ""),
        ];
        assert_eq!(
            asked("Use the crew, codex reviews.", &list),
            Ok(RunMode::Crew)
        );
        let own = said(Mode, "just do it alone", "", "captain");
        assert!(asked("Just do it alone.", &[own]).is_err());
        let picked = said(Mode, "the", "", "crew");
        assert!(asked("Fix the rounding in invoice/money.py", &[picked]).is_err());
        let effort = said(Effort, "max effort", "", "max");
        assert!(asked("Fix it on max effort.", &[effort]).is_err());
        assert!(asked("What does parse_due do?", &[]).is_err());
    }

    /// "Use a workflow" asks for the crew as "use the crew" does, and the agent named for a
    /// role counts whether the call put it in `who`, in `value` with the role in `who`, or only
    /// in the words.
    #[test]
    fn a_workflow_with_codex_as_the_reviewer_starts_the_crew_with_codex_reviewing() {
        use InstructionKind::{Mode, Review};
        let settings = captain();
        let message = "Use a workflow for this, with Codex as the reviewer, and choose \
                       everything else yourself. Fix the money rounding.";
        let workflow = said(Mode, "Use a workflow for this", "", "");
        for (who, value) in [("codex", ""), ("reviewer", "Codex"), ("the reviewer", "")] {
            let review = said(Review, "with Codex as the reviewer", who, value);
            let applied = checked(&settings, message, &[workflow.clone(), review]);
            assert_eq!(
                leaves(false, &settings, &applied),
                Ok(RunMode::Crew),
                "{who}"
            );
            assert!(
                applied
                    .settings
                    .contains(&("roles.reviewer.models".to_owned(), "[\"codex\"]".to_owned())),
                "{who}: {:?}",
                applied.settings
            );
        }
        // The model may copy the whole sentence into `says`: "choose everything else
        // yourself" hands the rest over to the crew, and never asks for Captain.
        let whole = "Use a workflow for this, with Codex as the reviewer, and choose everything \
                     else yourself";
        for value in ["crew", ""] {
            let workflow = said(Mode, whole, "", value);
            let review = said(Review, "with Codex as the reviewer", "codex", "");
            for list in [vec![workflow.clone(), review], vec![workflow]] {
                let applied = checked(&settings, message, &list);
                assert_eq!(
                    leaves(false, &settings, &applied),
                    Ok(RunMode::Crew),
                    "value {value:?}, {} instructions",
                    list.len()
                );
                assert_eq!(applied.asked.mode, Some(RunMode::Crew));
            }
        }
        // "Do it yourself" still asks for Captain.
        let yourself = said(Mode, "just do it yourself", "", "captain");
        let applied = checked(&settings, "Just do it yourself.", &[yourself]);
        assert_eq!(applied.asked.mode, Some(RunMode::Captain));
        // "Don't use a workflow" keeps it as Captain, and "the captain workflow" says Captain.
        let captain_words = said(Mode, "don't use a workflow", "", "captain");
        let applied = checked(
            &settings,
            "Fix it, but don't use a workflow.",
            &[captain_words],
        );
        assert_eq!(applied.asked.mode, Some(RunMode::Captain));
        let named = said(Mode, "the captain workflow", "", "");
        let applied = checked(&settings, "Run it on the captain workflow.", &[named]);
        assert_eq!(applied.asked.mode, Some(RunMode::Captain));
    }

    /// A refusal says why for each instruction, so the session can correct its call.
    #[test]
    fn a_refusal_names_each_instruction_and_why() {
        use InstructionKind::{Effort, Review};
        let settings = captain();
        let message = "Fix it on max effort, with the reviewer checking it.";
        let list = [
            said(Effort, "on max effort", "", "max"),
            said(Review, "with the reviewer checking it", "reviewer", ""),
            said(Review, "have codex review it", "codex", ""),
        ];
        let refused = leaves(false, &settings, &checked(&settings, message, &list)).unwrap_err();
        assert_eq!(refused.lines().next(), Some("No workflow started."));
        assert!(
            refused.contains("\"on max effort\" (effort): followed"),
            "{refused}"
        );
        assert!(
            refused.contains(
                "\"with the reviewer checking it\" (review): not followed: reviewer is a role, \
                 and the words name no agent for it."
            ),
            "{refused}"
        );
        assert!(
            refused.contains("\"have codex review it\" (review): dropped"),
            "{refused}"
        );
        assert!(refused.contains("never a role"), "{refused}");
        let nothing = leaves(false, &settings, &checked(&settings, message, &[])).unwrap_err();
        assert!(nothing.contains("passed no instruction"), "{nothing}");
        assert_eq!(nothing.lines().next(), Some("No workflow started."));
    }

    /// The instructions come as a list, as the tool asks, and also as one alone or as JSON
    /// text, which a session sometimes sends instead: a reviewer sent that way still counts.
    #[test]
    fn instructions_count_however_the_call_wrote_them() {
        let review =
            json!({"says": "with Codex as the reviewer", "kind": "review", "who": "codex"});
        let mode = json!({"says": "Use a workflow", "kind": "mode", "value": "crew"});
        let list = json!([mode, review]);
        let read = |value: Value| instructions_in(Some(&value)).len();
        assert_eq!(read(list.clone()), 2);
        assert_eq!(read(Value::String(list.to_string())), 2);
        assert_eq!(read(review.clone()), 1);
        assert_eq!(read(Value::String(review.to_string())), 1);
        assert_eq!(read(Value::String("use a workflow".to_owned())), 0);
        assert_eq!(instructions_in(None).len(), 0);
    }

    /// The session says what started and with whom, and a crew from Captain runs the
    /// orchestrator's workflow with the main agent planning and building.
    #[test]
    fn the_session_says_what_started_and_with_whom() {
        use InstructionKind::{Build, Mode, Review};
        let settings = captain();
        let message = "Use a workflow for this, with Codex as the reviewer. Fix the rounding.";
        let list = [
            said(Mode, "Use a workflow for this", "", "crew"),
            said(Review, "with Codex as the reviewer", "codex", ""),
        ];
        let applied = checked(&settings, message, &list);
        let main = &settings.crew.main;
        let name = |worker: &ModelRef| match worker.agent.as_str() {
            "codex" => "GPT 5.6 Luna on Codex".to_owned(),
            _ => "Haiku 5.5 on Claude Code".to_owned(),
        };
        assert_eq!(
            Crew::orchestrated(main, &applied).line(name),
            "Starting a workflow: Orchestrator, with **GPT 5.6 Luna on Codex** reviewing as you \
             asked; **Haiku 5.5 on Claude Code** plans and builds."
        );
        let main_list = toml::Value::String(main.to_string());
        assert_eq!(
            orchestrator_crew(&settings, &applied),
            (
                vec![
                    "workflow=\"orchestrator\"".to_owned(),
                    "crew.orchestrator=\"main\"".to_owned(),
                ],
                vec![format!("roles.worker.models=[{main_list}]")],
            )
        );
        // Builders you named build, and the workflow in use keeps its own models.
        let message = "Use the crew, have codex build it.";
        let list = [
            said(Mode, "Use the crew", "", "crew"),
            said(Build, "have codex build it", "codex", ""),
        ];
        let applied = checked(&settings, message, &list);
        assert_eq!(
            Crew::orchestrated(main, &applied).line(name),
            "Starting a workflow: Orchestrator; **Haiku 5.5 on Claude Code** plans and \
             **GPT 5.6 Luna on Codex** builds."
        );
        assert!(orchestrator_crew(&settings, &applied).1.is_empty());
        assert_eq!(
            Crew::asked("council", &Applied::default()).line(name),
            "Starting a workflow: Council."
        );
        // Claude Code runs the orchestrator's workflow as its own when it plans and builds.
        let list = [
            said(Mode, "Use a workflow for this", "", "crew"),
            said(Review, "with Codex as the reviewer", "codex", ""),
        ];
        let message = "Use a workflow for this, with Codex as the reviewer. Fix the rounding.";
        let applied = checked(&settings, message, &list);
        assert_eq!(
            Crew::claude_codes_own(main, &applied).line(name),
            "Starting a workflow: Captain, as Claude Code's own, with **GPT 5.6 Luna on Codex** \
             reviewing as you asked; **Haiku 5.5 on Claude Code** plans and builds."
        );
    }

    /// The recorded Captain run's Starting line, GPT 6 Luna on Codex reviewing and Haiku 5.5 on
    /// Claude Code planning and building, says what its picture says, which the screen's golden
    /// draws.
    #[test]
    fn the_starting_line_reads_as_its_picture() {
        use InstructionKind::{Mode, Review};
        const PICTURE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../tui/tests/golden/starting_line_after.txt"
        ));
        let settings = captain();
        let message = "Use a workflow for this, with Codex as the reviewer, and choose everything \
                       else yourself. Fix the money rounding so the three failing tests in \
                       tests/test_money.py pass without changing those tests.";
        let list = [
            said(Mode, "Use a workflow for this", "", "crew"),
            said(Review, "with Codex as the reviewer", "codex", ""),
        ];
        let applied = checked(&settings, message, &list);
        let name = |worker: &ModelRef| match worker.agent.as_str() {
            "codex" => "GPT 6 Luna on Codex".to_owned(),
            _ => "Haiku 5.5 on Claude Code".to_owned(),
        };
        let line = Crew::claude_codes_own(&settings.crew.main, &applied).line(name);
        let rows: Vec<&str> = PICTURE.lines().collect();
        let pictured = format!(
            "{} {}",
            rows[2].trim_start_matches("⏺ ").trim_end(),
            rows[3].trim()
        );
        assert_eq!(line.replace("**", ""), pictured);
    }

    /// The Starting line names the orchestrator and what it starts from, the reviewer you
    /// asked for and the builder, as both of its pictures have it: a fork on the main
    /// session's CLI, and agy, which starts from the brief.
    #[test]
    fn the_fork_start_lines_read_as_their_picture() {
        use crate::controller::conversation::fork::{Brief, MainSession, Start};
        use argonauts_core::display_names::DisplayNames;
        const PICTURE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../tui/tests/golden/fork_start.txt"
        ));
        let model = |text: &str| -> ModelRef { text.parse().unwrap() };
        let name = |worker: &ModelRef| {
            match (worker.agent.as_str(), worker.model.as_deref()) {
                ("codex", _) => "GPT 6 Luna on Codex",
                ("antigravity-cli", _) => "Gemini 3.6 Pro on agy",
                (_, Some("opus-5-5")) => "Opus 5.5 on Claude Code",
                _ => "Haiku 5.5 on Claude Code",
            }
            .to_owned()
        };
        let names = DisplayNames::default();
        let cli = |id: &str| names.cli(id);
        let opus = model("claude-code/opus-5-5");
        let forked = Start::Fork(MainSession {
            worker: opus.clone(),
            cli_session: "main-session-1".to_owned(),
            folder: "/repo".into(),
        });
        let from_the_brief = Start::Brief(Brief::OtherCli {
            orchestrator: "antigravity-cli".to_owned(),
            main: "claude-code".to_owned(),
        });
        let rows: Vec<&str> = PICTURE.lines().collect();
        for (plans, start) in [(opus, forked), (model("antigravity-cli"), from_the_brief)] {
            let crew = Crew {
                workflow: "Orchestrator".to_owned(),
                reviewers: vec![model("codex")],
                plans: Some(plans.clone()),
                builds: vec![model("claude-code/haiku-5-5")],
                starts: Some(start.words(cli)),
            };
            let line = crew.line(name).replace("**", "");
            let head = format!("⏺ Starting a workflow: Orchestrator, {},", name(&plans));
            let at = rows
                .iter()
                .position(|row| row.starts_with(&head))
                .unwrap_or_else(|| panic!("no row starts {head:?}"));
            let pictured = format!(
                "{} {}",
                rows[at].trim_start_matches("⏺ ").trim_end(),
                rows[at + 1].trim()
            );
            assert_eq!(line, pictured);
        }
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
        let own = said(Mode, "just do it yourself", "", "captain");
        assert_eq!(
            asked(&council, "Just do it yourself: add due dates.", &[own]),
            Err("The user asked you to do this one yourself, so do it here.".to_owned())
        );
        // Maker-checker is one worker in a checked worktree, and "use the crew" still wins.
        let mut maker_checker = one_worker("maker-checker");
        maker_checker.captain.review = true;
        assert_eq!(
            asked(&maker_checker, "Add due dates.", &[]),
            Ok(RunMode::Captain)
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
        assert_eq!(standing::workflow_in_use(&captain()), None);
    }
}
