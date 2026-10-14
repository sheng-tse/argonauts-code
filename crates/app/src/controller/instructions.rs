//! What a request says about how to run it. The estimate lists each instruction with the
//! request's own words; here code resolves the names, checks each one in a fixed order, and
//! turns the ones that pass into settings at the request layer, for that run only. Nothing a
//! model wrote decides anything here: it only sorted and copied.
//!
//! Commands beat words, and words beat every settings layer. A request never loosens a safety
//! setting; it may tighten five things, by the rules in `argonauts_registry::tightens`.

use argonauts_core::api::StartRun;
use argonauts_core::events::run::{CheckedInstruction, InstructionOutcome, RunMode};
use argonauts_core::goal::{CheckMeans, GoalFrom, GoalSpec};
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::registry::company_of;
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::budget::BudgetUnit;
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::settings::protocol::SoloStyle;
use argonauts_core::settings::workflows::RunStyle;
use argonauts_core::steps::intake::{Instruction, InstructionKind, InstructionScope};
use argonauts_crew::triage::Asked;

use crate::controller::turn::DirectWords;

mod roster;
mod words;

pub(crate) use roster::{Agent, AgentState, Roster};
use roster::{clamp, display_model, model_named};
use words::{
    Stance, dollars_in, effort_level, is_generic, joined, key, money, names_in, number_in,
    plainly_asks_you, quoted_at, stance, tokens, toml_list, toml_text, turned_around,
    turns_network_off, unknown, words,
};

/// Verbs that hand work to a workflow or the crew when one follows them, as in "use a workflow".
const HANDING_ON: [&str; 6] = ["use", "run", "start", "launch", "hand", "give"];

/// Whether `message` plainly asks for a workflow or the crew: a clause where one of
/// [`HANDING_ON`] comes just before the word, with no negation before it, and nothing in the
/// message asking for solo. "Explain how the workflow works" asks for nothing.
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
                let negated = before
                    .iter()
                    .any(|word| ["dont", "not", "no", "never", "without"].contains(&word.as_str()));
                cue && handed && !negated
            })
        })
}

/// Whether `says` stands in `message` word for word, two words at least, the way an
/// instruction's words are checked against your message.
pub(crate) fn said_in(message: &str, says: &str) -> bool {
    quoted_at(&tokens(message), &tokens(says)).is_some()
}

/// What a command or a card chose, which beats the words.
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
    /// The mode and solo style your words chose, for triage.
    pub asked: Asked,
    /// `(key, TOML value)` at the request layer.
    pub settings: Vec<(String, String)>,
    /// Who leads this run, when your words named someone.
    pub lead: Option<WorkerRef>,
    /// Who works solo, when your words named one builder and the run turns out solo.
    pub solo_lead: Option<WorkerRef>,
    /// Your words asked approvals to wait for you.
    pub supervised: bool,
    /// What "from now on" asked to keep, as `(key, TOML value)`, and as the line said it.
    pub keep: Vec<(String, String)>,
    pub keep_said: Vec<String>,
}

impl Applied {
    /// Your words, applied to the run they start: its instructions for the record and the
    /// Triage line, the request layer, the lead, and the approval mode.
    pub(crate) fn apply_to(&self, request: &mut StartRun) {
        request.instructions = self.instructions.clone();
        request.request_settings = self
            .settings
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        let lead = match (&self.lead, &self.solo_lead) {
            (Some(lead), _) => Some(lead),
            (None, Some(lead)) if request.mode == RunMode::Solo => Some(lead),
            _ => None,
        };
        if let Some(lead) = lead {
            request.main_agent = Some(lead.clone());
            // The run's own default agent too, since that's who designs and leads it.
            request
                .request_settings
                .push(format!("main_agent={}", toml_text(&lead.to_string())));
        }
        if self.supervised {
            request.approval_mode = Some(ApprovalMode::Supervised);
        }
    }

    /// The workers your words gave `role`, as the request layer holds them.
    pub(crate) fn workers(&self, role: RoleName) -> Vec<WorkerRef> {
        let key = format!("roles.{}.workers", role.as_str());
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
    /// budget, no network, and asking you first. The rest needs a run, which triage already
    /// chose when it applied.
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
            budget_usd: value("limits.spend_per_run").and_then(|value| {
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
/// names an agent or one of its models, or speaks of the crew, roles, a copy, the budget,
/// approvals, merging, the network, effort, or "from now on". A false yes costs a read-only
/// turn, and a false no runs the command as it always did.
pub(crate) fn might_say_how(message: &str, roster: &Roster) -> bool {
    let said = tokens(message);
    let has = |word: &str| said.iter().any(|said| said == word);
    let names = roster.agents.iter().any(|agent| {
        let id = tokens(&agent.id);
        let name = tokens(&agent.name);
        let models = agent
            .models
            .iter()
            .flat_map(|model| model.aliases.iter().chain([&model.name, &model.id]));
        let company = tokens(&company_of(&agent.family));
        [
            id.first(),
            name.first(),
            tokens(&agent.family).first(),
            company.first(),
        ]
        .into_iter()
        .flatten()
        .any(|word| has(word))
            || models
                .flat_map(|model| tokens(model))
                .any(|word| word.len() > 2 && word.chars().any(char::is_alphabetic) && has(&word))
    });
    names
        || HOW_CUES.iter().any(|cue| has(cue))
        || message.contains('$')
        || quoted_at(&said, &tokens("from now on")).is_some()
}

/// Words that may say how to run a request. A role's verb alone, such as "the build runs",
/// says nothing of who fills it: the agent's name does.
const HOW_CUES: [&str; 32] = [
    // "keep going until its tests pass" sets a goal.
    "until",
    "crew",
    "team",
    "workflow",
    "agents",
    "agy",
    "gemini",
    "solo",
    "yourself",
    "alone",
    "drafters",
    "copy",
    "checked",
    "worktree",
    "isolated",
    "budget",
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
    "cheapest",
    "coordinator",
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
    checker.review_against_owners(&mut entries, mode);
    needs_a_copy(&mut entries);

    let mut applied = Applied {
        asked: Asked { mode, solo: None },
        ..Applied::default()
    };
    for entry in &entries {
        let Ok(effect) = &entry.result else { continue };
        for (key, value) in effect.settings.iter().chain(&effect.inferred) {
            applied.settings.retain(|(known, _)| known != key);
            applied.settings.push((key.clone(), value.clone()));
        }
        if effect.lead.is_some() {
            applied.lead = effect.lead.clone();
        }
        if effect.solo_lead.is_some() {
            applied.solo_lead = effect.solo_lead.clone();
        }
        if effect.solo.is_some() && applied.asked.solo != Some(SoloStyle::Checked) {
            applied.asked.solo = effect.solo;
        }
        applied.supervised |= effect.supervised;
        if entry.instruction.scope == InstructionScope::FromNowOn {
            applied.keep.extend(effect.settings.iter().cloned());
            if let Some(lead) = &effect.lead {
                applied
                    .keep
                    .push(("main_agent".to_owned(), toml_text(&lead.to_string())));
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
    /// As the Triage line says it.
    said: String,
    note: Option<String>,
    /// Two instructions with the same slot disagree, and the later one wins.
    slot: Option<String>,
    /// Agents it names, so leaving one out and giving it a role disagree.
    names: Vec<String>,
    /// Agents it leaves out.
    leaves_out: Vec<String>,
    settings: Vec<(String, String)>,
    lead: Option<WorkerRef>,
    solo_lead: Option<WorkerRef>,
    /// The mode it asks for in so many words.
    mode: Option<RunMode>,
    /// The mode it needs: roles that only a crew has, or a checked copy that only solo has.
    implies: Option<RunMode>,
    solo: Option<SoloStyle>,
    supervised: bool,
    /// Settings it needs that the words didn't name, such as owners other than the reviewers.
    /// They apply to the run, and "from now on" doesn't keep them.
    inferred: Vec<(String, String)>,
    /// Who it gives the reviewer role to, for the family check against the owners.
    reviewers: Vec<WorkerRef>,
    /// Who it gives the owner role to.
    owners: Vec<WorkerRef>,
    /// Who "all the other agents" names. Which seats they fill is settled once every
    /// instruction is read, since a role another one names keeps that one.
    others: Vec<WorkerRef>,
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
            Kind::Lead
                | Kind::Coordinate
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
            Kind::Lead | Kind::Coordinate => self.lead(instruction),
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
            Kind::Approvals => self.approvals(instruction),
            Kind::Merge => self.merge(instruction),
            Kind::Budget => self.budget(instruction),
            Kind::Network => self.network(instruction),
            Kind::Gates => Err(
                "checks.run is a safety setting, /config changes the tests a change must pass"
                    .to_owned(),
            ),
            Kind::Sandbox => {
                Err("the sandbox is a safety setting, /permissions changes it".to_owned())
            }
            Kind::ForTheLead => for_the_lead(instruction),
            Kind::ForARole => for_a_role(instruction),
        }
    }

    /// "Keep going until its tests pass": the request's own words set a goal for this request,
    /// which the Triage line states back before work starts.
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

    /// A checked copy only tightens. Your own checkout is followed only where `solo.style`
    /// already says so, since a request never loosens a safety setting.
    fn isolation(&self, instruction: &Instruction) -> Result<Effect, String> {
        let says = &instruction.says;
        let copy = || stance(says, &COPY_CUES, &CHECKOUT_CUES);
        let checkout = || stance(says, &CHECKOUT_CUES, &COPY_CUES);
        let direct = match key(&instruction.value).as_str() {
            "checked" => match copy() {
                Stance::For => false,
                _ => return Err(SAYS_OTHERWISE.to_owned()),
            },
            "direct" => match checkout() {
                Stance::For => true,
                _ => return Err(SAYS_OTHERWISE.to_owned()),
            },
            _ => match (copy(), checkout()) {
                (Stance::For, _) => false,
                (_, Stance::For) => true,
                _ => return Err("it says neither a checked copy nor your checkout".to_owned()),
            },
        };
        if direct {
            if self.settings.solo.style == SoloStyle::Direct && !self.settings.solo.needs_a_copy() {
                return Ok(Effect {
                    said: "in your checkout".to_owned(),
                    slot: Some("isolation".to_owned()),
                    implies: Some(RunMode::Solo),
                    solo: Some(SoloStyle::Direct),
                    ..Effect::default()
                });
            }
            return Err("solo.kind is a safety setting, /config changes it".to_owned());
        }
        let mut effect = Effect {
            said: "checked in a copy".to_owned(),
            slot: Some("isolation".to_owned()),
            implies: Some(RunMode::Solo),
            solo: Some(SoloStyle::Checked),
            ..Effect::default()
        };
        if self.settings.solo.style != SoloStyle::Checked {
            self.tighten(&mut effect, "solo.kind", toml_text("checked"))?;
        }
        Ok(effect)
    }

    /// `lead` and `coordinate`: the agent designs the run. A coordinator also plans the cards
    /// and assigns their owners and reviewers, as the coordinated crew does.
    fn lead(&self, instruction: &Instruction) -> Result<Effect, String> {
        let coordinates = instruction.kind == InstructionKind::Coordinate;
        let (verb, role) = if coordinates {
            ("coordinates", "coordinate")
        } else {
            ("leads", "lead")
        };
        let fallback = format!(
            "{} leads",
            self.roster.name(&self.settings.main_agent.worker)
        );
        let seats = self.seats(instruction, &fallback)?;
        let [seat] = seats.seats.as_slice() else {
            return Err(format!("only one agent can {role}"));
        };
        if seat.endpoint {
            return Err(format!("an endpoint has no tools to {role} with"));
        }
        let mut effect = Effect {
            said: format!("{} {verb}{}", seat.name, seat.with()),
            note: seats.note.clone(),
            slot: Some("lead".to_owned()),
            names: vec![seat.worker.worker.clone()],
            lead: Some(seat.worker.clone()),
            ..Effect::default()
        };
        if let Some(effort) = &seat.effort {
            let key = format!("agents.{}.effort", seat.worker.worker);
            effect.settings.push((key, toml_text(effort)));
        }
        if coordinates {
            effect.implies = Some(RunMode::Crew);
            let worker = toml_list(std::slice::from_ref(&seat.worker));
            effect.settings.extend([
                ("phases".to_owned(), toml_text("orchestrator")),
                ("drafting.by".to_owned(), toml_text("orchestrator")),
                ("judging.method".to_owned(), toml_text("orchestrator")),
                ("routing.algorithm".to_owned(), toml_text("orchestrator")),
                ("review.assign".to_owned(), toml_text("orchestrator")),
                ("roles.orchestrator.workers".to_owned(), worker),
            ]);
        }
        Ok(effect)
    }

    fn build(&self, instruction: &Instruction) -> Result<Effect, String> {
        let seats = self.seats(instruction, "the crew's workers build")?;
        if seats.seats.iter().any(|seat| seat.endpoint) {
            return Err("an endpoint has no tools to build with".to_owned());
        }
        let workers: Vec<WorkerRef> = seats.seats.iter().map(|seat| seat.worker.clone()).collect();
        let mut effect = self.role_effect(&seats, RoleName::Worker, "builds", "build");
        effect.slot = Some("build".to_owned());
        if let [one] = workers.as_slice() {
            effect.solo_lead = Some(one.clone());
            // Your checkout is the main session's, so another agent builds solo in a copy.
            if one.worker != self.settings.main_agent.worker {
                effect.solo = Some(SoloStyle::Checked);
            }
        } else {
            effect.implies = Some(RunMode::Crew);
        }
        effect.owners = workers;
        Ok(effect)
    }

    /// Reviewers for every card. In solo, a review needs a copy, so it makes solo checked.
    fn review(&self, instruction: &Instruction) -> Result<Effect, String> {
        let seats = self.seats(instruction, "the crew's reviewers review")?;
        let workers: Vec<WorkerRef> = seats.seats.iter().map(|seat| seat.worker.clone()).collect();
        let mut effect = self.role_effect(&seats, RoleName::Reviewer, "reviews", "review");
        effect.slot = Some("review".to_owned());
        effect.settings.extend([
            ("review.per_task".to_owned(), workers.len().to_string()),
            ("solo.review".to_owned(), "true".to_owned()),
        ]);
        effect.solo = Some(SoloStyle::Checked);
        effect.reviewers = workers;
        Ok(effect)
    }

    /// Who decides and signs off. Only a crew has a judge.
    fn judge(&self, instruction: &Instruction) -> Result<Effect, String> {
        let seats = self.seats(instruction, "the crew's judge decides")?;
        let mut effect = self.role_effect(&seats, RoleName::Judge, "judges", "judge");
        effect.slot = Some("judge".to_owned());
        effect.implies = Some(RunMode::Crew);
        Ok(effect)
    }

    /// "All the other agents": the agents named fill every seat but the agent you talk to,
    /// which [`Checker::settle_others`] works out. They build, so none can be an endpoint.
    fn others(&self, instruction: &Instruction) -> Result<Effect, String> {
        let seats = self.seats(instruction, "the crew stays as it is")?;
        if seats.seats.iter().any(|seat| seat.endpoint) {
            return Err("an endpoint has no tools to build with".to_owned());
        }
        let workers: Vec<WorkerRef> = seats.seats.iter().map(|seat| seat.worker.clone()).collect();
        let who = match seats.seats.as_slice() {
            [seat] => format!("{}{}", seat.name, seat.with()),
            many => joined(
                &many
                    .iter()
                    .map(|seat| seat.name.clone())
                    .collect::<Vec<_>>(),
            ),
        };
        Ok(Effect {
            said: format!("{who} for all the other agents"),
            note: seats.note.clone(),
            slot: Some("others".to_owned()),
            names: workers.iter().map(|worker| worker.worker.clone()).collect(),
            implies: Some(RunMode::Crew),
            owners: workers.clone(),
            others: workers,
            ..Effect::default()
        })
    }

    /// The seats "all the other agents" fills: the workers, the reviewers, the judge, and an
    /// orchestrator that isn't the agent you talk to, except a role another of your
    /// instructions names, followed or not. As reviewers they would review their own work as
    /// workers, so while `review.other_provider` is on the reviewers stay as they are.
    fn settle_others(&self, entries: &mut [Entry]) {
        let named: Vec<CrewRole> = entries
            .iter()
            .filter_map(|entry| named_role(entry.instruction.kind))
            .collect();
        let main = &self.settings.main_agent;
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
                CrewRole::Workers,
                CrewRole::Reviewers,
                CrewRole::Judge,
            ] {
                if named.contains(&role) || (role == CrewRole::Orchestrator && orchestrator_is_main)
                {
                    continue;
                }
                if role == CrewRole::Reviewers && self.settings.review.other_provider {
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
                    if CrewRole::followed_by(*fine) == role && !own.workers.is_empty() {
                        effect
                            .settings
                            .push((format!("roles.{}.workers", fine.as_str()), list.clone()));
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
                said: format!("{count} drafters"),
                slot: Some("plan".to_owned()),
                settings: vec![("roles.drafter.count".to_owned(), count.to_string())],
                implies: Some(RunMode::Crew),
                ..Effect::default()
            });
        }
        let seats = self.seats(instruction, "the crew's drafters plan")?;
        let mut effect = self.role_effect(&seats, RoleName::Drafter, "plans", "plan");
        effect.slot = Some("plan".to_owned());
        effect.implies = Some(RunMode::Crew);
        Ok(effect)
    }

    /// Who checks the goal at the end, where this version has a final check to give.
    fn goal_check(&self, instruction: &Instruction) -> Result<Effect, String> {
        if !has_key(self.settings, "/goal/checker") {
            return Err("this version has no final goal check to give".to_owned());
        }
        let seats = self.seats(instruction, "the usual checker checks")?;
        let [seat] = seats.seats.as_slice() else {
            return Err("only one agent can check the goal".to_owned());
        };
        Ok(Effect {
            said: format!("{} checks{}", seat.name, seat.with()),
            note: seats.note.clone(),
            slot: Some("check".to_owned()),
            names: vec![seat.worker.worker.clone()],
            settings: vec![(
                "goal.checker".to_owned(),
                toml_text(&seat.worker.to_string()),
            )],
            ..Effect::default()
        })
    }

    /// Only these agents, the lead among them. When the default agent isn't one, the first
    /// named agent CLI leads.
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
        let seats = self.seats(instruction, "the crew stays as it is")?;
        let workers: Vec<WorkerRef> = seats.seats.iter().map(|seat| seat.worker.clone()).collect();
        let names: Vec<String> = seats.seats.iter().map(|seat| seat.name.clone()).collect();
        let mut effect = Effect {
            said: format!("Crew: {}", joined(&names)),
            note: seats.note.clone(),
            slot: Some("crew".to_owned()),
            names: workers.iter().map(|worker| worker.worker.clone()).collect(),
            settings: vec![("crew".to_owned(), toml_list(&workers))],
            ..Effect::default()
        };
        let default = &self.settings.main_agent.worker;
        if !workers.iter().any(|worker| &worker.worker == default) {
            effect.lead = seats
                .seats
                .iter()
                .find(|seat| !seat.endpoint)
                .map(|seat| seat.worker.clone());
        }
        if workers.len() > 1 {
            effect.implies = Some(RunMode::Crew);
        }
        Ok(effect)
    }

    /// Out of the crew and every role's workers. When it was the default agent, the first agent
    /// CLI left in the crew leads.
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
        let kept = |workers: &[WorkerRef]| -> Vec<WorkerRef> {
            workers
                .iter()
                .filter(|worker| !out.contains(&worker.worker))
                .cloned()
                .collect()
        };
        let crew = kept(&self.settings.crew.members());
        if crew.is_empty() {
            return Err("that leaves no agent in the crew".to_owned());
        }
        let mut settings = vec![("crew".to_owned(), toml_list(&crew))];
        for (role, role_settings) in &self.settings.roles {
            let left = kept(&role_settings.workers);
            if left.len() != role_settings.workers.len() {
                settings.push((format!("roles.{}.workers", role.as_str()), toml_list(&left)));
            }
        }
        let lead = out
            .contains(&self.settings.main_agent.worker)
            .then(|| {
                crew.iter()
                    .find(|worker| {
                        self.roster
                            .agent(&worker.worker)
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
            lead,
            ..Effect::default()
        })
    }

    /// The agent that runs a model uses it in every seat. "Cheapest" puts every agent in the
    /// crew on its cheapest model.
    fn model(&self, instruction: &Instruction) -> Result<Effect, String> {
        let asked = if instruction.model.trim().is_empty() {
            instruction.value.trim()
        } else {
            instruction.model.trim()
        };
        let text = words(asked, &instruction.says);
        if text.contains("cheap") {
            let mut settings = Vec::new();
            for worker in self.crew_and_lead() {
                let Some((model, effort)) = self
                    .roster
                    .agent(&worker.worker)
                    .and_then(|agent| agent.cheapest.clone())
                else {
                    continue;
                };
                settings.push((format!("agents.{}.model", worker.worker), toml_text(&model)));
                if let Some(effort) = effort {
                    settings.push((
                        format!("agents.{}.effort", worker.worker),
                        toml_text(&effort),
                    ));
                }
            }
            return Ok(Effect {
                said: "the cheapest models".to_owned(),
                slot: Some("model:*".to_owned()),
                settings,
                ..Effect::default()
            });
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
            for worker in self.crew_and_lead() {
                let agent = self.roster.agent(&worker.worker);
                let (taken, note) = clamp(&level, agent, worker.model.as_deref());
                notes.extend(note);
                if let Some(taken) = taken {
                    settings.push((
                        format!("agents.{}.effort", worker.worker),
                        toml_text(&taken),
                    ));
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

    /// Only toward asking you: every checkpoint waits for you, and the run starts supervised.
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
        for checkpoint in CHECKPOINTS {
            let key = format!("approvals.{checkpoint}.mode");
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
    fn budget(&self, instruction: &Instruction) -> Result<Effect, String> {
        let raising = "limits.spend_per_run is a safety setting, /limits changes it".to_owned();
        let Some(amount) = dollars_in(&instruction.value).or_else(|| dollars_in(&instruction.says))
        else {
            return Err("it names no amount in dollars".to_owned());
        };
        let now = match self.settings.limits.budget.run_limit() {
            Some(limit) if limit.unit == BudgetUnit::Usd => Some(limit.amount),
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
            slot: Some("budget".to_owned()),
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

    fn crew_and_lead(&self) -> Vec<WorkerRef> {
        let mut workers = self.settings.crew.members();
        if !workers
            .iter()
            .any(|worker| worker.worker == self.settings.main_agent.worker)
        {
            workers.push(self.settings.main_agent.clone());
        }
        workers
    }

    /// The agents `who` names for a role, each on the model and effort the instruction names.
    /// One that can't take part is left out with a note; when none can, it's not followed.
    fn seats(&self, instruction: &Instruction, fallback: &str) -> Result<Seats, String> {
        let mut seats: Vec<Seat> = Vec::new();
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
            if seats.iter().any(|seat| seat.worker.worker == agent.id) {
                continue;
            }
            seats.push(Seat {
                worker: WorkerRef::new(agent.id.clone(), model.clone()),
                name: agent.name.clone(),
                model_name: model.as_deref().map(|model| display_model(agent, model)),
                effort,
                endpoint: agent.endpoint,
            });
            problems.extend(note);
        }
        if seats.is_empty() {
            return Err(problems
                .into_iter()
                .next()
                .unwrap_or_else(|| "it names no agent".to_owned()));
        }
        Ok(Seats {
            seats,
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
        let (harness, endpoint) = match agent.state {
            AgentState::Ready => return Ok(()),
            AgentState::NotInstalled => ("isn't installed", "isn't set up"),
            AgentState::TurnedOff => ("is turned off", "is turned off"),
            AgentState::Down => (
                "is out of plan usage or down",
                "is out of plan usage or down",
            ),
        };
        let why = if agent.endpoint { endpoint } else { harness };
        Err(format!("{} {why}, so {fallback}", agent.name))
    }

    /// The role's workers for the seats, with the role's effort when one was named.
    fn role_effect(&self, seats: &Seats, role: RoleName, one: &str, many: &str) -> Effect {
        let workers: Vec<WorkerRef> = seats.seats.iter().map(|seat| seat.worker.clone()).collect();
        let mut settings = vec![(
            format!("roles.{}.workers", role.as_str()),
            toml_list(&workers),
        )];
        if let Some(effort) = seats.seats.iter().find_map(|seat| seat.effort.clone()) {
            settings.push((
                format!("roles.{}.effort", role.as_str()),
                toml_text(&effort),
            ));
        }
        let said = match seats.seats.as_slice() {
            [seat] => format!("{} {one}{}", seat.name, seat.with()),
            many_seats => {
                let names: Vec<String> = many_seats.iter().map(|seat| seat.name.clone()).collect();
                format!("{} {many}", joined(&names))
            }
        };
        Effect {
            said,
            note: seats.note.clone(),
            names: workers.iter().map(|worker| worker.worker.clone()).collect(),
            settings,
            ..Effect::default()
        }
    }

    /// Reviewers come from another family than the owners unless you asked for one of the
    /// same, and then your words win and the line says so. Reviewers you named review every
    /// card, so in a crew they own none while another agent can.
    fn review_against_owners(&self, entries: &mut [Entry], mode: Option<RunMode>) {
        let followed: Vec<&Effect> = entries
            .iter()
            .filter_map(|entry| entry.result.as_ref().ok())
            .collect();
        let owners_asked: Vec<WorkerRef> = followed
            .iter()
            .flat_map(|effect| effect.owners.clone())
            .collect();
        let lead = followed
            .iter()
            .find_map(|effect| effect.lead.clone().or(effect.solo_lead.clone()))
            .unwrap_or_else(|| self.settings.main_agent.clone());
        for entry in entries.iter_mut() {
            let Ok(effect) = &mut entry.result else {
                continue;
            };
            if effect.reviewers.is_empty() {
                continue;
            }
            let reviewers = effect.reviewers.clone();
            let reviews = |worker: &WorkerRef| reviewers.iter().any(|r| r.worker == worker.worker);
            let mut owners = owners_asked.clone();
            if owners_asked.is_empty() {
                // Solo's owner is its lead. Until triage picks the mode, the crew's owners are
                // the ones a family rule would refuse.
                if mode == Some(RunMode::Solo) {
                    owners.push(lead.clone());
                }
                if mode != Some(RunMode::Solo) {
                    let base = self
                        .settings
                        .roles
                        .get(&RoleName::Worker)
                        .map(|role| role.workers.clone())
                        .filter(|workers| !workers.is_empty())
                        .unwrap_or_else(|| self.crew_and_lead());
                    let others: Vec<WorkerRef> = base
                        .iter()
                        .filter(|worker| !reviews(worker))
                        .cloned()
                        .collect();
                    if !others.is_empty() && others.len() != base.len() {
                        effect
                            .inferred
                            .push(("roles.worker.workers".to_owned(), toml_list(&others)));
                        owners.extend(others);
                    } else {
                        owners.extend(base);
                    }
                }
            }
            let family = |worker: &WorkerRef| {
                self.roster
                    .agent(&worker.worker)
                    .map(|agent| agent.family.clone())
                    .unwrap_or_default()
            };
            let same = reviewers.iter().any(|reviewer| {
                let theirs = family(reviewer);
                !theirs.is_empty() && owners.iter().any(|owner| family(owner) == theirs)
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

struct Seats {
    seats: Vec<Seat>,
    /// What of the instruction couldn't be followed.
    note: Option<String>,
}

struct Seat {
    worker: WorkerRef,
    name: String,
    model_name: Option<String>,
    effort: Option<String>,
    endpoint: bool,
}

impl Seat {
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
        "argonauts" => "work each task as a goal, kept by Argonauts",
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
        // Workers are the crew's; the main agent alone has a goal in solo too.
        implies: (!main).then_some(RunMode::Crew),
        ..Effect::default()
    })
}

/// The checkpoints that wait for you when a request asks to approve things itself.
const CHECKPOINTS: [&str; 6] = ["goal", "plan", "merge", "stuck", "limit", "change_request"];

/// Why an instruction whose words ask for the opposite of what it was sorted as isn't followed,
/// such as "don't do this alone" sorted as solo.
const SAYS_OTHERWISE: &str = "its words say otherwise";

const SOLO_CUES: [&str; 8] = [
    "solo",
    "yourself",
    "alone",
    "on your own",
    "just you",
    "single agent",
    "one agent",
    "by itself",
];

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
const ROLE_WORDS: [&str; 26] = [
    "the",
    "a",
    "an",
    "as",
    "reviewer",
    "reviewers",
    "review",
    "judge",
    "judges",
    "lead",
    "leader",
    "orchestrator",
    "coordinator",
    "builder",
    "builders",
    "worker",
    "workers",
    "planner",
    "planners",
    "checker",
    "checkers",
    "drafter",
    "drafters",
    "owner",
    "owners",
    "maker",
];

/// Whether `who` holds a role, or nothing, where an agent belongs.
fn names_a_role(who: &str) -> bool {
    tokens(who)
        .iter()
        .all(|word| ROLE_WORDS.contains(&word.as_str()))
}

/// "Use a workflow" asks for the crew as "use the crew" does, and "don't use a workflow" keeps
/// the work solo. They count only in a clause whose words say neither, so "the solo workflow"
/// stays solo.
const WORKFLOW_CUES: [&str; 2] = ["workflow", "workflows"];

/// Verbs after which "yourself" hands a choice over rather than asking for solo work, as in
/// "choose everything else yourself".
const CHOOSING: [&str; 5] = ["choose", "decide", "pick", "select", "figure"];

/// How `says` stands on the crew, clause by clause.
fn crew_stance(says: &str) -> Stance {
    let (crew, solo) = leanings(says);
    side(crew, solo)
}

/// How `says` stands on solo, where "don't use a workflow" is for it.
fn solo_stance(says: &str) -> Stance {
    let (crew, solo) = leanings(says);
    side(solo, crew)
}

fn side(toward: bool, away: bool) -> Stance {
    match (toward, away) {
        (true, false) => Stance::For,
        (_, true) => Stance::Against,
        (false, false) => Stance::Silent,
    }
}

/// Whether any clause of `says` leans to the crew, and whether any leans to solo. Each clause
/// stands on its own: in "use a workflow, with Codex as the reviewer, and choose everything
/// else yourself" the first asks for the crew, and the last leaves the rest to it.
fn leanings(says: &str) -> (bool, bool) {
    let (mut crew, mut solo) = (false, false);
    for clause in says.split([',', ';', ':', '.', '!', '?', '(', ')', '\n']) {
        let clause = without_handed_over_choices(clause);
        let leaning = match stance(&clause, &CREW_CUES, &SOLO_CUES) {
            Stance::Silent => stance(&clause, &WORKFLOW_CUES, &[]),
            said => said,
        };
        match leaning {
            Stance::For => crew = true,
            Stance::Against => solo = true,
            Stance::Silent => {}
        }
    }
    (crew, solo)
}

/// The clause's words without a "yourself" that follows a verb of choosing, so "choose the
/// rest yourself" says nothing about solo while "do it yourself" still does.
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

const COPY_CUES: [&str; 9] = [
    "copy",
    "checked",
    "worktree",
    "branch",
    "isolated",
    "isolation",
    "separately",
    "separate",
    "sandboxed",
];

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

/// Solo or the crew, as `value` names it and the words say it. Without a value, the words
/// alone.
fn mode(instruction: &Instruction) -> Result<Effect, String> {
    let says = &instruction.says;
    let solo = || solo_stance(says);
    let crew = || crew_stance(says);
    let mode = match key(&instruction.value).as_str() {
        "solo" if solo() == Stance::For => RunMode::Solo,
        "crew" if crew() == Stance::For => RunMode::Crew,
        "solo" | "crew" => return Err(SAYS_OTHERWISE.to_owned()),
        _ => match (solo(), crew()) {
            (Stance::For, _) => RunMode::Solo,
            (_, Stance::For) => RunMode::Crew,
            _ => return Err("it says neither solo nor the crew".to_owned()),
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
        InstructionKind::Coordinate => Some(CrewRole::Orchestrator),
        InstructionKind::Build => Some(CrewRole::Workers),
        InstructionKind::Review => Some(CrewRole::Reviewers),
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

/// A review in solo needs a copy, so it wins over words asking for your own checkout.
fn needs_a_copy(entries: &mut [Entry]) {
    let review = entries.iter().find_map(|entry| {
        let effect = entry.result.as_ref().ok()?;
        (!effect.reviewers.is_empty()).then(|| entry.instruction.says.trim().to_owned())
    });
    let Some(review) = review else { return };
    for entry in entries.iter_mut() {
        if let Ok(effect) = &entry.result
            && effect.solo == Some(SoloStyle::Direct)
        {
            entry.result = Err(format!("you also said {review}, and a review needs a copy"));
        }
    }
}

/// The mode your words ask for. A command wins over them, and an instruction that only a crew
/// can follow loses to solo you asked for.
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
                || (command.mode == RunMode::Solo && effect.implies == Some(RunMode::Crew));
            if against {
                entry.result = Err(command.said.clone());
            }
        }
        return asked
            .filter(|(mode, _)| *mode == command.mode)
            .map(|(mode, _)| mode);
    }
    if let Some((RunMode::Solo, says)) = &asked {
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
        } else if implied(RunMode::Solo) {
            Some(RunMode::Solo)
        } else {
            None
        }
    })
}

/// Free text the lead gets as a must. It never touches a safety setting, as the lead can't
/// change one, and on its own it doesn't make the run a crew's.
fn for_the_lead(instruction: &Instruction) -> Result<Effect, String> {
    let says = instruction.says.trim();
    let words = tokens(says);
    let has = |cues: &[&str]| words.iter().any(|word| cues.contains(&word.as_str()));
    let loosens = has(&[
        "skip", "no", "without", "dont", "disable", "off", "never", "ignore",
    ]);
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
    } else if has(&["gate", "gates"]) || (has(&["test", "tests", "checks"]) && loosens) {
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
    } else if has(&["budget", "spend", "cost", "dollars", "usd"]) || says.contains('$') {
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
    let role = argonauts_core::prompt_parts::role_named(&instruction.who).ok_or_else(|| {
        "it doesn't say which role: the main agent, the orchestrator, the workers, the \
         reviewers or the judge"
            .to_owned()
    })?;
    let words = instruction.value.trim();
    if words.is_empty() {
        return Err("it doesn't say what to tell them".to_owned());
    }
    for_the_lead(instruction)?;
    let key = argonauts_core::settings::prompts::PromptSettings::request_key(role);
    Ok(Effect {
        said: argonauts_core::prompt_parts::for_role_line(role, words),
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
