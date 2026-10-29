use argonauts_core::setup_sheet::{SetupSheet, SheetRow};

use super::*;
use crate::controller::workflow_switch::Cli;

fn model(worker: &str, name: &str, provider: &str) -> Model {
    Model {
        worker: worker.parse().unwrap(),
        name: name.to_owned(),
        provider: provider.to_owned(),
    }
}

fn cli(id: &str, name: &str, now: Now) -> Cli {
    Cli {
        id: id.to_owned(),
        name: name.to_owned(),
        now,
    }
}

/// Claude Code alone is ready: Codex is logged out, agy and OpenCode aren't installed.
fn only_claude() -> Facts {
    Facts {
        present: Present::of(vec![
            cli("claude-code", "Claude Code", Now::Ready),
            cli(
                "codex",
                "Codex",
                Now::LoggedOut {
                    login: Some("codex login".into()),
                },
            ),
            cli(
                "antigravity-cli",
                "agy",
                Now::NotInstalled {
                    add: "/agents add agy".into(),
                },
            ),
            cli(
                "opencode",
                "OpenCode",
                Now::NotInstalled {
                    add: "/agents add opencode".into(),
                },
            ),
        ]),
        models: vec![
            model("claude-code/claude-opus-5-5", "Opus 5.5", "anthropic"),
            model("claude-code/claude-sonnet-5", "Sonnet 5", "anthropic"),
            model("codex/gpt-6-luna", "5.6 Luna", "openai"),
        ],
        runs: [("claude-code".to_owned(), "Opus 5.5".to_owned())].into(),
        workflows: vec!["maker-checker".into(), "spec-first".into()],
        ..Facts::default()
    }
}

fn everyone() -> Facts {
    let mut facts = only_claude();
    facts.present = Present::of(vec![
        cli("claude-code", "Claude Code", Now::Ready),
        cli("codex", "Codex", Now::Ready),
        cli("antigravity-cli", "agy", Now::Ready),
    ]);
    facts
}

fn rows(sheet: &SetupSheet) -> Vec<String> {
    sheet
        .rows
        .iter()
        .filter_map(|row| match row {
            SheetRow::Fill(fill) => {
                let who: Vec<String> = fill
                    .who
                    .iter()
                    .map(|part| match part {
                        Part::Named { name, .. } => name.clone(),
                        Part::Text { text } => text.clone(),
                    })
                    .collect();
                let state = fill.state.as_ref().map_or("", |state| state.text.as_str());
                Some(format!(
                    "{} | {} | {} | {state}",
                    fill.label,
                    fill.role.text,
                    who.concat()
                ))
            }
            SheetRow::Aside(aside) => {
                let parts: Vec<String> = aside
                    .parts
                    .iter()
                    .map(|part| match part {
                        Part::Named { name, .. } => name.clone(),
                        Part::Text { text } => text.clone(),
                    })
                    .collect();
                Some(format!("{} | {}", aside.name, parts.concat()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn council_with_one_cli_seats_it_once_and_points_at_add_one() {
    let facts = only_claude();
    let mut draft = Draft::new(&Settings::default(), COUNCIL, &facts, None);
    assert_eq!(
        draft.pointer,
        Some(Pointer {
            row: "add".into(),
            column: SheetColumn::Who
        })
    );
    assert_eq!(
        draft.ready(&facts),
        Err("Council needs two seats that can answer".into())
    );
    let sheet = draft.sheet(&facts, "~/.argonauts/config.toml", false);
    assert!(!sheet.ready);
    assert_eq!(
        rows(&sheet),
        [
            "Seat | Note keeper | Opus 5.5 | Ready",
            "Add one | Up to 6, the same model again too",
            "Codex | Logged out · run codex login, and it shows here",
            "agy | Not installed · /agents add agy",
            "OpenCode | Not installed · /agents add opencode",
            "Checks |  | A fresh Opus 5.5 session | Same maker",
        ]
    );

    // Add one seats the model again, and the two read apart.
    draft.toggle("add", &facts);
    draft.toggle("add", &facts);
    draft.change("seat-2", SheetColumn::Role, true, &facts);
    assert_eq!(draft.ready(&facts), Ok(()));
    let sheet = draft.sheet(&facts, "~/.argonauts/config.toml", true);
    assert!(sheet.ready && sheet.refresh);
    assert_eq!(
        rows(&sheet)[..3],
        [
            "Seat | Note keeper | Opus 5.5 · 1 | Ready",
            "Seat | Skeptic | Opus 5.5 · 2 | Ready",
            "Seat | Proposer | Opus 5.5 · 3 | Ready",
        ]
    );
    assert_eq!(
        sheet.notes[..3],
        [
            "Only Claude Code is set up, so a fresh Opus 5.5 session checks each task. It never \
             sees the talk or who made",
            "the change, but it may still favour its own style, and each check is marked as \
             such. ←  →  offers Sonnet 5:",
            "another model, but the same maker. Three Opus 5.5 seats tend to make the same \
             mistakes; the skeptic helps.",
        ]
    );
    let pairs = draft.pairs();
    assert_eq!(pairs[0], ("workflow".to_owned(), "\"council\"".to_owned()));
    let (key, seats) = &pairs[1];
    assert_eq!(key, "council.seat");
    let read: toml::Table = format!("seat = {seats}").parse().unwrap();
    let listed = read["seat"].as_array().unwrap();
    assert_eq!(listed.len(), 3);
    assert_eq!(listed[1]["role"].as_str(), Some("skeptic"));
    assert_eq!(listed[0]["model"].as_str(), Some("claude-code"));
}

#[test]
fn checks_take_the_checker_seat_or_another_maker_before_a_fresh_same_maker_session() {
    let facts = everyone();
    let mut draft = Draft::new(&Settings::default(), COUNCIL, &facts, None);
    // Claude Code and Codex sit: each task is checked by the other maker's seat.
    let checks = |draft: &Draft| {
        rows(&draft.sheet(&facts, "", true))
            .into_iter()
            .find(|row| row.starts_with("Checks"))
            .unwrap()
    };
    assert_eq!(
        checks(&draft),
        "Checks |  | A seat of another maker | Ready"
    );
    // → offers a model that doesn't sit yet; it takes the checker seat, and ← gives it back.
    // Sonnet 5 shares Opus 5.5's maker, so Opus 5.5's tasks still go to another maker.
    draft.change("checks", SheetColumn::Who, true, &facts);
    assert_eq!(
        checks(&draft),
        "Checks |  | Sonnet 5, the checker seat, and another maker | Ready"
    );
    let Body::Seats(seats) = &draft.body else {
        unreachable!()
    };
    assert_eq!(seats.last().unwrap().1, SeatRole::Checker);
    draft.change("checks", SheetColumn::Who, false, &facts);
    assert_eq!(
        checks(&draft),
        "Checks |  | A seat of another maker | Ready"
    );
}

/// The Checks row says what the ladder gives each seat that builds: the checker seat for every
/// task where no builder shares its maker, and a fresh session of the same model where every
/// seat and everything set up comes from one maker, checker seat or not.
#[test]
fn checks_say_what_the_ladder_gives_each_seat_that_builds() {
    let checks = |settings: &Settings, facts: &Facts| {
        let draft = Draft::new(settings, COUNCIL, facts, None);
        rows(&draft.sheet(facts, "", true))
            .into_iter()
            .find(|row| row.starts_with("Checks"))
            .unwrap()
    };
    let apart = seats(&[
        ("claude-code/claude-opus-5-5", SeatRole::Proposer),
        ("codex/gpt-6-luna", SeatRole::Checker),
    ]);
    assert_eq!(
        checks(&apart, &everyone()),
        "Checks |  | 5.6 Luna, the checker seat | Ready"
    );
    let one_maker = seats(&[
        ("claude-code/claude-opus-5-5", SeatRole::Proposer),
        ("claude-code/claude-sonnet-5", SeatRole::Checker),
    ]);
    let mut claude_alone = only_claude();
    claude_alone
        .models
        .retain(|model| model.provider == "anthropic");
    assert_eq!(
        checks(&one_maker, &claude_alone),
        "Checks |  | A fresh Opus 5.5 session | Same maker"
    );
}

/// Two skeptics are allowed, with a word that one is usual.
#[test]
fn two_skeptics_get_a_warning() {
    let facts = everyone();
    let settings = seats(&[
        ("claude-code/claude-opus-5-5", SeatRole::Proposer),
        ("codex/gpt-6-luna", SeatRole::Skeptic),
        ("claude-code/claude-sonnet-5", SeatRole::Skeptic),
    ]);
    let sheet = Draft::new(&settings, COUNCIL, &facts, None).sheet(&facts, "", false);
    assert!(
        sheet
            .notes
            .iter()
            .any(|note| note.starts_with("Two seats play the skeptic, where one is usual")),
        "{:#?}",
        sheet.notes
    );
    let one = seats(&[
        ("claude-code/claude-opus-5-5", SeatRole::Proposer),
        ("codex/gpt-6-luna", SeatRole::Skeptic),
    ]);
    let sheet = Draft::new(&one, COUNCIL, &facts, None).sheet(&facts, "", false);
    assert!(
        !sheet
            .notes
            .iter()
            .any(|note| note.contains("play the skeptic"))
    );
}

/// A model that sits twice through a CLI that keeps no memory apart for its seats says only
/// what its sessions don't share.
#[test]
fn a_seat_twice_through_a_cli_that_may_remember_says_so() {
    let mut facts = everyone();
    facts.models.push(model(
        "antigravity-cli/gemini-3.8-flash",
        "3.8 Flash",
        "google",
    ));
    let settings = seats(&[
        ("antigravity-cli/gemini-3.8-flash", SeatRole::Proposer),
        ("antigravity-cli/gemini-3.8-flash", SeatRole::Proposer),
    ]);
    let sheet = Draft::new(&settings, COUNCIL, &facts, None).sheet(&facts, "", false);
    assert_eq!(
        sheet.notes[0],
        "3.8 Flash sits twice: two sessions that share no session or folder, though agy may \
         keep a memory of its own. You see them as 3.8 Flash · 1 and · 2."
    );
}

#[test]
fn a_seat_that_sits_twice_says_so_and_space_drops_a_seat() {
    let facts = everyone();
    let mut settings = Settings::default();
    settings.council.seats = ["claude-code/claude-opus-5-5", "claude-code/claude-opus-5-5"]
        .iter()
        .zip([SeatRole::Proposer, SeatRole::Skeptic])
        .map(
            |(model, role)| argonauts_core::settings::council::CouncilSeat {
                model: model.parse().unwrap(),
                role,
            },
        )
        .collect();
    let mut draft = Draft::new(&settings, COUNCIL, &facts, None);
    let sheet = draft.sheet(&facts, "", false);
    assert_eq!(
        sheet.notes[0],
        "Opus 5.5 sits twice: two sessions that share nothing, not even memory. You see them as \
         Opus 5.5 · 1 and · 2."
    );
    assert!(
        sheet
            .notes
            .iter()
            .any(|note| note.starts_with("The skeptic answers alone"))
    );
    draft.toggle("seat-1", &facts);
    let Body::Seats(seats) = &draft.body else {
        unreachable!()
    };
    assert_eq!(seats.len(), 1);
    assert_eq!(
        draft.pointer.as_ref().map(|pointer| pointer.row.as_str()),
        Some("seat-1")
    );
}

fn orchestrator_steps() -> Vec<(String, CrewRole)> {
    [
        ("Plan", CrewRole::Orchestrator),
        ("Build", CrewRole::Worker),
        ("Review", CrewRole::Reviewer),
        ("Sign-off", CrewRole::Judge),
    ]
    .map(|(label, role)| (label.to_owned(), role))
    .to_vec()
}

#[test]
fn orchestrator_offers_its_own_pick_on_build_and_review_and_saves_it_as_such() {
    let facts = everyone();
    let mut draft = Draft::new(
        &Settings::default(),
        ORCHESTRATOR,
        &facts,
        Some(&orchestrator_steps()),
    );
    let sheet = draft.sheet(&facts, "~/.argonauts/config.toml", false);
    assert!(sheet.ready);
    assert_eq!(
        rows(&sheet),
        [
            "Plan | Orchestrator | Opus 5.5 | Ready",
            "Build | Workers | Opus 5.5, Codex, agy | Ready",
            "Review | Reviewers | Opus 5.5, Codex, agy | Ready",
            "Sign-off | Judge | Opus 5.5, Codex, agy | Ready",
        ]
    );
    // ← from following the orchestrator wraps round to the orchestrator's own pick.
    draft.change("build", SheetColumn::Who, false, &facts);
    let sheet = draft.sheet(&facts, "~/.argonauts/config.toml", true);
    assert_eq!(
        rows(&sheet)[1],
        "Build | Workers | The orchestrator picks | Ready"
    );
    let status: String = sheet
        .status
        .iter()
        .map(|words| words.text.as_str())
        .collect();
    assert_eq!(
        status,
        "The orchestrator now picks who builds each task. Enter saves it to \
         ~/.argonauts/config.toml and switches."
    );
    let SheetRow::Fill(build) = &sheet.rows[1] else {
        unreachable!()
    };
    assert_eq!(build.under.len(), 2, "Can pick, Picks by");
    assert_eq!(build.under[0].label, "Can pick");
    assert!(draft.pairs().contains(&(
        "crew.orchestrator_picks.worker".to_owned(),
        "true".to_owned()
    )));

    draft.change("review", SheetColumn::Who, false, &facts);
    let sheet = draft.sheet(&facts, "", true);
    assert_eq!(
        rows(&sheet)[2],
        "Review | Reviewers | The orchestrator picks | Ready"
    );
    assert_eq!(
        rows(&sheet)[3],
        "Sign-off | Judge | The orchestrator picks | Ready",
        "the judge follows the reviewers"
    );
    let status: String = sheet
        .status
        .iter()
        .map(|words| words.text.as_str())
        .collect();
    assert!(status.contains("who builds and who reviews"), "{status}");
}

#[test]
fn orchestrator_with_one_reviewer_ready_needs_two() {
    let facts = only_claude();
    let draft = Draft::new(
        &Settings::default(),
        ORCHESTRATOR,
        &facts,
        Some(&orchestrator_steps()),
    );
    let sheet = draft.sheet(&facts, "", false);
    assert!(!sheet.ready);
    assert_eq!(
        rows(&sheet)[2],
        "Review | Reviewers | Opus 5.5, Codex, agy | 1 of 2"
    );
    assert_eq!(
        draft.pointer.as_ref().map(|pointer| pointer.row.as_str()),
        Some("review")
    );
}

#[test]
fn how_tasks_are_done_allows_every_way_until_space_keeps_one_out() {
    let facts = everyone();
    let mut draft = Draft::new(
        &Settings::default(),
        ORCHESTRATOR,
        &facts,
        Some(&orchestrator_steps()),
    );
    assert_eq!(draft.pairs().len(), 1, "nothing but the workflow changed");
    draft.change("ways.workflows", SheetColumn::Who, true, &facts);
    let sheet = draft.sheet(&facts, "", true);
    let names: Vec<&str> = sheet
        .rows
        .iter()
        .filter_map(|row| match row {
            SheetRow::Way(way) => Some(way.name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        names,
        [
            "Own session",
            "Workflows",
            "Maker-checker",
            "Spec-first",
            "Agents"
        ]
    );
    draft.toggle("ways.workflow.spec-first", &facts);
    draft.toggle("ways.own_session", &facts);
    draft.change("ways.agents", SheetColumn::Who, true, &facts);
    let pairs = draft.pairs();
    assert!(pairs.contains(&("crew.ways.own_session".to_owned(), "false".to_owned())));
    assert!(pairs.contains(&(
        "crew.ways.workflows".to_owned(),
        "[\"maker-checker\"]".to_owned()
    )));
    assert!(pairs.contains(&("crew.ways.agents".to_owned(), "2".to_owned())));
    let council = Draft::new(&Settings::default(), COUNCIL, &facts, None);
    let sheet = council.sheet(&facts, "", false);
    assert_eq!(
        sheet.notes[sheet.notes.len() - 2],
        "The note keeper picks a way for each task from these and gives its reason in a few \
         words."
    );
}

#[test]
fn captain_picks_its_main_agent_from_the_ready_models() {
    let facts = everyone();
    let mut draft = Draft::new(&Settings::default(), CAPTAIN, &facts, None);
    draft.change("main", SheetColumn::Who, true, &facts);
    let pairs = draft.pairs();
    assert_eq!(
        pairs[1],
        (
            "crew.main".to_owned(),
            "\"claude-code/claude-opus-5-5\"".to_owned()
        )
    );
    let sheet = draft.sheet(&facts, "", true);
    assert_eq!(rows(&sheet), ["Main | Main agent | Opus 5.5 | Picked"]);
}

#[test]
fn a_name_in_a_note_never_breaks() {
    let text = format!(
        "{} offers {}: another model.",
        "x".repeat(100),
        whole("Sonnet 5.5")
    );
    assert_eq!(
        wrap(&text, NOTE_WIDTH),
        [
            format!("{} offers", "x".repeat(100)),
            "Sonnet 5.5: another model.".to_owned()
        ]
    );
}

#[test]
fn long_notes_break_where_the_pictures_break_them() {
    let text = "Only Claude Code is set up, so a fresh Opus 5.5 session checks each task. It never \
                sees the talk or who made the change.";
    assert_eq!(
        wrap(text, NOTE_WIDTH),
        [
            "Only Claude Code is set up, so a fresh Opus 5.5 session checks each task. It never \
             sees the talk or who made",
            "the change."
        ]
    );
}

fn seats(seated: &[(&str, SeatRole)]) -> Settings {
    let mut settings = Settings::default();
    settings.council.seats = seated
        .iter()
        .map(
            |(model, role)| argonauts_core::settings::council::CouncilSeat {
                model: model.parse().unwrap(),
                role: *role,
            },
        )
        .collect();
    settings
}

#[test]
fn who_names_each_seats_model_once_by_its_role() {
    let facts = everyone();
    let council = Draft::new(&Settings::default(), COUNCIL, &facts, None);
    assert_eq!(
        council.who(&facts).as_deref(),
        Some("Opus 5.5, Codex and agy propose")
    );

    let settings = seats(&[
        ("claude-code/claude-opus-5-5", SeatRole::Proposer),
        ("claude-code/claude-opus-5-5", SeatRole::Proposer),
        ("codex/gpt-6-luna", SeatRole::Proposer),
        ("claude-code/claude-sonnet-5", SeatRole::Skeptic),
    ]);
    let council = Draft::new(&settings, COUNCIL, &facts, None);
    assert_eq!(
        council.who(&facts).as_deref(),
        Some("two Opus 5.5 seats and 5.6 Luna propose, Sonnet 5 plays the skeptic")
    );
}

#[test]
fn who_says_steps_filled_alike_together() {
    let facts = everyone();
    let orchestrator = Draft::new(
        &Settings::default(),
        ORCHESTRATOR,
        &facts,
        Some(&orchestrator_steps()),
    );
    assert_eq!(
        orchestrator.who(&facts).as_deref(),
        Some("Opus 5.5 plans, Opus 5.5, Codex and agy build, review and sign off")
    );

    let mut settings = Settings::default();
    settings.crew.orchestrator_picks.worker = true;
    settings.crew.orchestrator_picks.reviewer = true;
    let picks = Draft::new(&settings, ORCHESTRATOR, &facts, Some(&orchestrator_steps()));
    assert_eq!(
        picks.who(&facts).as_deref(),
        Some(
            "Opus 5.5 plans, the orchestrator picks who builds and who reviews, Opus 5.5, Codex \
             and agy sign off"
        )
    );

    let captain = Draft::new(&Settings::default(), CAPTAIN, &facts, None);
    assert_eq!(
        captain.who(&facts).as_deref(),
        Some("Opus 5.5 does the work itself")
    );
    let mut settings = Settings::default();
    settings.captain.review = true;
    settings.roles.insert(
        argonauts_core::names::RoleName::Reviewer,
        argonauts_core::settings::roles::RoleSettings {
            models: vec!["codex/gpt-6-luna".parse().unwrap()],
            ..Default::default()
        },
    );
    let reviewed = Draft::new(&settings, CAPTAIN, &facts, None);
    assert_eq!(
        reviewed.who(&facts).as_deref(),
        Some("Opus 5.5 does the work itself, 5.6 Luna reviews")
    );
}
