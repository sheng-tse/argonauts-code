//! The setup's answers and the keys they write.

use std::collections::BTreeMap;

use argonauts_core::names::ModelRef;
use argonauts_core::settings::approvals::{ApprovalMode, ApprovalSettings};
use argonauts_registry::safety_keys_in;

use super::roles;

/// The approvals the approvals step sets, in the file's order.
pub(crate) const APPROVAL_POINTS: &[&str] = &[
    "understand",
    "plan",
    "merge",
    "stuck",
    "limit",
    "change_request",
];

pub(crate) const COUNCIL: &str = "council";
pub(crate) const ORCHESTRATOR: &str = "orchestrator";
pub(crate) const CAPTAIN: &str = "captain";

/// A local model server's model, picked to review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewerPick {
    /// The runtime's id, such as `ollama`.
    pub endpoint: String,
    pub base_url: String,
    pub model: String,
}

/// The answers so far. A step left out writes nothing of its own.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Answers {
    /// Every ready CLI. Empty writes no `crew`.
    pub crew: Vec<ModelRef>,
    pub reviewers: Vec<ReviewerPick>,
    /// llama-server and vLLM servers that said what they are, as (runtime id, base URL). Their
    /// ports are common, so their tables are written for sessions to reach them.
    pub confirmed: Vec<(String, String)>,
    pub main_agent: Option<ModelRef>,
    pub workflow: String,
    /// Only for `orchestrator`.
    pub orchestrator: Option<ModelRef>,
    /// Who fills each other role you set in "Who does what", by its `/model` key.
    pub roles: BTreeMap<String, ModelRef>,
    /// `None` keeps the modes your file has.
    pub approvals: Option<ApprovalMode>,
    pub trust_folder: bool,
}

impl Default for Answers {
    fn default() -> Self {
        Self {
            crew: Vec::new(),
            reviewers: Vec::new(),
            confirmed: Vec::new(),
            main_agent: None,
            workflow: ORCHESTRATOR.to_owned(),
            orchestrator: None,
            roles: BTreeMap::new(),
            approvals: Some(ApprovalMode::Auto),
            trust_folder: false,
        }
    }
}

/// Each key and TOML value to write to `~/.argonauts/config.toml`, in the file's order. Without a
/// terminal no safety key is among them.
pub(crate) fn keys(answers: &Answers, interactive: bool) -> Vec<(String, String)> {
    let mut keys = Vec::new();
    if let Some(agent) = &answers.main_agent {
        keys.push(("crew.main".to_owned(), string(&agent.to_string())));
    }
    // Each list is written out, since the main agent sits in the same table.
    if !answers.crew.is_empty() {
        let everyone = models(&answers.crew);
        for list in ["worker", "reviewer", "judge"] {
            keys.push((format!("crew.{list}"), everyone.clone()));
        }
    }
    keys.push(("workflow".to_owned(), string(&answers.workflow)));
    if answers.workflow == ORCHESTRATOR
        && let Some(orchestrator) = &answers.orchestrator
    {
        keys.push((
            "roles.orchestrator.models".to_owned(),
            models(std::slice::from_ref(orchestrator)),
        ));
    }
    for (key, worker) in &answers.roles {
        keys.push((key.clone(), roles::toml_value(key, worker)));
    }
    // A server's address is a safety key, so without a terminal it stays out.
    if interactive {
        let picked = answers
            .reviewers
            .iter()
            .map(|pick| (&pick.endpoint, &pick.base_url));
        let confirmed = answers.confirmed.iter().map(|(id, url)| (id, url));
        let mut written = Vec::new();
        for (endpoint, base_url) in picked.chain(confirmed) {
            if written.contains(&endpoint) {
                continue;
            }
            written.push(endpoint);
            keys.push((format!("endpoints.{endpoint}.base_url"), string(base_url)));
            keys.push((format!("endpoints.{endpoint}.local"), "true".to_owned()));
        }
    }
    if interactive && !answers.reviewers.is_empty() {
        // A reviewer you chose later, in "Who does what", is the one written.
        if !answers.roles.contains_key(roles::REVIEWER_KEY) {
            let reviewers: Vec<ModelRef> = answers
                .reviewers
                .iter()
                .map(|pick| ModelRef::new(pick.endpoint.clone(), Some(pick.model.clone())))
                .collect();
            keys.push((roles::REVIEWER_KEY.to_owned(), models(&reviewers)));
        }
    }
    // Autopilot is the one switch, over auto, so turning it off later leaves auto.
    if let Some(mode) = answers.approvals {
        let autopilot = mode == ApprovalMode::Autopilot;
        let each = if autopilot { ApprovalMode::Auto } else { mode };
        for point in APPROVAL_POINTS {
            keys.push((
                format!("approvals.{point}.mode"),
                string(approval_mode_name(each)),
            ));
        }
        keys.push(("approvals.autopilot".to_owned(), autopilot.to_string()));
    }
    if !interactive {
        keys.retain(|(key, value)| safety_keys_in(key, value).is_empty());
    }
    keys
}

pub(crate) fn approval_mode_name(mode: ApprovalMode) -> &'static str {
    match mode {
        ApprovalMode::Supervised => "supervised",
        ApprovalMode::AutoPreview => "auto_preview",
        ApprovalMode::Auto => "auto",
        ApprovalMode::Autopilot => "autopilot",
    }
}

/// The mode an approval has when your file leaves it out.
pub(crate) fn built_in_mode(point: &str) -> Option<ApprovalMode> {
    let defaults = toml::Value::try_from(ApprovalSettings::default()).ok()?;
    let mode = defaults.get(point)?.get("mode")?.as_str()?;
    approval_mode_from(mode)
}

pub(crate) fn approval_mode_from(name: &str) -> Option<ApprovalMode> {
    [
        ApprovalMode::Supervised,
        ApprovalMode::AutoPreview,
        ApprovalMode::Auto,
        ApprovalMode::Autopilot,
    ]
    .into_iter()
    .find(|mode| approval_mode_name(*mode) == name)
}

fn string(text: &str) -> String {
    toml::Value::String(text.to_owned()).to_string()
}

fn models(models: &[ModelRef]) -> String {
    let items = models
        .iter()
        .map(|model| toml::Value::String(model.to_string()))
        .collect();
    toml::Value::Array(items).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker(id: &str) -> ModelRef {
        ModelRef::new(id, None)
    }

    fn answers() -> Answers {
        Answers {
            crew: vec![worker("claude-code"), worker("codex")],
            main_agent: Some(worker("claude-code")),
            ..Answers::default()
        }
    }

    const CREW_KEYS: &[&str] = &["crew.main", "crew.worker", "crew.reviewer", "crew.judge"];

    fn names(keys: &[(String, String)]) -> Vec<&str> {
        keys.iter().map(|(key, _)| key.as_str()).collect()
    }

    #[test]
    fn the_recommendations_write_crew_agent_workflow_and_approvals() {
        let keys = keys(&answers(), true);
        assert_eq!(
            names(&keys),
            vec![
                "crew.main",
                "crew.worker",
                "crew.reviewer",
                "crew.judge",
                "workflow",
                "approvals.understand.mode",
                "approvals.plan.mode",
                "approvals.merge.mode",
                "approvals.stuck.mode",
                "approvals.limit.mode",
                "approvals.change_request.mode",
                "approvals.autopilot",
            ]
        );
        assert_eq!(keys[0].1, r#""claude-code""#);
        assert_eq!(keys[1].1, r#"["claude-code", "codex"]"#);
        assert_eq!(keys[4].1, r#""orchestrator""#);
        assert_eq!(keys[5].1, r#""auto""#);
        assert_eq!(keys[11].1, "false");
    }

    /// Autopilot turns the one switch on over auto, so turning it off later leaves auto.
    #[test]
    fn autopilot_writes_the_switch_over_auto() {
        let mut answers = answers();
        answers.approvals = Some(ApprovalMode::Autopilot);
        let keys = keys(&answers, true);
        let value = |key: &str| {
            keys.iter()
                .find(|(written, _)| written == key)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(value("approvals.autopilot"), Some("true"));
        for point in APPROVAL_POINTS {
            assert_eq!(
                value(&format!("approvals.{point}.mode")),
                Some(r#""auto""#),
                "{point}"
            );
        }
    }

    #[test]
    fn without_a_terminal_no_safety_key_is_written() {
        let mut answers = answers();
        answers.reviewers.push(ReviewerPick {
            endpoint: "ollama".to_owned(),
            base_url: "http://localhost:11434/v1".to_owned(),
            model: "llama3.2".to_owned(),
        });
        let keys = keys(&answers, false);
        assert_eq!(names(&keys), [CREW_KEYS, &["workflow"]].concat());
    }

    #[test]
    fn a_reviewer_and_a_orchestrator_write_their_roles() {
        let mut answers = answers();
        answers.workflow = ORCHESTRATOR.to_owned();
        answers.orchestrator = Some(worker("codex"));
        answers.reviewers.push(ReviewerPick {
            endpoint: "ollama".to_owned(),
            base_url: "http://localhost:11434/v1".to_owned(),
            model: "llama3.2".to_owned(),
        });
        let keys = keys(&answers, true);
        let find = |name: &str| {
            keys.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(find("roles.orchestrator.models"), Some(r#"["codex"]"#));
        assert_eq!(
            find("endpoints.ollama.base_url"),
            Some(r#""http://localhost:11434/v1""#)
        );
        assert_eq!(find("endpoints.ollama.local"), Some("true"));
        assert_eq!(
            find("roles.reviewer.models"),
            Some(r#"["ollama/llama3.2"]"#)
        );
    }

    #[test]
    fn a_confirmed_llama_server_gets_its_table_with_or_without_a_pick() {
        let mut answers = answers();
        answers
            .confirmed
            .push(("llamacpp".to_owned(), "http://127.0.0.1:8080/v1".to_owned()));
        let keys = keys(&answers, true);
        let find = |name: &str| {
            keys.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(
            find("endpoints.llamacpp.base_url"),
            Some(r#""http://127.0.0.1:8080/v1""#)
        );
        assert_eq!(find("endpoints.llamacpp.local"), Some("true"));
        assert_eq!(find("roles.reviewer.models"), None);
        // An address is a safety key, which needs a terminal.
        assert!(
            super::keys(&answers, false)
                .iter()
                .all(|(key, _)| !key.starts_with("endpoints."))
        );
    }

    #[test]
    fn kept_approvals_and_a_model_are_written_as_they_were() {
        let mut answers = answers();
        answers.approvals = None;
        answers.main_agent = Some(ModelRef::new("claude-code", Some("opus".to_owned())));
        let keys = keys(&answers, true);
        assert_eq!(names(&keys), [CREW_KEYS, &["workflow"]].concat());
        assert_eq!(keys[0].1, r#""claude-code/opus""#);
    }

    #[test]
    fn nothing_ready_writes_no_crew_and_no_agent() {
        let answers = Answers {
            workflow: CAPTAIN.to_owned(),
            ..Answers::default()
        };
        let keys = keys(&answers, false);
        assert_eq!(names(&keys), vec!["workflow"]);
    }

    #[test]
    fn approval_modes_round_trip_their_names() {
        for mode in [
            ApprovalMode::Supervised,
            ApprovalMode::AutoPreview,
            ApprovalMode::Auto,
            ApprovalMode::Autopilot,
        ] {
            assert_eq!(approval_mode_from(approval_mode_name(mode)), Some(mode));
        }
        assert_eq!(approval_mode_from("sometimes"), None);
    }

    #[test]
    fn every_approval_the_setup_sets_is_auto_unless_your_file_says_otherwise() {
        for point in APPROVAL_POINTS {
            assert_eq!(built_in_mode(point), Some(ApprovalMode::Auto), "{point}");
        }
        // The setup doesn't ask about learned workflow promotion, and its built-in mode is auto
        // too.
        assert_eq!(
            built_in_mode("workflow_promotion"),
            Some(ApprovalMode::Auto)
        );
        assert_eq!(built_in_mode("nothing"), None);
    }
}
