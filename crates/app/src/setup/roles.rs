//! Who does what: every role with who fills it now, the same rows as `/model`. Enter keeps them
//! all. Choosing a row asks which agent fills it, then which of that agent's models, and comes
//! back to the list. The coordinator's question ends with the same model question.

use argonauts_core::app::ChoiceKind;
use argonauts_core::model_catalog::display_name_from_id;
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::approvals::Decides;

use super::questions::{Question, SetupView, agent_answers, described, kept_or_new};
use super::steps::{Answers, COORDINATED_CREW};
use crate::choices::{InitAnswer, init_options};

/// Keeps every role, or one role, as it is.
pub(crate) const KEEP: &str = "keep";
/// The model an agent's CLI runs when none is named.
const CLI_DEFAULT: &str = "cli-default";
pub(crate) const COORDINATOR_KEY: &str = "roles.orchestrator.workers";
pub(crate) const REVIEWER_KEY: &str = "roles.reviewer.workers";
const DEFAULT_AGENT_KEY: &str = "main_agent";

/// One of an agent's models, as the setup offers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelOption {
    /// `None` is the CLI's own default.
    pub model: Option<String>,
    pub name: String,
    pub note: Option<String>,
}

/// The list itself.
pub(crate) fn who_does_what(view: &SetupView, answers: &Answers) -> Question {
    let mut rows = vec![described(
        KEEP,
        "Keep them all",
        "Each role keeps who fills it now.",
    )];
    rows.extend(view.role_keys.iter().map(|key| InitAnswer {
        label: label(key),
        value: key.clone(),
        detail: Some(now_words(view, answers, key)),
    }));
    Question {
        kind: ChoiceKind::Init,
        title: "Who does what".to_owned(),
        body: vec![
            "Each role the crew fills, and who fills it now. Choose one to change it.".to_owned(),
        ],
        options: init_options(&rows),
    }
}

/// Which agent fills the role at `key`. `None` when no agent is ready.
pub(crate) fn agent_question(view: &SetupView, answers: &Answers, key: &str) -> Option<Question> {
    let current = worker_for(view, answers, key);
    let agents = agent_answers(view, None, current.as_ref());
    if agents.is_empty() {
        return None;
    }
    let mut rows = vec![described(KEEP, "Keep it", &now_words(view, answers, key))];
    rows.extend(agents);
    Some(Question {
        kind: ChoiceKind::Init,
        title: format!("Who is {}", subject(key).to_lowercase()),
        body: Vec::new(),
        options: init_options(&rows),
    })
}

/// Which model of the agent just chosen for `key`. `None` when it offers only its default.
pub(crate) fn model_question(view: &SetupView, answers: &Answers, key: &str) -> Option<Question> {
    let worker = worker_for(view, answers, key)?;
    let models = view.models.get(&worker.worker)?;
    if models.len() < 2 {
        return None;
    }
    let first = worker.model.as_deref().unwrap_or(CLI_DEFAULT);
    let mut rows: Vec<InitAnswer> = models
        .iter()
        .map(|option| InitAnswer {
            label: option.name.clone(),
            value: option
                .model
                .clone()
                .unwrap_or_else(|| CLI_DEFAULT.to_owned()),
            detail: option.note.clone(),
        })
        .collect();
    rows.sort_by_key(|row| row.value != first);
    Some(Question {
        kind: ChoiceKind::Init,
        title: format!("{}'s model", subject(key)),
        body: vec![format!("It runs on {}.", view.agent_name(&worker.worker))],
        options: init_options(&rows),
    })
}

/// The list's answer: `KEEP`, or the key of the role to change.
pub(crate) fn chose_row(value: &str) -> String {
    if value == KEEP {
        return "Every role keeps who fills it.".to_owned();
    }
    format!("Choosing who is {}.", subject(value).to_lowercase())
}

/// Sets the agent for `key`, keeping the model your file names when the agent is the same.
pub(crate) fn chose_agent(
    view: &SetupView,
    answers: &mut Answers,
    key: &str,
    value: &str,
) -> String {
    if value == KEEP {
        return format!("{} stays {}.", subject(key), now_words(view, answers, key));
    }
    let worker = kept_or_new(worker_for(view, answers, key), value);
    let line = format!("{} is {}.", subject(key), worker_words(view, &worker));
    set_worker(answers, key, worker);
    line
}

pub(crate) fn chose_model(
    view: &SetupView,
    answers: &mut Answers,
    key: &str,
    value: &str,
) -> String {
    let Some(worker) = worker_for(view, answers, key) else {
        return String::new();
    };
    let model = (value != CLI_DEFAULT).then(|| value.to_owned());
    let worker = WorkerRef::new(worker.worker, model);
    let line = format!("{} runs {}.", subject(key), worker_words(view, &worker));
    set_worker(answers, key, worker);
    line
}

/// Who fills the role at `key`: what you chose in this setup, the later choice first, else
/// what your file names. A local server's model you picked to review is the reviewer until
/// you choose another here.
pub(crate) fn worker_for(view: &SetupView, answers: &Answers, key: &str) -> Option<WorkerRef> {
    match key {
        DEFAULT_AGENT_KEY => {
            return answers
                .main_agent
                .clone()
                .or_else(|| view.current_str(key)?.parse().ok());
        }
        COORDINATOR_KEY
            if answers.workflow == COORDINATED_CREW && answers.coordinator.is_some() =>
        {
            return answers.coordinator.clone();
        }
        _ => {}
    }
    if let Some(worker) = answers.roles.get(key) {
        return Some(worker.clone());
    }
    if key == REVIEWER_KEY
        && let Some(pick) = answers.reviewers.first()
    {
        return Some(WorkerRef::new(
            pick.endpoint.clone(),
            Some(pick.model.clone()),
        ));
    }
    let named = match view.current_str(key) {
        Some(one) => Some(one.to_owned()),
        None => view.current_list(key).into_iter().next(),
    };
    let named = named?;
    // Left to the judge, the row names nobody.
    if key == "approvals.decides" && named.trim().eq_ignore_ascii_case(Decides::JUDGE) {
        return None;
    }
    named.parse().ok()
}

fn set_worker(answers: &mut Answers, key: &str, worker: WorkerRef) {
    match key {
        DEFAULT_AGENT_KEY => answers.main_agent = Some(worker),
        COORDINATOR_KEY if answers.workflow == COORDINATED_CREW => {
            answers.coordinator = Some(worker)
        }
        _ => {
            answers.roles.insert(key.to_owned(), worker);
        }
    }
}

/// `Opus 5.5 on Claude Code`, or what fills the role when nobody is named.
fn now_words(view: &SetupView, answers: &Answers, key: &str) -> String {
    match worker_for(view, answers, key) {
        Some(worker) => worker_words(view, &worker),
        None => match key {
            "narrator.model" => "your main agent".to_owned(),
            "approvals.decides" => "the cheapest model from another provider".to_owned(),
            _ => "anyone in your crew".to_owned(),
        },
    }
}

fn worker_words(view: &SetupView, worker: &WorkerRef) -> String {
    let agent = view.agent_name(&worker.worker);
    match &worker.model {
        None => agent,
        Some(model) => {
            let name = view
                .models
                .get(&worker.worker)
                .and_then(|models| {
                    models
                        .iter()
                        .find(|option| option.model.as_deref() == Some(model))
                })
                .map_or_else(|| display_name_from_id(model), |option| option.name.clone());
            format!("{name} on {agent}")
        }
    }
}

/// The row's name, as `/model` writes it.
fn label(key: &str) -> String {
    match key {
        DEFAULT_AGENT_KEY => "Main agent".to_owned(),
        "narrator.model" => "narrator".to_owned(),
        "approvals.decides" => "approver".to_owned(),
        _ => key
            .strip_prefix("roles.")
            .and_then(|rest| rest.strip_suffix(".workers"))
            .unwrap_or(key)
            .to_owned(),
    }
}

/// `The reviewer`, or `Your default agent`, to start a sentence.
fn subject(key: &str) -> String {
    match key {
        DEFAULT_AGENT_KEY => "Your main agent".to_owned(),
        _ => format!("The {}", label(key)),
    }
}

/// The TOML value the role's key takes: a list for a role's workers, else one worker.
pub(crate) fn toml_value(key: &str, worker: &WorkerRef) -> String {
    let one = toml::Value::String(worker.to_string());
    if key.ends_with(".workers") {
        toml::Value::Array(vec![one]).to_string()
    } else {
        one.to_string()
    }
}

#[cfg(test)]
mod tests {
    use argonauts_conductor::AgentStatus;
    use argonauts_core::adapter::SetupHints;

    use super::*;
    use crate::setup::probe::{FoundAgent, LoginState};
    use crate::setup::questions::value_of;

    fn codex_view() -> SetupView {
        let codex = FoundAgent {
            id: "codex".to_owned(),
            name: "Codex".to_owned(),
            version: Some("0.158.0".to_owned()),
            path: Some("/bin/codex".into()),
            status: AgentStatus::Tested,
            enabled: true,
            hints: SetupHints::default(),
            login: LoginState::Unknown,
            newest_tested: None,
            unread: Vec::new(),
            builtins: Vec::new(),
        };
        let mut view = SetupView {
            agents: vec![codex],
            logins_known: true,
            ..SetupView::default()
        };
        view.models.insert(
            "codex".to_owned(),
            vec![
                ModelOption {
                    model: None,
                    name: "Default".to_owned(),
                    note: Some("runs gpt-5.6".to_owned()),
                },
                ModelOption {
                    model: Some("gpt-5.6-luna".to_owned()),
                    name: "GPT-5.6 Luna".to_owned(),
                    note: None,
                },
            ],
        );
        view
    }

    #[test]
    fn the_coordinators_model_is_one_of_its_agents_and_sets_it() {
        let view = codex_view();
        let mut answers = Answers {
            workflow: COORDINATED_CREW.to_owned(),
            coordinator: Some(WorkerRef::new("codex", None)),
            ..Answers::default()
        };
        let question = model_question(&view, &answers, COORDINATOR_KEY).unwrap();
        assert_eq!(question.title, "The orchestrator's model");
        let values: Vec<String> = question
            .options
            .iter()
            .filter_map(|option| value_of(&option.action))
            .collect();
        assert_eq!(values, [CLI_DEFAULT, "gpt-5.6-luna"]);
        let line = chose_model(&view, &mut answers, COORDINATOR_KEY, "gpt-5.6-luna");
        assert_eq!(line, "The orchestrator runs GPT-5.6 Luna on Codex.");
        assert_eq!(
            answers.coordinator,
            Some(WorkerRef::new("codex", Some("gpt-5.6-luna".to_owned())))
        );
        // Asked again, the model it runs now comes first.
        let again = model_question(&view, &answers, COORDINATOR_KEY).unwrap();
        assert_eq!(
            value_of(&again.options[0].action).as_deref(),
            Some("gpt-5.6-luna")
        );
    }

    #[test]
    fn an_agent_with_only_its_default_asks_no_model() {
        let mut view = codex_view();
        view.models.get_mut("codex").unwrap().truncate(1);
        let answers = Answers {
            roles: [(
                "roles.reviewer.workers".to_owned(),
                WorkerRef::new("codex", None),
            )]
            .into(),
            ..Answers::default()
        };
        assert!(model_question(&view, &answers, "roles.reviewer.workers").is_none());
    }

    #[test]
    fn a_local_reviewer_shows_as_the_reviewer_and_a_later_choice_wins() {
        use crate::setup::steps::{ReviewerPick, keys};
        let view = codex_view();
        let mut answers = Answers {
            reviewers: vec![ReviewerPick {
                endpoint: "ollama".to_owned(),
                base_url: "http://localhost:11434".to_owned(),
                model: "qwen3-coder".to_owned(),
            }],
            ..Answers::default()
        };
        // The Servers step's pick is the reviewer's "now", not your file's.
        assert_eq!(
            worker_for(&view, &answers, REVIEWER_KEY),
            Some(WorkerRef::new("ollama", Some("qwen3-coder".to_owned())))
        );
        // Choosing another reviewer afterwards is what gets written, once.
        chose_agent(&view, &mut answers, REVIEWER_KEY, "codex");
        let written: Vec<(String, String)> = keys(&answers, true)
            .into_iter()
            .filter(|(key, _)| key == REVIEWER_KEY)
            .collect();
        assert_eq!(
            written,
            [(REVIEWER_KEY.to_owned(), r#"["codex"]"#.to_owned())]
        );
    }

    /// The settings template writes `decides = "judge"`, which names nobody: the approver's
    /// row reads as the judge the policy picks, never as a CLI called judge.
    #[test]
    fn an_approver_left_to_the_judge_names_nobody() {
        const KEY: &str = "approvals.decides";
        let mut view = codex_view();
        let answers = Answers::default();
        view.current = toml::from_str("[approvals]\ndecides = \"judge\"\n").unwrap();
        assert_eq!(worker_for(&view, &answers, KEY), None);
        assert_eq!(
            now_words(&view, &answers, KEY),
            "the cheapest model from another provider"
        );
        view.current = toml::from_str("[approvals]\ndecides = \"codex/gpt-5.6-luna\"\n").unwrap();
        let luna = WorkerRef::new("codex", Some("gpt-5.6-luna".to_owned()));
        assert_eq!(worker_for(&view, &answers, KEY), Some(luna));
        assert_eq!(now_words(&view, &answers, KEY), "GPT-5.6 Luna on Codex");
    }

    #[test]
    fn rows_read_as_model_names_them() {
        assert_eq!(label("main_agent"), "Main agent");
        assert_eq!(label("roles.reviewer.workers"), "reviewer");
        assert_eq!(label("approvals.decides"), "approver");
        assert_eq!(subject("roles.phase-lead.workers"), "The phase-lead");
        assert_eq!(subject("main_agent"), "Your main agent");
    }

    #[test]
    fn a_roles_key_takes_a_list_and_the_others_one_worker() {
        let worker = WorkerRef::new("codex", Some("gpt-5.6".to_owned()));
        assert_eq!(
            toml_value("roles.reviewer.workers", &worker),
            r#"["codex/gpt-5.6"]"#
        );
        assert_eq!(toml_value("narrator.model", &worker), r#""codex/gpt-5.6""#);
    }
}
