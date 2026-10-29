//! The instruction checks, table by table, against a roster of agents made by hand. A last
//! test hands what they apply to the real resolver, so every key and value they write resolves.

use argonauts_core::events::run::{InstructionOutcome, RunMode};
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::CaptainKind;
use argonauts_core::steps::intake::{Instruction, InstructionKind, InstructionScope};
use argonauts_registry::{SettingsSources, resolve};

use super::roster::Model;
use super::*;

fn levels(levels: &[&str]) -> Option<Vec<String>> {
    Some(levels.iter().map(|level| (*level).to_owned()).collect())
}

fn model(id: &str, name: &str, aliases: &[&str], efforts: Option<Vec<String>>) -> Model {
    Model {
        id: id.to_owned(),
        name: name.to_owned(),
        aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
        efforts,
    }
}

fn agent(id: &str, name: &str, provider: &str, state: AgentState, models: Vec<Model>) -> Agent {
    Agent {
        id: id.to_owned(),
        name: name.to_owned(),
        provider: provider.to_owned(),
        endpoint: false,
        state,
        models,
        cheapest: None,
        by_price: Vec::new(),
        default_model: None,
    }
}

/// Claude Code, Codex, and agy ready, OpenCode ready on a gpt model, Grok Build
/// declared but not installed, and a local endpoint.
pub(super) fn roster() -> Roster {
    let mut claude = agent(
        "claude-code",
        "Claude Code",
        "anthropic",
        AgentState::Ready,
        vec![
            model(
                "claude-opus-5-5",
                "Opus 5.5",
                &["opus"],
                levels(&["low", "medium", "high", "xhigh", "max"]),
            ),
            model(
                "claude-haiku-4-5",
                "Haiku 4.5",
                &["haiku"],
                levels(&["low", "medium", "high"]),
            ),
        ],
    );
    claude.cheapest = Some(("claude-haiku-4-5".to_owned(), None));
    let mut codex = agent(
        "codex",
        "Codex",
        "openai",
        AgentState::Ready,
        vec![model(
            "gpt-6-luna",
            "GPT-6 Luna",
            &[],
            levels(&["low", "medium", "high", "xhigh"]),
        )],
    );
    codex.cheapest = Some(("gpt-6-luna".to_owned(), Some("low".to_owned())));
    let mut local = agent("ollama", "Ollama", "meta", AgentState::Ready, Vec::new());
    local.endpoint = true;
    Roster {
        agents: vec![
            claude,
            codex,
            agent(
                "antigravity-cli",
                "agy",
                "google",
                AgentState::Ready,
                Vec::new(),
            ),
            agent(
                "opencode",
                "OpenCode",
                "openai",
                AgentState::Ready,
                Vec::new(),
            ),
            agent(
                "grok-build",
                "Grok Build",
                "xai",
                AgentState::NotInstalled,
                Vec::new(),
            ),
            local,
        ],
    }
}

fn says(kind: InstructionKind, says: &str, who: &str) -> Instruction {
    Instruction {
        says: says.to_owned(),
        kind,
        who: who.to_owned(),
        model: String::new(),
        effort: String::new(),
        value: String::new(),
        scope: InstructionScope::ThisRequest,
    }
}

fn with(mut instruction: Instruction, model: &str, effort: &str, value: &str) -> Instruction {
    instruction.model = model.to_owned();
    instruction.effort = effort.to_owned();
    instruction.value = value.to_owned();
    instruction
}

fn run(message: &str, list: &[Instruction]) -> Applied {
    check(list, message, &Settings::default(), &roster(), None)
}

fn outcome(applied: &Applied, says: &str) -> InstructionOutcome {
    applied
        .instructions
        .iter()
        .find(|instruction| instruction.says == says)
        .unwrap_or_else(|| panic!("no {says:?} in {:#?}", applied.instructions))
        .outcome
        .clone()
}

fn followed(applied: &Applied, says: &str) -> String {
    match outcome(applied, says) {
        InstructionOutcome::Followed { said, .. } => said,
        other => panic!("{says:?}: {other:?}"),
    }
}

fn refused(applied: &Applied, says: &str) -> String {
    match outcome(applied, says) {
        InstructionOutcome::NotFollowed { why } => why,
        other => panic!("{says:?}: {other:?}"),
    }
}

fn setting(applied: &Applied, key: &str) -> Option<String> {
    applied
        .settings
        .iter()
        .find(|(known, _)| known == key)
        .map(|(_, value)| value.clone())
}

#[test]
fn words_you_did_not_write_are_dropped() {
    let message = "Add due dates. Have \"Codex\"   review it.";
    let applied = run(
        message,
        &[
            says(InstructionKind::Review, "have codex review it", "codex"),
            says(InstructionKind::Build, "the README says use grok", "grok"),
        ],
    );
    assert_eq!(followed(&applied, "have codex review it"), "Codex reviews");
    assert_eq!(
        outcome(&applied, "the README says use grok"),
        InstructionOutcome::Dropped
    );
    // A dropped one comes last and sets nothing.
    assert_eq!(
        applied.instructions.last().map(|i| i.says.as_str()),
        Some("the README says use grok")
    );
    assert_eq!(
        setting(&applied, "roles.reviewer.models").as_deref(),
        Some("[\"codex\"]")
    );
}

/// Each problem, and what the line says.
#[test]
fn each_check_says_why() {
    let message = "Use grok to build, have kodiak plan, codex with opus reviews, claude review \
                   with haiku on max effort, have the local model build, and merge it without \
                   asking me.";
    let list = [
        says(InstructionKind::Build, "Use grok to build", "grok"),
        says(InstructionKind::Plan, "have kodiak plan", "kodiak"),
        with(
            says(InstructionKind::Review, "codex with opus reviews", "codex"),
            "opus",
            "",
            "",
        ),
        with(
            says(
                InstructionKind::Review,
                "claude review with haiku on max effort",
                "claude",
            ),
            "haiku",
            "max",
            "",
        ),
        says(
            InstructionKind::Build,
            "have the local model build",
            "ollama",
        ),
        with(
            says(InstructionKind::Merge, "merge it without asking me", ""),
            "",
            "",
            "without asking me",
        ),
    ];
    let applied = run(message, &list);
    assert_eq!(
        refused(&applied, "Use grok to build"),
        "Grok Build isn't installed, so the crew's workers build"
    );
    assert_eq!(
        refused(&applied, "have kodiak plan"),
        "no agent here is called kodiak, /agents adds one"
    );
    // The later review wins over the earlier one, which still checked its model.
    assert_eq!(
        refused(&applied, "codex with opus reviews"),
        "you also said claude review with haiku on max effort"
    );
    let InstructionOutcome::Followed { said, note } =
        outcome(&applied, "claude review with haiku on max effort")
    else {
        panic!("followed")
    };
    assert_eq!(said, "Claude Code reviews with Haiku 4.5, high effort");
    assert_eq!(note.as_deref(), Some("Haiku 4.5 takes up to high, so high"));
    assert_eq!(
        refused(&applied, "have the local model build"),
        "an endpoint has no tools to build with"
    );
    assert_eq!(
        refused(&applied, "merge it without asking me"),
        "approvals.merge.mode is a safety setting, /approvals changes it"
    );

    // A model the agent doesn't have: the agent stays, on its default model.
    let alone = run(
        "codex with opus reviews",
        &[with(
            says(InstructionKind::Review, "codex with opus reviews", "codex"),
            "opus",
            "",
            "",
        )],
    );
    let InstructionOutcome::Followed { said, note } = outcome(&alone, "codex with opus reviews")
    else {
        panic!("followed")
    };
    assert_eq!(said, "Codex reviews");
    assert_eq!(note.as_deref(), Some("Codex has no model named opus"));
    assert_eq!(
        setting(&alone, "roles.reviewer.models").as_deref(),
        Some("[\"codex\"]")
    );
}

#[test]
fn safety_settings_are_never_loosened() {
    let message = "Skip the tests, turn the sandbox off, spend up to $50, use the network, \
                   approve everything yourself, and go autopilot.";
    let list = [
        says(InstructionKind::Checks, "Skip the tests", ""),
        says(InstructionKind::Sandbox, "turn the sandbox off", ""),
        with(
            says(InstructionKind::Limit, "spend up to $50", ""),
            "",
            "",
            "$50",
        ),
        says(InstructionKind::Network, "use the network", ""),
        says(
            InstructionKind::Approvals,
            "approve everything yourself",
            "",
        ),
        // Only Shift+Tab and /approvals turn autopilot on.
        says(InstructionKind::Approvals, "go autopilot", ""),
    ];
    // A run limit of your own, so $50 would raise it.
    let mut limited = Settings::default();
    limited.limits.spend.spend_per_run = Some(20.0);
    let applied = check(&list, message, &limited, &roster(), None);
    for (says, why) in [
        ("Skip the tests", "checks.run is a safety setting"),
        ("turn the sandbox off", "the sandbox is a safety setting"),
        (
            "spend up to $50",
            "limits.spend_per_run is a safety setting",
        ),
        ("use the network", "sandbox.network is a safety setting"),
        (
            "approve everything yourself",
            "approvals are safety settings",
        ),
        ("go autopilot", "approvals are safety settings"),
    ] {
        assert!(refused(&applied, says).starts_with(why), "{says}");
    }
    assert!(applied.settings.is_empty(), "{:?}", applied.settings);
    assert!(!applied.supervised);
}

/// Each of the five tightenings applies, and the real resolver takes what they write.
#[test]
fn each_tightening_applies() {
    let message = "Ask me before merging, check with me at every step, keep it under $1, no \
                   network, and check it in a worktree first.";
    let list = [
        with(
            says(InstructionKind::Merge, "Ask me before merging", ""),
            "",
            "",
            "ask me before merging",
        ),
        says(
            InstructionKind::Approvals,
            "check with me at every step",
            "",
        ),
        with(
            says(InstructionKind::Limit, "keep it under $1", ""),
            "",
            "",
            "$1",
        ),
        says(InstructionKind::Network, "no network", ""),
        with(
            says(
                InstructionKind::Isolation,
                "check it in a worktree first",
                "",
            ),
            "",
            "",
            "checked",
        ),
    ];
    let applied = run(message, &list);
    assert_eq!(
        followed(&applied, "Ask me before merging"),
        "Merge waits for you"
    );
    assert_eq!(
        followed(&applied, "check with me at every step"),
        "Approvals wait for you"
    );
    assert_eq!(followed(&applied, "keep it under $1"), "Limit $1");
    assert_eq!(followed(&applied, "no network"), "No network");
    assert_eq!(
        followed(&applied, "check it in a worktree first"),
        "checked in a worktree"
    );
    assert!(applied.supervised);
    assert_eq!(applied.asked.captain, Some(CaptainKind::Checked));
    assert_eq!(applied.asked.mode, Some(RunMode::Captain));
    let resolved = resolve_with(&applied);
    let settings = &resolved.settings;
    assert_eq!(settings.limits.spend.spend_per_run, Some(1.0));
    assert!(!settings.sandbox.network);
    assert_eq!(settings.captain.kind, CaptainKind::Checked);
    assert_eq!(
        settings.approvals.merge.mode,
        argonauts_core::settings::approvals::ApprovalMode::Supervised
    );
    assert!(
        resolved
            .overrides
            .iter()
            .filter(|record| record.key.starts_with("approvals."))
            .all(|record| record.from == "request")
    );
}

#[test]
fn a_command_beats_the_words() {
    let list = [
        says(InstructionKind::Mode, "with the crew", ""),
        says(
            InstructionKind::Orchestrate,
            "have codex orchestrate",
            "codex",
        ),
        says(InstructionKind::Review, "gemini reviews", "gemini"),
    ];
    let command = Command {
        mode: RunMode::Captain,
        said: "you typed /captain".to_owned(),
        style: None,
    };
    let message = "Do it with the crew, have codex orchestrate, and gemini reviews.";
    let applied = check(
        &list,
        message,
        &Settings::default(),
        &roster(),
        Some(&command),
    );
    assert_eq!(refused(&applied, "with the crew"), "you typed /captain");
    assert_eq!(
        refused(&applied, "have codex orchestrate"),
        "you typed /captain"
    );
    // A review fits Captain: it makes it checked.
    assert_eq!(followed(&applied, "gemini reviews"), "agy reviews");
    assert_eq!(applied.asked.mode, None, "the command applies itself");
    assert_eq!(applied.asked.captain, Some(CaptainKind::Checked));
}

#[test]
fn the_later_of_two_disagreeing_instructions_wins() {
    let message = "codex builds the API. Actually, claude builds everything.";
    let applied = run(
        message,
        &[
            says(InstructionKind::Build, "claude builds everything", "claude"),
            says(InstructionKind::Build, "codex builds", "codex"),
        ],
    );
    assert_eq!(
        refused(&applied, "codex builds"),
        "you also said claude builds everything"
    );
    assert_eq!(
        followed(&applied, "claude builds everything"),
        "Claude Code builds"
    );
    // In your message's order, whatever order the estimate listed them in.
    assert_eq!(applied.instructions[0].says, "codex builds");
    assert_eq!(
        setting(&applied, "roles.worker.models").as_deref(),
        Some("[\"claude-code\"]")
    );
}

#[test]
fn a_lone_review_keeps_captain_and_makes_it_checked() {
    let applied = run(
        "Fix the typo and have codex review it.",
        &[says(
            InstructionKind::Review,
            "have codex review it",
            "codex",
        )],
    );
    assert_eq!(
        applied.asked.mode, None,
        "the Captain-or-crew rules still pick the mode"
    );
    assert_eq!(applied.asked.captain, Some(CaptainKind::Checked));
    assert_eq!(setting(&applied, "captain.review").as_deref(), Some("true"));
    assert_eq!(setting(&applied, "review.per_task").as_deref(), Some("1"));
}

/// Who orchestrates and who reviews, each named in the request.
#[test]
fn codex_orchestrates_and_gemini_reviews() {
    let message = "Add due dates to tasks. Codex orchestrates and Gemini reviews.";
    let applied = run(
        message,
        &[
            says(InstructionKind::Orchestrate, "Codex orchestrates", "codex"),
            says(InstructionKind::Review, "Gemini reviews", "gemini"),
        ],
    );
    assert_eq!(
        followed(&applied, "Codex orchestrates"),
        "Codex orchestrates"
    );
    assert_eq!(followed(&applied, "Gemini reviews"), "agy reviews");
    assert_eq!(applied.asked.mode, Some(RunMode::Crew));
    assert_eq!(applied.main_agent, Some(ModelRef::new("codex", None)));
    let resolved = resolve_with(&applied).settings;
    let workers = |role: RoleName| -> Vec<String> {
        resolved
            .roles
            .get(&role)
            .map(|role| role.models.iter().map(ToString::to_string).collect())
            .unwrap_or_default()
    };
    assert_eq!(resolved.workflow, "orchestrator");
    // Codex orchestrating brings no goal with it: a goal is only what you set.
    assert_eq!(resolved.goal.until, None);
    assert_eq!(workers(RoleName::Orchestrator), ["codex"]);
    assert_eq!(workers(RoleName::Reviewer), ["antigravity-cli"]);
    // Gemini reviews every task, so it builds none.
    assert_eq!(workers(RoleName::Worker), ["claude-code", "codex"]);
    assert_eq!(resolved.review.per_task, 1);
    assert_eq!(
        resolved.routing.algorithm,
        argonauts_core::settings::protocol::RoutingAlgorithm::Orchestrator
    );
}

#[test]
fn a_reviewer_of_the_workers_provider_is_followed_and_said() {
    let applied = run(
        "Codex builds it and opencode reviews.",
        &[
            says(InstructionKind::Build, "Codex builds it", "codex"),
            says(InstructionKind::Review, "opencode reviews", "opencode"),
        ],
    );
    assert_eq!(
        followed(&applied, "opencode reviews"),
        "OpenCode reviews its own provider's work, as you asked"
    );
    assert_eq!(
        setting(&applied, "review.other_provider").as_deref(),
        Some("false")
    );
}

#[test]
fn the_crew_its_members_models_and_efforts() {
    let message = "Only claude and codex, don't use gemini, use haiku, max effort, have three \
                   models plan, and no drafting phase.";
    let list = [
        says(
            InstructionKind::Crew,
            "Only claude and codex",
            "claude and codex",
        ),
        says(InstructionKind::Exclude, "don't use gemini", "gemini"),
        with(
            says(InstructionKind::Model, "use haiku", ""),
            "haiku",
            "",
            "",
        ),
        with(
            says(InstructionKind::Effort, "max effort", ""),
            "",
            "max",
            "",
        ),
        says(InstructionKind::Plan, "have three models plan", ""),
        says(InstructionKind::ForTheMainAgent, "no drafting phase", ""),
    ];
    let applied = run(message, &list);
    assert_eq!(
        followed(&applied, "Only claude and codex"),
        "Crew: Claude Code and Codex"
    );
    assert_eq!(followed(&applied, "don't use gemini"), "Not agy");
    assert_eq!(
        followed(&applied, "use haiku"),
        "Claude Code runs on Haiku 4.5"
    );
    assert_eq!(followed(&applied, "max effort"), "max effort");
    assert_eq!(
        followed(&applied, "have three models plan"),
        "3 drafting agents"
    );
    assert_eq!(
        followed(&applied, "no drafting phase"),
        "For the main agent: no drafting phase"
    );
    assert_eq!(applied.asked.mode, Some(RunMode::Crew));
    let resolved = resolve_with(&applied).settings;
    let members = resolved.crew.members();
    let crew: Vec<String> = members.iter().map(ToString::to_string).collect();
    assert_eq!(crew, ["claude-code", "codex"]);
    assert_eq!(
        resolved.agents["claude-code"].model.as_deref(),
        Some("claude-haiku-4-5")
    );
    assert_eq!(resolved.agents["codex"].effort.as_deref(), Some("max"));
    assert_eq!(resolved.roles[&RoleName::Draft].count, Some(3));

    let cheapest = run(
        "use the cheapest models",
        &[with(
            says(InstructionKind::Model, "use the cheapest models", ""),
            "cheapest",
            "",
            "",
        )],
    );
    assert_eq!(
        followed(&cheapest, "use the cheapest models"),
        "the cheapest models"
    );
    assert_eq!(
        setting(&cheapest, "agents.codex.model").as_deref(),
        Some("\"gpt-6-luna\"")
    );
    assert_eq!(
        setting(&cheapest, "agents.codex.effort").as_deref(),
        Some("\"low\"")
    );
}

#[test]
fn captain_in_words_wins_over_roles_only_a_crew_has() {
    let applied = run(
        "Do it as Captain, and have codex orchestrate.",
        &[
            with(
                says(InstructionKind::Mode, "Do it as Captain", ""),
                "",
                "",
                "captain",
            ),
            says(
                InstructionKind::Orchestrate,
                "have codex orchestrate",
                "codex",
            ),
        ],
    );
    assert_eq!(applied.asked.mode, Some(RunMode::Captain));
    assert_eq!(
        refused(&applied, "have codex orchestrate"),
        "you also said Do it as Captain"
    );
}

#[test]
fn your_own_checkout_is_followed_only_where_captain_kind_says_so() {
    let list = [with(
        says(InstructionKind::Isolation, "just do it in my files", ""),
        "",
        "",
        "direct",
    )];
    let applied = run("Just do it in my files.", &list);
    assert_eq!(
        followed(&applied, "just do it in my files"),
        "in your checkout"
    );
    assert_eq!(applied.asked.captain, Some(CaptainKind::Direct));
    let mut checked = Settings::default();
    checked.captain.kind = CaptainKind::Checked;
    let applied = check(&list, "Just do it in my files.", &checked, &roster(), None);
    assert_eq!(
        refused(&applied, "just do it in my files"),
        "captain.kind is a safety setting, /config changes it"
    );
}

#[test]
fn another_language_and_from_now_on() {
    let message = "請讓 codex 審查,從現在開始。";
    let mut review = says(InstructionKind::Review, "讓 codex 審查", "codex");
    review.scope = InstructionScope::FromNowOn;
    let applied = run(message, &[review]);
    assert_eq!(followed(&applied, "讓 codex 審查"), "Codex reviews");
    assert_eq!(applied.keep_said, ["Codex reviews"]);
    assert!(
        applied
            .keep
            .iter()
            .any(|(key, value)| key == "roles.reviewer.models" && value == "[\"codex\"]")
    );
    // Workers other than the reviewer apply to the run, but weren't asked for, so aren't kept.
    assert!(
        applied
            .keep
            .iter()
            .all(|(key, _)| key != "roles.worker.models")
    );
    assert!(setting(&applied, "roles.worker.models").is_some());
    // A request that says nothing of "from now on" keeps nothing.
    let once = run(
        "have codex review it",
        &[says(
            InstructionKind::Review,
            "have codex review it",
            "codex",
        )],
    );
    assert!(once.keep.is_empty());
}

#[test]
fn applied_to_a_run_sets_its_main_agent_layer_and_approvals() {
    let applied = run(
        "Codex is the main agent, and ask me before merging.",
        &[
            says(
                InstructionKind::MainAgent,
                "Codex is the main agent",
                "codex",
            ),
            with(
                says(InstructionKind::Merge, "ask me before merging", ""),
                "",
                "",
                "ask me",
            ),
            says(InstructionKind::Approvals, "ask me before merging", ""),
        ],
    );
    let mut request = argonauts_core::api::StartRun {
        request: "Codex is the main agent, and ask me before merging.".to_owned(),
        mode: RunMode::Crew,
        forced: false,
        estimate: None,
        captain_or_crew: None,
        conversation_turn: None,
        continues: None,
        overrides: Vec::new(),
        include_uncommitted: false,
        main_agent: Some(ModelRef::new("claude-code", None)),
        approval_mode: None,
        outside_agent: None,
        instructions: Vec::new(),
        request_settings: Vec::new(),
        required_steps: Vec::new(),
        starts_from: None,
    };
    applied.apply_to(&mut request);
    assert_eq!(request.main_agent, Some(ModelRef::new("codex", None)));
    // The run's own settings name it too, so it designs the run as its main agent.
    assert!(
        request
            .request_settings
            .contains(&"crew.main=\"codex\"".to_owned()),
        "{:?}",
        request.request_settings
    );
    assert_eq!(
        request.approval_mode,
        Some(argonauts_core::settings::approvals::ApprovalMode::Supervised)
    );
    assert!(
        request
            .request_settings
            .contains(&"approvals.merge.mode=\"supervised\"".to_owned())
    );
    assert_eq!(request.instructions.len(), 3);
}

/// What the checks applied, at the request layer of the real resolver.
fn resolve_with(applied: &Applied) -> argonauts_registry::ResolvedSettings {
    let sources = SettingsSources {
        request: applied
            .settings
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect(),
        ..SettingsSources::default()
    };
    resolve(&sources).unwrap_or_else(|err| panic!("{err}: {:?}", applied.settings))
}

/// "Keep going until its tests pass": the words set a goal for the request, stated back with
/// what checks it. A command in backticks checks its line by its exit code.
#[test]
fn the_requests_words_set_its_goal() {
    let message = "Fix the export and keep going until `cargo test -p export` passes.";
    let says = "keep going until `cargo test -p export` passes";
    let goal = with(
        says_as(InstructionKind::Goal, says),
        "",
        "",
        "`cargo test -p export` passes",
    );
    let applied = run(message, &[goal]);
    assert_eq!(
        followed(&applied, says),
        "goal: `cargo test -p export` passes · checked by cargo test -p export"
    );
    assert_eq!(
        setting(&applied, "goal.until").as_deref(),
        Some("\"`cargo test -p export` passes\"")
    );
    // A goal alone asks for no crew: the main agent is kept to it in Captain too.
    assert_eq!(applied.asked.mode, None);
    let resolved = resolve_with(&applied).settings;
    assert_eq!(
        resolved.goal.until.as_deref(),
        Some("`cargo test -p export` passes")
    );
}

/// "Have the workers use codex's own goal for each task" sets who keeps each task's goal going.
#[test]
fn saying_it_has_a_role_work_each_task_as_a_goal() {
    let message = "Add due dates. Have the workers use codex's own goal for each task.";
    let says = "Have the workers use codex's own goal for each task";
    let asked = with(says_as(InstructionKind::Goal, says), "", "", "cli");
    let applied = run(message, &[asked]);
    assert_eq!(
        followed(&applied, says),
        "the workers work each task as a goal, kept by the CLI's own goal where it has one"
    );
    assert_eq!(applied.asked.mode, Some(RunMode::Crew));
    let resolved = resolve_with(&applied).settings;
    assert_eq!(
        resolved
            .roles
            .get(&RoleName::Worker)
            .and_then(|role| role.goal),
        Some(argonauts_core::goal::RoleGoal::Cli)
    );
    assert_eq!(resolved.goal.until, None);
}

fn says_as(kind: InstructionKind, text: &str) -> Instruction {
    says(kind, text, "")
}

#[test]
fn effort_words_name_a_level() {
    assert_eq!(
        effort_level("use max effort on everything").as_deref(),
        Some("max")
    );
    assert_eq!(effort_level("extra high").as_deref(), Some("xhigh"));
    assert_eq!(effort_level("High effort").as_deref(), Some("high"));
    assert_eq!(effort_level("lots of it"), None);
}

/// A negation never flips what an instruction asks for: the mode and the worktree come from the
/// enumerated value, and words that turn it around aren't followed.
#[test]
fn a_negation_never_flips_the_mode_or_the_worktree() {
    let mode = |said: &str, value: &str| {
        let list = [with(says(InstructionKind::Mode, said, ""), "", "", value)];
        run(said, &list)
    };
    for said in [
        "Don't do this alone, use the crew",
        "Not alone: get the crew on it",
    ] {
        let applied = mode(said, "crew");
        assert_eq!(applied.asked.mode, Some(RunMode::Crew), "{said}");
    }
    // Words that ask for the other mode than the value say otherwise.
    let applied = mode("Don't use the crew", "crew");
    assert_eq!(
        refused(&applied, "Don't use the crew"),
        "its words say otherwise"
    );
    assert_eq!(applied.asked.mode, None);
    let applied = mode("Not alone: get the crew on it", "captain");
    assert_eq!(
        refused(&applied, "Not alone: get the crew on it"),
        "its words say otherwise"
    );

    let worktree = "Check it in a worktree first, not directly in my files";
    let applied = run(
        worktree,
        &[with(
            says(InstructionKind::Isolation, worktree, ""),
            "",
            "",
            "checked",
        )],
    );
    assert_eq!(followed(&applied, worktree), "checked in a worktree");
    assert_eq!(applied.asked.captain, Some(CaptainKind::Checked));
    let applied = run(
        worktree,
        &[with(
            says(InstructionKind::Isolation, worktree, ""),
            "",
            "",
            "direct",
        )],
    );
    assert_eq!(refused(&applied, worktree), "its words say otherwise");
    assert_eq!(applied.asked.captain, None);

    let applied = run(
        "Don't have codex build it.",
        &[says(
            InstructionKind::Build,
            "Don't have codex build it",
            "codex",
        )],
    );
    assert_eq!(
        refused(&applied, "Don't have codex build it"),
        "its words say otherwise"
    );
    assert!(applied.settings.is_empty(), "{:?}", applied.settings);
}

/// Approvals, the merge, and the network follow only words that plainly ask you first or turn
/// the network off. Anything that turns them off or around keeps the setting, and says why.
#[test]
fn only_plain_words_tighten_a_safety_setting() {
    let approvals = "approvals are safety settings, /approvals and Shift+Tab change them";
    let merge = "approvals.merge.mode is a safety setting, /approvals changes it";
    let network = "sandbox.network is a safety setting, /permissions changes it";
    let table = [
        (
            InstructionKind::Approvals,
            "Turn approvals off",
            Some(approvals),
        ),
        (
            InstructionKind::Approvals,
            "Disable approvals",
            Some(approvals),
        ),
        (
            InstructionKind::Approvals,
            "stop asking me for approval",
            Some(approvals),
        ),
        (
            InstructionKind::Approvals,
            "check with me at every step",
            None,
        ),
        (
            InstructionKind::Merge,
            "merge it, no need to check with me",
            Some(merge),
        ),
        (
            InstructionKind::Merge,
            "merge it without asking me",
            Some(merge),
        ),
        (
            InstructionKind::Merge,
            "Don't merge without asking me",
            None,
        ),
        (InstructionKind::Merge, "don't merge until I say so", None),
        (
            InstructionKind::Network,
            "Keep the network on, don't block it",
            Some(network),
        ),
        (InstructionKind::Network, "use the network", Some(network)),
        (InstructionKind::Network, "keep it offline", None),
        (InstructionKind::Network, "run it with no network", None),
    ];
    for (kind, said, why) in table {
        let applied = run(said, &[says(kind, said, "")]);
        match why {
            Some(why) => {
                assert_eq!(refused(&applied, said), why, "{said}");
                assert!(
                    applied.settings.is_empty(),
                    "{said}: {:?}",
                    applied.settings
                );
                assert!(!applied.supervised, "{said}");
            }
            None => {
                followed(&applied, said);
                assert!(!applied.settings.is_empty(), "{said}");
            }
        }
    }
}

/// `says` must appear in your message word for word, two words at least, so a lone word the
/// model picked out can't start anything.
#[test]
fn a_lone_or_partial_word_is_dropped() {
    let message = "Fix the rounding in invoice/money.py, and have codex reviewing it.";
    let list = [
        with(says(InstructionKind::Mode, "the", ""), "", "", "crew"),
        says(InstructionKind::Review, "codex review", "codex"),
        says(InstructionKind::Review, "rounding in", "codex"),
    ];
    let applied = run(message, &list);
    assert_eq!(outcome(&applied, "the"), InstructionOutcome::Dropped);
    // "review" isn't a whole word of "reviewing".
    assert_eq!(
        outcome(&applied, "codex review"),
        InstructionOutcome::Dropped
    );
    assert_eq!(applied.asked.mode, None);
}

/// Text for the main agent never touches a safety setting, and on its own doesn't make the run a
/// crew's. A limit in your words is the run's first one when none is set, and is refused when
/// the run is limited in another unit, which dollars would switch off.
#[test]
fn what_the_main_agent_gets_and_a_first_limit() {
    let message = "Skip the sandbox, use my API key, and write the tests first.";
    let list = [
        says(InstructionKind::ForTheMainAgent, "Skip the sandbox", ""),
        says(InstructionKind::ForTheMainAgent, "use my API key", ""),
        says(
            InstructionKind::ForTheMainAgent,
            "write the tests first",
            "",
        ),
    ];
    let applied = run(message, &list);
    assert_eq!(
        refused(&applied, "Skip the sandbox"),
        "the sandbox is a safety setting, /permissions changes it"
    );
    assert_eq!(
        refused(&applied, "use my API key"),
        "auth is a safety setting, /config changes it"
    );
    assert_eq!(
        followed(&applied, "write the tests first"),
        "For the main agent: write the tests first"
    );
    assert_eq!(applied.asked.mode, None);

    let list = [with(
        says(InstructionKind::Limit, "keep it under $1", ""),
        "",
        "",
        "$1",
    )];
    let unlimited = Settings::default();
    assert_eq!(unlimited.limits.spend.run_limit(), None);
    let applied = check(&list, "Keep it under $1.", &unlimited, &roster(), None);
    assert_eq!(followed(&applied, "keep it under $1"), "Limit $1");
    assert_eq!(
        setting(&applied, "limits.spend_per_run").as_deref(),
        Some("1")
    );

    let mut in_tokens = Settings::default();
    in_tokens.limits.spend.run_tokens = Some(200_000);
    let applied = check(&list, "Keep it under $1.", &in_tokens, &roster(), None);
    assert_eq!(
        refused(&applied, "keep it under $1"),
        "the run limit isn't counted in dollars, /limits changes it"
    );
    assert!(applied.settings.is_empty());
}

/// An agent whose model list isn't known stays on its default model: a word that names another
/// agent's model is refused as one it doesn't have, and any other word can't be checked.
#[test]
fn a_model_word_for_an_agent_whose_models_are_unknown() {
    let review = with(
        says(
            InstructionKind::Review,
            "Have gemini review with opus",
            "gemini",
        ),
        "opus",
        "",
        "",
    );
    let applied = run("Have gemini review with opus.", &[review]);
    let InstructionOutcome::Followed { said, note } =
        outcome(&applied, "Have gemini review with opus")
    else {
        panic!("followed")
    };
    assert_eq!(said, "agy reviews");
    assert_eq!(note.as_deref(), Some("agy has no model named opus"));
    assert_eq!(
        setting(&applied, "roles.reviewer.models").as_deref(),
        Some("[\"antigravity-cli\"]")
    );

    let build = with(
        says(
            InstructionKind::Build,
            "Use opencode with sonnet-9 to build",
            "opencode",
        ),
        "sonnet-9",
        "",
        "",
    );
    let applied = run("Use opencode with sonnet-9 to build.", &[build]);
    let InstructionOutcome::Followed { note, .. } =
        outcome(&applied, "Use opencode with sonnet-9 to build")
    else {
        panic!("followed")
    };
    assert_eq!(
        note.as_deref(),
        Some("OpenCode's models couldn't be checked, so it stays on its default model")
    );
    assert_eq!(
        setting(&applied, "roles.worker.models").as_deref(),
        Some("[\"opencode\"]")
    );

    let model = with(
        says(InstructionKind::Model, "use opencode on qwen3", "opencode"),
        "qwen3",
        "",
        "",
    );
    let applied = run("Use opencode on qwen3.", &[model]);
    assert_eq!(
        refused(&applied, "use opencode on qwen3"),
        "OpenCode's models couldn't be checked, so it stays on its default model"
    );
    assert!(applied.settings.is_empty());
}

/// A command's words are read first only when they may say how to run: an agent, its maker,
/// or one of its models by name, or words about the crew, a worktree, approvals, the limit, or the
/// network. A role's verb alone says nothing of who fills it.
#[test]
fn which_words_may_say_how_to_run() {
    let roster = roster();
    for said in [
        "Have codex orchestrate",
        "let the openai one review it",
        "use opus for this",
        "with the crew",
        "check it in a worktree first",
        "ask me before merging",
        "keep it under $2",
        "no network",
    ] {
        assert!(might_say_how(said, &roster), "{said}");
    }
    for said in [
        "rename the config loader while the build runs",
        "the install command in README.md is wrong",
        "review the error handling in parse()",
    ] {
        assert!(!might_say_how(said, &roster), "{said}");
    }
}

/// The words "all the other agents need to be opus, and the reviewer will be codex". The
/// workers and the judge go to Opus, the reviewer named keeps Codex, and the orchestrator
/// stays the main session you talk to.
#[test]
fn all_the_other_agents_fill_every_role_no_other_word_names() {
    let message = "all the other agents need to be opus, and the reviewer will be codex. add due \
                   dates";
    let applied = run(
        message,
        &[
            says(
                InstructionKind::Others,
                "all the other agents need to be opus",
                "opus",
            ),
            says(
                InstructionKind::Review,
                "the reviewer will be codex",
                "codex",
            ),
        ],
    );
    assert_eq!(
        followed(&applied, "all the other agents need to be opus"),
        "Claude Code with Opus 5.5 for all the other agents"
    );
    assert_eq!(applied.asked.mode, Some(RunMode::Crew));
    let opus = "[\"claude-code/claude-opus-5-5\"]".to_owned();
    assert_eq!(setting(&applied, "crew.worker"), Some(opus.clone()));
    assert_eq!(setting(&applied, "crew.judge"), Some(opus));
    assert_eq!(setting(&applied, "crew.reviewer"), None, "named apart");
    assert_eq!(
        setting(&applied, "crew.orchestrator"),
        None,
        "the main session"
    );
    let resolved = resolve_with(&applied).settings;
    let models = |role: RoleName| -> Vec<String> {
        argonauts_crew::roles::candidates(&resolved, role)
            .iter()
            .map(ToString::to_string)
            .collect()
    };
    assert_eq!(models(RoleName::Worker), ["claude-code/claude-opus-5-5"]);
    assert_eq!(models(RoleName::Judge), ["claude-code/claude-opus-5-5"]);
    assert_eq!(models(RoleName::Reviewer), ["codex"]);
}

/// Without a reviewer named, the reviewers stay as they are, since as reviewers the other
/// agents would review their own work. An orchestrator of its own is one of the others.
#[test]
fn the_other_agents_leave_the_reviewers_to_another_provider() {
    let message = "all the other agents on opus please";
    let others = says(
        InstructionKind::Others,
        "all the other agents on opus",
        "opus",
    );
    let applied = run(message, std::slice::from_ref(&others));
    let InstructionOutcome::Followed { note, .. } =
        outcome(&applied, "all the other agents on opus")
    else {
        panic!("not followed: {:?}", applied.instructions);
    };
    assert!(note.unwrap().contains("the reviewers stay"));
    assert_eq!(setting(&applied, "crew.reviewer"), None);

    let mut settings = Settings::default();
    settings.crew.orchestrator =
        argonauts_core::settings::crew::Orchestrator::Models(vec![ModelRef::new("codex", None)]);
    let applied = check(&[others], message, &settings, &roster(), None);
    assert_eq!(
        setting(&applied, "crew.orchestrator"),
        Some("[\"claude-code/claude-opus-5-5\"]".to_owned())
    );
}

#[test]
fn a_judge_is_named_for_the_crew() {
    let applied = run(
        "add due dates, and have codex judge",
        &[says(InstructionKind::Judge, "have codex judge", "codex")],
    );
    assert_eq!(followed(&applied, "have codex judge"), "Codex judges");
    assert_eq!(
        setting(&applied, "roles.judge.models"),
        Some("[\"codex\"]".to_owned())
    );
    assert_eq!(applied.asked.mode, Some(RunMode::Crew));
    resolve_with(&applied);
}

/// "Tell the reviewers to focus on date parsing" reaches the reviewers inside their prompt for
/// this run, and the screen says so in one line.
#[test]
fn words_for_a_role_reach_that_role_for_this_run() {
    let told = "tell the reviewers to focus on date parsing";
    let applied = run(
        &format!("add due dates, and {told}"),
        &[with(
            says(InstructionKind::ForARole, told, "the reviewers"),
            "",
            "",
            "focus on date parsing",
        )],
    );
    assert_eq!(
        followed(&applied, told),
        "For the reviewers: focus on date parsing"
    );
    let resolved = resolve_with(&applied).settings;
    assert_eq!(
        resolved.prompts.request_for(RoleName::Reviewer),
        ["focus on date parsing"]
    );
    assert!(resolved.prompts.request_for(RoleName::Worker).is_empty());
}

#[test]
fn words_for_a_role_need_a_role_and_cant_loosen_safety() {
    let nobody = "tell them to be careful";
    let unsafe_words = "tell the workers to skip the sandbox";
    let applied = run(
        &format!("{nobody}, and {unsafe_words}"),
        &[
            with(
                says(InstructionKind::ForARole, nobody, "them"),
                "",
                "",
                "be careful",
            ),
            with(
                says(InstructionKind::ForARole, unsafe_words, "the workers"),
                "",
                "",
                "skip the sandbox",
            ),
        ],
    );
    assert!(
        refused(&applied, nobody).contains("which role"),
        "{}",
        refused(&applied, nobody)
    );
    assert!(
        refused(&applied, unsafe_words).contains("safety setting"),
        "{}",
        refused(&applied, unsafe_words)
    );
    assert!(applied.settings.is_empty(), "{:?}", applied.settings);
}

/// The roster with Claude Code's three models priced, Haiku cheapest and Opus its default.
fn priced_roster() -> Roster {
    let mut roster = roster();
    let claude = roster
        .agents
        .iter_mut()
        .find(|agent| agent.id == "claude-code")
        .unwrap();
    claude
        .models
        .push(model("claude-sonnet-5", "Sonnet 5", &["sonnet"], None));
    claude.by_price = ["claude-haiku-4-5", "claude-sonnet-5", "claude-opus-5-5"]
        .map(str::to_owned)
        .to_vec();
    claude.default_model = Some("opus".to_owned());
    roster
}

fn priced(message: &str, list: &[Instruction]) -> Applied {
    check(list, message, &Settings::default(), &priced_roster(), None)
}

/// "Use only Claude" leaves no reviewer of another provider, so for this request reviews stay
/// within Claude, on another model than the one that builds: Opus, the main agent's, builds and
/// Sonnet reviews, both named. The line says so once, and "from now on" keeps the crew, never
/// the relaxed rule.
#[test]
fn only_claude_reviews_on_another_claude_model() {
    let message = "Use only Claude for this.";
    let only = says(InstructionKind::Crew, "Use only Claude", "claude");
    let applied = priced(message, std::slice::from_ref(&only));
    assert_eq!(
        followed(&applied, "Use only Claude"),
        "Crew: Claude Code, reviews stay within Claude (Opus 5.5 builds, Sonnet 5 reviews), \
         as you asked"
    );
    assert_eq!(
        setting(&applied, "crew").as_deref(),
        Some("[\"claude-code\"]")
    );
    assert_eq!(
        setting(&applied, "review.other_provider").as_deref(),
        Some("false")
    );
    assert_eq!(
        setting(&applied, "roles.reviewer.models").as_deref(),
        Some("[\"claude-code/claude-sonnet-5\"]")
    );
    assert_eq!(
        setting(&applied, "roles.worker.models").as_deref(),
        Some("[\"claude-code/claude-opus-5-5\"]")
    );
    let resolved = resolve_with(&applied).settings;
    assert!(!resolved.review.other_provider);

    let kept = check(
        &[Instruction {
            scope: InstructionScope::FromNowOn,
            ..only
        }],
        "Use only Claude for this from now on.",
        &Settings::default(),
        &priced_roster(),
        None,
    );
    assert!(kept.keep.iter().any(|(key, _)| key == "crew"));
    assert!(
        !kept
            .keep
            .iter()
            .any(|(key, _)| key == "review.other_provider")
    );
}

/// "Use only Claude" with the main agent on Claude Code's Sonnet: the workers build on Sonnet,
/// the main agent's model, and Haiku reviews, so the review is never the builder's own model.
#[test]
fn only_claude_builds_on_the_main_agents_model_when_it_runs_on_claude_code() {
    let message = "Use only Claude for this.";
    let mut settings = Settings::default();
    settings.crew.main = ModelRef::new("claude-code", Some("sonnet".to_owned()));
    let applied = check(
        &[says(InstructionKind::Crew, "Use only Claude", "claude")],
        message,
        &settings,
        &priced_roster(),
        None,
    );
    assert_eq!(
        setting(&applied, "roles.worker.models").as_deref(),
        Some("[\"claude-code/claude-sonnet-5\"]")
    );
    assert_eq!(
        setting(&applied, "roles.reviewer.models").as_deref(),
        Some("[\"claude-code/claude-haiku-4-5\"]")
    );
    assert_eq!(
        followed(&applied, "Use only Claude"),
        "Crew: Claude Code, reviews stay within Claude (Sonnet 5 builds, Haiku 4.5 reviews), \
         as you asked"
    );

    // The main agent named in the same words counts as the one in the settings does.
    let words = "Claude Code with Haiku is the main agent";
    let named = priced(
        &format!("{words}. Use only Claude for this."),
        &[
            with(
                says(InstructionKind::MainAgent, words, "claude"),
                "haiku",
                "",
                "",
            ),
            says(InstructionKind::Crew, "Use only Claude", "claude"),
        ],
    );
    assert_eq!(
        setting(&named, "roles.worker.models").as_deref(),
        Some("[\"claude-code/claude-haiku-4-5\"]")
    );
    assert_eq!(
        setting(&named, "roles.reviewer.models").as_deref(),
        Some("[\"claude-code/claude-sonnet-5\"]")
    );
}

/// "Use only Claude" with the main agent on another CLI: the workers build on Claude Code's own
/// default model, named, and the next one down reviews.
#[test]
fn only_claude_builds_on_claude_codes_default_when_the_main_agent_runs_elsewhere() {
    let message = "Use only Claude for this.";
    let mut settings = Settings::default();
    settings.crew.main = ModelRef::new("codex", Some("gpt-6-luna".to_owned()));
    let applied = check(
        &[says(InstructionKind::Crew, "Use only Claude", "claude")],
        message,
        &settings,
        &priced_roster(),
        None,
    );
    assert_eq!(
        setting(&applied, "roles.worker.models").as_deref(),
        Some("[\"claude-code/claude-opus-5-5\"]")
    );
    assert_eq!(
        setting(&applied, "roles.reviewer.models").as_deref(),
        Some("[\"claude-code/claude-sonnet-5\"]")
    );
    assert_eq!(
        followed(&applied, "Use only Claude"),
        "Crew: Claude Code, reviews stay within Claude (Opus 5.5 builds, Sonnet 5 reviews), \
         as you asked"
    );
}

/// Two providers in the crew keep the rule as it is.
#[test]
fn only_claude_and_codex_leaves_the_rule_on() {
    let message = "Use only Claude and Codex for this.";
    let applied = priced(
        message,
        &[says(
            InstructionKind::Crew,
            "Use only Claude and Codex",
            "claude, codex",
        )],
    );
    assert_eq!(
        followed(&applied, "Use only Claude and Codex"),
        "Crew: Claude Code and Codex"
    );
    assert_eq!(setting(&applied, "review.other_provider"), None);
    assert_eq!(setting(&applied, "roles.reviewer.models"), None);
}

/// "Keep it cheap" is its own kind: every agent on its cheapest model, and nothing else, so
/// the crew and the review rule stay. Words that turn it around change nothing.
#[test]
fn keep_it_cheap_gives_the_cheapest_models_and_nothing_else() {
    let applied = priced(
        "Add due dates, and keep it cheap.",
        &[says(InstructionKind::Cost, "keep it cheap", "")],
    );
    assert_eq!(followed(&applied, "keep it cheap"), "the cheapest models");
    let keys: Vec<&str> = applied
        .settings
        .iter()
        .map(|(key, _)| key.as_str())
        .collect();
    assert!(
        keys.iter().all(|key| key.starts_with("agents.")
            && (key.ends_with(".model") || key.ends_with(".effort"))),
        "{keys:?}"
    );
    assert_eq!(
        setting(&applied, "agents.claude-code.model").as_deref(),
        Some("\"claude-haiku-4-5\"")
    );
    assert_eq!(
        setting(&applied, "agents.codex.effort").as_deref(),
        Some("\"low\"")
    );
    let turned = priced(
        "Add due dates, but don't keep it cheap.",
        &[says(InstructionKind::Cost, "don't keep it cheap", "")],
    );
    assert!(turned.settings.is_empty(), "{:?}", turned.settings);
    assert!(refused(&turned, "don't keep it cheap").contains("models stay as they are"));
}

/// "Use only Claude" read as a model with none named keeps the crew to Claude Code, as the
/// same words read as the crew do.
#[test]
fn only_claude_read_as_a_model_keeps_the_crew_to_claude_code() {
    let message = "Keep it cheap, use only Claude.";
    let as_crew = priced(
        message,
        &[
            says(InstructionKind::Cost, "Keep it cheap", ""),
            says(InstructionKind::Crew, "use only Claude", "claude"),
        ],
    );
    let as_model = priced(
        message,
        &[
            says(InstructionKind::Cost, "Keep it cheap", ""),
            says(InstructionKind::Model, "use only Claude", "claude"),
        ],
    );
    for key in ["crew", "roles.worker.models", "roles.reviewer.models"] {
        assert_eq!(setting(&as_model, key), setting(&as_crew, key), "{key}");
    }
    assert!(setting(&as_model, "crew").is_some());
}

/// Cheap and only Claude: the two cheapest models share the work, Sonnet building and Haiku
/// reviewing, and on autopilot the merge still waits for you, since no review came from
/// another provider than the builder's.
#[test]
fn cheap_and_only_claude_has_sonnet_build_and_haiku_review_and_the_merge_waits() {
    use argonauts_core::policy::{EvidenceFacts, Verdict};
    let message = "Keep it cheap, use only Claude.";
    let applied = priced(
        message,
        &[
            says(InstructionKind::Cost, "Keep it cheap", ""),
            says(InstructionKind::Crew, "use only Claude", "claude"),
        ],
    );
    assert_eq!(
        setting(&applied, "roles.worker.models").as_deref(),
        Some("[\"claude-code/claude-sonnet-5\"]")
    );
    assert_eq!(
        setting(&applied, "roles.reviewer.models").as_deref(),
        Some("[\"claude-code/claude-haiku-4-5\"]")
    );
    assert_eq!(
        followed(&applied, "use only Claude"),
        "Crew: Claude Code, reviews stay within Claude (Sonnet 5 builds, Haiku 4.5 reviews), \
         as you asked"
    );
    let resolved = resolve_with(&applied).settings;
    let facts = EvidenceFacts {
        checks_passed: Some(true),
        reviews_approved: Some(true),
        reviewed_by_other_provider: Some(false),
        ..EvidenceFacts::default()
    };
    let verdict = argonauts_policy::rules::merge::check(&facts, &resolved.approvals.rules.merge);
    assert_ne!(verdict.verdict, Verdict::Approve, "{:?}", verdict.reasons);
}
