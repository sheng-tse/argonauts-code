use argonauts_core::events::run::{CheckedInstruction, InstructionOutcome};
use argonauts_core::names::RoleName;
use argonauts_core::settings::crew::{CrewSettings, Orchestrator};
use argonauts_core::settings::roles::RoleSettings;

use super::*;

fn followed(kind: InstructionKind, says: &str) -> CheckedInstruction {
    CheckedInstruction {
        says: says.to_owned(),
        kind,
        outcome: InstructionOutcome::Followed {
            said: String::new(),
            note: None,
        },
    }
}

fn applied(instructions: Vec<CheckedInstruction>, settings: &[(&str, &str)]) -> Applied {
    Applied {
        instructions,
        settings: settings
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect(),
        ..Applied::default()
    }
}

/// Fable is the main session and orchestrates.
fn fable() -> Settings {
    Settings {
        main_agent: "claude-code/fable-5-1".parse().unwrap(),
        ..Settings::default()
    }
}

/// The words "all the other agents need to be opus, and the reviewer will be
/// astra", as the checks apply them.
fn opus_workers_and_an_astra_reviewer() -> Applied {
    applied(
        vec![
            followed(
                InstructionKind::Others,
                "all the other agents need to be opus",
            ),
            followed(InstructionKind::Review, "the reviewer will be astra"),
        ],
        &[
            ("crew.workers", "[\"claude-code/opus-5-5\"]"),
            ("crew.judge", "[\"claude-code/opus-5-5\"]"),
            ("roles.reviewer.workers", "[\"astra\"]"),
            ("review.per_task", "1"),
        ],
    )
}

/// The words make the workers and the judge Opus and the reviewer Astra. The
/// orchestrator stays the main session, and the judge, which those words reach without
/// naming it, shows them.
#[test]
fn the_words_show_against_where_the_rest_came_from() {
    let lines = understood(&fable(), &opus_workers_and_an_astra_reviewer())
        .expect("the words changed the crew");
    let from: Vec<(&str, &str)> = lines
        .iter()
        .map(|line| (line.role.as_str(), line.from.as_str()))
        .collect();
    assert_eq!(
        from,
        [
            ("orchestrator", "your main session"),
            ("workers", "as you asked"),
            ("reviewers", "as you asked"),
            ("judge", "as you asked: \"all the other agents\""),
        ]
    );
    assert_eq!(lines[1].who, lines[3].who, "the judge is Opus too");
    let shown = text(&lines);
    assert!(
        shown.starts_with("Crew for this request\norchestrator   "),
        "{shown}"
    );
}

/// A crew set with two lines, the orchestrator and the reviewers, and words naming the
/// reviewer: the judge follows the reviewers your words named, and says so.
#[test]
fn a_role_that_follows_says_so() {
    let mut settings = fable();
    settings.crew = CrewSettings {
        orchestrator: Orchestrator::Main,
        workers: Vec::new(),
        reviewers: vec!["antigravity-cli".parse().unwrap()],
        judge: Vec::new(),
        worker_workflows: false,
    };
    let words = applied(
        vec![followed(InstructionKind::Review, "astra reviews")],
        &[("roles.reviewer.workers", "[\"astra\"]")],
    );
    let lines = understood(&settings, &words).unwrap();
    assert_eq!(lines[3].who, lines[2].who, "the judge is Astra too");
    assert_eq!(lines[2].from, "as you asked");
    assert_eq!(lines[3].from, "follows the reviewers");
    assert_eq!(lines[1].from, "follows the orchestrator");
    assert!(lines[3].asked.is_none(), "a role that follows isn't kept");
}

/// Nothing is shown without words that seat agents: no words, words about something else,
/// or settings that came another way, such as ultracode's same-model workflow.
#[test]
fn only_your_words_show_a_crew() {
    assert!(understood(&fable(), &Applied::default()).is_none());
    let bell = applied(
        vec![followed(InstructionKind::ForTheLead, "keep it short")],
        &[("ui.bell", "false")],
    );
    assert!(understood(&fable(), &bell).is_none());
    let not_words = applied(Vec::new(), &[("crew.workers", "[\"codex\"]")]);
    assert!(understood(&fable(), &not_words).is_none());
    let not_followed = applied(
        vec![CheckedInstruction {
            outcome: InstructionOutcome::NotFollowed {
                why: "no agent here is called zed".to_owned(),
            },
            ..followed(InstructionKind::Review, "zed reviews")
        }],
        &[],
    );
    assert!(understood(&fable(), &not_followed).is_none());
}

/// "Keep this crew?" writes the roles your words set under `[crew]`, or under a role's own
/// workers where your settings set those, since they beat the crew's.
#[test]
fn keeping_the_crew_writes_the_roles_your_words_set() {
    let settings = fable();
    let lines = understood(&settings, &opus_workers_and_an_astra_reviewer()).unwrap();
    assert_eq!(
        to_keep(&settings, &lines),
        [
            (
                "crew.workers".to_owned(),
                "[\"claude-code/opus-5-5\"]".to_owned()
            ),
            ("crew.reviewers".to_owned(), "[\"astra\"]".to_owned()),
            (
                "crew.judge".to_owned(),
                "[\"claude-code/opus-5-5\"]".to_owned()
            ),
        ]
    );

    let mut own_reviewers = fable();
    own_reviewers.roles.insert(
        RoleName::Reviewer,
        RoleSettings {
            workers: vec!["codex".parse().unwrap()],
            ..RoleSettings::default()
        },
    );
    let lines = understood(&own_reviewers, &opus_workers_and_an_astra_reviewer()).unwrap();
    let kept = to_keep(&own_reviewers, &lines);
    assert!(
        kept.contains(&(
            "roles.reviewer.workers".to_owned(),
            "[\"astra\"]".to_owned()
        )),
        "{kept:?}"
    );
}

#[test]
fn the_phrase_is_the_words_that_reach_every_other_role() {
    assert_eq!(
        phrase("all the other agents need to be opus"),
        "all the other agents"
    );
    assert_eq!(phrase("every other agent, on opus"), "every other agent");
    assert_eq!(phrase("the others on opus"), "the others");
    assert_eq!(phrase("everyone else on opus"), "everyone else on opus");
}
