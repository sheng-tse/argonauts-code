//! The day's event: what was used and how the work went, counted from the usage and work ledgers
//! the Overview reads, with nothing in it that names you, your files or your repositories. Every
//! field is a count, a fixed word, or a bracket; nothing is timed finer than a day.

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::agents::adapter::KNOWN_AGENT_IDS;
use argonauts_core::agents::runtimes::RUNTIMES;
use argonauts_core::agents::session::AuthMode;
use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::models::pricing::builtin_price;
use argonauts_core::runs::events::run::RunOutcome;
use argonauts_core::settings::Settings;
use argonauts_core::{RunId, TaskId};
use serde::Serialize;
use time::Date;

use super::uses::{Use, UseLine};
use crate::behaviors::report::BehaviorCounts;
use crate::usage::usage_ledger::LedgerLine;
use crate::usage::work_ledger::{ApprovedBy, FallbackCause, Work, WorkLine};

pub(crate) const EVENT: &str = "daily_usage";

/// The field list's version. A new one shows the notice once more.
pub(crate) const FIELDS_VERSION: u32 = 3;

/// The CLIs whose names are sent as they are. Any other agent is an endpoint you named, so it
/// goes as `endpoint`.
const NAMED_CLIS: &[&str] = &["opencode"];

/// The open model series a local model is sent by, never by its own name.
const SERIES: &[(&str, &str)] = &[
    ("qwen", "qwen"),
    ("gemma", "gemma"),
    ("llama", "llama"),
    ("mistral", "mistral"),
    ("devstral", "mistral"),
    ("codestral", "mistral"),
    ("deepseek", "deepseek"),
    ("gpt-oss", "gpt-oss"),
    ("phi", "phi"),
    ("granite", "granite"),
    ("glm", "glm"),
    ("kimi", "kimi"),
];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct DailyUsage {
    /// The field list's version.
    pub fields: u32,
    /// Random, made on this computer.
    pub install: String,
    pub version: String,
    pub os: String,
    pub machine: Machine,
    pub setup: SetupFacts,
    /// Each installed CLI and its version.
    pub clis: BTreeMap<String, String>,
    pub models: Vec<ModelUse>,
    /// Runs by workflow and how each ended. A workflow you wrote goes as `custom`.
    pub runs: BTreeMap<String, RunEnds>,
    /// Turns the conversation's own agent answered itself.
    pub turns: u32,
    pub tasks: Tasks,
    pub findings: Findings,
    pub checks: Checks,
    pub merges: Merges,
    pub fallbacks: Fallbacks,
    pub features: Features,
    pub health: Health,
    /// Actions under each behavior label, from the run records.
    #[serde(flatten)]
    pub behaviors: BehaviorCounts,
}

/// The machine, in brackets and fixed words.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Machine {
    /// `apple_silicon`, `intel`, or the architecture's name.
    pub chip: String,
    /// Memory rounded down to a power of two, in GB.
    pub ram_gb: Option<u32>,
    /// CPU cores rounded down to a power of two.
    pub cores: Option<u32>,
    /// The terminal's kind from a fixed list, or `other`.
    pub terminal: String,
}

/// How Argonauts Code is set up, in fixed words.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct SetupFacts {
    /// `inline` or `fullscreen`.
    pub screen: String,
    /// The sandbox mode's name.
    pub sandbox: String,
    /// The approval mode's name.
    pub approvals: String,
    pub autopilot: bool,
    /// The folder it started in is a git repository.
    pub git_repo: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ModelUse {
    pub cli: String,
    /// A model with a public list price by its id; a local one by its series.
    pub model: String,
    /// Input and output tokens together, as the first field list sent them.
    pub tokens: u64,
    pub input: u64,
    pub output: u64,
    pub cached: u64,
    /// The tokens' value at list prices, in dollars, to the cent.
    pub usd: f64,
    /// `plan`, `api_key`, or `local`.
    pub billing: &'static str,
    /// The roles it filled: `main`, `worker`, `reviewer` and so on.
    pub roles: BTreeSet<String>,
    /// The effort levels your settings run those roles at.
    pub efforts: BTreeSet<String>,
}

/// How a workflow's runs ended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct RunEnds {
    pub merged: u32,
    pub pull_request: u32,
    pub branch_left: u32,
    /// Dropped at the merge approval.
    pub abandoned: u32,
    pub stopped: u32,
    pub failed: u32,
    /// Ended before end states were kept.
    pub unknown: u32,
}

/// How review took each task: at its first review, after one more round, after two or more.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Tasks {
    pub reviewed: u32,
    pub first_pass: u32,
    pub second_pass: u32,
    pub more_rounds: u32,
    pub never_passed: u32,
}

/// What reviews found, and how much of it was fixed: a sent-back review's findings count as
/// fixed when a later review of the same task approves.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Findings {
    pub given: u32,
    pub blocking: u32,
    pub fixed: u32,
    pub by_reviewer: BTreeMap<String, u32>,
    pub by_builder: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Checks {
    /// Tasks whose checks ran.
    pub tasks_checked: u32,
    pub first_try: u32,
    pub after_review: u32,
    /// Runs whose checks failed after the merge.
    pub final_check_failed: u32,
}

/// Who approved the merges, and the times a merge approval sent the work back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Merges {
    pub you: u32,
    pub autopilot: u32,
    pub judge: u32,
    pub rules: u32,
    pub sent_back: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Fallbacks {
    pub signed_out: u32,
    pub usage_limit: u32,
    pub down: u32,
    pub model_unavailable: u32,
    pub stuck: u32,
    pub other: u32,
    /// The models fallen back to.
    pub to: BTreeMap<String, u32>,
}

impl Fallbacks {
    pub(crate) fn total(&self) -> u32 {
        self.signed_out
            + self.usage_limit
            + self.down
            + self.model_unavailable
            + self.stuck
            + self.other
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct Features {
    /// Runs by workflow. A workflow you wrote goes as `custom`, since its name is yours.
    pub workflows: BTreeMap<String, u32>,
    /// Goals set, by runs and by the conversation.
    pub goals: u32,
    /// Turns that went out with ultracode, and runs started because of it.
    pub ultracode: u32,
    /// Mod files loaded.
    pub mods: usize,
    /// Commands used, by name.
    pub commands: BTreeMap<String, u32>,
    /// Times the workflow view opened.
    pub workflow_view: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Health {
    /// Crashes, by the part of Argonauts Code's own code they happened in.
    pub crashes: BTreeMap<String, u32>,
    /// Kinds of output the installed CLIs printed that Argonauts Code didn't understand.
    pub not_understood: u64,
    /// Starts, by how long the screen took to show: `under_1s`, `1_to_3s`, `over_3s`.
    pub startup: BTreeMap<String, u32>,
}

/// What the event counts besides the ledgers, as the app finds them when it starts.
#[derive(Debug, Clone, Default)]
pub(crate) struct Found {
    pub clis: BTreeMap<String, String>,
    pub mods: usize,
    pub machine: Machine,
    pub git_repo: bool,
    /// Kinds of output the CLIs printed that Argonauts Code doesn't read yet.
    pub not_understood: u64,
}

type TaskKey<'a> = (&'a RunId, &'a TaskId);

/// The days from `from` up to but not including `until`.
pub(crate) fn count(
    install: &str,
    usage: &[LedgerLine],
    work: &[WorkLine],
    uses: &[UseLine],
    settings: &Settings,
    found: &Found,
    (from, until): (Date, Date),
) -> DailyUsage {
    let within = |day: Date| from <= day && day < until;
    let usage: Vec<&LedgerLine> = usage.iter().filter(|line| within(line.day)).collect();
    let work: Vec<&WorkLine> = work.iter().filter(|line| within(line.day)).collect();
    let mut features = Features {
        mods: found.mods,
        ..Features::default()
    };
    let mut health = Health {
        not_understood: found.not_understood,
        ..Health::default()
    };
    for line in uses.iter().filter(|line| within(line.day)) {
        match &line.used {
            Use::Goal => features.goals += 1,
            Use::Ultracode => features.ultracode += 1,
            Use::Command { name } => *features.commands.entry(name.clone()).or_default() += 1,
            Use::WorkflowView => features.workflow_view += 1,
            Use::Crash { place } => *health.crashes.entry(place.clone()).or_default() += 1,
            Use::Started { bracket } => *health.startup.entry(bracket.clone()).or_default() += 1,
        }
    }
    let mut counted = DailyUsage {
        fields: FIELDS_VERSION,
        install: install.to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        os: std::env::consts::OS.to_owned(),
        machine: found.machine.clone(),
        setup: setup_facts(settings, found),
        clis: found.clis.clone(),
        models: models(&usage, &work, settings),
        runs: BTreeMap::new(),
        turns: usage.iter().filter(|line| is_turn(line)).count() as u32,
        tasks: Tasks::default(),
        findings: Findings::default(),
        checks: Checks::default(),
        merges: Merges::default(),
        fallbacks: Fallbacks::default(),
        features,
        health,
        behaviors: BehaviorCounts::default(),
    };
    count_work(&work, settings, &mut counted);
    counted
}

/// A turn of the conversation that no run made.
fn is_turn(line: &LedgerLine) -> bool {
    line.conversation.is_some() && line.run.is_none()
}

fn setup_facts(settings: &Settings, found: &Found) -> SetupFacts {
    let word = |value: Result<serde_json::Value, serde_json::Error>| {
        value
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default()
    };
    SetupFacts {
        screen: word(serde_json::to_value(settings.ui.screen)),
        sandbox: word(serde_json::to_value(settings.sandbox.mode)),
        approvals: settings.approvals.mode().label().replace(' ', "_"),
        autopilot: settings.approvals.autopilot,
        git_repo: found.git_repo,
    }
}

/// Each model's tokens and value, with the roles it filled and the efforts they run it at.
fn models(usage: &[&LedgerLine], work: &[&WorkLine], settings: &Settings) -> Vec<ModelUse> {
    let mut roles_of: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for line in work {
        if let Work::Session { role, by, .. } = &line.what {
            let (cli, model, _) = sent_as(&by.agent, by.model.as_deref(), settings);
            roles_of
                .entry((cli, model))
                .or_default()
                .insert(role.clone());
        }
    }
    let mut models: BTreeMap<(String, String, &'static str), ModelUse> = BTreeMap::new();
    for line in usage {
        let (cli, model, billing) = sent_as(&line.agent, line.model.as_deref(), settings);
        let entry = models
            .entry((cli.clone(), model.clone(), billing))
            .or_insert_with(|| ModelUse {
                cli,
                model,
                tokens: 0,
                input: 0,
                output: 0,
                cached: 0,
                usd: 0.0,
                billing,
                roles: BTreeSet::new(),
                efforts: BTreeSet::new(),
            });
        entry.input += line.input;
        entry.output += line.output;
        entry.cached += line.cached;
        entry.tokens += line.input + line.output;
        entry.usd += line.cost;
        // The effort your settings run a role on this CLI at.
        let effort = |role: &str| {
            let of_role = RoleName::ALL
                .into_iter()
                .find(|known| known.as_str() == role)
                .and_then(|role| settings.roles.get(&role)?.effort.clone());
            of_role.or_else(|| settings.agents.get(&line.agent)?.effort.clone())
        };
        if is_turn(line) {
            entry.roles.insert("main".to_owned());
            entry.efforts.extend(effort("main"));
        }
        let key = (entry.cli.clone(), entry.model.clone());
        for role in roles_of.get(&key).into_iter().flatten() {
            entry.roles.insert(role.clone());
            entry.efforts.extend(effort(role));
        }
    }
    let mut models: Vec<ModelUse> = models
        .into_values()
        .map(|mut model| {
            model.usd = (model.usd * 100.0).round() / 100.0;
            model
        })
        .collect();
    models.sort_by_key(|model| std::cmp::Reverse(model.tokens));
    models
}

/// Runs, tasks, findings, checks, merges and fallbacks, from the work ledger's lines.
fn count_work(work: &[&WorkLine], settings: &Settings, day: &mut DailyUsage) {
    let workflow_of: BTreeMap<&RunId, String> = work
        .iter()
        .filter_map(|line| match &line.what {
            Work::Run { workflow, .. } => Some((&line.run, sent_workflow(workflow.as_deref()))),
            _ => None,
        })
        .collect();
    // Each task's builder, for the findings it was given.
    let mut builders: BTreeMap<TaskKey<'_>, String> = BTreeMap::new();
    for line in work {
        match &line.what {
            Work::Run {
                ultracode, goals, ..
            } => {
                let name = workflow_of[&line.run].clone();
                *day.features.workflows.entry(name).or_default() += 1;
                day.features.ultracode += u32::from(*ultracode);
                day.features.goals += goals;
            }
            Work::Outcome {
                ended,
                checks_failed,
                merge_approved_by,
                merge_sent_back,
                ..
            } => {
                let name = workflow_of
                    .get(&line.run)
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_owned());
                let ends = day.runs.entry(name).or_default();
                match ended {
                    Some(RunOutcome::Merged) => ends.merged += 1,
                    Some(RunOutcome::PullRequest) => ends.pull_request += 1,
                    Some(RunOutcome::BranchLeft) => ends.branch_left += 1,
                    Some(RunOutcome::Abandoned) => ends.abandoned += 1,
                    Some(RunOutcome::Stopped) => ends.stopped += 1,
                    Some(RunOutcome::Failed) => ends.failed += 1,
                    None => ends.unknown += 1,
                }
                day.checks.final_check_failed += u32::from(*checks_failed);
                match merge_approved_by {
                    Some(ApprovedBy::You) => day.merges.you += 1,
                    Some(ApprovedBy::Autopilot) => day.merges.autopilot += 1,
                    Some(ApprovedBy::Judge) => day.merges.judge += 1,
                    Some(ApprovedBy::Rules) => day.merges.rules += 1,
                    None => {}
                }
                day.merges.sent_back += merge_sent_back;
            }
            Work::Task {
                task,
                by,
                sent_back,
                merged,
                reviewed,
                checks_first,
                checks_last,
                ..
            } => {
                builders.insert((&line.run, task), sent_model(by, settings));
                let tasks = &mut day.tasks;
                if *reviewed {
                    tasks.reviewed += 1;
                    match (*merged, *sent_back) {
                        (true, 0) => tasks.first_pass += 1,
                        (true, 1) => tasks.second_pass += 1,
                        (true, _) => tasks.more_rounds += 1,
                        (false, _) => tasks.never_passed += 1,
                    }
                }
                if checks_first.is_some() {
                    day.checks.tasks_checked += 1;
                    day.checks.first_try += u32::from(*checks_first == Some(true));
                    day.checks.after_review += u32::from(*checks_last == Some(true));
                }
            }
            Work::Session {
                by,
                fallback_for: Some(_),
                cause,
                ..
            } => {
                let fallbacks = &mut day.fallbacks;
                match cause.unwrap_or(FallbackCause::Other) {
                    FallbackCause::SignedOut => fallbacks.signed_out += 1,
                    FallbackCause::UsageLimit => fallbacks.usage_limit += 1,
                    FallbackCause::Down => fallbacks.down += 1,
                    FallbackCause::ModelUnavailable => fallbacks.model_unavailable += 1,
                    FallbackCause::Stuck => fallbacks.stuck += 1,
                    FallbackCause::Other => fallbacks.other += 1,
                }
                *fallbacks.to.entry(sent_model(by, settings)).or_default() += 1;
            }
            _ => {}
        }
    }
    count_findings(work, settings, &builders, &mut day.findings);
}

/// Each review's findings by its reviewer and by the task's builder, and those a later approval
/// of the same task shows were fixed.
fn count_findings(
    work: &[&WorkLine],
    settings: &Settings,
    builders: &BTreeMap<TaskKey<'_>, String>,
    findings: &mut Findings,
) {
    // A task's findings sent back so far, waiting for the approval that shows them fixed.
    let mut waiting: BTreeMap<TaskKey<'_>, u32> = BTreeMap::new();
    for line in work {
        let Work::Review {
            by,
            task,
            sent_back,
            findings: given,
            blocking,
            ..
        } = &line.what
        else {
            continue;
        };
        findings.given += given;
        findings.blocking += blocking;
        if *given > 0 {
            *findings
                .by_reviewer
                .entry(sent_model(by, settings))
                .or_default() += given;
        }
        let Some(task) = task else { continue };
        let key = (&line.run, task);
        if *given > 0
            && let Some(builder) = builders.get(&key)
        {
            *findings.by_builder.entry(builder.clone()).or_default() += given;
        }
        if *sent_back {
            *waiting.entry(key).or_default() += given;
        } else {
            findings.fixed += waiting.remove(&key).unwrap_or(0);
        }
    }
}

/// A workflow's name as it may be sent: a built-in by its name, any other as `custom`.
fn sent_workflow(workflow: Option<&str>) -> String {
    match workflow {
        Some(name) if argonauts_registry::builtin_workflow(name).is_ok() => name.to_owned(),
        Some(_) => "custom".to_owned(),
        None => "unknown".to_owned(),
    }
}

fn sent_model(worker: &ModelRef, settings: &Settings) -> String {
    model_name(&worker.agent, worker.model.as_deref(), settings)
}

/// Which CLI and model a line is, and how it was paid. A model is named only when it is one
/// Argonauts Code knows a public list price for, whatever runs it, so a model you or your provider
/// named, such as a cloud deployment's id, never leaves the computer. Any other model served on
/// this computer goes by its open series alone.
fn sent_as(
    agent: &str,
    model: Option<&str>,
    settings: &Settings,
) -> (String, String, &'static str) {
    let on_this_computer = model.is_some_and(is_local_path)
        || settings
            .endpoints
            .get(agent)
            .is_some_and(|endpoint| endpoint.local);
    let model = match model.map(|model| (model, shareable_model(model))) {
        None => "default".to_owned(),
        Some((model, other)) if on_this_computer && other == OTHER => series_of(model),
        Some((_, shared)) => shared,
    };
    if KNOWN_AGENT_IDS.contains(&agent) || NAMED_CLIS.contains(&agent) {
        let billing = match settings.agents.get(agent).map(|agent| agent.auth) {
            _ if on_this_computer => "local",
            Some(AuthMode::ApiKey) => "api_key",
            _ => "plan",
        };
        return (agent.to_owned(), model, billing);
    }
    let billing = if on_this_computer { "local" } else { "api_key" };
    ("endpoint".to_owned(), model, billing)
}

/// A model a CLI reaches on a local server, named by that server: `ollama/qwen3-coder:30b`.
fn is_local_path(model: &str) -> bool {
    model
        .split_once('/')
        .is_some_and(|(server, _)| RUNTIMES.iter().any(|runtime| runtime.id == server))
}

/// The open series a local model belongs to, from a fixed list, or `other`.
pub(crate) fn series_of(model: &str) -> String {
    let name = model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase();
    SERIES
        .iter()
        .find(|(start, _)| name.starts_with(start))
        .map_or_else(|| "other".to_owned(), |(_, series)| (*series).to_owned())
}

/// Whether `model` is a series' name, as a local model is sent.
pub(crate) fn is_series(model: &str) -> bool {
    SERIES.iter().any(|(_, series)| *series == model)
}

/// A model as a report may name it, by the rule the models list follows: `default` when only
/// its CLI is known.
pub(crate) fn model_name(agent: &str, model: Option<&str>, settings: &Settings) -> String {
    sent_as(agent, model, settings).1
}

/// A built-in model id as it may be sent, read without a context suffix such as `[1m]`, or
/// "other" for any id Argonauts Code has no list price for.
fn shareable_model(model: &str) -> String {
    let base = model.split_once('[').map_or(model, |(base, _)| base).trim();
    if builtin_price(base).is_some() {
        base.to_ascii_lowercase()
    } else {
        OTHER.to_owned()
    }
}

/// Any model that may not be named.
const OTHER: &str = "other";

/// The terminal's kind, from the variables terminals set, as a word from a fixed list.
pub(crate) fn terminal_kind(var: impl Fn(&str) -> Option<String>) -> String {
    let program = var("TERM_PROGRAM").unwrap_or_default().to_ascii_lowercase();
    let term = var("TERM").unwrap_or_default().to_ascii_lowercase();
    let kind = if program.contains("ghostty") || term.contains("ghostty") {
        "ghostty"
    } else if program.contains("iterm") {
        "iterm2"
    } else if program == "apple_terminal" {
        "apple_terminal"
    } else if program.contains("wezterm") {
        "wezterm"
    } else if program == "vscode" {
        "vscode"
    } else if program.contains("warp") {
        "warp"
    } else if program == "tmux" || var("TMUX").is_some() {
        "tmux"
    } else if term.contains("kitty") || var("KITTY_WINDOW_ID").is_some() {
        "kitty"
    } else if term.contains("alacritty") {
        "alacritty"
    } else if var("WT_SESSION").is_some() {
        "windows_terminal"
    } else {
        "other"
    };
    kind.to_owned()
}

/// The chip, in a word: Apple silicon and Intel Macs by name, any other by its architecture.
pub(crate) fn chip() -> String {
    let arch = std::env::consts::ARCH;
    match (std::env::consts::OS, arch) {
        ("macos", "aarch64") => "apple_silicon".to_owned(),
        ("macos", _) => "intel".to_owned(),
        _ => arch.to_owned(),
    }
}

/// `n` rounded down to a power of two, so a machine is one of a few of its kind.
pub(crate) fn bracket(n: u64) -> Option<u32> {
    (n > 0).then(|| 1u32 << (63 - n.leading_zeros()).min(31))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sent(agent: &str, model: &str) -> String {
        sent_as(agent, Some(model), &Settings::default()).1
    }

    /// Only a model Argonauts Code knows by its public id is named; an id a cloud, a gateway or a
    /// provider of your own gave it goes as "other".
    #[test]
    fn only_built_in_models_are_named() {
        let arn = "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/abc";
        assert_eq!(sent("claude-code", arn), "other");
        assert_eq!(sent("opencode", "acme-gateway/team-model"), "other");
        assert_eq!(sent("codex", "acme-prod-gpt"), "other");
        assert_eq!(sent("claude-code", "claude-opus-5-5"), "claude-opus-5-5");
        assert_eq!(
            sent("claude-code", "claude-opus-5-5[1m]"),
            "claude-opus-5-5"
        );
        assert_eq!(
            sent("claude-code", "claude-haiku-4-5-20251001"),
            "claude-haiku-4-5-20251001"
        );
        assert_eq!(sent("codex", "gpt-5.6-luna"), "gpt-5.6-luna");
    }

    /// A model served on this computer goes by its series, never by the tag you gave it.
    #[test]
    fn a_local_model_goes_by_its_series() {
        let settings = Settings::default();
        let (cli, model, billing) = sent_as("opencode", Some("ollama/qwen3-coder:30b"), &settings);
        assert_eq!(
            (cli.as_str(), model.as_str(), billing),
            ("opencode", "qwen", "local")
        );
        assert_eq!(series_of("my-own-finetune:7b"), "other");
        assert_eq!(series_of("devstral:24b"), "mistral");
    }

    #[test]
    fn a_machine_is_told_in_brackets() {
        assert_eq!(bracket(64 + 5), Some(64));
        assert_eq!(bracket(12), Some(8));
        assert_eq!(bracket(0), None);
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            }
        };
        assert_eq!(
            terminal_kind(env(&[("TERM_PROGRAM", "ghostty")])),
            "ghostty"
        );
        assert_eq!(
            terminal_kind(env(&[("TERM_PROGRAM", "iTerm.app")])),
            "iterm2"
        );
        assert_eq!(terminal_kind(env(&[("TERM", "xterm-kitty")])), "kitty");
        assert_eq!(
            terminal_kind(env(&[("TERM_PROGRAM", "acme-term")])),
            "other"
        );
    }

    /// The day's behaviors come from the run records, counted by model, role and label, with
    /// nothing that names the project, the run or what was run.
    #[test]
    fn the_day_s_behaviors_are_counted_from_the_run_records() {
        use argonauts_core::base::names::{RoleName, SessionKey};
        use argonauts_core::{AgentEvent, Paths};

        use crate::client::test_support::{agent, base_ts, session_started, write_journal};
        use crate::telemetry::{State, usage_between};

        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let session = SessionKey::numbered(1);
        let call = |seq, id: &str, command: &str| {
            agent(
                seq,
                &session,
                AgentEvent::ToolStarted {
                    id: id.to_owned(),
                    name: "command_execution".to_owned(),
                    input: serde_json::json!({ "command": command }),
                    parent: None,
                },
            )
        };
        let log = paths
            .home()
            .join("state/acme-secret-0123456789ab/runs/run-20260930-101500-00ab");
        std::fs::create_dir_all(&log).unwrap();
        write_journal(
            &log.join("events.jsonl"),
            &[
                session_started(0, &session, None, RoleName::Worker, "codex"),
                call(1, "item_1", "git show ef3f187 -- secret/bucket.py"),
                call(2, "item_2", "acme-deploy --now"),
                call(3, "item_3", "git log -3"),
            ],
        );
        let day = crate::usage::usage_ledger::local_day(base_ts());
        let days = (day, day.next_day().unwrap());
        let event = usage_between(
            &paths,
            &Settings::default(),
            &Found::default(),
            &State::new(day),
            days,
        );
        let sent = serde_json::to_value(&event).unwrap();
        assert_eq!(
            sent["behaviors"],
            serde_json::json!({"default": {"worker": {"git_history": 2, "shell": 1}}})
        );
        assert_eq!(sent["behavior_outcomes"], serde_json::json!({}));
        assert_eq!(sent["behaviors_unlabelled"], serde_json::json!({}));
        let text = sent.to_string();
        for private in ["acme", "secret", "ef3f187", "run-2026", "s-001", "item_1"] {
            assert!(!text.contains(private), "{private} in {text}");
        }
    }

    /// The behaviors name a model as the models list does, so one served on this computer goes
    /// by its series and never by the tag you gave it.
    #[test]
    fn the_behaviors_name_a_local_model_by_its_series() {
        use argonauts_core::base::names::{RoleName, SessionKey};
        use argonauts_core::runs::events::RunEventKind;
        use argonauts_core::runs::events::session::SessionEvent;
        use argonauts_core::{AgentEvent, Paths};

        use crate::client::test_support::{agent, base_ts, session_started, write_journal};
        use crate::telemetry::{State, usage_between};

        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let session = SessionKey::numbered(1);
        let mut started = session_started(0, &session, None, RoleName::Worker, "opencode");
        if let RunEventKind::Session(SessionEvent::Started(started)) = &mut started.kind {
            let tag = "ollama/qwen3-coder:30b".to_owned();
            started.worker = ModelRef::new("opencode", Some(tag));
        }
        let call = agent(
            1,
            &session,
            AgentEvent::ToolStarted {
                id: "call_1".to_owned(),
                name: "bash".to_owned(),
                input: serde_json::json!({ "command": "git log -3" }),
                parent: None,
            },
        );
        let log = paths
            .home()
            .join("state/demo-0123456789ab/runs/run-20260930-101500-00ab");
        std::fs::create_dir_all(&log).unwrap();
        write_journal(&log.join("events.jsonl"), &[started, call]);
        let day = crate::usage::usage_ledger::local_day(base_ts());
        let days = (day, day.next_day().unwrap());
        let state = State::new(day);
        let event = usage_between(
            &paths,
            &Settings::default(),
            &Found::default(),
            &state,
            days,
        );
        let sent = serde_json::to_value(&event).unwrap();
        assert_eq!(
            sent["behaviors"],
            serde_json::json!({"qwen": {"worker": {"git_history": 1}}})
        );
        assert!(!sent.to_string().contains("coder:30b"), "{sent}");
    }
}
