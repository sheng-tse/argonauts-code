use std::collections::BTreeMap;

use argonauts_core::events::run::{
    CheckedInstruction, InstructionOutcome, OverrideRecord, RunMode, RunStarted,
};

use super::*;

const CLAUDE: &str = "claude-code";
const CODEX: &str = "codex";
const AGY: &str = "antigravity-cli";

fn worker(text: &str) -> WorkerRef {
    text.parse().unwrap()
}

fn workers(texts: &[&str]) -> Vec<WorkerRef> {
    texts.iter().map(|text| worker(text)).collect()
}

fn family(worker: &WorkerRef) -> String {
    match worker.worker.as_str() {
        "claude-code" => "claude",
        "codex" | "opencode" => "gpt",
        _ => "gemini",
    }
    .to_owned()
}

/// The crew of the coordinated sample repo: Claude Code, Codex, and Antigravity CLI.
fn settings(owners: &[&str], reviewers: &[&str]) -> Settings {
    let mut settings = Settings {
        crew: workers(&[CLAUDE, CODEX, AGY]).into(),
        ..Settings::default()
    };
    settings.review.per_task = 1;
    settings.review.other_provider = true;
    settings.roles.entry(RoleName::Worker).or_default().workers = workers(owners);
    settings
        .roles
        .entry(RoleName::Reviewer)
        .or_default()
        .workers = workers(reviewers);
    settings
}

fn layers(owners: Layer, reviewers: Layer) -> Layers {
    Layers {
        owners,
        reviewers,
        ..Layers::default()
    }
}

/// "Have codex coordinate and gemini review it": the request's words pick the reviewer over
/// the repo's pin to Codex, and leave the owners to the other two. Every rule holds.
#[test]
fn the_requests_reviewer_and_the_rest_as_owners_hold() {
    let settled = settle(
        &settings(&[CLAUDE, CODEX], &[AGY]),
        layers(Layer::Settings, Layer::Words),
        &family,
    );
    assert_eq!(settled.owners, workers(&[CLAUDE, CODEX]));
    assert_eq!(settled.reviewers, workers(&[AGY]));
    assert_eq!(settled.relaxed, None);
    assert!(settled.left_out.is_empty());
    let lines = settled.lines(&family);
    assert_eq!(
        lines,
        [
            "`claude-code` is Claude Code, from claude: it may own tasks, not review them.",
            "`codex` is Codex, from gpt: it may own tasks, not review them.",
            "`antigravity-cli` is Antigravity CLI, from gemini: it may review tasks, not \
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

/// The only reviewer you allowed can't own a card, since nobody reviews their own work, and the
/// planner hears why in plain words.
#[test]
fn the_only_reviewer_is_left_out_of_the_owners() {
    let settled = settle(&settings(&[], &[AGY]), Layers::default(), &family);
    assert_eq!(settled.owners, workers(&[CLAUDE, CODEX]));
    assert_eq!(settled.relaxed, None);
    let why =
        "Antigravity CLI is the only reviewer you allowed, so Antigravity CLI can't own a task";
    assert_eq!(settled.left_out, [(worker(AGY), why.to_owned())]);
    assert_eq!(settled.why_not_owner(&worker(AGY)), why);
    assert!(
        settled
            .lines(&family)
            .iter()
            .any(|line| line.ends_with(&format!("may review tasks, not own them. {why}."))),
        "{:?}",
        settled.lines(&family)
    );
    // Dividing the work goes by the owners that fit.
    let base = settings(&[], &[AGY]);
    let applied = settled.apply(&base);
    assert_eq!(
        applied.roles[&RoleName::Worker].workers,
        workers(&[CLAUDE, CODEX])
    );
}

/// Antigravity CLI alone may own and alone may review: the rules can't all hold. The owners'
/// pin is from your settings and the reviewers' from your request, so the owners give way, and
/// one line says so.
#[test]
fn the_lowest_rule_gives_way_and_says_so() {
    let settled = settle(
        &settings(&[AGY], &[AGY]),
        layers(Layer::Settings, Layer::Words),
        &family,
    );
    assert_eq!(settled.owners, workers(&[CLAUDE, CODEX]));
    assert_eq!(settled.reviewers, workers(&[AGY]));
    let relaxed = settled.relaxed.clone().expect("a rule gave way");
    let mut lines = relaxed.lines();
    assert_eq!(
        lines.next(),
        Some(
            "Your rules for who owns and reviews tasks can't all hold, so the one from your \
             settings gives way: any of the crew may own a task, not only Antigravity CLI."
        )
    );
    assert_eq!(
        lines.next(),
        Some(
            "Antigravity CLI is the only reviewer you allowed, so Antigravity CLI can't own a task."
        )
    );

    // The other way round, the reviewers' pin is the lower one.
    let settled = settle(
        &settings(&[AGY], &[AGY]),
        layers(Layer::Words, Layer::Settings),
        &family,
    );
    assert_eq!(settled.owners, workers(&[AGY]));
    assert_eq!(settled.reviewers, workers(&[AGY, CLAUDE, CODEX]));
    assert!(
        settled
            .relaxed
            .as_deref()
            .unwrap_or_default()
            .contains("any of the crew may review, not only Antigravity CLI"),
        "{:?}",
        settled.relaxed
    );
}

/// A crew of one company can't review across companies, so that rule gives way last of all.
#[test]
fn one_companys_crew_reviews_within_it() {
    let mut one_company = settings(&[], &[]);
    one_company.crew = workers(&["codex/gpt-5.6", "opencode/gpt-5.6"]).into();
    let settled = settle(&one_company, Layers::default(), &family);
    assert!(!settled.other_provider);
    assert_eq!(settled.owners.len(), 2);
    assert!(
        settled
            .relaxed
            .as_deref()
            .unwrap_or_default()
            .contains("a task may be reviewed by its worker's provider"),
        "{:?}",
        settled.relaxed
    );
    let applied = settled.apply(&one_company);
    assert!(!applied.review.other_provider);
}

/// Two reviewers a card can't be had from one reviewer of another company, so each card gets
/// what there is.
#[test]
fn fewer_reviewers_give_way_before_other_agents() {
    let mut two = settings(&[CLAUDE], &[CODEX, AGY]);
    two.review.per_task = 3;
    let settled = settle(&two, Layers::default(), &family);
    assert_eq!(settled.per_task, 2);
    assert_eq!(settled.owners, workers(&[CLAUDE]));
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
        request: "Have codex coordinate and gemini review it.".to_owned(),
        title: String::new(),
        mode: RunMode::Crew,
        forced: false,
        workflow: String::new(),
        estimate: None,
        triage: None,
        chat_turn: None,
        continues: None,
        repo_root: "/repo".into(),
        base_branch: None,
        base_commit: String::new(),
        uncommitted: Vec::new(),
        uncommitted_included: false,
        settings_sha256: String::new(),
        settings: serde_json::Value::Null,
        overrides: vec![OverrideRecord {
            key: "roles.worker.workers".to_owned(),
            value: "[\"codex\"]".to_owned(),
            from: "flag".to_owned(),
        }],
        harness_versions: BTreeMap::new(),
        version: String::new(),
        front_end: None,
        instructions: vec![CheckedInstruction {
            says: "gemini review it".to_owned(),
            kind: InstructionKind::Review,
            outcome: InstructionOutcome::Followed {
                said: "Antigravity CLI reviews".to_owned(),
                note: None,
            },
        }],
        required_steps: Vec::new(),
    };
    let layers = Layers::of(Some(&started));
    assert_eq!(layers.owners, Layer::Command);
    assert_eq!(layers.reviewers, Layer::Words);
    assert_eq!(layers.per_task, Layer::Words);
    assert_eq!(layers.other_provider, Layer::Settings);

    started.instructions[0].outcome = InstructionOutcome::NotFollowed {
        why: "no".to_owned(),
    };
    started.overrides[0].from = "project".to_owned();
    assert_eq!(Layers::of(Some(&started)), Layers::default());
}
