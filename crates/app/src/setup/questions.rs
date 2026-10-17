//! Each step's question and what its answer changes. The first option is what Enter and Esc
//! take: the recommendation, or on a rerun the value your global file already holds. A rerun
//! never loosens what the file sets more finely than a question asks, such as a model or one
//! approval's approval mode.

use std::collections::BTreeMap;
use std::path::PathBuf;

use argonauts_core::app::ChoiceKind;
use argonauts_core::events::approval::{AnswerAction, AnswerOption};
use argonauts_core::model_catalog::display_name_from_id;
use argonauts_core::names::ModelRef;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::text::shown_name;
use argonauts_registry::TERMS_NOTE;

use super::probe::{FoundAgent, LocalServer, LoginState};
use super::roles::{self, ModelOption};
use super::steps::{
    APPROVAL_POINTS, Answers, CAPTAIN, COUNCIL, ORCHESTRATOR, ReviewerPick, approval_mode_from,
    approval_mode_name, built_in_mode,
};
use crate::choices::{InitAnswer, choose, init_options, option, options};

/// What the Continue option carries back.
pub(crate) const CONTINUE: &str = "continue";
const NOT_NOW: &str = "not-now";
/// Leaves every approval's approval mode as your file has it.
const KEEP: &str = "keep";

const ABOUT: &str = "Argonauts Code runs your coding-agent CLIs as one crew, each in its own git \
                     worktree, and merges only the work that passes your rules.";

/// What the recommended approvals mean, in one line on that question.
pub(crate) const AUTONOMY: &str = "With auto, the agents work and merge on their own under the \
                                   rules. Change that any time in /permissions and /approvals.";

/// The order the main agent is recommended in.
const PREFERRED_AGENTS: &[&str] = &["claude-code", "codex", "antigravity-cli"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    Terms,
    Agents,
    /// Becomes one [`Step::Server`] per local server that answered, once they're probed.
    Servers,
    Server(LocalServer),
    MainAgent,
    Workflow,
    Orchestrator,
    /// Every role with who fills it now; Enter keeps them all.
    WhoDoesWhat,
    /// Which agent fills the role at this `/model` key.
    RoleAgent(String),
    /// Which of its agent's models fills the role at this key.
    RoleModel(String),
    Approvals,
    /// Whether to send the day's anonymous usage counts.
    Telemetry,
    Trust,
    ProjectFolder,
    /// Whether the guide pages go into this repo's `.argonauts/` too, asked once per repo.
    Guides,
}

/// What the questions are built from.
#[derive(Debug, Clone, Default)]
pub(crate) struct SetupView {
    pub agents: Vec<FoundAgent>,
    /// False until the login checks are back.
    pub logins_known: bool,
    /// `None` until the servers are probed.
    pub servers: Option<Vec<LocalServer>>,
    /// Your global settings file as it is, so a rerun preselects what you chose before.
    pub current: toml::Table,
    /// This repo's settings files that exist and aren't trusted yet.
    pub untrusted: Vec<PathBuf>,
    pub project_dir_exists: bool,
    /// Your answer about the guide pages in this repo, once you gave one.
    pub guides_answer: Option<bool>,
    /// The keys `/model` lists, one row of "Who does what" each.
    pub role_keys: Vec<String>,
    /// Each agent's models, by its id, its default first.
    pub models: BTreeMap<String, Vec<ModelOption>>,
    /// The workflows you can pick: the built-ins, then yours and the repo's.
    pub workflows: Vec<String>,
}

impl SetupView {
    pub(crate) fn ready(&self) -> impl Iterator<Item = &FoundAgent> {
        self.agents.iter().filter(|agent| agent.ready())
    }

    /// `Claude Code` for `claude-code`, or the id of an agent the probe didn't find.
    pub(crate) fn agent_name(&self, id: &str) -> String {
        self.agent(id)
            .map_or_else(|| id.to_owned(), |agent| agent.name.clone())
    }

    fn agent(&self, id: &str) -> Option<&FoundAgent> {
        self.agents.iter().find(|agent| agent.id == id)
    }

    /// `Claude Code`, or `Claude Code (opus)` with a model.
    fn name_of(&self, worker: &ModelRef) -> String {
        let Some(agent) = self.agent(&worker.agent) else {
            return worker.to_string();
        };
        match &worker.model {
            Some(model) => format!("{} ({model})", agent.name),
            None => agent.name.clone(),
        }
    }

    /// The value at a dotted key of your global file.
    fn current_value(&self, key: &str) -> Option<&toml::Value> {
        let mut parts = key.split('.');
        let mut value = self.current.get(parts.next()?)?;
        for part in parts {
            value = value.as_table()?.get(part)?;
        }
        Some(value)
    }

    pub(crate) fn current_str(&self, key: &str) -> Option<&str> {
        self.current_value(key)?.as_str()
    }

    /// Your main agent now, when its CLI is ready.
    fn current_main_agent(&self) -> Option<ModelRef> {
        self.ready_worker(self.current_str("crew.main")?)
    }

    /// Your orchestrator now, when its CLI is ready.
    fn current_orchestrator(&self) -> Option<ModelRef> {
        let workers = self.current_list("roles.orchestrator.models");
        self.ready_worker(workers.first()?)
    }

    /// `claude-code` or `claude-code/opus`, when that CLI is ready.
    fn ready_worker(&self, text: &str) -> Option<ModelRef> {
        let worker: ModelRef = text.parse().ok()?;
        self.ready()
            .any(|agent| agent.id == worker.agent)
            .then_some(worker)
    }

    /// The one mode every approval the setup sets has now, or `None` when they are different. A
    /// approval your file leaves out has its built-in mode, and the autopilot switch puts
    /// them all in autopilot.
    fn current_approvals(&self) -> Option<ApprovalMode> {
        let switch = self.current_value("approvals.autopilot");
        if switch.and_then(toml::Value::as_bool) == Some(true) {
            return Some(ApprovalMode::Autopilot);
        }
        let mut modes = APPROVAL_POINTS.iter().map(|point| {
            match self.current_value(&format!("approvals.{point}.mode")) {
                None => built_in_mode(point),
                Some(value) => value.as_str().and_then(approval_mode_from),
            }
        });
        let first = modes.next().flatten()?;
        modes.all(|mode| mode == Some(first)).then_some(first)
    }

    /// The strings in an array at a dotted key of your global file.
    pub(crate) fn current_list(&self, key: &str) -> Vec<String> {
        let (table, last) = match key.rsplit_once('.') {
            Some((path, last)) => {
                let mut table = &self.current;
                for part in path.split('.') {
                    match table.get(part).and_then(toml::Value::as_table) {
                        Some(inner) => table = inner,
                        None => return Vec::new(),
                    }
                }
                (table, last)
            }
            None => (&self.current, key),
        };
        table
            .get(last)
            .and_then(toml::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether the step can be asked yet, or waits for a probe.
    pub(crate) fn can_ask(&self, step: &Step) -> bool {
        match step {
            Step::Agents
            | Step::MainAgent
            | Step::Workflow
            | Step::Orchestrator
            | Step::WhoDoesWhat
            | Step::RoleAgent(_)
            | Step::RoleModel(_) => self.logins_known,
            Step::Servers => self.servers.is_some(),
            _ => true,
        }
    }
}

pub(crate) struct Question {
    pub kind: ChoiceKind,
    pub title: String,
    pub body: Vec<String>,
    pub options: Vec<AnswerOption>,
}

impl Question {
    /// The value Enter takes.
    pub(crate) fn first_value(&self) -> Option<String> {
        self.options
            .first()
            .and_then(|option| value_of(&option.action))
    }

    pub(crate) fn offers(&self, value: &str) -> bool {
        self.options
            .iter()
            .any(|option| value_of(&option.action).as_deref() == Some(value))
    }
}

/// What an option carries back: its value, or [`CONTINUE`].
pub(crate) fn value_of(action: &AnswerAction) -> Option<String> {
    match action {
        AnswerAction::Choose { value } => Some(value.clone()),
        AnswerAction::Approve => Some(CONTINUE.to_owned()),
        _ => None,
    }
}

/// `None` when the step has nothing to ask, such as the main agent with none ready.
pub(crate) fn question(step: &Step, view: &SetupView, answers: &Answers) -> Option<Question> {
    match step {
        Step::Terms => Some(Question {
            kind: ChoiceKind::TermsNote,
            title: "Welcome to Argonauts Code".to_owned(),
            body: vec![ABOUT.to_owned(), TERMS_NOTE.to_owned()],
            options: options(ChoiceKind::TermsNote),
        }),
        Step::Agents => Some(agents_question(view)),
        Step::Servers => None,
        Step::Server(server) => Some(server_question(server, view)),
        Step::MainAgent => main_agent_question(view),
        Step::Workflow => Some(workflow_question(view)),
        Step::Orchestrator => orchestrator_question(view, answers),
        Step::WhoDoesWhat => Some(roles::who_does_what(view, answers)),
        Step::RoleAgent(key) => roles::agent_question(view, answers, key),
        Step::RoleModel(key) => roles::model_question(view, answers, key),
        Step::Approvals => Some(approvals_question(view)),
        Step::Telemetry => Some(telemetry_question(view)),
        Step::Trust => trust_question(view),
        Step::ProjectFolder => folder_question(view),
        Step::Guides => guides_question(view, answers),
    }
}

fn agents_question(view: &SetupView) -> Question {
    let mut body: Vec<String> = view.agents.iter().map(agent_line).collect();
    let ready: Vec<&str> = view.ready().map(|agent| agent.name.as_str()).collect();
    body.push(if ready.is_empty() {
        "None is ready yet. Install one, then run /setup again.".to_owned()
    } else {
        format!("Ready ones join your crew: {}.", ready.join(", "))
    });
    Question {
        kind: ChoiceKind::Init,
        title: "Your agents".to_owned(),
        body,
        options: init_options(&[]),
    }
}

/// `Codex 0.158.0 · logged out · codex login`
fn agent_line(agent: &FoundAgent) -> String {
    let mut parts = vec![match &agent.version {
        Some(version) => format!("{} {version}", agent.name),
        None => agent.name.clone(),
    }];
    let state = if !agent.installed() {
        "not installed"
    } else if !agent.enabled {
        "turned off"
    } else if agent.login == LoginState::LoggedOut {
        "logged out"
    } else if agent.login == LoginState::LoggedIn {
        "logged in"
    } else {
        "ready"
    };
    parts.push(state.to_owned());
    if let Some(fix) = agent.fix() {
        parts.push(fix.to_owned());
    }
    parts.join(" · ")
}

fn server_question(server: &LocalServer, view: &SetupView) -> Question {
    let picked = view.current_list("roles.reviewer.models");
    let current = server
        .models
        .iter()
        .find(|model| picked.contains(&format!("{}/{model}", server.id)));
    let mut options = vec![option('n', "Not now", choose(NOT_NOW), current.is_none())];
    // Keys skip `n`, which Not now keeps.
    let keys = ('a'..='z').filter(|key| *key != 'n');
    let names: Vec<String> = server
        .models
        .iter()
        .map(|model| display_name_from_id(model))
        .collect();
    for ((model, name), key) in server.models.iter().zip(&names).zip(keys) {
        let mut model_option = option(
            key,
            name,
            choose(model),
            current.is_some_and(|current| current == model),
        );
        // Tags such as `qwen3:8b` and `qwen3:8B` can read as one name; their ids tell them apart.
        if names.iter().filter(|other| *other == name).count() > 1 {
            model_option.detail = Some(model.clone());
        }
        options.push(model_option);
    }
    // Enter takes the first option, so the one you use now goes first.
    options.sort_by_key(|option| !option.recommended);
    Question {
        kind: ChoiceKind::Init,
        title: format!("A local model server: {}", server.name()),
        body: vec![format!(
            "{} answers at {}. Use one of its models to review your crew's work? It runs on \
             this machine.",
            server.name(),
            server.base_url
        )],
        options,
    }
}

fn main_agent_question(view: &SetupView) -> Option<Question> {
    let current = view.current_main_agent();
    let first = current
        .as_ref()
        .map(|worker| worker.agent.clone())
        .or_else(|| recommended_agent(view));
    let answers = agent_answers(view, first.as_deref(), current.as_ref());
    if answers.is_empty() {
        return None;
    }
    Some(Question {
        kind: ChoiceKind::Init,
        title: "Who you talk to".to_owned(),
        body: vec![
            "The agent you talk to, and the one that works alone on small changes.".to_owned(),
        ],
        options: init_options(&answers),
    })
}

/// The first ready of Claude Code, Codex, and Antigravity CLI, else the first ready one.
fn recommended_agent(view: &SetupView) -> Option<String> {
    PREFERRED_AGENTS
        .iter()
        .find(|id| view.ready().any(|agent| agent.id == **id))
        .map(|id| (*id).to_owned())
        .or_else(|| view.ready().next().map(|agent| agent.id.clone()))
}

/// Every ready agent, `first` leading. The one `current` names shows the model it keeps.
pub(super) fn agent_answers(
    view: &SetupView,
    first: Option<&str>,
    current: Option<&ModelRef>,
) -> Vec<InitAnswer> {
    let mut ready: Vec<&FoundAgent> = view.ready().collect();
    ready.sort_by_key(|agent| Some(agent.id.as_str()) != first);
    ready
        .into_iter()
        .map(|agent| {
            let model = current
                .filter(|worker| worker.agent == agent.id)
                .and_then(|worker| worker.model.as_ref())
                .map(|model| format!("model {model}"));
            let detail: Vec<String> = agent.version.iter().cloned().chain(model).collect();
            InitAnswer {
                label: agent.name.clone(),
                value: agent.id.clone(),
                detail: (!detail.is_empty()).then(|| detail.join(" · ")),
            }
        })
        .collect()
}

/// The CLI you picked, keeping the model your file sets when the CLI is the same.
pub(super) fn kept_or_new(current: Option<ModelRef>, picked: &str) -> ModelRef {
    current
        .filter(|worker| worker.agent == picked)
        .unwrap_or_else(|| ModelRef::new(picked, None))
}

/// Orchestrator first and recommended, whatever is ready: with one CLI its main agent
/// orchestrates. A workflow of your own that the file names comes after the built-in three.
fn workflow_question(view: &SetupView) -> Question {
    let mut answers = vec![
        described(
            ORCHESTRATOR,
            "Orchestrator",
            "One orchestrator you pick plans and delegates.",
        ),
        described(
            COUNCIL,
            "Council",
            "No single planner: the agents propose, challenge, and review each other.",
        ),
        described(
            CAPTAIN,
            "Captain",
            "The CLI you talk to does the work itself; ask in your own words for a reviewer or a workflow.",
        ),
    ];
    // A name no workflow has, such as one an older file kept, runs the default, so it isn't
    // offered back.
    let yours = view
        .current_str("workflow")
        .filter(|name| view.workflows.iter().any(|known| known == name))
        .filter(|name| !answers.iter().any(|answer| answer.value == *name))
        .map(str::to_owned);
    if let Some(yours) = yours {
        answers.push(described(&yours, &shown_name(&yours), "Your workflow now."));
    }
    Question {
        kind: ChoiceKind::Init,
        title: "How the crew works".to_owned(),
        body: vec!["Change it any time with /workflow.".to_owned()],
        options: init_options(&answers),
    }
}

pub(super) fn described(value: &str, label: &str, detail: &str) -> InitAnswer {
    InitAnswer {
        label: label.to_owned(),
        value: value.to_owned(),
        detail: Some(detail.to_owned()),
    }
}

fn orchestrator_question(view: &SetupView, answers: &Answers) -> Option<Question> {
    let current = view.current_orchestrator();
    let first = current
        .as_ref()
        .or(answers.main_agent.as_ref())
        .map(|worker| worker.agent.clone());
    let choices = agent_answers(view, first.as_deref(), current.as_ref());
    if choices.is_empty() {
        return None;
    }
    Some(Question {
        kind: ChoiceKind::Init,
        title: "Who orchestrates".to_owned(),
        body: vec!["The orchestrator plans the work and hands it out.".to_owned()],
        options: init_options(&choices),
    })
}

fn approvals_question(view: &SetupView) -> Question {
    let now = view.current_approvals();
    let first = now.map_or(KEEP, approval_mode_name);
    let mut body = Vec::new();
    if now.is_none() {
        body.push(
            "Your approvals use different modes now. Picking a mode sets it for every one."
                .to_owned(),
        );
    }
    body.push(AUTONOMY.to_owned());
    let mut answers = vec![
        described(
            "auto",
            "Auto",
            "Rules and a reviewer from another provider decide, and you're asked when a \
             rule fails.",
        ),
        described(
            "autopilot",
            "Autopilot",
            "Nothing waits for you: the main agent answers every question, a reviewer from another \
             provider checks, and what can't be approved ends safely.",
        ),
        described(
            "supervised",
            "Supervised",
            "You approve the goal, the plan, and every merge.",
        ),
        described(
            "auto_preview",
            "Auto preview",
            "You approve, and Argonauts Code learns what it would have decided.",
        ),
    ];
    if now.is_none() {
        answers.push(described(
            KEEP,
            "Keep them as they are",
            "Each approval keeps its mode.",
        ));
    }
    answers.sort_by_key(|answer| answer.value != first);
    Question {
        kind: ChoiceKind::Init,
        title: "Approvals".to_owned(),
        body,
        options: init_options(&answers),
    }
}

/// Off until you say yes, so No comes first unless you said yes before.
fn telemetry_question(view: &SetupView) -> Question {
    let no = described(
        "no",
        "No",
        "Nothing is sent. argonauts telemetry on turns it on later.",
    );
    let yes = described(
        "yes",
        "Yes, send it",
        "Once a day. argonauts telemetry off or DO_NOT_TRACK=1 stops it.",
    );
    let on = view
        .current_value("telemetry.enabled")
        .and_then(toml::Value::as_bool)
        == Some(true);
    let answers = if on { [yes, no] } else { [no, yes] };
    Question {
        kind: ChoiceKind::Init,
        title: "Usage counts".to_owned(),
        body: vec![
            "Argonauts Code can send one anonymous event a day: its version, your system, the CLIs \
             and models you use with their tokens and value, and which features you use. Never \
             prompts, code, or names of files, folders, repositories, accounts or keys. \
             argonauts telemetry show prints it."
                .to_owned(),
        ],
        options: init_options(&answers),
    }
}

fn trust_question(view: &SetupView) -> Option<Question> {
    if view.untrusted.is_empty() {
        return None;
    }
    let files: Vec<String> = view
        .untrusted
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| format!(".argonauts/{}", name.to_string_lossy()))
        .collect();
    Some(Question {
        kind: ChoiceKind::Init,
        title: "This folder".to_owned(),
        body: vec![format!(
            "This repo has settings files: {}. Until you trust them, their safety settings \
             don't apply.",
            files.join(", ")
        )],
        options: init_options(&[
            described("no", "Not now", "You're asked again when they matter."),
            described("yes", "Trust them", "Their safety settings apply."),
        ]),
    })
}

fn folder_question(view: &SetupView) -> Option<Question> {
    if view.project_dir_exists {
        return None;
    }
    Some(Question {
        kind: ChoiceKind::Init,
        title: "This folder".to_owned(),
        body: vec!["Settings just for this repo live in .argonauts/.".to_owned()],
        options: init_options(&[
            described(
                "later",
                "When I first save a project setting",
                "Nothing is made now.",
            ),
            described("now", "Now", "Makes .argonauts/ in this repo."),
        ]),
    })
}

/// Asked once in each repo that has an `.argonauts/` folder, or gets one now: the pages that say
/// what goes there are written into it only after a yes.
fn guides_question(view: &SetupView, answers: &Answers) -> Option<Question> {
    let has_the_folder = view.project_dir_exists || answers.create_project_dir;
    if view.guides_answer.is_some() || !has_the_folder {
        return None;
    }
    Some(Question {
        kind: ChoiceKind::Init,
        title: "Guide pages in this repo".to_owned(),
        body: vec![
            "README pages that say what each file in .argonauts/ does. ~/.argonauts/ has them \
             either way."
                .to_owned(),
        ],
        options: init_options(&[
            described("no", "Not here", "Nothing is written into this repo."),
            described(
                "yes",
                "Write them",
                ".argonauts/README.md, and one each in workflows/, roles/ and mods/.",
            ),
        ]),
    })
}

/// Records the answer and returns the line the transcript keeps. The caller checked that the
/// question offered `value`.
pub(crate) fn apply(step: &Step, value: &str, view: &SetupView, answers: &mut Answers) -> String {
    match step {
        Step::Terms => "You read the note on your plan's terms.".to_owned(),
        Step::Agents => {
            answers.crew = view.ready().map(FoundAgent::worker).collect();
            let mut line = if answers.crew.is_empty() {
                "No agent CLI is ready yet.".to_owned()
            } else {
                let names: Vec<String> = answers.crew.iter().map(|w| view.name_of(w)).collect();
                format!("Your crew: {}.", names.join(", "))
            };
            // The question closes on the answer, so the transcript keeps how to log back in.
            let logged_out = view
                .agents
                .iter()
                .filter(|agent| agent.installed() && agent.enabled)
                .filter(|agent| agent.login == LoginState::LoggedOut);
            for agent in logged_out {
                if let Some(fix) = agent.fix() {
                    line.push_str(&format!(" {} is logged out: {fix}.", agent.name));
                }
            }
            line
        }
        Step::Servers => String::new(),
        Step::Server(server) => {
            if value == NOT_NOW {
                return format!("{} won't review your crew's work for now.", server.name());
            }
            answers.reviewers.push(ReviewerPick {
                endpoint: server.id.clone(),
                base_url: server.base_url.clone(),
                model: value.to_owned(),
            });
            format!(
                "{} on {} reviews your crew's work.",
                display_name_from_id(value),
                server.name()
            )
        }
        Step::MainAgent => {
            let agent = kept_or_new(view.current_main_agent(), value);
            let line = format!("You talk to {}.", view.name_of(&agent));
            answers.main_agent = Some(agent);
            line
        }
        Step::Workflow => {
            answers.workflow = value.to_owned();
            format!("Workflow: {value}.")
        }
        Step::Orchestrator => {
            let agent = kept_or_new(view.current_orchestrator(), value);
            let line = format!("{} orchestrates.", view.name_of(&agent));
            answers.orchestrator = Some(agent);
            line
        }
        Step::WhoDoesWhat => roles::chose_row(value),
        Step::RoleAgent(key) => roles::chose_agent(view, answers, key, value),
        Step::RoleModel(key) => roles::chose_model(view, answers, key, value),
        Step::Approvals => {
            // The question offers the four modes and, when they are different now, Keep.
            answers.approvals = approval_mode_from(value);
            match answers.approvals {
                Some(_) => format!("Approvals: {value}."),
                None => "Approvals: each approval keeps its mode.".to_owned(),
            }
        }
        Step::Telemetry => {
            answers.telemetry = Some(value == "yes");
            if value == "yes" {
                "Usage counts: one anonymous event a day.".to_owned()
            } else {
                "Usage counts: nothing is sent.".to_owned()
            }
        }
        Step::Trust => {
            answers.trust_folder = value == "yes";
            if answers.trust_folder {
                "Trusted this repo's settings files.".to_owned()
            } else {
                "This repo's settings files stay untrusted for now.".to_owned()
            }
        }
        Step::ProjectFolder => {
            answers.create_project_dir = value == "now";
            if answers.create_project_dir {
                "Made .argonauts/ here.".to_owned()
            } else {
                ".argonauts/ is made when you first save a project setting.".to_owned()
            }
        }
        Step::Guides => {
            answers.guides = Some(value == "yes");
            if value == "yes" {
                "Guide pages: written into .argonauts/ here too.".to_owned()
            } else {
                "Guide pages: only in ~/.argonauts/.".to_owned()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use argonauts_conductor::AgentStatus;
    use argonauts_core::adapter::SetupHints;

    use super::*;

    pub(crate) fn agent(id: &str, name: &str, status: AgentStatus) -> FoundAgent {
        FoundAgent {
            id: id.to_owned(),
            name: name.to_owned(),
            version: (status != AgentStatus::NotInstalled).then(|| "1.0.0".to_owned()),
            path: (status != AgentStatus::NotInstalled)
                .then(|| PathBuf::from(format!("/bin/{id}"))),
            status,
            enabled: true,
            hints: SetupHints {
                install: Some(format!("install {id}")),
                login: Some(format!("{id} login")),
                login_check: None,
            },
            login: LoginState::Unknown,
            newest_tested: None,
            unread: Vec::new(),
            builtins: Vec::new(),
            problem: None,
            lacks: Vec::new(),
            check: Default::default(),
        }
    }

    fn view(agents: Vec<FoundAgent>) -> SetupView {
        SetupView {
            agents,
            logins_known: true,
            servers: Some(Vec::new()),
            workflows: argonauts_registry::workflow_choices()
                .into_iter()
                .map(|workflow| workflow.name)
                .collect(),
            ..SetupView::default()
        }
    }

    fn values(question: &Question) -> Vec<String> {
        question
            .options
            .iter()
            .filter_map(|option| value_of(&option.action))
            .collect()
    }

    #[test]
    fn the_first_ready_preferred_agent_is_recommended() {
        let view = view(vec![
            agent("claude-code", "Claude Code", AgentStatus::NotInstalled),
            agent("codex", "Codex", AgentStatus::Tested),
            agent("antigravity-cli", "Antigravity CLI", AgentStatus::Untested),
            agent("opencode", "OpenCode", AgentStatus::Tested),
        ]);
        let question = main_agent_question(&view).unwrap();
        assert_eq!(
            values(&question),
            vec!["codex", "antigravity-cli", "opencode"]
        );
        assert!(question.options[0].recommended);
    }

    #[test]
    fn a_logged_out_agent_is_not_ready_and_shows_its_fix() {
        let mut codex = agent("codex", "Codex", AgentStatus::Tested);
        codex.login = LoginState::LoggedOut;
        let missing = agent("claude-code", "Claude Code", AgentStatus::NotInstalled);
        let view = view(vec![missing, codex]);
        let question = agents_question(&view);
        assert_eq!(
            question.body,
            vec![
                "Claude Code · not installed · install claude-code",
                "Codex 1.0.0 · logged out · codex login",
                "None is ready yet. Install one, then run /setup again.",
            ]
        );
        assert!(main_agent_question(&view).is_none());
        let kept = apply(&Step::Agents, "c", &view, &mut Answers::default());
        assert_eq!(
            kept,
            "No agent CLI is ready yet. Codex is logged out: codex login."
        );
    }

    #[test]
    fn orchestrator_comes_first_and_is_recommended_whatever_is_ready() {
        let none = view(Vec::new());
        let one = view(vec![agent("codex", "Codex", AgentStatus::Tested)]);
        let two = view(vec![
            agent("codex", "Codex", AgentStatus::Tested),
            agent("claude-code", "Claude Code", AgentStatus::Tested),
        ]);
        for view in [none, one, two] {
            let question = workflow_question(&view);
            assert_eq!(values(&question), [ORCHESTRATOR, COUNCIL, CAPTAIN]);
            assert!(question.options[0].recommended);
        }
    }

    #[test]
    fn captain_says_the_cli_you_talk_to_does_the_work_itself() {
        let question = workflow_question(&view(Vec::new()));
        let captain = question
            .options
            .iter()
            .find(|option| value_of(&option.action).as_deref() == Some(CAPTAIN))
            .unwrap();
        assert_eq!(
            captain.detail.as_deref(),
            Some(
                "The CLI you talk to does the work itself; ask in your own words for a reviewer \
                 or a workflow."
            )
        );
    }

    #[test]
    fn a_workflow_name_no_workflow_has_is_never_offered_or_recommended() {
        let mut one = view(vec![agent("codex", "Codex", AgentStatus::Tested)]);
        one.current = toml::from_str("workflow = \"retired-mode\"\n").unwrap();
        let question = workflow_question(&one);
        assert_eq!(values(&question), [ORCHESTRATOR, COUNCIL, CAPTAIN]);
        assert!(question.options[0].recommended);
        assert!(
            question.options[1..]
                .iter()
                .all(|option| !option.recommended)
        );

        let mut two = view(vec![
            agent("codex", "Codex", AgentStatus::Tested),
            agent("claude-code", "Claude Code", AgentStatus::Tested),
        ]);
        two.current = one.current.clone();
        assert_eq!(
            values(&workflow_question(&two)),
            [ORCHESTRATOR, COUNCIL, CAPTAIN]
        );
    }

    #[test]
    fn the_crew_question_reads_as_the_picture_draws_it() {
        use crate::golden::{assert_looks, drawn_rows, picture_rows, still, theme};
        let question = workflow_question(&view(vec![agent("codex", "Codex", AgentStatus::Tested)]));
        let choice = crate::choices::app_choice(
            argonauts_core::names::ChoiceId::numbered(1),
            question.kind,
            question.title,
            question.body,
            question.options,
        );
        let theme = theme();
        let lines = argonauts_tui::questions::choice_question(1, &choice).lines(&still(&theme));
        crate::golden::dump("setup_question", &lines);
        assert_eq!(
            drawn_rows(&lines),
            picture_rows(include_str!("../../tests/golden/setup_question.txt"))
        );
        assert_looks(
            &theme,
            &lines,
            include_str!("../../tests/golden/setup_question.html"),
        );
    }

    #[test]
    fn a_kept_workflow_is_named_as_the_screen_names_workflows() {
        let mut view = view(vec![agent("codex", "Codex", AgentStatus::Tested)]);
        view.current = toml::from_str("workflow = \"best-of-n\"\n").unwrap();
        let question = workflow_question(&view);
        assert_eq!(values(&question)[3], "best-of-n");
        assert_eq!(question.options[3].label, "Best-of-n");
        assert!(question.options[0].recommended);
    }

    #[test]
    fn a_rerun_preselects_the_global_values() {
        let mut view = view(vec![
            agent("claude-code", "Claude Code", AgentStatus::Tested),
            agent("codex", "Codex", AgentStatus::Tested),
        ]);
        let mut file = "crew.main = \"codex\"\nworkflow = \"best-of-n\"\n".to_owned();
        for point in APPROVAL_POINTS {
            file.push_str(&format!("[approvals.{point}]\nmode = \"auto_preview\"\n"));
        }
        view.current = toml::from_str(&file).unwrap();
        assert_eq!(values(&main_agent_question(&view).unwrap())[0], "codex");
        // The workflow step recommends Orchestrator every time, and keeps yours on offer.
        let workflows = values(&workflow_question(&view));
        assert_eq!(workflows, [ORCHESTRATOR, COUNCIL, CAPTAIN, "best-of-n"]);
        assert_eq!(
            values(&approvals_question(&view)),
            vec!["auto_preview", "auto", "autopilot", "supervised"]
        );
    }

    /// Autopilot is one answer away: picking it makes every approval a run opens autopilot,
    /// so the kinds without settings of their own follow too, and a rerun offers it first.
    #[test]
    fn autopilot_is_one_answer_for_every_approval() {
        let mut view = view(Vec::new());
        let mut answers = Answers::default();
        let line = apply(&Step::Approvals, "autopilot", &view, &mut answers);
        assert_eq!(answers.approvals, Some(ApprovalMode::Autopilot));
        assert_eq!(line, "Approvals: autopilot.");
        // What the setup writes for it: the switch, over auto.
        let mut file = "[approvals]\nautopilot = true\n".to_owned();
        for point in APPROVAL_POINTS {
            file.push_str(&format!("[approvals.{point}]\nmode = \"auto\"\n"));
        }
        view.current = toml::from_str(&file).unwrap();
        assert_eq!(values(&approvals_question(&view))[0], "autopilot");
        let settings: argonauts_core::settings::approvals::ApprovalSettings =
            toml::from_str::<toml::Value>(&file)
                .unwrap()
                .get("approvals")
                .cloned()
                .unwrap()
                .try_into()
                .unwrap();
        assert_eq!(settings.mode(), ApprovalMode::Autopilot);
    }

    #[test]
    fn mixed_approval_modes_offer_keep_first_and_keep_writes_nothing() {
        let mut view = view(Vec::new());
        view.current = toml::from_str(
            "[approvals.understand]\nmode = \"auto\"\n[approvals.merge]\nmode = \"supervised\"\n",
        )
        .unwrap();
        let question = approvals_question(&view);
        assert_eq!(
            values(&question),
            vec![KEEP, "auto", "autopilot", "supervised", "auto_preview"]
        );
        let mut answers = Answers::default();
        let line = apply(&Step::Approvals, KEEP, &view, &mut answers);
        assert_eq!(answers.approvals, None);
        assert_eq!(line, "Approvals: each approval keeps its mode.");
        apply(&Step::Approvals, "auto", &view, &mut answers);
        assert_eq!(answers.approvals, Some(ApprovalMode::Auto));
    }

    #[test]
    fn an_approval_left_out_counts_as_its_built_in_mode() {
        let mut view = view(Vec::new());
        let question = approvals_question(&view);
        assert_eq!(values(&question)[0], "auto");
        assert_eq!(question.body, vec![AUTONOMY.to_owned()]);
        view.current = toml::from_str("[approvals.understand]\nmode = \"supervised\"\n").unwrap();
        assert_eq!(values(&approvals_question(&view))[0], KEEP);
        let mut file = String::new();
        for point in APPROVAL_POINTS {
            file.push_str(&format!("[approvals.{point}]\nmode = \"supervised\"\n"));
        }
        view.current = toml::from_str(&file).unwrap();
        assert_eq!(values(&approvals_question(&view))[0], "supervised");
        view.current = toml::from_str("[approvals.understand]\nmode = \"auto_preview\"\n").unwrap();
        assert_eq!(values(&approvals_question(&view))[0], KEEP);
        view.current = toml::from_str("[approvals.merge]\nmode = \"sometimes\"\n").unwrap();
        assert_eq!(values(&approvals_question(&view))[0], KEEP);
    }

    #[test]
    fn a_rerun_keeps_the_model_of_the_cli_you_keep() {
        let mut view = view(vec![
            agent("claude-code", "Claude Code", AgentStatus::Tested),
            agent("codex", "Codex", AgentStatus::Tested),
        ]);
        view.current = toml::from_str(
            "crew.main = \"codex/gpt-5.6-luna\"\n\
             [roles.orchestrator]\nmodels = [\"claude-code/opus\"]\n",
        )
        .unwrap();
        let question = main_agent_question(&view).unwrap();
        assert_eq!(values(&question), vec!["codex", "claude-code"]);
        assert_eq!(
            question.options[0].detail.as_deref(),
            Some("1.0.0 · model gpt-5.6-luna")
        );
        let mut answers = Answers::default();
        let line = apply(&Step::MainAgent, "codex", &view, &mut answers);
        assert_eq!(
            answers.main_agent.as_ref().map(ToString::to_string),
            Some("codex/gpt-5.6-luna".to_owned())
        );
        assert_eq!(line, "You talk to Codex (gpt-5.6-luna).");

        let orchestrator = orchestrator_question(&view, &answers).unwrap();
        assert_eq!(values(&orchestrator)[0], "claude-code");
        apply(&Step::Orchestrator, "claude-code", &view, &mut answers);
        assert_eq!(
            answers.orchestrator.as_ref().map(ToString::to_string),
            Some("claude-code/opus".to_owned())
        );

        apply(&Step::MainAgent, "claude-code", &view, &mut answers);
        assert_eq!(answers.main_agent, Some(ModelRef::new("claude-code", None)));
    }

    #[test]
    fn a_server_question_keeps_n_for_not_now() {
        let server = LocalServer {
            id: "ollama".to_owned(),
            base_url: "http://localhost:11434/v1".to_owned(),
            models: (0..14).map(|n| format!("m{n}")).collect(),
        };
        let question = server_question(&server, &view(Vec::new()));
        assert_eq!(question.options[0].key, 'n');
        assert!(question.options[0].recommended);
        let keys: Vec<char> = question.options.iter().map(|option| option.key).collect();
        assert_eq!(keys.iter().filter(|key| **key == 'n').count(), 1);
        assert_eq!(keys[1], 'a');
        let mut answers = Answers::default();
        apply(&Step::Server(server), "m3", &view(Vec::new()), &mut answers);
        assert_eq!(answers.reviewers[0].model, "m3");
    }

    #[test]
    fn a_server_question_names_its_models() {
        let server = LocalServer {
            id: "ollama".to_owned(),
            base_url: "http://localhost:11434/v1".to_owned(),
            models: vec![
                "qwen3-coder:30b".to_owned(),
                "llama3.2:latest".to_owned(),
                "llama3.2".to_owned(),
            ],
        };
        let question = server_question(&server, &view(Vec::new()));
        let shown: Vec<(&str, Option<&str>)> = question.options[1..]
            .iter()
            .map(|option| (option.label.as_str(), option.detail.as_deref()))
            .collect();
        assert_eq!(
            shown,
            [
                ("Qwen3 Coder 30B", None),
                ("Llama3.2", Some("llama3.2:latest")),
                ("Llama3.2", Some("llama3.2")),
            ]
        );
        assert_eq!(values(&question)[1], "qwen3-coder:30b");
    }

    #[test]
    fn usage_counts_are_off_first_unless_you_said_yes() {
        let mut view = view(Vec::new());
        assert_eq!(values(&telemetry_question(&view)), ["no", "yes"]);
        view.current = toml::from_str("[telemetry]\nenabled = true\n").unwrap();
        assert_eq!(values(&telemetry_question(&view)), ["yes", "no"]);
        let mut answers = Answers::default();
        apply(&Step::Telemetry, "yes", &view, &mut answers);
        assert_eq!(answers.telemetry, Some(true));
    }

    #[test]
    fn trust_and_folder_are_asked_only_when_they_matter() {
        let mut view = view(Vec::new());
        view.project_dir_exists = true;
        assert!(trust_question(&view).is_none());
        assert!(folder_question(&view).is_none());
        view.untrusted
            .push(PathBuf::from("/repo/.argonauts/config.toml"));
        view.project_dir_exists = false;
        assert_eq!(values(&trust_question(&view).unwrap())[0], "no");
        assert_eq!(values(&folder_question(&view).unwrap())[0], "later");
    }

    /// Each record says what was decided, never just "Noted." or "Not now."
    #[test]
    fn the_records_say_what_was_decided() {
        let view = view(Vec::new());
        let mut answers = Answers::default();
        let mut line = |step: &Step, value: &str| apply(step, value, &view, &mut answers);
        assert_eq!(
            line(&Step::Terms, "c"),
            "You read the note on your plan's terms."
        );
        let server = LocalServer {
            id: "ollama".to_owned(),
            base_url: "http://localhost:11434/v1".to_owned(),
            models: vec!["llama3.2".to_owned()],
        };
        assert_eq!(
            line(&Step::Server(server.clone()), "llama3.2"),
            "Llama3.2 on Ollama reviews your crew's work."
        );
        let declined = line(&Step::Server(server.clone()), NOT_NOW);
        assert_eq!(
            declined,
            format!("{} won't review your crew's work for now.", server.name())
        );
        assert_eq!(
            line(&Step::Trust, "no"),
            "This repo's settings files stay untrusted for now."
        );
        assert_eq!(
            line(&Step::Trust, "yes"),
            "Trusted this repo's settings files."
        );
        assert_eq!(
            line(&Step::ProjectFolder, "later"),
            ".argonauts/ is made when you first save a project setting."
        );
    }
}
