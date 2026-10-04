//! The instruction checks, table by table, against a roster of agents made by hand. A last test
//! hands what they apply to the real resolver, so every key and value they write resolves.

use argonauts_core::events::run::{InstructionOutcome, RunMode};
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::SoloStyle;
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

fn agent(id: &str, name: &str, family: &str, state: AgentState, models: Vec<Model>) -> Agent {
    Agent {
        id: id.to_owned(),
        name: name.to_owned(),
        family: family.to_owned(),
        endpoint: false,
        state,
        models,
        cheapest: None,
    }
}

/// Claude Code, Codex, and Antigravity CLI ready, OpenCode ready on a gpt model, Grok Build
/// declared but not installed, and a local endpoint.
fn roster() -> Roster {
    let mut claude = agent(
        "claude-code",
        "Claude Code",
        "claude",
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
        "gpt",
        AgentState::Ready,
        vec![model(
            "gpt-5.6-luna",
            "GPT-5.6 Luna",
            &[],
            levels(&["low", "medium", "high", "xhigh"]),
        )],
    );
    codex.cheapest = Some(("gpt-5.6-luna".to_owned(), Some("low".to_owned())));
    let mut local = agent("ollama", "Ollama", "llama", AgentState::Ready, Vec::new());
    local.endpoint = true;
    Roster {
        agents: vec![
            claude,
            codex,
            agent(
                "antigravity-cli",
                "Antigravity CLI",
                "gemini",
                AgentState::Ready,
                Vec::new(),
            ),
            agent("opencode", "OpenCode", "gpt", AgentState::Ready, Vec::new()),
            agent(
                "grok-build",
                "Grok Build",
                "grok",
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
        setting(&applied, "roles.reviewer.workers").as_deref(),
        Some("[\"codex\"]")
    );
}

/// The checks' table: each problem, and what the line says.
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
        "Grok Build isn't installed, so the crew's owners build"
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
        setting(&alone, "roles.reviewer.workers").as_deref(),
        Some("[\"codex\"]")
    );
}

#[test]
fn safety_settings_are_never_loosened() {
    let message = "Skip the tests, turn the sandbox off, spend up to $50, use the network, \
                   approve everything yourself, and go full auto.";
    let list = [
        says(InstructionKind::Gates, "Skip the tests", ""),
        says(InstructionKind::Sandbox, "turn the sandbox off", ""),
        with(
            says(InstructionKind::Budget, "spend up to $50", ""),
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
        // Only Shift+Tab and /approvals turn full auto on.
        says(InstructionKind::Approvals, "go full auto", ""),
    ];
    let applied = run(message, &list);
    for (says, why) in [
        ("Skip the tests", "gates.run is a safety setting"),
        ("turn the sandbox off", "the sandbox is a safety setting"),
        ("spend up to $50", "budget.run_usd is a safety setting"),
        ("use the network", "sandbox.network is a safety setting"),
        (
            "approve everything yourself",
            "approvals are safety settings",
        ),
        ("go full auto", "approvals are safety settings"),
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
                   network, and check it in a copy first.";
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
            says(InstructionKind::Budget, "keep it under $1", ""),
            "",
            "",
            "$1",
        ),
        says(InstructionKind::Network, "no network", ""),
        with(
            says(InstructionKind::Isolation, "check it in a copy first", ""),
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
    assert_eq!(followed(&applied, "keep it under $1"), "Budget $1");
    assert_eq!(followed(&applied, "no network"), "No network");
    assert_eq!(
        followed(&applied, "check it in a copy first"),
        "checked in a copy"
    );
    assert!(applied.supervised);
    assert_eq!(applied.asked.solo, Some(SoloStyle::Checked));
    assert_eq!(applied.asked.mode, Some(RunMode::Solo));
    let resolved = resolve_with(&applied);
    let settings = &resolved.settings;
    assert_eq!(settings.budget.run_usd, Some(1.0));
    assert!(!settings.sandbox.network);
    assert_eq!(settings.solo.style, SoloStyle::Checked);
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
            InstructionKind::Coordinate,
            "have codex coordinate",
            "codex",
        ),
        says(InstructionKind::Review, "gemini reviews", "gemini"),
    ];
    let command = Command {
        mode: RunMode::Solo,
        said: "you typed /solo".to_owned(),
        style: None,
    };
    let message = "Do it with the crew, have codex coordinate, and gemini reviews.";
    let applied = check(
        &list,
        message,
        &Settings::default(),
        &roster(),
        Some(&command),
    );
    assert_eq!(refused(&applied, "with the crew"), "you typed /solo");
    assert_eq!(
        refused(&applied, "have codex coordinate"),
        "you typed /solo"
    );
    // A review fits solo: it makes it checked.
    assert_eq!(
        followed(&applied, "gemini reviews"),
        "Antigravity CLI reviews"
    );
    assert_eq!(applied.asked.mode, None, "the command applies itself");
    assert_eq!(applied.asked.solo, Some(SoloStyle::Checked));
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
        setting(&applied, "roles.owner.workers").as_deref(),
        Some("[\"claude-code\"]")
    );
}

#[test]
fn a_lone_review_keeps_solo_and_makes_it_checked() {
    let applied = run(
        "Fix the typo and have codex review it.",
        &[says(
            InstructionKind::Review,
            "have codex review it",
            "codex",
        )],
    );
    assert_eq!(applied.asked.mode, None, "triage still picks the mode");
    assert_eq!(applied.asked.solo, Some(SoloStyle::Checked));
    assert_eq!(setting(&applied, "solo.review").as_deref(), Some("true"));
    assert_eq!(setting(&applied, "review.per_card").as_deref(), Some("1"));
}

/// Who coordinates and who reviews, each named in the request.
#[test]
fn codex_coordinates_and_gemini_reviews() {
    let message = "Add due dates to tasks. Codex coordinates and Gemini reviews.";
    let applied = run(
        message,
        &[
            says(InstructionKind::Coordinate, "Codex coordinates", "codex"),
            says(InstructionKind::Review, "Gemini reviews", "gemini"),
        ],
    );
    assert_eq!(followed(&applied, "Codex coordinates"), "Codex coordinates");
    assert_eq!(
        followed(&applied, "Gemini reviews"),
        "Antigravity CLI reviews"
    );
    assert_eq!(applied.asked.mode, Some(RunMode::Crew));
    assert_eq!(applied.lead, Some(WorkerRef::new("codex", None)));
    let resolved = resolve_with(&applied).settings;
    let workers = |role: RoleName| -> Vec<String> {
        resolved
            .roles
            .get(&role)
            .map(|role| role.workers.iter().map(ToString::to_string).collect())
            .unwrap_or_default()
    };
    assert_eq!(resolved.workflow.to_string(), "coordinated-crew");
    // Codex would lead a goal; coordinating keeps the run a workflow.
    assert_eq!(
        resolved.workflows.style_for_lead("codex"),
        argonauts_core::settings::workflows::RunStyle::Workflow
    );
    assert_eq!(workers(RoleName::Coordinator), ["codex"]);
    assert_eq!(workers(RoleName::Reviewer), ["antigravity-cli"]);
    // Gemini reviews every card, so it owns none.
    assert_eq!(workers(RoleName::Owner), ["claude-code", "codex"]);
    assert_eq!(resolved.review.per_card, 1);
    assert_eq!(
        resolved.routing.algorithm,
        argonauts_core::settings::protocol::RoutingAlgorithm::Coordinator
    );
}

#[test]
fn a_reviewer_of_the_owners_family_is_followed_and_said() {
    let applied = run(
        "Codex builds it and opencode reviews.",
        &[
            says(InstructionKind::Build, "Codex builds it", "codex"),
            says(InstructionKind::Review, "opencode reviews", "opencode"),
        ],
    );
    assert_eq!(
        followed(&applied, "opencode reviews"),
        "OpenCode reviews its own company's work, as you asked"
    );
    assert_eq!(
        setting(&applied, "review.other_family").as_deref(),
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
        says(InstructionKind::ForTheLead, "no drafting phase", ""),
    ];
    let applied = run(message, &list);
    assert_eq!(
        followed(&applied, "Only claude and codex"),
        "Crew: Claude Code and Codex"
    );
    assert_eq!(
        followed(&applied, "don't use gemini"),
        "Not Antigravity CLI"
    );
    assert_eq!(
        followed(&applied, "use haiku"),
        "Claude Code seats on Haiku 4.5"
    );
    assert_eq!(followed(&applied, "max effort"), "max effort");
    assert_eq!(followed(&applied, "have three models plan"), "3 drafters");
    assert_eq!(
        followed(&applied, "no drafting phase"),
        "For the lead: no drafting phase"
    );
    assert_eq!(applied.asked.mode, Some(RunMode::Crew));
    let resolved = resolve_with(&applied).settings;
    let crew: Vec<String> = resolved.crew.iter().map(ToString::to_string).collect();
    assert_eq!(crew, ["claude-code", "codex"]);
    assert_eq!(
        resolved.agents["claude-code"].model.as_deref(),
        Some("claude-haiku-4-5")
    );
    assert_eq!(resolved.agents["codex"].effort.as_deref(), Some("max"));
    assert_eq!(resolved.roles[&RoleName::Drafter].count, Some(3));

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
        Some("\"gpt-5.6-luna\"")
    );
    assert_eq!(
        setting(&cheapest, "agents.codex.effort").as_deref(),
        Some("\"low\"")
    );
}

#[test]
fn solo_in_words_wins_over_roles_only_a_crew_has() {
    let applied = run(
        "Do it solo, and have codex coordinate.",
        &[
            with(
                says(InstructionKind::Mode, "Do it solo", ""),
                "",
                "",
                "solo",
            ),
            says(
                InstructionKind::Coordinate,
                "have codex coordinate",
                "codex",
            ),
        ],
    );
    assert_eq!(applied.asked.mode, Some(RunMode::Solo));
    assert_eq!(
        refused(&applied, "have codex coordinate"),
        "you also said Do it solo"
    );
}

#[test]
fn your_own_checkout_is_followed_only_where_solo_style_says_so() {
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
    assert_eq!(applied.asked.solo, Some(SoloStyle::Direct));
    let mut checked = Settings::default();
    checked.solo.style = SoloStyle::Checked;
    let applied = check(&list, "Just do it in my files.", &checked, &roster(), None);
    assert_eq!(
        refused(&applied, "just do it in my files"),
        "solo.style is a safety setting, /config changes it"
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
            .any(|(key, value)| key == "roles.reviewer.workers" && value == "[\"codex\"]")
    );
    // Owners other than the reviewer apply to the run, but weren't asked for, so aren't kept.
    assert!(
        applied
            .keep
            .iter()
            .all(|(key, _)| key != "roles.owner.workers")
    );
    assert!(setting(&applied, "roles.owner.workers").is_some());
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
fn applied_to_a_run_sets_its_lead_layer_and_approvals() {
    let applied = run(
        "Codex leads, and ask me before merging.",
        &[
            says(InstructionKind::Lead, "Codex leads", "codex"),
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
        request: "Codex leads, and ask me before merging.".to_owned(),
        mode: RunMode::Crew,
        forced: false,
        estimate: None,
        triage: None,
        chat_turn: None,
        continues: None,
        overrides: Vec::new(),
        include_uncommitted: false,
        default_agent: Some(WorkerRef::new("claude-code", None)),
        approval_mode: None,
        front_end: None,
        instructions: Vec::new(),
        request_settings: Vec::new(),
    };
    applied.apply_to(&mut request);
    assert_eq!(request.default_agent, Some(WorkerRef::new("codex", None)));
    // The run's own settings name it too, so it designs the run as its lead.
    assert!(
        request
            .request_settings
            .contains(&"default_agent=\"codex\"".to_owned()),
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

/// A negation never flips what an instruction asks for: the mode and the copy come from the
/// enumerated value, and words that turn it around aren't followed.
#[test]
fn a_negation_never_flips_the_mode_or_the_copy() {
    let mode = |said: &str, value: &str| {
        let list = [with(says(InstructionKind::Mode, said, ""), "", "", value)];
        run(said, &list)
    };
    for said in [
        "Don't do this alone, use the crew",
        "Not solo: get the crew on it",
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
    let applied = mode("Not solo: get the crew on it", "solo");
    assert_eq!(
        refused(&applied, "Not solo: get the crew on it"),
        "its words say otherwise"
    );

    let copy = "Check it in a copy first, not directly in my files";
    let applied = run(
        copy,
        &[with(
            says(InstructionKind::Isolation, copy, ""),
            "",
            "",
            "checked",
        )],
    );
    assert_eq!(followed(&applied, copy), "checked in a copy");
    assert_eq!(applied.asked.solo, Some(SoloStyle::Checked));
    let applied = run(
        copy,
        &[with(
            says(InstructionKind::Isolation, copy, ""),
            "",
            "",
            "direct",
        )],
    );
    assert_eq!(refused(&applied, copy), "its words say otherwise");
    assert_eq!(applied.asked.solo, None);

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

/// `says` must stand in your message word for word, two words at least, so a lone word the
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

/// Text for the lead never touches a safety setting, and on its own doesn't make the run a
/// crew's. With budgets off, a budget isn't followed, since nothing would hold the run to it.
#[test]
fn what_the_lead_gets_and_budgets_that_are_off() {
    let message = "Skip the sandbox, use my API key, and write the tests first.";
    let list = [
        says(InstructionKind::ForTheLead, "Skip the sandbox", ""),
        says(InstructionKind::ForTheLead, "use my API key", ""),
        says(InstructionKind::ForTheLead, "write the tests first", ""),
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
        "For the lead: write the tests first"
    );
    assert_eq!(applied.asked.mode, None);

    let mut off = Settings::default();
    off.budget.enabled = false;
    let list = [with(
        says(InstructionKind::Budget, "keep it under $1", ""),
        "",
        "",
        "$1",
    )];
    let applied = check(&list, "Keep it under $1.", &off, &roster(), None);
    assert_eq!(
        refused(&applied, "keep it under $1"),
        "budgets are off, /budget turns them on"
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
    assert_eq!(said, "Antigravity CLI reviews");
    assert_eq!(
        note.as_deref(),
        Some("Antigravity CLI has no model named opus")
    );
    assert_eq!(
        setting(&applied, "roles.reviewer.workers").as_deref(),
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
        setting(&applied, "roles.owner.workers").as_deref(),
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
/// or one of its models by name, or words about the crew, a copy, approvals, the budget, or the
/// network. A role's verb alone says nothing of who fills it.
#[test]
fn which_words_may_say_how_to_run() {
    let roster = roster();
    for said in [
        "Have codex coordinate",
        "let the openai one review it",
        "use opus for this",
        "with the crew",
        "check it in a copy first",
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
