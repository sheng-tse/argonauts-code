//! The setup's answers and the keys they write.

use std::collections::BTreeMap;

use argonauts_core::names::WorkerRef;
use argonauts_core::settings::approvals::{ApprovalMode, ApprovalSettings};
use argonauts_registry::safety_keys_in;

use super::roles;

/// The checkpoints the approvals step sets, in the file's order.
pub(crate) const APPROVAL_POINTS: &[&str] =
    &["goal", "plan", "merge", "stuck", "limit", "change_request"];

pub(crate) const PEER_CREW: &str = "council";
pub(crate) const COORDINATED_CREW: &str = "orchestrator";
pub(crate) const SOLO: &str = "solo";

/// A local model server's model, picked to review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewerPick {
    /// `ollama` or `lmstudio`.
    pub endpoint: String,
    pub base_url: String,
    pub model: String,
}

/// The answers so far. A step left out writes nothing of its own.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Answers {
    /// Every ready harness. Empty writes no `crew`.
    pub crew: Vec<WorkerRef>,
    pub reviewers: Vec<ReviewerPick>,
    pub main_agent: Option<WorkerRef>,
    pub workflow: String,
    /// Only for `orchestrator`.
    pub coordinator: Option<WorkerRef>,
    /// Who fills each other role you set in "Who does what", by its `/model` key.
    pub roles: BTreeMap<String, WorkerRef>,
    /// `None` keeps the modes your file has.
    pub approvals: Option<ApprovalMode>,
    pub trust_folder: bool,
    pub create_project_dir: bool,
}

impl Default for Answers {
    fn default() -> Self {
        Self {
            crew: Vec::new(),
            reviewers: Vec::new(),
            main_agent: None,
            workflow: PEER_CREW.to_owned(),
            coordinator: None,
            roles: BTreeMap::new(),
            approvals: Some(ApprovalMode::Auto),
            trust_folder: false,
            create_project_dir: false,
        }
    }
}

/// Each key and TOML value to write to `~/.argonauts/config.toml`, in the file's order. Without a
/// terminal no safety key is among them.
pub(crate) fn keys(answers: &Answers, interactive: bool) -> Vec<(String, String)> {
    let mut keys = Vec::new();
    if !answers.crew.is_empty() {
        keys.push(("crew".to_owned(), workers(&answers.crew)));
    }
    if let Some(agent) = &answers.main_agent {
        keys.push(("main_agent".to_owned(), string(&agent.to_string())));
    }
    keys.push(("workflow".to_owned(), string(&answers.workflow)));
    if answers.workflow == COORDINATED_CREW
        && let Some(coordinator) = &answers.coordinator
    {
        keys.push((
            "roles.orchestrator.workers".to_owned(),
            workers(std::slice::from_ref(coordinator)),
        ));
    }
    for (key, worker) in &answers.roles {
        keys.push((key.clone(), roles::toml_value(key, worker)));
    }
    // A reviewer's address is a safety key, so without a terminal the whole pick stays out.
    if interactive && !answers.reviewers.is_empty() {
        for pick in &answers.reviewers {
            let endpoint = &pick.endpoint;
            keys.push((
                format!("endpoints.{endpoint}.base_url"),
                string(&pick.base_url),
            ));
            keys.push((format!("endpoints.{endpoint}.local"), "true".to_owned()));
        }
        // A reviewer you chose later, in "Who does what", is the one written.
        if !answers.roles.contains_key(roles::REVIEWER_KEY) {
            let reviewers: Vec<WorkerRef> = answers
                .reviewers
                .iter()
                .map(|pick| WorkerRef::new(pick.endpoint.clone(), Some(pick.model.clone())))
                .collect();
            keys.push((roles::REVIEWER_KEY.to_owned(), workers(&reviewers)));
        }
    }
    // Full auto is the one switch, over auto, so turning it off later leaves auto.
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

/// The mode a checkpoint has when your file leaves it out.
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

fn workers(workers: &[WorkerRef]) -> String {
    let items = workers
        .iter()
        .map(|worker| toml::Value::String(worker.to_string()))
        .collect();
    toml::Value::Array(items).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker(id: &str) -> WorkerRef {
        WorkerRef::new(id, None)
    }

    fn answers() -> Answers {
        Answers {
            crew: vec![worker("claude-code"), worker("codex")],
            main_agent: Some(worker("claude-code")),
            ..Answers::default()
        }
    }

    fn names(keys: &[(String, String)]) -> Vec<&str> {
        keys.iter().map(|(key, _)| key.as_str()).collect()
    }

    #[test]
    fn the_recommendations_write_crew_agent_preset_and_approvals() {
        let keys = keys(&answers(), true);
        assert_eq!(
            names(&keys),
            vec![
                "crew",
                "main_agent",
                "workflow",
                "approvals.goal.mode",
                "approvals.plan.mode",
                "approvals.merge.mode",
                "approvals.stuck.mode",
                "approvals.limit.mode",
                "approvals.change_request.mode",
                "approvals.autopilot",
            ]
        );
        assert_eq!(keys[0].1, r#"["claude-code", "codex"]"#);
        assert_eq!(keys[1].1, r#""claude-code""#);
        assert_eq!(keys[2].1, r#""council""#);
        assert_eq!(keys[3].1, r#""auto""#);
        assert_eq!(keys[9].1, "false");
    }

    /// Full auto turns the one switch on over auto, so turning it off later leaves auto.
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
        assert_eq!(names(&keys), vec!["crew", "main_agent", "workflow"]);
    }

    #[test]
    fn a_reviewer_and_a_coordinator_write_their_roles() {
        let mut answers = answers();
        answers.workflow = COORDINATED_CREW.to_owned();
        answers.coordinator = Some(worker("codex"));
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
        assert_eq!(find("roles.orchestrator.workers"), Some(r#"["codex"]"#));
        assert_eq!(
            find("endpoints.ollama.base_url"),
            Some(r#""http://localhost:11434/v1""#)
        );
        assert_eq!(find("endpoints.ollama.local"), Some("true"));
        assert_eq!(
            find("roles.reviewer.workers"),
            Some(r#"["ollama/llama3.2"]"#)
        );
    }

    #[test]
    fn kept_approvals_and_a_model_are_written_as_they_were() {
        let mut answers = answers();
        answers.approvals = None;
        answers.main_agent = Some(WorkerRef::new("claude-code", Some("opus".to_owned())));
        let keys = keys(&answers, true);
        assert_eq!(names(&keys), vec!["crew", "main_agent", "workflow"]);
        assert_eq!(keys[1].1, r#""claude-code/opus""#);
    }

    #[test]
    fn nothing_ready_writes_no_crew_and_no_agent() {
        let answers = Answers {
            workflow: SOLO.to_owned(),
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
    fn every_checkpoint_the_setup_sets_is_auto_unless_your_file_says_otherwise() {
        for point in APPROVAL_POINTS {
            assert_eq!(built_in_mode(point), Some(ApprovalMode::Auto), "{point}");
        }
        // The setup doesn't ask about recipe promotion, and its built-in mode is auto too.
        assert_eq!(
            built_in_mode("workflow_promotion"),
            Some(ApprovalMode::Auto)
        );
        assert_eq!(built_in_mode("nothing"), None);
    }
}
