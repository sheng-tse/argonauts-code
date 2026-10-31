//! What a request says about how to run it. The estimate lists each instruction with the
//! request's own words; here code resolves the names, checks each one in a fixed order, and
//! turns the ones that pass into settings at the request layer, for that run only. Nothing a
//! model wrote decides anything here: it only sorted and copied.
//!
//! Commands beat words, and words beat every settings layer. A request never loosens a safety
//! setting; it may tighten five things, by the rules in `argonauts_registry::tightens`.

use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::models::registry::provider::is_known;
use argonauts_core::runs::api::StartRun;
use argonauts_core::runs::events::run::{CheckedInstruction, InstructionOutcome, RunMode};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::settings::limit::LimitUnit;
use argonauts_core::settings::protocol::CaptainKind;
use argonauts_core::settings::workflows::RunStyle;
use argonauts_core::workflows::goal::{CheckMeans, GoalFrom, GoalSpec};
use argonauts_core::workflows::steps::intake::{Instruction, InstructionKind, InstructionScope};
use argonauts_crew::roles::captain_or_crew::Asked;

use crate::controller::turns::turn::DirectWords;

mod checks;
mod request_words;
mod roster;
mod words;

use checks::LOOSENING;
pub(crate) use checks::{asks_to_keep_checks, refusal as checks_refusal};
pub(crate) use roster::{Agent, AgentState, Roster};
use roster::{clamp, display_model, known_model, model_named};
use words::{
    Stance, dollars_in, effort_level, is_generic, joined, key, money, names_in, number_in,
    plainly_asks_you, quoted_at, stance, tokens, toml_list, toml_text, turned_around,
    turns_network_off, unknown, words,
};

/// Verbs that hand work to a workflow or the crew when one follows them, as in "use a workflow".
const HANDING_ON: [&str; 6] = ["use", "run", "start", "launch", "hand", "give"];

/// Whether `message` plainly asks for a workflow or the crew: a clause where one of
/// [`HANDING_ON`] comes just before the word, with no negation before it, and nothing in the
/// message asking for Captain. "Explain how the workflow works" asks for nothing.
pub(crate) fn plainly_asks_for_a_workflow(message: &str) -> bool {
    if crew_stance(message) != Stance::For {
        return false;
    }
    message
        .split([',', ';', ':', '.', '!', '?', '(', ')', '\n'])
        .any(|clause| {
            let words = tokens(clause);
            words.iter().enumerate().any(|(at, word)| {
                let cue =
                    WORKFLOW_CUES.contains(&word.as_str()) || CREW_CUES.contains(&word.as_str());
                let before = &words[at.saturating_sub(3)..at];
                let handed = before
                    .iter()
                    .any(|word| HANDING_ON.contains(&word.as_str()));
                let negated = before.iter().any(|word| NEGATIONS.contains(&word.as_str()));
                cue && handed && !negated
            })
        })
}

/// Words that end a negation's reach before the word they come before, as in "don't".
const NEGATIONS: [&str; 5] = ["dont", "not", "no", "never", "without"];

/// Whether `message` names who reviews the work: a clause that calls an agent here, or one of
/// its models, the reviewer, or has it review, as in "with Codex as the reviewer", "have Opus
/// review it" and "reviewed by Codex". "Review the parser" names nobody, and "review the Codex
/// adapter" names what to read, so neither asks for a reviewer.
pub(crate) fn names_a_reviewer(message: &str, roster: &Roster) -> bool {
    let names = name_words(roster);
    let is_name = |word: &String| names.contains(word);
    message
        .split([',', ';', ':', '.', '!', '?', '(', ')', '\n'])
        .any(|clause| {
            let words = tokens(clause);
            words.iter().enumerate().any(|(at, word)| {
                let before = &words[at.saturating_sub(5)..at];
                let after = &words[(at + 1).min(words.len())..(at + 4).min(words.len())];
                let named = match word.as_str() {
                    // "Codex as the reviewer", "the reviewer is Codex".
                    "reviewer" | "reviewers" => {
                        before.iter().any(is_name) || after.iter().any(is_name)
                    }
                    // "have Codex review it", "Codex reviews". The name comes first: after
                    // the verb it is what gets reviewed.
                    "review" | "reviews" | "reviewing" => {
                        before[before.len().saturating_sub(3)..].iter().any(is_name)
                    }
                    "reviewed" => {
                        after.first().is_some_and(|by| by == "by") && after.iter().any(is_name)
                    }
                    _ => false,
                };
                let negated = before.iter().any(|word| NEGATIONS.contains(&word.as_str()));
                named && !negated
            })
        })
}

/// The words that name an agent here: the first word of its id, its name and its provider,
/// and each word of its models' names, ids and aliases that isn't a bare number.
fn name_words(roster: &Roster) -> Vec<String> {
    let mut names = Vec::new();
    for agent in &roster.agents {
        for of in [&agent.id, &agent.name, &agent.provider] {
            names.extend(tokens(of).into_iter().next());
        }
        let models = agent
            .models
            .iter()
            .flat_map(|model| model.aliases.iter().chain([&model.name, &model.id]));
        names.extend(
            models
                .flat_map(|model| tokens(model))
                .filter(|word| word.len() > 2 && word.chars().any(char::is_alphabetic)),
        );
    }
    names
}

/// Whether `says` stands in `message` word for word, two words at least, the way an
/// instruction's words are checked against your message.
pub(crate) fn said_in(message: &str, says: &str) -> bool {
    quoted_at(&tokens(message), &tokens(says)).is_some()
}

/// What a command or a question chose, which beats the words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Command {
    pub mode: RunMode,
    /// Why the words lose, such as "you typed /plan".
    pub said: String,
    /// The style `/workflow` gives its run, which the words can't change either.
    pub style: Option<RunStyle>,
}

/// The checked instructions, and what the run takes from the ones followed.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Applied {
    /// Every instruction in your message's order, then the dropped ones.
    pub instructions: Vec<CheckedInstruction>,
    /// The mode and Captain kind your words chose, for the Captain-or-crew rules.
    pub asked: Asked,
    /// `(key, TOML value)` at the request layer.
    pub settings: Vec<(String, String)>,
    /// Who is the main agent of this run, when your words named someone.
    pub main_agent: Option<ModelRef>,
    /// Who works as Captain, when your words named one builder and the run turns out Captain.
    pub captain_main_agent: Option<ModelRef>,
    /// Your words asked approvals to wait for you.
    pub supervised: bool,
    /// What "from now on" asked to keep, as `(key, TOML value)`, and as the line said it.
    pub keep: Vec<(String, String)>,
    pub keep_said: Vec<String>,
}

impl Applied {
    /// Your words, applied to the run they start: its instructions for the record and the
    /// start line, the request layer, the main agent, and the approval mode.
    pub(crate) fn apply_to(&self, request: &mut StartRun) {
        request.instructions = self.instructions.clone();
        request.request_settings = self
            .settings
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        let main_agent = match (&self.main_agent, &self.captain_main_agent) {
            (Some(main_agent), _) => Some(main_agent),
            (None, Some(main_agent)) if request.mode == RunMode::Captain => Some(main_agent),
            _ => None,
        };
        if let Some(main_agent) = main_agent {
            request.main_agent = Some(main_agent.clone());
            // The run's own main agent too, since that's who designs and drives it.
            request
                .request_settings
                .push(format!("crew.main={}", toml_text(&main_agent.to_string())));
        }
        if self.supervised {
            request.approval_mode = Some(ApprovalMode::Supervised);
        }
    }

    /// The workers your words gave `role`, as the request layer holds them.
    pub(crate) fn workers(&self, role: RoleName) -> Vec<ModelRef> {
        let key = format!("roles.{}.models", role.as_str());
        let Some((_, list)) = self.settings.iter().rev().find(|(known, _)| *known == key) else {
            return Vec::new();
        };
        let Ok(mut table) = format!("value = {list}").parse::<toml::Table>() else {
            return Vec::new();
        };
        let Some(toml::Value::Array(items)) = table.remove("value") else {
            return Vec::new();
        };
        items
            .iter()
            .filter_map(|item| item.as_str()?.parse().ok())
            .collect()
    }

    /// What a direct turn of `main` takes from your words: its model and effort, a lower
    /// limit, no network, and asking you first. The rest needs a run, which the Captain-or-crew
    /// rules already chose when it applied.
    pub(crate) fn for_direct(&self, main: &str) -> DirectWords {
        let value = |key: &str| -> Option<toml::Value> {
            let (_, value) = self.settings.iter().rev().find(|(known, _)| known == key)?;
            format!("value = {value}")
                .parse::<toml::Table>()
                .ok()?
                .remove("value")
        };
        let text = |key: &str| value(key).and_then(|value| value.as_str().map(str::to_owned));
        DirectWords {
            model: text(&format!("agents.{main}.model")),
            effort: text(&format!("agents.{main}.effort")),
            limit_usd: value("limits.spend_per_run").and_then(|value| {
                value
                    .as_float()
                    .or_else(|| value.as_integer().map(|whole| whole as f64))
            }),
            no_network: value("sandbox.network").and_then(|value| value.as_bool()) == Some(false),
            supervised: self.supervised,
            goal: text("goal.until"),
            ..DirectWords::default()
        }
    }
}

/// Whether a message may say how to run it, so a command's words are worth an estimate first: it
/// names an agent or one of its models, or speaks of the crew, roles, a worktree, the limit,
/// approvals, merging, the network, effort, or "from now on". A false yes costs a read-only
/// turn, and a false no runs the command as it always did.
pub(crate) fn might_say_how(message: &str, roster: &Roster) -> bool {
    let said = tokens(message);
    let has = |word: &str| said.iter().any(|said| said == word);
    let names = name_words(roster).iter().any(|word| has(word));
    names
        || HOW_CUES
            .iter()
            .chain(&request_words::SAY_HOW)
            .any(|cue| has(cue))
        || message.contains('$')
        || quoted_at(&said, &tokens("from now on")).is_some()
}

/// Words that may say how to run a request. A role's verb alone, such as "the build runs",
/// says nothing of who fills it: the agent's name does.
const HOW_CUES: [&str; 31] = [
    // "keep going until its tests pass" sets a goal.
    "until",
    "crew",
    "team",
    "workflow",
    "agents",
    "agy",
    "gemini",
    "captain",
    "yourself",
    "alone",
    "checked",
    "worktree",
    "isolated",
    "limit",
    "dollars",
    "usd",
    "network",
    "offline",
    "internet",
    "approve",
    "approval",
    "approvals",
    "ask",
    "merge",
    "merging",
    "effort",
    "model",
    "models",
    "cheap",
    "cheaper",
    "cheapest",
];

/// Checks each instruction against your message, the agents here, and your settings, in this
/// order: words you didn't write, unknown or unready agents, unknown models, efforts,
/// roles an agent can't fill, safety settings, a command that says otherwise, then two
/// instructions that disagree, where the later one in your message wins.
pub(crate) fn check(
    how_to_run: &[Instruction],
    message: &str,
    settings: &Settings,
    roster: &Roster,
    command: Option<&Command>,
) -> Applied {
    let said = tokens(message);
    let mut dropped = Vec::new();
    let mut entries: Vec<Entry> = Vec::new();
    for instruction in how_to_run {
        match quoted_at(&said, &tokens(&instruction.says)) {
            Some(at) => entries.push(Entry {
                instruction: instruction.clone(),
                at,
                result: Err(String::new()),
            }),
            None => dropped.push(CheckedInstruction {
                says: instruction.says.clone(),
                kind: instruction.kind,
                outcome: InstructionOutcome::Dropped,
            }),
        }
    }
    entries.sort_by_key(|entry| entry.at);
    let checker = Checker {
        settings,
        roster,
        current: current_values(settings),
    };
    for entry in &mut entries {
        entry.result = checker.resolve(&entry.instruction);
    }
    settle_disagreements(&mut entries);
    checker.settle_others(&mut entries);
    let mode = settle_mode(&mut entries, command);
    checker.review_against_workers(&mut entries, mode);
    checker.within_one_provider(&mut entries);
    needs_a_worktree(&mut entries);

    let mut applied = Applied {
        asked: Asked {
            mode,
            captain: None,
        },
        ..Applied::default()
    };
    for entry in &entries {
        let Ok(effect) = &entry.result else { continue };
        for (key, value) in effect.settings.iter().chain(&effect.inferred) {
            applied.settings.retain(|(known, _)| known != key);
            applied.settings.push((key.clone(), value.clone()));
        }
        if effect.main_agent.is_some() {
            applied.main_agent = effect.main_agent.clone();
        }
        if effect.captain_main_agent.is_some() {
            applied.captain_main_agent = effect.captain_main_agent.clone();
        }
        if effect.captain.is_some() && applied.asked.captain != Some(CaptainKind::Checked) {
            applied.asked.captain = effect.captain;
        }
        applied.supervised |= effect.supervised;
        if entry.instruction.scope == InstructionScope::FromNowOn {
            applied.keep.extend(effect.settings.iter().cloned());
            if let Some(main_agent) = &effect.main_agent {
                applied
                    .keep
                    .push(("crew.main".to_owned(), toml_text(&main_agent.to_string())));
            }
            if !effect.said.is_empty() {
                applied.keep_said.push(effect.said.clone());
            }
        }
    }
    applied.instructions = entries
        .into_iter()
        .map(|entry| CheckedInstruction {
            says: entry.instruction.says,
            kind: entry.instruction.kind,
            outcome: match entry.result {
                Ok(Effect {
                    noted: Some(line), ..
                }) => InstructionOutcome::Noted { line },
                Ok(effect) => InstructionOutcome::Followed {
                    said: effect.said,
                    note: effect.note,
                },
                Err(why) => InstructionOutcome::NotFollowed { why },
            },
        })
        .chain(dropped)
        .collect();
    applied
}

struct Entry {
    instruction: Instruction,
    /// Where its words start in your message.
    at: usize,
    result: Result<Effect, String>,
}

/// What one followed instruction does.
#[derive(Debug, Clone, Default)]
struct Effect {
    /// As the start line says it.
    said: String,
    note: Option<String>,
    /// Nothing to apply or refuse, and a line that says how things stand instead.
    noted: Option<String>,
    /// Two instructions with the same slot disagree, and the later one wins.
    slot: Option<String>,
    /// Agents it names, so leaving one out and giving it a role disagree.
    names: Vec<String>,
    /// Agents it leaves out.
    leaves_out: Vec<String>,
    settings: Vec<(String, String)>,
    main_agent: Option<ModelRef>,
    captain_main_agent: Option<ModelRef>,
    /// The mode it asks for in so many words.
    mode: Option<RunMode>,
    /// The mode it needs: roles that only a crew has, or a checked worktree that only Captain has.
    implies: Option<RunMode>,
    captain: Option<CaptainKind>,
    supervised: bool,
    /// Settings it needs that the words didn't name, such as workers other than the reviewers.
    /// They apply to the run, and "from now on" doesn't keep them.
    inferred: Vec<(String, String)>,
    /// Who it gives the reviewer role to, for the provider check against the workers.
    reviewers: Vec<ModelRef>,
    /// Who it gives the worker role to.
    workers: Vec<ModelRef>,
    /// Who "all the other agents" names. Which roles they fill is settled once every
    /// instruction is read, since a role another one names keeps that one.
    others: Vec<ModelRef>,
    /// The crew it leaves, for a crew or an agent left out of it.
    crew: Vec<ModelRef>,
}

struct Checker<'a> {
    settings: &'a Settings,
    roster: &'a Roster,
    /// The settings as TOML, to check a tightening against what they say now.
    current: Option<toml::Value>,
}

impl Checker<'_> {
    fn resolve(&self, instruction: &Instruction) -> Result<Effect, String> {
        use InstructionKind as Kind;
        let gives_a_role = matches!(
            instruction.kind,
            Kind::MainAgent
                | Kind::Orchestrate
                | Kind::Build
                | Kind::Review
                | Kind::Judge
                | Kind::Plan
                | Kind::Check
                | Kind::Others
        );
        let named;
        let instruction = if gives_a_role {
            named = self.agent_in_who(instruction)?;
            &named
        } else {
            instruction
        };
        if gives_a_role && turned_around(&instruction.says, &names_in(&instruction.who)) {
            return Err(SAYS_OTHERWISE.to_owned());
        }
        match instruction.kind {
            Kind::Mode => mode(instruction),
            Kind::Goal => self.goal(instruction),
            Kind::Isolation => self.isolation(instruction),
            Kind::MainAgent | Kind::Orchestrate => self.main_agent(instruction),
            Kind::Build => self.build(instruction),
            Kind::Review => self.review(instruction),
            Kind::Judge => self.judge(instruction),
            Kind::Plan => self.plan(instruction),
            Kind::Check => self.goal_check(instruction),
            Kind::Crew => self.crew(instruction),
            Kind::Others => self.others(instruction),
            Kind::Exclude => self.exclude(instruction),
            Kind::Model => self.model(instruction),
            Kind::Effort => self.effort(instruction),
            Kind::Cost => self.cost(instruction),
            Kind::Approvals => self.approvals(instruction),
            Kind::Merge => self.merge(instruction),
            Kind::Limit => self.limit(instruction),
            Kind::Network => self.network(instruction),
            Kind::Checks => checks::resolve(instruction, self.settings),
            Kind::Sandbox => {
                Err("the sandbox is a safety setting, /permissions changes it".to_owned())
            }
            Kind::ForTheMainAgent => for_the_main_agent(instruction),
            Kind::ForARole => for_a_role(instruction),
        }
    }

    /// "Keep going until its tests pass": the request's own words set a goal for this request,
    /// which the start line states back before work starts.
    fn goal(&self, instruction: &Instruction) -> Result<Effect, String> {
        if let Some(effect) = role_goal(instruction) {
            return Ok(effect);
        }
        let condition = match instruction.value.trim() {
            "" => instruction.says.trim(),
            value => value,
        };
        let means = CheckMeans {
            project_checks: !self.settings.checks.run.is_empty(),
        };
        let check = self.settings.goal.check;
        let goal = GoalSpec::from_words(condition, check, means, GoalFrom::Request)?;
        Ok(Effect {
            said: format!("goal: {} · {}", goal.condition, goal.checked_by()),
            slot: Some("goal".to_owned()),
            settings: vec![("goal.until".to_owned(), toml_text(&goal.condition))],
            ..Effect::default()
        })
    }

    /// A checked worktree only tightens. Your own checkout is followed only where `captain.kind`
    /// already says so, since a request never loosens a safety setting.
    fn isolation(&self, instruction: &Instruction) -> Result<Effect, String> {
        let says = &instruction.says;
        let worktree = || stance(says, &worktree_cues(), &CHECKOUT_CUES);
        let checkout = || stance(says, &CHECKOUT_CUES, &worktree_cues());
        let direct = match key(&instruction.value).as_str() {
            "checked" => match worktree() {
                Stance::For => false,
                _ => return Err(SAYS_OTHERWISE.to_owned()),
            },
            "direct" => match checkout() {
                Stance::For => true,
                _ => return Err(SAYS_OTHERWISE.to_owned()),
            },
            _ => match (worktree(), checkout()) {
                (Stance::For, _) => false,
                (_, Stance::For) => true,
                _ => return Err("it says neither a checked worktree nor your checkout".to_owned()),
            },
        };
        if direct {
            if self.settings.captain.kind == CaptainKind::Direct
                && !self.settings.captain.needs_a_worktree()
            {
                return Ok(Effect {
                    said: "in your checkout".to_owned(),
                    slot: Some("isolation".to_owned()),
                    implies: Some(RunMode::Captain),
                    captain: Some(CaptainKind::Direct),
                    ..Effect::default()
                });
            }
            return Err("captain.kind is a safety setting, /config changes it".to_owned());
        }
        let mut effect = Effect {
            said: "checked in a worktree".to_owned(),
            slot: Some("isolation".to_owned()),
            implies: Some(RunMode::Captain),
            captain: Some(CaptainKind::Checked),
            ..Effect::default()
        };
        if self.settings.captain.kind != CaptainKind::Checked {
            self.tighten(&mut effect, "captain.kind", toml_text("checked"))?;
        }
        Ok(effect)
    }

    /// The main agent or the orchestrator your words name: the agent designs the run. An
    /// orchestrator also plans the tasks and assigns their workers and reviewers, as the
    /// orchestrator workflow does.
    fn main_agent(&self, instruction: &Instruction) -> Result<Effect, String> {
        let orchestrates = instruction.kind == InstructionKind::Orchestrate;
        let (verb, role) = if orchestrates {
            ("orchestrates", "orchestrate")
        } else {
            ("is the main agent", "be the main agent")
        };
        let fallback = format!(
            "{} is the main agent",
            self.roster.name(&self.settings.crew.main.agent)
        );
        let named = self.named(instruction, &fallback)?;
        let [agent] = named.agents.as_slice() else {
            return Err(format!("only one agent can {role}"));
        };
        if agent.endpoint {
            return Err(format!("an endpoint has no tools to {role} with"));
        }
        let mut effect = Effect {
            said: format!("{} {verb}{}", agent.name, agent.with()),
            note: named.note.clone(),
            slot: Some("main_agent".to_owned()),
            names: vec![agent.worker.agent.clone()],
            main_agent: Some(agent.worker.clone()),
            ..Effect::default()
        };
        if let Some(effort) = &agent.effort {
            let key = format!("agents.{}.effort", agent.worker.agent);
            effect.settings.push((key, toml_text(effort)));
        }
        if orchestrates {
            effect.implies = Some(RunMode::Crew);
            let worker = toml_list(std::slice::from_ref(&agent.worker));
            effect.settings.extend([
                ("workflow".to_owned(), toml_text("orchestrator")),
                ("drafting.by".to_owned(), toml_text("orchestrator")),
                ("judging.method".to_owned(), toml_text("orchestrator")),
                ("routing.algorithm".to_owned(), toml_text("orchestrator")),
                ("review.assign".to_owned(), toml_text("orchestrator")),
                ("roles.orchestrator.models".to_owned(), worker),
            ]);
        }
        Ok(effect)
    }

    fn build(&self, instruction: &Instruction) -> Result<Effect, String> {
        let named = self.named(instruction, "the crew's workers build")?;
        if named.agents.iter().any(|agent| agent.endpoint) {
            return Err("an endpoint has no tools to build with".to_owned());
        }
        let workers: Vec<ModelRef> = named
            .agents
            .iter()
            .map(|agent| agent.worker.clone())
            .collect();
        let mut effect = self.role_effect(&named, RoleName::Worker, "builds", "build");
        effect.slot = Some("build".to_owned());
        if let [one] = workers.as_slice() {
            effect.captain_main_agent = Some(one.clone());
            // Your checkout is the main session's, so another agent builds as Captain in a
            // worktree.
            if one.agent != self.settings.crew.main.agent {
                effect.captain = Some(CaptainKind::Checked);
            }
        } else {
            effect.implies = Some(RunMode::Crew);
        }
        effect.workers = workers;
        Ok(effect)
    }

    /// Reviewers for every task. In Captain, a review needs a worktree, so it makes Captain
    /// checked.
    fn review(&self, instruction: &Instruction) -> Result<Effect, String> {
        let named = self.named(instruction, "the crew's reviewers review")?;
        let workers: Vec<ModelRef> = named
            .agents
            .iter()
            .map(|agent| agent.worker.clone())
            .collect();
        let mut effect = self.role_effect(&named, RoleName::Reviewer, "reviews", "review");
        effect.slot = Some("review".to_owned());
        effect.settings.extend([
            ("review.per_task".to_owned(), workers.len().to_string()),
            ("captain.review".to_owned(), "true".to_owned()),
        ]);
        effect.captain = Some(CaptainKind::Checked);
        effect.reviewers = workers;
        Ok(effect)
    }

    /// Who decides and signs off. Only a crew has a judge.
    fn judge(&self, instruction: &Instruction) -> Result<Effect, String> {
        let named = self.named(instruction, "the crew's judge decides")?;
        let mut effect = self.role_effect(&named, RoleName::Judge, "judges", "judge");
        effect.slot = Some("judge".to_owned());
        effect.implies = Some(RunMode::Crew);
        Ok(effect)
    }

    /// "All the other agents": the agents named fill every role but the agent you talk to,
    /// which [`Checker::settle_others`] works out. They build, so none can be an endpoint.
    fn others(&self, instruction: &Instruction) -> Result<Effect, String> {
        let named = self.named(instruction, "the crew stays as it is")?;
        if named.agents.iter().any(|agent| agent.endpoint) {
            return Err("an endpoint has no tools to build with".to_owned());
        }
        let workers: Vec<ModelRef> = named
            .agents
            .iter()
            .map(|agent| agent.worker.clone())
            .collect();
        let who = match named.agents.as_slice() {
            [agent] => format!("{}{}", agent.name, agent.with()),
            many => joined(
                &many
                    .iter()
                    .map(|agent| agent.name.clone())
                    .collect::<Vec<_>>(),
            ),
        };
        Ok(Effect {
            said: format!("{who} for all the other agents"),
            note: named.note.clone(),
            slot: Some("others".to_owned()),
            names: workers.iter().map(|worker| worker.agent.clone()).collect(),
            implies: Some(RunMode::Crew),
            workers: workers.clone(),
            others: workers,
            ..Effect::default()
        })
    }

    /// The roles "all the other agents" fills: the workers, the reviewers, the judge, and an
    /// orchestrator that isn't the agent you talk to, except a role another of your
    /// instructions names, followed or not. As reviewers they would review their own work as
    /// workers, so while `review.other_provider` is on the reviewers stay as they are.
    fn settle_others(&self, entries: &mut [Entry]) {
        let named: Vec<CrewRole> = entries
            .iter()
            .filter_map(|entry| named_role(entry.instruction.kind))
            .collect();
        let main = &self.settings.crew.main;
        let orchestrator_is_main =
            argonauts_crew::roles::candidates(self.settings, RoleName::Orchestrator)
                == [main.clone()];
        for entry in entries.iter_mut() {
            let Ok(effect) = &mut entry.result else {
                continue;
            };
            if effect.others.is_empty() {
                continue;
            }
            let list = toml_list(&effect.others);
            let mut filled = 0;
            for role in [
                CrewRole::Orchestrator,
                CrewRole::Worker,
                CrewRole::Reviewer,
                CrewRole::Judge,
            ] {
                if named.contains(&role) || (role == CrewRole::Orchestrator && orchestrator_is_main)
                {
                    continue;
                }
                if role == CrewRole::Reviewer && self.settings.review.other_provider {
                    effect.note = Some(join_notes(
                        effect.note.take(),
                        "the reviewers stay as they are, since a review comes from another \
                         provider",
                    ));
                    continue;
                }
                effect
                    .settings
                    .push((format!("crew.{}", role.as_str()), list.clone()));
                // A role's own workers beat the crew's, so the ones you set are replaced too.
                for (fine, own) in &self.settings.roles {
                    if CrewRole::followed_by(*fine) == role && !own.models.is_empty() {
                        effect
                            .settings
                            .push((format!("roles.{}.models", fine.as_str()), list.clone()));
                    }
                }
                filled += 1;
            }
            if filled == 0 {
                entry.result = Err("your other words name every role".to_owned());
            }
        }
    }

    fn plan(&self, instruction: &Instruction) -> Result<Effect, String> {
        let count = number_in(&instruction.value).or_else(|| number_in(&instruction.says));
        if instruction.who.trim().is_empty() || is_generic(&instruction.who) {
            let Some(count) = count.filter(|count| *count > 0) else {
                return Err("it names no one to plan and no number of plans".to_owned());
            };
            return Ok(Effect {
                said: format!("{count} drafting agents"),
                slot: Some("plan".to_owned()),
                settings: vec![("roles.draft.count".to_owned(), count.to_string())],
                implies: Some(RunMode::Crew),
                ..Effect::default()
            });
        }
        let named = self.named(instruction, "the crew's drafting agents plan")?;
        let mut effect = self.role_effect(&named, RoleName::Draft, "plans", "plan");
        effect.slot = Some("plan".to_owned());
        effect.implies = Some(RunMode::Crew);
        Ok(effect)
    }

    /// Who checks the goal at the end, where this version has a final check to give.
    fn goal_check(&self, instruction: &Instruction) -> Result<Effect, String> {
        if !has_key(self.settings, "/goal/checker") {
            return Err("this version has no final goal check to give".to_owned());
        }
        let named = self.named(instruction, "the usual checker checks")?;
        let [agent] = named.agents.as_slice() else {
            return Err("only one agent can check the goal".to_owned());
        };
        Ok(Effect {
            said: format!("{} checks{}", agent.name, agent.with()),
            note: named.note.clone(),
            slot: Some("check".to_owned()),
            names: vec![agent.worker.agent.clone()],
            settings: vec![(
                "goal.checker".to_owned(),
                toml_text(&agent.worker.to_string()),
            )],
            ..Effect::default()
        })
    }

    /// Only these agents, the main agent among them. When the main agent isn't one, the first
    /// named agent CLI is the main agent.
    fn crew(&self, instruction: &Instruction) -> Result<Effect, String> {
        if instruction.who.trim().is_empty() || is_generic(&instruction.who) {
            if crew_stance(&instruction.says) != Stance::For {
                return Err(SAYS_OTHERWISE.to_owned());
            }
            return Ok(Effect {
                mode: Some(RunMode::Crew),
                slot: Some("mode".to_owned()),
                ..Effect::default()
            });
        }
        let named = self.named(instruction, "the crew stays as it is")?;
        let workers: Vec<ModelRef> = named
            .agents
            .iter()
            .map(|agent| agent.worker.clone())
            .collect();
        let names: Vec<String> = named
            .agents
            .iter()
            .map(|agent| agent.name.clone())
            .collect();
        let mut effect = Effect {
            said: format!("Crew: {}", joined(&names)),
            note: named.note.clone(),
            slot: Some("crew".to_owned()),
            names: workers.iter().map(|worker| worker.agent.clone()).collect(),
            settings: vec![("crew".to_owned(), toml_list(&workers))],
            crew: workers.clone(),
            ..Effect::default()
        };
        let default = &self.settings.crew.main.agent;
        if !workers.iter().any(|worker| &worker.agent == default) {
            effect.main_agent = named
                .agents
                .iter()
                .find(|agent| !agent.endpoint)
                .map(|agent| agent.worker.clone());
        }
        if workers.len() > 1 {
            effect.implies = Some(RunMode::Crew);
        }
        Ok(effect)
    }

    /// Out of the crew and every role's workers. When it was the main agent, the first agent
    /// CLI left in the crew is.
    fn exclude(&self, instruction: &Instruction) -> Result<Effect, String> {
        let mut out = Vec::new();
        for name in names_in(&instruction.who) {
            match self.roster.find(name) {
                Some((agent, _)) => out.push(agent.id.clone()),
                None => return Err(format!("no agent here is called {}", name.trim())),
            }
        }
        if out.is_empty() {
            return Err("it names no agent to leave out".to_owned());
        }
        let kept = |workers: &[ModelRef]| -> Vec<ModelRef> {
            workers
                .iter()
                .filter(|worker| !out.contains(&worker.agent))
                .cloned()
                .collect()
        };
        let crew = kept(&self.settings.crew.members());
        if crew.is_empty() {
            return Err("that leaves no agent in the crew".to_owned());
        }
        let mut settings = vec![("crew".to_owned(), toml_list(&crew))];
        for (role, role_settings) in &self.settings.roles {
            let left = kept(&role_settings.models);
            if left.len() != role_settings.models.len() {
                settings.push((format!("roles.{}.models", role.as_str()), toml_list(&left)));
            }
        }
        let main_agent = out
            .contains(&self.settings.crew.main.agent)
            .then(|| {
                crew.iter()
                    .find(|worker| {
                        self.roster
                            .agent(&worker.agent)
                            .is_some_and(|a| !a.endpoint)
                    })
                    .cloned()
            })
            .flatten();
        let names: Vec<String> = out.iter().map(|id| self.roster.name(id)).collect();
        Ok(Effect {
            said: format!("Not {}", joined(&names)),
            leaves_out: out,
            settings,
            main_agent,
            crew,
            ..Effect::default()
        })
    }

    /// The agent that runs a model uses it in every role. "Cheapest" puts every agent in the
    /// crew on its cheapest model, as "keep it cheap" does.
    fn model(&self, instruction: &Instruction) -> Result<Effect, String> {
        let asked = if instruction.model.trim().is_empty() {
            instruction.value.trim()
        } else {
            instruction.model.trim()
        };
        let text = words(asked, &instruction.says);
        if text.contains("cheap") {
            return Ok(self.cheapest());
        }
        let named = if instruction.who.trim().is_empty() {
            asked
        } else {
            instruction.who.trim()
        };
        let Some((agent, model)) = self.roster.find(named) else {
            return Err(format!("no agent here has a model called {}", named));
        };
        self.ready(agent, "your settings stay")?;
        let model = match model {
            Some(model) => model,
            None if asked.is_empty() || key(asked) == key(named) => {
                // "Use only Claude" names a CLI and no model: it says who makes up the crew.
                if says_only(&instruction.says) {
                    let crew = Instruction {
                        who: named.to_owned(),
                        ..instruction.clone()
                    };
                    return self.crew(&crew);
                }
                return Err(format!("it names no model for {}", agent.name));
            }
            None => model_named(self.roster, agent, asked)?,
        };
        let model_name = display_model(agent, &model);
        Ok(Effect {
            said: format!("{} runs on {model_name}", agent.name),
            slot: Some(format!("model:{}", agent.id)),
            names: vec![agent.id.clone()],
            settings: vec![(format!("agents.{}.model", agent.id), toml_text(&model))],
            ..Effect::default()
        })
    }

    /// "Keep it cheap": every agent in the crew on its cheapest model, and nothing else, so the
    /// crew stays as it is. Words that turn it around, as "don't keep it cheap", change nothing.
    fn cost(&self, instruction: &Instruction) -> Result<Effect, String> {
        let says = format!("{} {}", instruction.value.trim(), instruction.says.trim());
        match stance(&says, &CHEAP, &DEAR) {
            Stance::For => Ok(self.cheapest()),
            Stance::Against => {
                Err("it asks to spend no less, so the models stay as they are".into())
            }
            Stance::Silent => {
                Err("it names no way to spend less, so the models stay as they are".into())
            }
        }
    }

    /// Every agent in the crew, the main agent too, on the model and effort its cheapest work
    /// runs on.
    fn cheapest(&self) -> Effect {
        let mut settings = Vec::new();
        for worker in self.crew_and_main_agent() {
            let Some((model, effort)) = self
                .roster
                .agent(&worker.agent)
                .and_then(|agent| agent.cheapest.clone())
            else {
                continue;
            };
            settings.push((format!("agents.{}.model", worker.agent), toml_text(&model)));
            if let Some(effort) = effort {
                settings.push((
                    format!("agents.{}.effort", worker.agent),
                    toml_text(&effort),
                ));
            }
        }
        Effect {
            said: "the cheapest models".to_owned(),
            slot: Some(CHEAPEST.to_owned()),
            settings,
            ..Effect::default()
        }
    }

    /// The named agent's effort, or every agent's in the crew, at the nearest level each
    /// model takes.
    fn effort(&self, instruction: &Instruction) -> Result<Effect, String> {
        let asked = if instruction.effort.trim().is_empty() {
            words(&instruction.value, &instruction.says)
        } else {
            instruction.effort.clone()
        };
        let level = effort_level(&asked).ok_or_else(|| {
            format!(
                "no effort level called {}, /effort lists them",
                asked.trim()
            )
        })?;
        if instruction.who.trim().is_empty() || is_generic(&instruction.who) {
            let mut settings = Vec::new();
            let mut notes = Vec::new();
            for worker in self.crew_and_main_agent() {
                let agent = self.roster.agent(&worker.agent);
                let (taken, note) = clamp(&level, agent, worker.model.as_deref());
                notes.extend(note);
                if let Some(taken) = taken {
                    settings.push((format!("agents.{}.effort", worker.agent), toml_text(&taken)));
                }
            }
            return Ok(Effect {
                said: format!("{level} effort"),
                note: (!notes.is_empty()).then(|| notes.join("; ")),
                slot: Some("effort:*".to_owned()),
                settings,
                ..Effect::default()
            });
        }
        let Some((agent, model)) = self.roster.find(&instruction.who) else {
            return Err(unknown(&instruction.who));
        };
        self.ready(agent, "your settings stay")?;
        let (taken, note) = clamp(&level, Some(agent), model.as_deref());
        let taken = taken.ok_or_else(|| note.clone().unwrap_or_default())?;
        Ok(Effect {
            said: format!("{} on {taken} effort", agent.name),
            note,
            slot: Some(format!("effort:{}", agent.id)),
            names: vec![agent.id.clone()],
            settings: vec![(format!("agents.{}.effort", agent.id), toml_text(&taken))],
            ..Effect::default()
        })
    }

    /// Only toward asking you: every approval waits for you, and the run starts supervised.
    fn approvals(&self, instruction: &Instruction) -> Result<Effect, String> {
        if !plainly_asks_you(&instruction.says) {
            return Err(
                "approvals are safety settings, /approvals and Shift+Tab change them".to_owned(),
            );
        }
        let mut effect = Effect {
            said: "Approvals wait for you".to_owned(),
            slot: Some("approvals".to_owned()),
            supervised: true,
            ..Effect::default()
        };
        for approval in APPROVALS {
            let key = format!("approvals.{approval}.mode");
            if self.now(&key).as_deref() != Some("supervised") {
                self.tighten(&mut effect, &key, toml_text("supervised"))?;
            }
        }
        Ok(effect)
    }

    fn merge(&self, instruction: &Instruction) -> Result<Effect, String> {
        if !plainly_asks_you(&instruction.says) {
            return Err(
                "approvals.merge.mode is a safety setting, /approvals changes it".to_owned(),
            );
        }
        let mut effect = Effect {
            said: "Merge waits for you".to_owned(),
            slot: Some("merge".to_owned()),
            ..Effect::default()
        };
        if self.now("approvals.merge.mode").as_deref() != Some("supervised") {
            self.tighten(&mut effect, "approvals.merge.mode", toml_text("supervised"))?;
        }
        Ok(effect)
    }

    /// Only a lower run limit in dollars, as code compares the amounts. With no run limit
    /// set, any amount is a lower one.
    fn limit(&self, instruction: &Instruction) -> Result<Effect, String> {
        let raising = "limits.spend_per_run is a safety setting, /limits changes it".to_owned();
        let Some(amount) = dollars_in(&instruction.value).or_else(|| dollars_in(&instruction.says))
        else {
            return Err("it names no amount in dollars".to_owned());
        };
        let now = match self.settings.limits.spend.run_limit() {
            Some(limit) if limit.unit == LimitUnit::Usd => Some(limit.amount),
            Some(_) => {
                return Err("the run limit isn't counted in dollars, /limits changes it".to_owned());
            }
            None => None,
        };
        if amount <= 0.0 || now.is_some_and(|now| amount >= now) {
            return Err(raising);
        }
        let mut effect = Effect {
            said: format!("Limit ${}", money(amount)),
            slot: Some("limit".to_owned()),
            ..Effect::default()
        };
        self.tighten(&mut effect, "limits.spend_per_run", format!("{amount}"))
            .map_err(|_| raising)?;
        Ok(effect)
    }

    fn network(&self, instruction: &Instruction) -> Result<Effect, String> {
        if !turns_network_off(&instruction.says) {
            return Err("sandbox.network is a safety setting, /permissions changes it".to_owned());
        }
        let mut effect = Effect {
            said: "No network".to_owned(),
            slot: Some("network".to_owned()),
            ..Effect::default()
        };
        if self.settings.sandbox.network {
            self.tighten(&mut effect, "sandbox.network", "false".to_owned())?;
        }
        Ok(effect)
    }

    /// A safety key the request sets, which must only tighten what the settings say now.
    fn tighten(&self, effect: &mut Effect, key: &str, value: String) -> Result<(), String> {
        let parsed: Option<toml::Value> = format!("value = {value}")
            .parse::<toml::Table>()
            .ok()
            .and_then(|mut table| table.remove("value"));
        let current = self
            .current
            .as_ref()
            .and_then(|current| value_at(current, key));
        match parsed {
            Some(new) if argonauts_registry::tightens(key, &new, current) => {
                effect.settings.push((key.to_owned(), value));
                Ok(())
            }
            _ => Err(format!(
                "{key} is a safety setting, /permissions changes it"
            )),
        }
    }

    fn now(&self, key: &str) -> Option<String> {
        let current = self.current.as_ref()?;
        value_at(current, key)?.as_str().map(str::to_owned)
    }

    fn crew_and_main_agent(&self) -> Vec<ModelRef> {
        let mut workers = self.settings.crew.members();
        if !workers
            .iter()
            .any(|worker| worker.agent == self.settings.crew.main.agent)
        {
            workers.push(self.settings.crew.main.clone());
        }
        workers
    }

    /// The agents `who` names for a role, each on the model and effort the instruction names.
    /// One that can't take part is left out with a note; when none can, it's not followed.
    fn named(&self, instruction: &Instruction, fallback: &str) -> Result<Named, String> {
        let mut agents: Vec<NamedAgent> = Vec::new();
        let mut problems = Vec::new();
        for name in names_in(&instruction.who) {
            let Some((agent, named_model)) = self.roster.find(name) else {
                problems.push(unknown(name));
                continue;
            };
            if let Err(why) = self.ready(agent, fallback) {
                problems.push(why);
                continue;
            }
            let mut note = None;
            let model = match (named_model, instruction.model.trim()) {
                (Some(model), _) => Some(model),
                (None, "") => None,
                (None, asked) => match model_named(self.roster, agent, asked) {
                    Ok(model) => Some(model),
                    Err(why) => {
                        note = Some(why);
                        None
                    }
                },
            };
            let effort = match effort_level(&instruction.effort) {
                Some(level) => {
                    let (taken, clamped) = clamp(&level, Some(agent), model.as_deref());
                    note = note.or(clamped);
                    taken
                }
                None => None,
            };
            if agents.iter().any(|known| known.worker.agent == agent.id) {
                continue;
            }
            agents.push(NamedAgent {
                worker: ModelRef::new(agent.id.clone(), model.clone()),
                name: agent.name.clone(),
                model_name: model.as_deref().map(|model| display_model(agent, model)),
                effort,
                endpoint: agent.endpoint,
            });
            problems.extend(note);
        }
        if agents.is_empty() {
            return Err(problems
                .into_iter()
                .next()
                .unwrap_or_else(|| "it names no agent".to_owned()));
        }
        Ok(Named {
            agents,
            note: (!problems.is_empty()).then(|| problems.join("; ")),
        })
    }

    /// The instruction with the agent it gives a role to in `who`. A model may put the role
    /// there instead, as `who: "reviewer"` for "with Codex as the reviewer", with the agent in
    /// `value` or only in the words; the agent counts all the same.
    fn agent_in_who(&self, instruction: &Instruction) -> Result<Instruction, String> {
        let mut named = instruction.clone();
        let here = |text: &str| {
            names_in(text)
                .into_iter()
                .any(|name| self.roster.find(name).is_some())
        };
        if here(&instruction.who) || !names_a_role(&instruction.who) {
            return Ok(named);
        }
        if here(&instruction.value) {
            named.who = std::mem::take(&mut named.value);
            return Ok(named);
        }
        let agents: Vec<String> = tokens(&instruction.says)
            .into_iter()
            .filter(|word| matches!(self.roster.find(word), Some((_, None))))
            .collect();
        let role = instruction.who.trim();
        match agents.is_empty() {
            false => named.who = agents.join(", "),
            true if role.is_empty() => {}
            true => {
                return Err(format!(
                    "{role} is a role, and the words name no agent for it"
                ));
            }
        }
        Ok(named)
    }

    /// Whether `agent` can work now, and if not, why and what happens instead.
    fn ready(&self, agent: &Agent, fallback: &str) -> Result<(), String> {
        let (cli, endpoint) = match agent.state {
            AgentState::Ready => return Ok(()),
            AgentState::NotInstalled => ("isn't installed", "isn't set up"),
            AgentState::TurnedOff => ("is turned off", "is turned off"),
            AgentState::Down => (
                "is out of plan usage or down",
                "is out of plan usage or down",
            ),
        };
        let why = if agent.endpoint { endpoint } else { cli };
        Err(format!("{} {why}, so {fallback}", agent.name))
    }

    /// The role's workers for the agents named, with the role's effort when one was named.
    fn role_effect(&self, named: &Named, role: RoleName, one: &str, many: &str) -> Effect {
        let workers: Vec<ModelRef> = named
            .agents
            .iter()
            .map(|agent| agent.worker.clone())
            .collect();
        let mut settings = vec![(
            format!("roles.{}.models", role.as_str()),
            toml_list(&workers),
        )];
        if let Some(effort) = named.agents.iter().find_map(|agent| agent.effort.clone()) {
            settings.push((
                format!("roles.{}.effort", role.as_str()),
                toml_text(&effort),
            ));
        }
        let said = match named.agents.as_slice() {
            [agent] => format!("{} {one}{}", agent.name, agent.with()),
            several => {
                let names: Vec<String> = several.iter().map(|agent| agent.name.clone()).collect();
                format!("{} {many}", joined(&names))
            }
        };
        Effect {
            said,
            note: named.note.clone(),
            names: workers.iter().map(|worker| worker.agent.clone()).collect(),
            settings,
            ..Effect::default()
        }
    }

    /// Reviewers come from another provider than the workers unless you asked for one of the
    /// same, and then your words win and the line says so. Reviewers you named review every
    /// task, so in a crew they build none while another agent can.
    fn review_against_workers(&self, entries: &mut [Entry], mode: Option<RunMode>) {
        let followed: Vec<&Effect> = entries
            .iter()
            .filter_map(|entry| entry.result.as_ref().ok())
            .collect();
        let workers_asked: Vec<ModelRef> = followed
            .iter()
            .flat_map(|effect| effect.workers.clone())
            .collect();
        let main_agent = followed
            .iter()
            .find_map(|effect| {
                effect
                    .main_agent
                    .clone()
                    .or(effect.captain_main_agent.clone())
            })
            .unwrap_or_else(|| self.settings.crew.main.clone());
        for entry in entries.iter_mut() {
            let Ok(effect) = &mut entry.result else {
                continue;
            };
            if effect.reviewers.is_empty() {
                continue;
            }
            let reviewers = effect.reviewers.clone();
            let reviews = |model: &ModelRef| reviewers.iter().any(|r| r.agent == model.agent);
            let mut workers = workers_asked.clone();
            if workers_asked.is_empty() {
                // Captain's worker is its main agent. Until the rules pick the mode, the crew's
                // workers are the ones a provider rule would refuse.
                if mode == Some(RunMode::Captain) {
                    workers.push(main_agent.clone());
                }
                if mode != Some(RunMode::Captain) {
                    let base = self
                        .settings
                        .roles
                        .get(&RoleName::Worker)
                        .map(|role| role.models.clone())
                        .filter(|models| !models.is_empty())
                        .unwrap_or_else(|| self.crew_and_main_agent());
                    let others: Vec<ModelRef> = base
                        .iter()
                        .filter(|model| !reviews(model))
                        .cloned()
                        .collect();
                    if !others.is_empty() && others.len() != base.len() {
                        effect
                            .inferred
                            .push(("roles.worker.models".to_owned(), toml_list(&others)));
                        workers.extend(others);
                    } else {
                        workers.extend(base);
                    }
                }
            }
            let provider = |model: &ModelRef| {
                self.roster
                    .agent(&model.agent)
                    .map(|agent| agent.provider.clone())
                    .unwrap_or_default()
            };
            let same = reviewers.iter().any(|reviewer| {
                let theirs = provider(reviewer);
                !theirs.is_empty() && workers.iter().any(|worker| provider(worker) == theirs)
            });
            if same && self.settings.review.other_provider {
                effect
                    .settings
                    .push(("review.other_provider".to_owned(), "false".to_owned()));
                let own = if reviewers.len() == 1 { "its" } else { "their" };
                effect.said = if effect.said.contains(" with ") || effect.said.contains(" on ") {
                    format!(
                        "{}, reviewing {own} own provider's work, as you asked",
                        effect.said
                    )
                } else {
                    format!("{} {own} own provider's work, as you asked", effect.said)
                };
            }
        }
    }
}

impl Checker<'_> {
    /// "Only Claude": a crew of one provider has nobody from another to review its work, so
    /// for this request its reviews stay within that provider, as naming such a reviewer
    /// does, on another model of the builder's CLI, and the crew's words say so once, naming
    /// both models. The workers' model is named too, so whatever plans the work can't hand
    /// them the reviewer's: the main agent's when it runs on that CLI, else the CLI's own
    /// default. With "keep it cheap" the two cheapest models share the work: the dearer one
    /// builds and the cheaper one reviews. A crew of two providers keeps the rule.
    fn within_one_provider(&self, entries: &mut [Entry]) {
        if !self.settings.review.other_provider {
            return;
        }
        let followed: Vec<&Effect> = entries
            .iter()
            .filter_map(|entry| entry.result.as_ref().ok())
            .collect();
        // Reviewers you named settle the rule against the workers already.
        if followed.iter().any(|effect| !effect.reviewers.is_empty()) {
            return;
        }
        let cheap = followed
            .iter()
            .any(|effect| effect.slot.as_deref() == Some(CHEAPEST));
        let builders: Vec<ModelRef> = followed
            .iter()
            .flat_map(|effect| effect.workers.clone())
            .collect();
        let asked: Vec<(String, String)> = followed
            .iter()
            .flat_map(|effect| effect.settings.clone())
            .collect();
        let main = followed
            .iter()
            .rev()
            .find_map(|effect| effect.main_agent.clone())
            .unwrap_or_else(|| self.settings.crew.main.clone());
        let Some(entry) = entries.iter_mut().rev().find(|entry| {
            entry
                .result
                .as_ref()
                .is_ok_and(|effect| !effect.crew.is_empty())
        }) else {
            return;
        };
        let (kind, who, says) = (
            entry.instruction.kind,
            entry.instruction.who.clone(),
            entry.instruction.says.clone(),
        );
        let Ok(effect) = &mut entry.result else {
            return;
        };
        let provider = |worker: &ModelRef| {
            self.roster
                .agent(&worker.agent)
                .map(|agent| agent.provider.clone())
                .unwrap_or_default()
        };
        let theirs = provider(&effect.crew[0]);
        if !is_known(&theirs) || effect.crew.iter().any(|worker| provider(worker) != theirs) {
            return;
        }
        let builder = builders.first().cloned().or_else(|| {
            effect
                .crew
                .iter()
                .find(|worker| {
                    self.roster
                        .agent(&worker.agent)
                        .is_some_and(|a| !a.endpoint)
                })
                .cloned()
        });
        let Some((agent, builder)) =
            builder.and_then(|builder| Some((self.roster.agent(&builder.agent)?, builder)))
        else {
            return;
        };
        // Workers you named run on what that CLI runs; the workers this names run on the main
        // agent's model when it shares their CLI.
        let mains = (builders.is_empty() && main.agent == agent.id)
            .then(|| main.model.clone())
            .flatten();
        let runs = builder
            .model
            .clone()
            .or_else(|| asked_text(&asked, &format!("agents.{}.model", agent.id)))
            .or(mains)
            .or_else(|| {
                self.settings
                    .agents
                    .get(&agent.id)
                    .and_then(|settings| settings.model.clone())
            })
            .or_else(|| agent.default_model.clone());
        let Some((builds, reviews)) = review_pair(agent, runs.as_deref(), cheap) else {
            effect.note = Some(join_notes(
                effect.note.take(),
                &format!(
                    "{} has no second model here to review on, so its work waits for a \
                     reviewer from another provider",
                    agent.name
                ),
            ));
            return;
        };
        effect
            .inferred
            .push(("review.other_provider".to_owned(), "false".to_owned()));
        let on = |model: &str| ModelRef::new(agent.id.clone(), Some(model.to_owned()));
        effect.inferred.push((
            "roles.reviewer.models".to_owned(),
            toml_list(&[on(&reviews)]),
        ));
        // Workers you named keep the model they run on, which is named only when it is known.
        let building = if builders.is_empty() {
            effect
                .inferred
                .push(("roles.worker.models".to_owned(), toml_list(&[on(&builds)])));
            Some(builds)
        } else {
            runs.map(|model| known_model(agent, &model).unwrap_or(model))
        };
        let within = match kind {
            InstructionKind::Crew => as_written(&says, &who).unwrap_or_else(|| agent.name.clone()),
            _ => agent.name.clone(),
        };
        let reviewing = display_model(agent, &reviews);
        let models = match building {
            Some(builds) => format!(
                "{} builds, {reviewing} reviews",
                display_model(agent, &builds)
            ),
            None => format!("{reviewing} reviews"),
        };
        effect.said = format!(
            "{}, reviews stay within {within} ({models}), as you asked",
            effect.said
        );
    }
}

/// Who builds and who reviews on `agent`'s models when reviews stay within its provider, each
/// as the CLI names it. The builder keeps the model it `runs` on and the reviewer takes the next
/// one down, or up when the builder's is the cheapest; with `cheap` the dearer of the two
/// cheapest builds. `None` when the CLI has fewer than two priced models.
fn review_pair(agent: &Agent, runs: Option<&str>, cheap: bool) -> Option<(String, String)> {
    let ladder = &agent.by_price;
    if ladder.len() < 2 {
        return None;
    }
    if cheap {
        return Some((ladder[1].clone(), ladder[0].clone()));
    }
    let at = runs.and_then(|model| {
        ladder
            .iter()
            .position(|listed| same_model(agent, listed, model))
    });
    Some(match (at, runs) {
        (Some(0), _) => (ladder[0].clone(), ladder[1].clone()),
        (Some(at), _) => (ladder[at].clone(), ladder[at - 1].clone()),
        // A model nothing prices keeps building, and the cheapest reviews.
        (None, Some(model)) => (
            known_model(agent, model).unwrap_or_else(|| model.to_owned()),
            ladder[0].clone(),
        ),
        // Nothing says what the CLI runs: the two cheapest share the work.
        (None, None) => (ladder[1].clone(), ladder[0].clone()),
    })
}

/// Whether two of `agent`'s names for models, an id or an alias, are one model.
fn same_model(agent: &Agent, a: &str, b: &str) -> bool {
    if key(a) == key(b) {
        return true;
    }
    agent.models.iter().any(|model| {
        let names = |name: &str| {
            key(&model.id) == key(name) || model.aliases.iter().any(|alias| key(alias) == key(name))
        };
        names(a) && names(b)
    })
}

/// A TOML text value the request layer holds for `key`, without its quotes.
fn asked_text(settings: &[(String, String)], key: &str) -> Option<String> {
    let (_, value) = settings.iter().rev().find(|(known, _)| known == key)?;
    format!("value = {value}")
        .parse::<toml::Table>()
        .ok()?
        .remove("value")?
        .as_str()
        .map(str::to_owned)
}

/// `who` as your words spell it, such as `Claude` for `claude`.
fn as_written(says: &str, who: &str) -> Option<String> {
    let wanted = key(who);
    says.split(|c: char| !c.is_alphanumeric() && c != '-')
        .find(|word| !word.is_empty() && key(word) == wanted)
        .map(str::to_owned)
}

/// The slot the cheapest models fill, which a model named for one agent leaves that agent's.
const CHEAPEST: &str = "model:*";

/// Words that ask to spend less on models, and words that turn it around.
const CHEAP: [&str; 9] = [
    "cheap",
    "cheaper",
    "cheapest",
    "cheaply",
    "inexpensive",
    "low cost",
    "lower cost",
    "save money",
    "spend less",
];
const DEAR: [&str; 3] = ["expensive", "best models", "whatever it costs"];

struct Named {
    agents: Vec<NamedAgent>,
    /// What of the instruction couldn't be followed.
    note: Option<String>,
}

struct NamedAgent {
    worker: ModelRef,
    name: String,
    model_name: Option<String>,
    effort: Option<String>,
    endpoint: bool,
}

impl NamedAgent {
    /// ` with Opus 5.5, high effort`, or what of it was named.
    fn with(&self) -> String {
        match (&self.model_name, &self.effort) {
            (Some(model), Some(effort)) => format!(" with {model}, {effort} effort"),
            (Some(model), None) => format!(" with {model}"),
            (None, Some(effort)) => format!(" on {effort} effort"),
            (None, None) => String::new(),
        }
    }
}

/// "Have the workers use codex's own goal for each task": who keeps each task's goal going,
/// for the workers unless your words name the main agent. `None` when the words set a
/// condition instead.
fn role_goal(instruction: &Instruction) -> Option<Effect> {
    let kept_by = key(&instruction.value);
    let said = match kept_by.as_str() {
        "cli" => "work each task as a goal, kept by the CLI's own goal where it has one",
        "argonauts" => "work each task as a goal, kept by Argonauts Code",
        "off" => "don't work their tasks as goals",
        _ => return None,
    };
    let main = key(&instruction.who).contains("main");
    let (role, who) = if main {
        (RoleName::Main, "the main agent")
    } else {
        (RoleName::Worker, "the workers")
    };
    Some(Effect {
        said: format!("{who} {said}"),
        slot: Some("role goal".to_owned()),
        settings: vec![(format!("roles.{}.goal", role.as_str()), toml_text(&kept_by))],
        // Workers are the crew's; the main agent alone has a goal in Captain too.
        implies: (!main).then_some(RunMode::Crew),
        ..Effect::default()
    })
}

/// The approvals that wait for you when a request asks to approve things itself.
const APPROVALS: [&str; 6] = [
    "understand",
    "plan",
    "merge",
    "stuck",
    "limit",
    "change_request",
];

/// Why an instruction whose words ask for the opposite of what it was sorted as isn't followed,
/// such as "don't do this alone" sorted as Captain.
const SAYS_OTHERWISE: &str = "its words say otherwise";

/// Whether the words keep the work to what they name, as "use only Claude" and "Claude only" do.
fn says_only(says: &str) -> bool {
    says.split(|ch: char| !ch.is_alphanumeric())
        .any(|word| word.eq_ignore_ascii_case("only"))
}

const CAPTAIN_CUES: [&str; 8] = [
    "captain",
    "yourself",
    "alone",
    "on your own",
    "just you",
    "single agent",
    "one agent",
    "by itself",
];

/// Captain's cues, with the other words people type for it.
fn captain_cues() -> Vec<&'static str> {
    [&CAPTAIN_CUES[..], &request_words::FOR_CAPTAIN].concat()
}

const CREW_CUES: [&str; 9] = [
    "crew",
    "team",
    "agents",
    "everyone",
    "ultracode",
    "together",
    "other agents",
    "several agents",
    "multiple agents",
];

/// Words for a role rather than for who fills it, as in "the reviewer".
const ROLE_WORDS: [&str; 19] = [
    "the",
    "a",
    "an",
    "as",
    "reviewer",
    "reviewers",
    "review",
    "judge",
    "judges",
    "orchestrator",
    "builder",
    "builders",
    "worker",
    "workers",
    "planner",
    "planners",
    "checker",
    "checkers",
    "maker",
];

/// Whether `who` holds a role, or nothing, where an agent belongs.
fn names_a_role(who: &str) -> bool {
    tokens(who).iter().all(|word| {
        let word = word.as_str();
        ROLE_WORDS.contains(&word) || request_words::FOR_ROLES.contains(&word)
    })
}

/// "Use a workflow" asks for the crew as "use the crew" does, and "don't use a workflow" keeps
/// the work as Captain. They count only in a clause whose words say neither, so "the captain
/// workflow" stays as Captain.
const WORKFLOW_CUES: [&str; 2] = ["workflow", "workflows"];

/// Verbs after which "yourself" hands a choice over rather than asking for Captain work, as in
/// "choose everything else yourself".
const CHOOSING: [&str; 5] = ["choose", "decide", "pick", "select", "figure"];

/// How `says` stands on the crew, clause by clause.
fn crew_stance(says: &str) -> Stance {
    let (crew, captain) = leanings(says);
    side(crew, captain)
}

/// How `says` stands on Captain, where "don't use a workflow" is for it.
fn captain_stance(says: &str) -> Stance {
    let (crew, captain) = leanings(says);
    side(captain, crew)
}

fn side(toward: bool, away: bool) -> Stance {
    match (toward, away) {
        (true, false) => Stance::For,
        (_, true) => Stance::Against,
        (false, false) => Stance::Silent,
    }
}

/// Whether any clause of `says` leans to the crew, and whether any leans to Captain. Each clause
/// stands on its own: in "use a workflow, with Codex as the reviewer, and choose everything
/// else yourself" the first asks for the crew, and the last leaves the rest to it.
fn leanings(says: &str) -> (bool, bool) {
    let (mut crew, mut captain) = (false, false);
    for clause in says.split([',', ';', ':', '.', '!', '?', '(', ')', '\n']) {
        let clause = without_handed_over_choices(clause);
        let leaning = match stance(&clause, &CREW_CUES, &captain_cues()) {
            Stance::Silent => stance(&clause, &WORKFLOW_CUES, &[]),
            said => said,
        };
        match leaning {
            Stance::For => crew = true,
            Stance::Against => captain = true,
            Stance::Silent => {}
        }
    }
    (crew, captain)
}

/// The clause's words without a "yourself" that follows a verb of choosing, so "choose the
/// rest yourself" says nothing about Captain while "do it yourself" still does.
fn without_handed_over_choices(clause: &str) -> String {
    let words = tokens(clause);
    let mut kept: Vec<&str> = Vec::new();
    for (at, word) in words.iter().enumerate() {
        let handed_over = word == "yourself"
            && words[..at]
                .iter()
                .any(|before| CHOOSING.contains(&before.as_str()));
        if !handed_over {
            kept.push(word);
        }
    }
    kept.join(" ")
}

const WORKTREE_CUES: [&str; 8] = [
    "checked",
    "worktree",
    "branch",
    "isolated",
    "isolation",
    "separately",
    "separate",
    "sandboxed",
];

/// A worktree's cues, with the other words people type for one.
fn worktree_cues() -> Vec<&'static str> {
    [&WORKTREE_CUES[..], &request_words::FOR_A_WORKTREE].concat()
}

const CHECKOUT_CUES: [&str; 10] = [
    "directly",
    "direct",
    "in place",
    "my files",
    "my checkout",
    "my folder",
    "my repo",
    "my working tree",
    "my tree",
    "right here",
];

/// Captain or the crew, as `value` names it and the words say it. Without a value, the words
/// alone.
fn mode(instruction: &Instruction) -> Result<Effect, String> {
    let says = &instruction.says;
    let captain = || captain_stance(says);
    let crew = || crew_stance(says);
    let mode = match key(&instruction.value).as_str() {
        "captain" if captain() == Stance::For => RunMode::Captain,
        "crew" if crew() == Stance::For => RunMode::Crew,
        "captain" | "crew" => return Err(SAYS_OTHERWISE.to_owned()),
        _ => match (captain(), crew()) {
            (Stance::For, _) => RunMode::Captain,
            (_, Stance::For) => RunMode::Crew,
            _ => return Err("it says neither Captain nor the crew".to_owned()),
        },
    };
    Ok(Effect {
        mode: Some(mode),
        slot: Some("mode".to_owned()),
        ..Effect::default()
    })
}

/// The crew role an instruction of this kind gives to the agents it names.
fn named_role(kind: InstructionKind) -> Option<CrewRole> {
    match kind {
        InstructionKind::Orchestrate => Some(CrewRole::Orchestrator),
        InstructionKind::Build => Some(CrewRole::Worker),
        InstructionKind::Review => Some(CrewRole::Reviewer),
        InstructionKind::Judge => Some(CrewRole::Judge),
        _ => None,
    }
}

fn join_notes(before: Option<String>, note: &str) -> String {
    match before {
        Some(before) => format!("{before}; {note}"),
        None => note.to_owned(),
    }
}

/// Two followed instructions that disagree: the later one in your message wins.
fn settle_disagreements(entries: &mut [Entry]) {
    for later in 1..entries.len() {
        let (before, rest) = entries.split_at_mut(later);
        let current = &rest[0];
        let Ok(effect) = &current.result else {
            continue;
        };
        let slot = effect.slot.clone();
        let names = effect.names.clone();
        let leaves_out = effect.leaves_out.clone();
        let says = current.instruction.says.trim().to_owned();
        for earlier in before.iter_mut() {
            let Ok(old) = &earlier.result else { continue };
            let same_slot = slot.is_some() && old.slot == slot;
            let crossed = names.iter().any(|name| old.leaves_out.contains(name))
                || leaves_out.iter().any(|name| old.names.contains(name));
            if same_slot || crossed {
                earlier.result = Err(format!("you also said {says}"));
            }
        }
    }
}

/// A review in Captain needs a worktree, so it wins over words asking for your own checkout.
fn needs_a_worktree(entries: &mut [Entry]) {
    let review = entries.iter().find_map(|entry| {
        let effect = entry.result.as_ref().ok()?;
        (!effect.reviewers.is_empty()).then(|| entry.instruction.says.trim().to_owned())
    });
    let Some(review) = review else { return };
    for entry in entries.iter_mut() {
        if let Ok(effect) = &entry.result
            && effect.captain == Some(CaptainKind::Direct)
        {
            entry.result = Err(format!(
                "you also said {review}, and a review needs a worktree"
            ));
        }
    }
}

/// The mode your words ask for. A command wins over them, and an instruction that only a crew
/// can follow loses to Captain you asked for.
fn settle_mode(entries: &mut [Entry], command: Option<&Command>) -> Option<RunMode> {
    let asked = entries.iter().rev().find_map(|entry| {
        Some((
            entry.result.as_ref().ok()?.mode?,
            entry.instruction.says.clone(),
        ))
    });
    if let Some(command) = command {
        for entry in entries.iter_mut() {
            let Ok(effect) = &entry.result else { continue };
            let against = effect.mode.is_some_and(|mode| mode != command.mode)
                || (command.mode == RunMode::Captain && effect.implies == Some(RunMode::Crew));
            if against {
                entry.result = Err(command.said.clone());
            }
        }
        return asked
            .filter(|(mode, _)| *mode == command.mode)
            .map(|(mode, _)| mode);
    }
    if let Some((RunMode::Captain, says)) = &asked {
        for entry in entries.iter_mut() {
            let Ok(effect) = &entry.result else { continue };
            if effect.implies == Some(RunMode::Crew) && effect.mode.is_none() {
                entry.result = Err(format!("you also said {}", says.trim()));
            }
        }
    }
    let implied = |mode: RunMode| {
        entries
            .iter()
            .filter_map(|entry| entry.result.as_ref().ok())
            .any(|effect| effect.implies == Some(mode))
    };
    asked.map(|(mode, _)| mode).or_else(|| {
        if implied(RunMode::Crew) {
            Some(RunMode::Crew)
        } else if implied(RunMode::Captain) {
            Some(RunMode::Captain)
        } else {
            None
        }
    })
}

/// Free text the main agent gets as a must. It never touches a safety setting, as the main agent
/// can't change one, and on its own it doesn't make the run a crew's.
fn for_the_main_agent(instruction: &Instruction) -> Result<Effect, String> {
    let says = instruction.says.trim();
    let words = tokens(says);
    let has = |cues: &[&str]| words.iter().any(|word| cues.contains(&word.as_str()));
    let loosens = has(&LOOSENING);
    let why = if has(&["sandbox", "sandboxed", "sandboxing"]) {
        Some("the sandbox is a safety setting, /permissions changes it")
    } else if words.iter().any(|word| word.starts_with("approv"))
        || has(&[
            "merge",
            "merging",
            "permission",
            "permissions",
            "ask",
            "asking",
        ])
    {
        Some("approvals are safety settings, /approvals and Shift+Tab change them")
    } else if has(&["test", "tests", "check", "checks"]) && loosens {
        Some("checks.run is a safety setting, /config changes the tests a change must pass")
    } else if has(&[
        "key",
        "keys",
        "auth",
        "login",
        "credentials",
        "token",
        "tokens",
    ]) {
        Some("auth is a safety setting, /config changes it")
    } else if has(&["limit", "spend", "cost", "dollars", "usd"]) || says.contains('$') {
        Some("the limit is a safety setting, /limits changes it")
    } else if has(&["network", "internet", "online", "offline"]) {
        Some("sandbox.network is a safety setting, /permissions changes it")
    } else {
        None
    };
    match why {
        Some(why) => Err(why.to_owned()),
        None => Ok(Effect {
            said: format!("For the main agent: {says}"),
            ..Effect::default()
        }),
    }
}

/// Words for one role's agents, which travel inside their prompt for this run only. They pass
/// the same check as words for the main agent, since a role can't change a safety setting
/// either.
fn for_a_role(instruction: &Instruction) -> Result<Effect, String> {
    let role =
        argonauts_core::prompts::prompt_parts::role_named(&instruction.who).ok_or_else(|| {
            "it doesn't say which role: the main agent, the orchestrator, the workers, the \
         reviewers or the judge"
                .to_owned()
        })?;
    let words = instruction.value.trim();
    if words.is_empty() {
        return Err("it doesn't say what to tell them".to_owned());
    }
    for_the_main_agent(instruction)?;
    let key = argonauts_core::settings::prompts::PromptSettings::request_key(role);
    Ok(Effect {
        said: argonauts_core::prompts::prompt_parts::for_role_line(role, words),
        slot: Some(key.clone()),
        settings: vec![(key, toml::Value::String(words.to_owned()).to_string())],
        ..Effect::default()
    })
}

fn current_values(settings: &Settings) -> Option<toml::Value> {
    toml::Value::try_from(settings).ok()
}

fn value_at<'a>(table: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    key.split('.')
        .try_fold(table, |value, segment| value.as_table()?.get(segment))
}

/// Whether this version has a setting, even one that's unset now.
fn has_key(settings: &Settings, pointer: &str) -> bool {
    serde_json::to_value(settings)
        .ok()
        .is_some_and(|value| value.pointer(pointer).is_some())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod asked_for_a_workflow_tests {
    use super::plainly_asks_for_a_workflow;

    #[test]
    fn a_reviewer_is_named_only_where_an_agent_is_given_the_review() {
        let roster = super::tests::roster();
        for named in [
            "Fix the money rounding with Codex as the reviewer.",
            "Add due dates, and have Opus review it before it lands",
            "Rename the loader. The reviewer is codex.",
            "fix the flaky test, reviewed by Claude Code",
            "Use haiku as reviewer for this one: tidy the imports",
        ] {
            assert!(super::names_a_reviewer(named, &roster), "{named}");
        }
        for not_named in [
            "Review the error handling in parse()",
            "review the Codex adapter's retry code",
            "Fix the rounding. Don't have Codex review it.",
            "What does the reviewer do?",
            "Add a reviewer field to the Codex report",
            "Fix the money rounding",
        ] {
            assert!(!super::names_a_reviewer(not_named, &roster), "{not_named}");
        }
    }

    #[test]
    fn only_words_that_hand_the_work_on_ask_for_a_workflow() {
        for asked in [
            "Use a workflow for this, with Codex as the reviewer, and choose everything else \
             yourself. Fix the money rounding.",
            "Fix the export bug, and use the crew.",
            "Start a workflow to add due dates",
        ] {
            assert!(plainly_asks_for_a_workflow(asked), "{asked}");
        }
        for not_asked in [
            "Explain how the workflow works.",
            "Fix it, but don't use a workflow.",
            "Fix the money rounding by yourself.",
            "Use a workflow and do it yourself.",
            "What does the crew do?",
        ] {
            assert!(!plainly_asks_for_a_workflow(not_asked), "{not_asked}");
        }
    }
}
