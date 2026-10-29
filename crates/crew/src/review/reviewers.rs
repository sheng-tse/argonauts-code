//! Who reviews: the task's reviewers from the ring, never the worker, other providers than the
//! worker's while `review.other_provider` holds, and never the worker's notes. A task that names
//! no reviewers, as one a script made does, takes the crew's. Each review says whether you chose
//! its reviewer or code picked it, and code never picks whoever built the work.

use argonauts_core::TaskId;
use argonauts_core::events::task::{PickedBy, TaskKind};
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::settings::Settings;

use crate::roles;
use crate::state::RunState;
use crate::state::tasks::TaskEntry;

/// Why a task has nobody to review it when it names none and the crew has none either.
pub const NONE_SET: &str = "No reviewer is set for this task";

/// Who chose `model` to review or judge in `role`. You did when your settings, a `--set` or your
/// request name it for the role (`roles.<role>.models`, and a judge you named none for follows
/// the reviewers you named), or when a step of the workflow names it (`named_by_step`). Words
/// that keep the crew to one provider ("use only Claude") name its reviewer that way too, and
/// a bare "use the crew" names none. Code did otherwise, picking from the crew, and so did any
/// fallback that took a reviewer's place.
pub fn picked_by(
    settings: &Settings,
    role: RoleName,
    model: &ModelRef,
    named_by_step: bool,
) -> PickedBy {
    let models = |role: RoleName| {
        settings
            .roles
            .get(&role)
            .map(|settings| settings.models.as_slice())
            .unwrap_or_default()
    };
    let names = |role: RoleName| models(role).iter().any(|named| same(named, model));
    let follows_reviewers = role == RoleName::Judge && models(RoleName::Judge).is_empty();
    let yours = named_by_step || names(role) || (follows_reviewers && names(RoleName::Reviewer));
    if yours { PickedBy::You } else { PickedBy::Code }
}

/// Whether `asked` and the model that answered are one: the same, or a bare CLI answered by the
/// model it runs. Anything else answered in its place, as a fallback.
pub fn answered_as_asked(asked: &ModelRef, answered: &ModelRef) -> bool {
    same(asked, answered)
}

/// Whether a review or a ruling of the builder's work stands apart from it: you chose who gave
/// it, or they are of another provider than the builder's. A provider nobody knows can't be
/// shown to be another, so it doesn't stand apart on that ground. A task's reviews and rulings
/// count toward the merge by this.
pub fn counts(picked_by: Option<PickedBy>, theirs: Option<&str>, builders: Option<&str>) -> bool {
    picked_by == Some(PickedBy::You)
        || matches!((theirs, builders), (Some(theirs), Some(builders)) if theirs != builders)
}

fn same(named: &ModelRef, model: &ModelRef) -> bool {
    named == model || (named.model.is_none() && named.agent == model.agent)
}

/// Code's picks to review or judge the work `builder` made, never `builder` itself: `models`
/// without it, in their order. Empty when nobody else is there, so the work waits with its
/// branch kept rather than being reviewed by whoever made it. Every automatic pick goes
/// through here, a workflow script's included; a model you chose is yours to keep.
pub fn not_the_builder(
    models: &[ModelRef],
    builder: &ModelRef,
    providers: &dyn Fn(&ModelRef) -> String,
) -> Vec<ModelRef> {
    models
        .iter()
        .filter(|model| {
            roles::conflict(RoleName::Reviewer, model, Some(builder), providers).is_none()
        })
        .cloned()
        .collect()
}

/// Why `task` has no reviewer once its builder, and under `other_provider` its builder's
/// provider, are left out.
pub fn nobody_left(task: &str, other_provider: bool) -> String {
    if other_provider {
        format!("Nobody outside its builder's provider is left to review {task}")
    } else {
        format!("Nobody but its builder is left to review {task}")
    }
}

/// The task's reviewers, skipping the worker and, under `other_provider`, any reviewer now of the
/// worker's provider (after a fallback, say). A task that names none takes `crew`, the crew's
/// reviewers, without the providers whose commits are on the task. `providers` gives a worker's
/// model provider from the registry.
pub fn for_task(
    state: &RunState,
    task: &TaskId,
    providers: &dyn Fn(&ModelRef) -> String,
    other_provider: bool,
    crew: &[ModelRef],
) -> Vec<ModelRef> {
    let Some(record) = state.tasks.get(task) else {
        return Vec::new();
    };
    if takes_the_crews(record) {
        return without_authors(crew, record, providers, other_provider);
    }
    let worker = record.worker.as_ref().or(record.spec.worker.as_ref());
    eligible(&record.spec.reviewers, worker, providers, other_provider)
}

/// Whether nobody could review the task whatever its worker: it names no reviewers and the
/// crew has none it could take.
pub fn none_set(state: &RunState, task: &TaskId, crew: &[ModelRef]) -> bool {
    state.tasks.get(task).is_some_and(|record| {
        record.spec.reviewers.is_empty() && (crew.is_empty() || !takes_the_crews(record))
    })
}

/// A task that names no reviewers takes the crew's, except a Captain's, whose review is its own.
fn takes_the_crews(record: &TaskEntry) -> bool {
    record.spec.reviewers.is_empty() && record.spec.kind != TaskKind::Captain
}

/// `crew` without the authors of the task's commits and, under `other_provider`, their
/// providers. A try that committed nothing isn't an author, so it doesn't keep its provider out.
fn without_authors(
    crew: &[ModelRef],
    record: &TaskEntry,
    providers: &dyn Fn(&ModelRef) -> String,
    other_provider: bool,
) -> Vec<ModelRef> {
    let authors: Vec<&ModelRef> = record
        .commits
        .iter()
        .filter(|commit| commit.sha.is_some())
        .filter_map(|commit| commit.by.as_ref())
        .collect();
    let authored: Vec<String> = authors.iter().map(|author| providers(author)).collect();
    let mut kept: Vec<ModelRef> = Vec::new();
    for reviewer in crew {
        let own = authors.contains(&reviewer);
        let same_provider = other_provider && authored.contains(&providers(reviewer));
        if !own && !same_provider && !kept.contains(reviewer) {
            kept.push(reviewer.clone());
        }
    }
    kept
}

/// `reviewers` without the worker, the worker's provider under `other_provider`, or repeats.
pub fn eligible(
    reviewers: &[ModelRef],
    worker: Option<&ModelRef>,
    providers: &dyn Fn(&ModelRef) -> String,
    other_provider: bool,
) -> Vec<ModelRef> {
    let worker_provider = worker.map(providers);
    let mut kept: Vec<ModelRef> = Vec::new();
    for reviewer in reviewers {
        let own = Some(reviewer) == worker;
        let same_provider =
            other_provider && worker_provider.as_deref() == Some(providers(reviewer).as_str());
        if !own && !same_provider && !kept.contains(reviewer) {
            kept.push(reviewer.clone());
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(worker: &ModelRef) -> String {
        match worker.agent.as_str() {
            "claude-code" => "claude",
            "codex" | "opencode" => "gpt",
            _ => "gemini",
        }
        .to_owned()
    }

    fn workers(names: &[&str]) -> Vec<ModelRef> {
        names.iter().map(|n| n.parse().unwrap()).collect()
    }

    #[test]
    fn reviewers_of_the_workers_provider_are_skipped() {
        // After a fallback to codex, the reviewer of Codex's own provider can't review.
        let reviewers = workers(&["opencode", "antigravity-cli", "codex", "antigravity-cli"]);
        let worker: ModelRef = "codex".parse().unwrap();
        let kept = eligible(&reviewers, Some(&worker), &provider, true);
        assert_eq!(kept, workers(&["antigravity-cli"]));
        // With review across providers off, only the worker itself is skipped.
        let kept = eligible(&reviewers, Some(&worker), &provider, false);
        assert_eq!(kept, workers(&["opencode", "antigravity-cli"]));
    }

    #[test]
    fn with_no_worker_every_distinct_reviewer_stays() {
        let reviewers = workers(&["codex", "codex", "claude-code"]);
        assert_eq!(
            eligible(&reviewers, None, &provider, true),
            workers(&["codex", "claude-code"])
        );
    }

    /// A reviewer you named, in your settings or your request, or by keeping the crew to one
    /// provider, or that a workflow step names, is yours; code picked any other, and a judge you
    /// named none for follows the reviewers you named.
    #[test]
    fn a_review_says_whether_you_chose_its_reviewer() {
        use argonauts_core::settings::roles::RoleSettings;

        let codex: ModelRef = "codex".parse().unwrap();
        let luna: ModelRef = "codex/gpt-5.6-luna".parse().unwrap();
        let claude: ModelRef = "claude-code/haiku".parse().unwrap();
        let mut settings = Settings::default();
        assert_eq!(
            picked_by(&settings, RoleName::Reviewer, &codex, false),
            PickedBy::Code
        );
        assert_eq!(
            picked_by(&settings, RoleName::Reviewer, &codex, true),
            PickedBy::You
        );
        settings.roles.insert(
            RoleName::Reviewer,
            RoleSettings {
                models: vec![codex.clone()],
                ..RoleSettings::default()
            },
        );
        for role in [RoleName::Reviewer, RoleName::Judge] {
            // A bare CLI you named counts for the model it runs.
            assert_eq!(picked_by(&settings, role, &luna, false), PickedBy::You);
            assert_eq!(picked_by(&settings, role, &claude, false), PickedBy::Code);
        }
        // "Use only Claude" names its reviewer as your settings do; "use the crew" names none.
        settings.roles.insert(
            RoleName::Reviewer,
            RoleSettings {
                models: vec![claude.clone()],
                ..RoleSettings::default()
            },
        );
        assert_eq!(
            picked_by(&settings, RoleName::Reviewer, &claude, false),
            PickedBy::You
        );
        assert_eq!(
            picked_by(&settings, RoleName::Reviewer, &codex, false),
            PickedBy::Code
        );
        assert!(answered_as_asked(&codex, &luna));
        assert!(!answered_as_asked(&luna, &claude));
    }

    /// Code never picks the builder to review its own work, and with nobody else there is
    /// nobody to pick.
    #[test]
    fn an_automatic_pick_is_never_the_builder() {
        let builder: ModelRef = "claude-code/haiku".parse().unwrap();
        let models = workers(&["claude-code/haiku", "claude-code/sonnet", "codex"]);
        assert_eq!(
            not_the_builder(&models, &builder, &provider),
            workers(&["claude-code/sonnet", "codex"])
        );
        assert!(not_the_builder(&workers(&["claude-code/haiku"]), &builder, &provider).is_empty());
        assert_eq!(
            nobody_left("task-001", false),
            "Nobody but its builder is left to review task-001"
        );
    }

    /// A word on the builder's work counts from another provider, or from one you chose
    /// whatever their provider, and never on an unknown provider alone.
    #[test]
    fn a_word_counts_from_another_provider_or_from_one_you_chose() {
        let code = Some(PickedBy::Code);
        let you = Some(PickedBy::You);
        assert!(counts(code, Some("openai"), Some("anthropic")));
        assert!(!counts(code, Some("anthropic"), Some("anthropic")));
        assert!(counts(you, Some("anthropic"), Some("anthropic")));
        assert!(!counts(None, Some("anthropic"), Some("anthropic")));
        assert!(!counts(code, None, Some("anthropic")));
        assert!(!counts(code, Some("openai"), None));
        assert!(counts(you, None, None));
    }

    #[test]
    fn an_unknown_task_has_no_reviewers() {
        let other = TaskId::new("task-009").unwrap();
        let crew = workers(&["codex"]);
        assert!(for_task(&RunState::default(), &other, &provider, true, &crew).is_empty());
    }

    /// Claude Code built the task, then Codex tried again and committed nothing: Codex isn't an
    /// author, so it and agy may review, and Claude Code may not.
    #[test]
    fn a_task_that_names_no_reviewers_takes_the_crews_without_its_authors() {
        use argonauts_core::{CommitInfo, DiffStat};

        let task = TaskId::new("task-001").unwrap();
        let mut record = TaskEntry::new(crate::testutil::task_spec("task-001", &[], None));
        let commit = |by: &str, sha: Option<&str>| CommitInfo {
            branch: "argonauts/run/task-001".to_owned(),
            sha: sha.map(str::to_owned),
            message: String::new(),
            stat: DiffStat::default(),
            by: Some(by.parse().unwrap()),
        };
        record.commits = vec![
            commit("claude-code", Some("bee47e62")),
            commit("codex", None),
        ];
        record.worker = Some("codex".parse().unwrap());
        let mut state = RunState::default();
        state.tasks.by_id.insert(task.clone(), record);
        let crew = workers(&["claude-code", "codex", "antigravity-cli"]);
        assert_eq!(
            for_task(&state, &task, &provider, true, &crew),
            workers(&["codex", "antigravity-cli"])
        );
        assert!(!none_set(&state, &task, &crew));
        assert!(none_set(&state, &task, &[]));
    }
}
