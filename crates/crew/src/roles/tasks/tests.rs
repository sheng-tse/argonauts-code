use std::collections::BTreeMap;

use argonauts_core::runs::events::run::{
    CheckedInstruction, InstructionOutcome, OverrideRecord, RunMode, RunStarted,
};

use super::*;

const CLAUDE: &str = "claude-code";
const CODEX: &str = "codex";
const AGY: &str = "antigravity-cli";

fn worker(text: &str) -> ModelRef {
    text.parse().unwrap()
}

fn workers(texts: &[&str]) -> Vec<ModelRef> {
    texts.iter().map(|text| worker(text)).collect()
}

fn provider(worker: &ModelRef) -> String {
    match worker.agent.as_str() {
        "claude-code" => "claude",
        "codex" | "opencode" => "gpt",
        _ => "gemini",
    }
    .to_owned()
}

/// The crew of the orchestrated sample repo: Claude Code, Codex, and agy.
fn settings(building: &[&str], reviewers: &[&str]) -> Settings {
    let mut settings = Settings {
        crew: workers(&[CLAUDE, CODEX, AGY]).into(),
        ..Settings::default()
    };
    settings.review.per_task = 1;
    settings.review.other_provider = true;
    settings.roles.entry(RoleName::Worker).or_default().models = workers(building);
    settings.roles.entry(RoleName::Reviewer).or_default().models = workers(reviewers);
    settings
}

fn layers(workers: Layer, reviewers: Layer) -> Layers {
    Layers {
        workers,
        reviewers,
        ..Layers::default()
    }
}

/// "Have codex orchestrate and gemini review it": the request's words pick the reviewer over
/// the repo's pin to Codex, and leave the workers to the other two. Every rule holds.
#[test]
fn the_requests_reviewer_and_the_rest_as_workers_hold() {
    let settled = settle(
        &settings(&[CLAUDE, CODEX], &[AGY]),
        layers(Layer::Settings, Layer::Words),
        &provider,
    );
    assert_eq!(settled.workers, workers(&[CLAUDE, CODEX]));
    assert_eq!(settled.reviewers, workers(&[AGY]));
    assert_eq!(settled.relaxed, None);
    assert!(settled.left_out.is_empty());
    let lines = settled.lines(&provider);
    assert_eq!(
        lines,
        [
            "`claude-code` is Claude Code, from claude: it may own tasks, not review them.",
            "`codex` is Codex, from gpt: it may own tasks, not review them.",
            "`antigravity-cli` is agy, from gemini: it may review tasks, not \
             own them.",
        ]
    );
    assert_eq!(
        settled.review_line(),
        "Each task gets 1 reviewer, never its worker, each from another provider than its worker."
    );
    let same = settings(&[CLAUDE, CODEX], &[AGY]);
    assert!(matches!(settled.apply(&same), Cow::Borrowed(_)));
}

/// The only reviewer you allowed can't own a task, since nobody reviews their own work, and the
/// planner hears why in plain words.
#[test]
fn the_only_reviewer_is_left_out_of_the_workers() {
    let settled = settle(&settings(&[], &[AGY]), Layers::default(), &provider);
    assert_eq!(settled.workers, workers(&[CLAUDE, CODEX]));
    assert_eq!(settled.relaxed, None);
    let why = "agy is the only reviewer you allowed, so agy can't own a task";
    assert_eq!(settled.left_out, [(worker(AGY), why.to_owned())]);
    assert_eq!(settled.why_not_worker(&worker(AGY)), why);
    assert!(
        settled
            .lines(&provider)
            .iter()
            .any(|line| line.ends_with(&format!("may review tasks, not own them. {why}."))),
        "{:?}",
        settled.lines(&provider)
    );
    // Dividing the work goes by the workers that fit.
    let base = settings(&[], &[AGY]);
    let applied = settled.apply(&base);
    assert_eq!(
        applied.roles[&RoleName::Worker].models,
        workers(&[CLAUDE, CODEX])
    );
}

/// agy alone may own and alone may review: the rules can't all hold. The workers'
/// pin is from your settings and the reviewers' from your request, so the workers give way, and
/// one line says so.
#[test]
fn the_lowest_rule_gives_way_and_says_so() {
    let settled = settle(
        &settings(&[AGY], &[AGY]),
        layers(Layer::Settings, Layer::Words),
        &provider,
    );
    assert_eq!(settled.workers, workers(&[CLAUDE, CODEX]));
    assert_eq!(settled.reviewers, workers(&[AGY]));
    let relaxed = settled.relaxed.clone().expect("a rule gave way");
    let mut lines = relaxed.lines();
    assert_eq!(
        lines.next(),
        Some(
            "Your rules for who owns and reviews tasks can't all hold, so the one from your \
             settings gives way: any of the crew may own a task, not only agy."
        )
    );
    assert_eq!(
        lines.next(),
        Some("agy is the only reviewer you allowed, so agy can't own a task.")
    );

    // The other way round, the reviewers' pin is the lower one.
    let settled = settle(
        &settings(&[AGY], &[AGY]),
        layers(Layer::Words, Layer::Settings),
        &provider,
    );
    assert_eq!(settled.workers, workers(&[AGY]));
    assert_eq!(settled.reviewers, workers(&[AGY, CLAUDE, CODEX]));
    assert!(
        settled
            .relaxed
            .as_deref()
            .unwrap_or_default()
            .contains("any of the crew may review, not only agy"),
        "{:?}",
        settled.relaxed
    );
}

/// A crew of one provider can't review across providers, so that rule gives way last of all.
#[test]
fn one_providers_crew_reviews_within_it() {
    let mut one_provider = settings(&[], &[]);
    one_provider.crew = workers(&["codex/gpt-5.6", "opencode/gpt-5.6"]).into();
    let settled = settle(&one_provider, Layers::default(), &provider);
    assert!(!settled.other_provider);
    assert_eq!(settled.workers.len(), 2);
    assert!(
        settled
            .relaxed
            .as_deref()
            .unwrap_or_default()
            .contains("a task may be reviewed by its worker's provider"),
        "{:?}",
        settled.relaxed
    );
    let applied = settled.apply(&one_provider);
    assert!(!applied.review.other_provider);
}

/// Two reviewers a task can't be had from one reviewer of another provider, so each task gets
/// what there is.
#[test]
fn fewer_reviewers_give_way_before_other_agents() {
    let mut two = settings(&[CLAUDE], &[CODEX, AGY]);
    two.review.per_task = 3;
    let settled = settle(&two, Layers::default(), &provider);
    assert_eq!(settled.per_task, 2);
    assert_eq!(settled.workers, workers(&[CLAUDE]));
    assert!(
        settled
            .relaxed
            .as_deref()
            .unwrap_or_default()
            .contains("each task gets 2 reviewers, not 3"),
        "{:?}",
        settled.relaxed
    );
}

/// A `--set` is a command and the request's followed words are words; the rest is settings.
#[test]
fn layers_come_from_the_runs_start() {
    let mut started = RunStarted {
        request: "Have codex orchestrate and gemini review it.".to_owned(),
        title: String::new(),
        mode: RunMode::Crew,
        forced: false,
        workflow: String::new(),
        estimate: None,
        captain_or_crew: None,
        conversation_turn: None,
        continues: None,
        repo_root: "/repo".into(),
        base_branch: None,
        base_commit: String::new(),
        uncommitted: Vec::new(),
        uncommitted_included: false,
        settings_sha256: String::new(),
        settings: serde_json::Value::Null,
        overrides: vec![OverrideRecord {
            key: "roles.worker.models".to_owned(),
            value: "[\"codex\"]".to_owned(),
            from: "flag".to_owned(),
        }],
        cli_versions: BTreeMap::new(),
        version: String::new(),
        outside_agent: None,
        instructions: vec![CheckedInstruction {
            says: "gemini review it".to_owned(),
            kind: InstructionKind::Review,
            outcome: InstructionOutcome::Followed {
                said: "agy reviews".to_owned(),
                note: None,
            },
        }],
        required_steps: Vec::new(),
        starts_from: None,
    };
    let layers = Layers::of(Some(&started));
    assert_eq!(layers.workers, Layer::Command);
    assert_eq!(layers.reviewers, Layer::Words);
    assert_eq!(layers.per_task, Layer::Words);
    assert_eq!(layers.other_provider, Layer::Settings);

    started.instructions[0].outcome = InstructionOutcome::NotFollowed {
        why: "no".to_owned(),
    };
    started.overrides[0].from = "project".to_owned();
    assert_eq!(Layers::of(Some(&started)), Layers::default());
}
