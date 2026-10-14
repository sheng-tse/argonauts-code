//! `/mods` end to end, through the real controller: the files it lists, what a page's save,
//! reset and trust do on disk, and when a mod's command runs.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use argonauts_core::app::{AppEvent, UiCommand, ViewData, ViewRequest};
use argonauts_core::input::{ModsAction, SlashCommand};
use argonauts_core::settings::mods::view::{ModChange, ModRowKind, ModsView};
use argonauts_core::settings::mods::{ModForm, ModOrigin, ModPlace};

use crate::controller::test_support::{CliOptions, TestDriver};

const CLOCK: &str =
    "body = \"text\"\nplaces = [\"above\"]\n[data]\ncommand = [\"./clock.sh\"]\nevery = \"1s\"\n";

fn script(dir: &Path, name: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The test driver with the sandbox off, so a test's command runs as it is.
async fn driver(prepare: impl FnOnce(&argonauts_testkit::TempRepo) + 'static) -> TestDriver {
    TestDriver::start_with(CliOptions {
        flags: vec!["sandbox.mode=\"none\"".to_owned()],
        prepare: Some(Box::new(prepare)),
        ..CliOptions::default()
    })
    .await
}

async fn mods_view(driver: &mut TestDriver, what: impl Fn(&ModsView) -> bool) -> ModsView {
    let event = driver
        .until(|event| {
            matches!(event, AppEvent::View { data }
                if matches!(&**data, ViewData::Mods { view } if what(view)))
        })
        .await;
    match event {
        AppEvent::View { data } => match *data {
            ViewData::Mods { view } => *view,
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }
}

async fn open(driver: &mut TestDriver) -> ModsView {
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::Mods {
                action: ModsAction::List,
                scope: None,
            },
        })
        .await;
    mods_view(driver, |view| !view.refresh).await
}

fn notices(driver: &TestDriver) -> Vec<String> {
    driver
        .seen()
        .iter()
        .filter_map(|event| match event {
            AppEvent::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn mods_lists_the_built_in_cards_your_files_and_what_didnt_load() {
    let mut driver = driver(|repo| {
        let mods = repo.home().join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::write(mods.join("clock.toml"), CLOCK).unwrap();
        std::fs::write(mods.join("broken.toml"), "body = \"pie\"\n").unwrap();
    })
    .await;
    let view = open(&mut driver).await;
    let names: Vec<&str> = view.rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "context",
            "plan-usage",
            "spend",
            "tokens",
            "runs",
            "waiting",
            "clock",
            "stats",
            "bottom-line"
        ]
    );
    let clock = view.row("clock").unwrap();
    assert_eq!(clock.origin, ModOrigin::Yours);
    assert!(clock.path.ends_with("mods/clock.toml"), "{}", clock.path);
    assert_eq!(view.problems.len(), 1);
    assert!(view.problems[0].path.ends_with("mods/broken.toml"));
    // A built-in card says where a change to it would be saved, and why it has no numbers.
    let ModRowKind::Card {
        built_in,
        reading,
        note,
        ..
    } = &view.row("plan-usage").unwrap().kind
    else {
        panic!("a card");
    };
    assert!(built_in.is_some());
    assert!(reading.is_none());
    assert_eq!(
        note.as_deref(),
        Some("no CLI has reported its plan usage yet")
    );
    // The workflows card always has something to say.
    let ModRowKind::Card { reading, .. } = &view.row("runs").unwrap().kind else {
        panic!("a card");
    };
    assert_eq!(reading.as_ref().unwrap().summary, "no workflows yet");
    assert_eq!(view.line.mode, "auto mode on");
    driver.exit().await;
}

#[tokio::test]
async fn a_name_opens_that_page_and_an_unknown_one_says_what_there_is() {
    let mut driver = driver(|_| {}).await;
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::ModFile {
                name: "bottom-line".to_owned(),
                words: Vec::new(),
            },
        })
        .await;
    let view = mods_view(&mut driver, |view| view.open.is_some()).await;
    assert_eq!(view.open.as_deref(), Some("bottom-line"));
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::ModFile {
                name: "ctx".to_owned(),
                words: Vec::new(),
            },
        })
        .await;
    driver
        .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains("no mod named ctx")))
        .await;
    let said = notices(&driver).join("\n");
    assert!(said.contains("the mods are context, plan-usage"), "{said}");
    driver.exit().await;
}

#[tokio::test]
async fn a_page_s_save_writes_your_copy_and_a_reset_deletes_it() {
    let mut driver = driver(|_| {}).await;
    open(&mut driver).await;
    driver
        .send(UiCommand::Mod {
            change: ModChange::Save {
                name: "context".to_owned(),
                pairs: vec![("form".to_owned(), "\"line\"".to_owned())],
                origin: ModOrigin::Yours,
            },
        })
        .await;
    let view = mods_view(&mut driver, |view| {
        view.refresh
            && view
                .row("context")
                .is_some_and(|row| row.origin == ModOrigin::Yours)
    })
    .await;
    let ModRowKind::Card { file, .. } = &view.row("context").unwrap().kind else {
        panic!("a card");
    };
    assert_eq!(file.form, ModForm::Line);
    let path = driver.repo.home().join("mods/context.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("form   = \"line\""), "{text}");
    assert!(text.starts_with("# How full"), "{text}");

    driver
        .send(UiCommand::Mod {
            change: ModChange::Reset {
                name: "context".to_owned(),
                origin: ModOrigin::Yours,
            },
        })
        .await;
    mods_view(&mut driver, |view| {
        view.row("context")
            .is_some_and(|row| row.origin == ModOrigin::BuiltIn)
    })
    .await;
    assert!(!path.exists());
    driver.exit().await;
}

#[tokio::test]
async fn a_save_that_wouldnt_load_writes_nothing_and_says_why() {
    let mut driver = driver(|_| {}).await;
    driver
        .send(UiCommand::Mod {
            change: ModChange::Save {
                name: "context".to_owned(),
                pairs: vec![("body".to_owned(), "\"pie\"".to_owned())],
                origin: ModOrigin::Yours,
            },
        })
        .await;
    driver
        .until(
            |event| matches!(event, AppEvent::Notice { text, .. } if text.contains("doesn't load")),
        )
        .await;
    assert!(!driver.repo.home().join("mods/context.toml").exists());
    driver.exit().await;
}

#[tokio::test]
async fn your_command_mod_runs_and_a_new_file_loads_without_a_restart() {
    let mut driver = driver(|repo| {
        script(&repo.home().join("mods"), "clock.sh", "echo 12:00");
    })
    .await;
    // The file arrives after the app started.
    std::fs::write(driver.repo.home().join("mods/clock.toml"), CLOCK).unwrap();
    let view = mods_view(&mut driver, |view| {
        view.row("clock").is_some_and(|row| {
            matches!(
                &row.kind,
                ModRowKind::Card {
                    reading: Some(_),
                    ..
                }
            )
        })
    })
    .await;
    let ModRowKind::Card { reading, .. } = &view.row("clock").unwrap().kind else {
        panic!("a card");
    };
    assert_eq!(reading.as_ref().unwrap().summary, "12:00");
    driver.exit().await;
}

#[tokio::test]
async fn an_edited_program_runs_again_at_once() {
    // Its next turn is an hour off, so only the edit can bring the new words.
    let hourly = CLOCK.replace("\"1s\"", "\"1h\"");
    assert_ne!(hourly, CLOCK);
    let mut driver = driver(move |repo| {
        let mods = repo.home().join("mods");
        script(&mods, "clock.sh", "echo 12:00");
        std::fs::write(mods.join("clock.toml"), hourly).unwrap();
    })
    .await;
    let says = |view: &ModsView, words: &str| {
        matches!(&view.row("clock").unwrap().kind,
            ModRowKind::Card { reading: Some(reading), .. } if reading.summary == words)
    };
    mods_view(&mut driver, |view| says(view, "12:00")).await;
    script(
        &driver.repo.home().join("mods"),
        "clock.sh",
        "echo half past twelve",
    );
    mods_view(&mut driver, |view| says(view, "half past twelve")).await;
    driver.exit().await;
}

#[tokio::test]
async fn one_failed_run_keeps_the_numbers_it_had_and_says_why() {
    let mut driver = driver(|repo| {
        let mods = repo.home().join("mods");
        script(&mods, "clock.sh", "echo 12:00");
        std::fs::write(mods.join("clock.toml"), CLOCK).unwrap();
    })
    .await;
    let card = |view: &ModsView| match &view.row("clock").unwrap().kind {
        ModRowKind::Card { reading, note, .. } => (
            reading.as_ref().map(|reading| reading.summary.clone()),
            note.clone(),
        ),
        ModRowKind::Own { .. } => panic!("a card"),
    };
    mods_view(&mut driver, |view| card(view).0.is_some()).await;
    script(
        &driver.repo.home().join("mods"),
        "clock.sh",
        "echo 'no clock' >&2; exit 4",
    );
    let view = mods_view(&mut driver, |view| card(view).1.is_some()).await;
    assert_eq!(
        card(&view),
        (
            Some("12:00".to_owned()),
            Some(
                "These are from before its last run, which failed: it ended with exit code 4: \
                 no clock"
                    .to_owned()
            )
        )
    );
    driver.exit().await;
}

#[tokio::test]
async fn the_project_s_command_waits_for_your_trust() {
    let mut driver = driver(|repo| {
        let mods = repo.root().join(".argonauts/mods");
        script(&mods, "clock.sh", "echo 12:00");
        std::fs::write(mods.join("clock.toml"), CLOCK).unwrap();
    })
    .await;
    let view = open(&mut driver).await;
    let clock = view.row("clock").unwrap();
    assert_eq!(clock.origin, ModOrigin::Project);
    assert_eq!(clock.path, ".argonauts/mods/clock.toml");
    assert!(matches!(
        &clock.kind,
        ModRowKind::Card {
            needs_trust: true,
            reading: None,
            ..
        }
    ));
    // Looked at for a while, it still hasn't run.
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    driver
        .send(UiCommand::Request {
            view: ViewRequest::Mods {
                open: None,
                refresh: true,
            },
        })
        .await;
    let view = mods_view(&mut driver, |view| view.refresh).await;
    assert!(matches!(
        &view.row("clock").unwrap().kind,
        ModRowKind::Card {
            needs_trust: true,
            reading: None,
            ..
        }
    ));
    driver
        .send(UiCommand::Mod {
            change: ModChange::Trust {
                name: "clock".to_owned(),
            },
        })
        .await;
    let view = mods_view(&mut driver, |view| {
        view.row("clock").is_some_and(|row| {
            matches!(
                &row.kind,
                ModRowKind::Card {
                    reading: Some(_),
                    ..
                }
            )
        })
    })
    .await;
    assert!(matches!(
        &view.row("clock").unwrap().kind,
        ModRowKind::Card {
            needs_trust: false,
            ..
        }
    ));
    // A change to its script asks again.
    script(
        &driver.repo.root().join(".argonauts/mods"),
        "clock.sh",
        "echo 13:00",
    );
    let view = open(&mut driver).await;
    assert!(matches!(
        &view.row("clock").unwrap().kind,
        ModRowKind::Card {
            needs_trust: true,
            ..
        }
    ));
    driver.exit().await;
}

#[tokio::test]
async fn hiding_pinning_or_saving_a_project_s_mod_never_trusts_it() {
    let mut driver = driver(|repo| {
        let mods = repo.root().join(".argonauts/mods");
        script(&mods, "clock.sh", "echo 12:00");
        std::fs::write(mods.join("clock.toml"), CLOCK).unwrap();
    })
    .await;
    driver
        .until(|event| {
            matches!(event, AppEvent::Notice { text, .. }
                if text.contains("The project's mod clock runs a command, so it waits for your trust"))
        })
        .await;
    open(&mut driver).await;
    let file = |words: &[&str]| UiCommand::Slash {
        command: SlashCommand::ModFile {
            name: "clock".to_owned(),
            words: words.iter().map(|word| (*word).to_owned()).collect(),
        },
    };
    for (words, said) in [
        (&["toggle"][..], "clock is hidden"),
        (&["toggle"][..], "clock shows again"),
    ] {
        driver.send(file(words)).await;
        driver
            .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains(said)))
            .await;
    }
    driver.send(file(&["unpin"])).await;
    driver.send(file(&["pin"])).await;
    driver
        .send(UiCommand::Mod {
            change: ModChange::Save {
                name: "clock".to_owned(),
                pairs: vec![("every".to_owned(), "\"2s\"".to_owned())],
                origin: ModOrigin::Project,
            },
        })
        .await;
    // Long enough for a trusted command to have run.
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    let view = open(&mut driver).await;
    assert!(
        matches!(
            &view.row("clock").unwrap().kind,
            ModRowKind::Card {
                needs_trust: true,
                reading: None,
                ..
            }
        ),
        "{:?}",
        view.row("clock")
    );
    let told = notices(&driver)
        .iter()
        .filter(|text| text.contains("waits for your trust"))
        .count();
    assert_eq!(told, 1, "said once a session");
    driver.exit().await;
}

#[tokio::test]
async fn with_commands_off_a_command_mod_says_so_and_never_runs() {
    let mut driver = TestDriver::start_with(CliOptions {
        flags: vec![
            "sandbox.mode=\"none\"".to_owned(),
            "mods.commands=false".to_owned(),
        ],
        prepare: Some(Box::new(|repo| {
            let mods = repo.home().join("mods");
            script(&mods, "clock.sh", "echo 12:00");
            std::fs::write(mods.join("clock.toml"), CLOCK).unwrap();
        })),
        ..CliOptions::default()
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let view = open(&mut driver).await;
    let ModRowKind::Card { reading, note, .. } = &view.row("clock").unwrap().kind else {
        panic!("a card");
    };
    assert!(reading.is_none());
    assert_eq!(
        note.as_deref(),
        Some("mods.commands is off, so its command doesn't run")
    );
    driver.exit().await;
}

#[tokio::test]
async fn pin_and_a_place_change_one_key_of_the_mod_s_file() {
    let mut driver = driver(|repo| {
        let mods = repo.home().join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::write(mods.join("clock.toml"), CLOCK.replace("[\"above\"]", "[]")).unwrap();
    })
    .await;
    let places = |view: &ModsView| -> Vec<ModPlace> {
        match &view.row("clock").unwrap().kind {
            ModRowKind::Card { file, .. } => file.places.clone(),
            ModRowKind::Own { .. } => panic!("a card"),
        }
    };
    let send = |name: &str, words: &[&str]| UiCommand::Slash {
        command: SlashCommand::ModFile {
            name: name.to_owned(),
            words: words.iter().map(|word| (*word).to_owned()).collect(),
        },
    };
    driver.send(send("clock", &["pin"])).await;
    // The watcher's first look sends a view of its own, so wait for the one after the save.
    let view = mods_view(&mut driver, |view| view.refresh && !places(view).is_empty()).await;
    assert_eq!(places(&view), [ModPlace::Above]);
    driver.send(send("clock", &["usage"])).await;
    let view = mods_view(&mut driver, |view| view.refresh && places(view).len() == 2).await;
    assert_eq!(places(&view), [ModPlace::Above, ModPlace::Usage]);
    driver.send(send("clock", &["unpin"])).await;
    let view = mods_view(&mut driver, |view| view.refresh && places(view).len() == 1).await;
    assert_eq!(places(&view), [ModPlace::Usage]);
    driver.send(send("clock", &["sideways"])).await;
    driver
        .until(
            |event| matches!(event, AppEvent::Notice { text, .. } if text.contains("use a place")),
        )
        .await;
    driver.exit().await;
}

/// Whether the view has the mod on the bottom line, and whether its file only asks to be.
fn on_line(view: &ModsView, name: &str) -> (bool, bool) {
    match &view.row(name).unwrap().kind {
        ModRowKind::Card {
            file,
            asks_for_line,
            ..
        } => (file.places.contains(&ModPlace::BottomLine), *asks_for_line),
        ModRowKind::Own { .. } => panic!("a card"),
    }
}

#[tokio::test]
async fn the_bottom_line_s_own_file_says_which_mods_are_on_it() {
    let mut driver = driver(|repo| {
        let mods = repo.home().join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        // It asks for the line in its own file, which by itself puts it nowhere.
        let asking = CLOCK.replace("[\"above\"]", "[\"bottom-line\"]");
        std::fs::write(mods.join("clock.toml"), asking).unwrap();
    })
    .await;
    let home = driver.repo.home().to_path_buf();
    let line_file = home.join("mods/bottom-line.toml");
    let view = open(&mut driver).await;
    assert_eq!(on_line(&view, "clock"), (false, true));
    // As it ships the line lists plan usage, so that mod is on it.
    assert_eq!(on_line(&view, "plan-usage"), (true, false));
    assert!(!line_file.exists());

    let place = |words: &[&str]| UiCommand::Slash {
        command: SlashCommand::ModFile {
            name: "clock".to_owned(),
            words: words.iter().map(|word| (*word).to_owned()).collect(),
        },
    };
    driver.send(place(&["bottom-line"])).await;
    let view = mods_view(&mut driver, |view| view.refresh && on_line(view, "clock").0).await;
    assert_eq!(on_line(&view, "clock"), (true, false));
    let line = std::fs::read_to_string(&line_file).unwrap();
    assert!(
        line.contains("right = [\"plan-usage\", \"clock\"]"),
        "{line}"
    );
    // The copy keeps the built-in file's comments, and the mod's own file is as you wrote it.
    assert!(line.starts_with("# The line under the input"), "{line}");
    let clock = std::fs::read_to_string(home.join("mods/clock.toml")).unwrap();
    assert!(clock.contains("places = [\"bottom-line\"]"), "{clock}");
    assert!(
        notices(&driver)
            .iter()
            .any(|text| text.starts_with("clock now shows on the bottom line, last on the right")),
        "{:?}",
        notices(&driver)
    );

    // Taken off, it leaves the list, and its file stops asking.
    driver.send(place(&["bottom-line"])).await;
    let view = mods_view(&mut driver, |view| {
        view.refresh && !on_line(view, "clock").0
    })
    .await;
    assert_eq!(on_line(&view, "clock"), (false, false));
    let line = std::fs::read_to_string(&line_file).unwrap();
    assert!(line.contains("right = [\"plan-usage\"]"), "{line}");
    let clock = std::fs::read_to_string(home.join("mods/clock.toml")).unwrap();
    assert!(clock.contains("places = []"), "{clock}");
    driver.exit().await;
}

#[tokio::test]
async fn the_bottom_line_s_page_starts_from_both_files_and_saves_into_the_project_s() {
    use argonauts_core::settings::bottom_line::LineForm;
    use argonauts_core::settings::mods::bottom_line::read;

    let mut driver = driver(|repo| {
        let yours = repo.home().join("mods");
        std::fs::create_dir_all(&yours).unwrap();
        std::fs::write(
            yours.join("bottom-line.toml"),
            "left = [\"mode\", \"branch\"]\n",
        )
        .unwrap();
        std::fs::write(yours.join("clock.toml"), CLOCK).unwrap();
        let project = repo.root().join(".argonauts/mods");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("bottom-line.toml"), "form = \"compact\"\n").unwrap();
    })
    .await;
    let project_file = driver.repo.root().join(".argonauts/mods/bottom-line.toml");
    let line = |view: &ModsView| match &view.row("bottom-line").unwrap().kind {
        ModRowKind::Own { text, .. } => read(text).unwrap(),
        ModRowKind::Card { .. } => panic!("it draws itself"),
    };
    let view = open(&mut driver).await;
    // The project's form over your parts, as the line itself reads the two.
    assert_eq!(view.row("bottom-line").unwrap().origin, ModOrigin::Project);
    assert_eq!(line(&view).form, LineForm::Compact);
    assert_eq!(line(&view).left, ["mode", "branch"]);

    driver
        .send(UiCommand::Slash {
            command: SlashCommand::ModFile {
                name: "clock".to_owned(),
                words: vec!["bottom-line".to_owned()],
            },
        })
        .await;
    let view = mods_view(&mut driver, |view| view.refresh && on_line(view, "clock").0).await;
    assert_eq!(line(&view).right, ["plan-usage", "clock"]);
    assert_eq!(line(&view).left, ["mode", "branch"]);
    let text = std::fs::read_to_string(&project_file).unwrap();
    assert!(text.contains("form = \"compact\""), "{text}");
    assert!(
        text.contains("right = [\"plan-usage\", \"clock\"]"),
        "{text}"
    );
    driver.exit().await;
}

/// `/mods spend bottom-line` puts the built-in spend card on the bottom line, in the line's own
/// file, which is what the line reads.
#[tokio::test]
async fn a_built_in_card_goes_onto_the_bottom_line_by_its_mod_s_name() {
    let mut driver = driver(|_| {}).await;
    open(&mut driver).await;
    driver
        .send(UiCommand::Slash {
            command: argonauts_core::input::parse_input("/mods spend bottom-line")
                .map(|parsed| match parsed {
                    argonauts_core::input::ParsedInput::Slash { command } => command,
                    other => panic!("{other:?}"),
                })
                .unwrap(),
        })
        .await;
    mods_view(&mut driver, |view| view.refresh && on_line(view, "spend").0).await;
    let text = std::fs::read_to_string(driver.repo.home().join("mods/bottom-line.toml")).unwrap();
    assert!(text.contains("\"spend\""), "{text}");
    driver.exit().await;
}

#[tokio::test]
async fn a_page_s_tick_for_the_bottom_line_is_written_in_the_line_s_file() {
    let mut driver = driver(|repo| {
        let mods = repo.home().join("mods");
        script(&mods, "clock.sh", "echo 12:00");
        std::fs::write(mods.join("clock.toml"), CLOCK).unwrap();
    })
    .await;
    let home = driver.repo.home().to_path_buf();
    open(&mut driver).await;
    let save = |name: &str, places: &str| UiCommand::Mod {
        change: ModChange::Save {
            name: name.to_owned(),
            pairs: vec![("places".to_owned(), places.to_owned())],
            origin: ModOrigin::Yours,
        },
    };
    let before = std::fs::read_to_string(home.join("mods/clock.toml")).unwrap();
    driver
        .send(save("clock", "[\"above\", \"bottom-line\"]"))
        .await;
    let view = mods_view(&mut driver, |view| view.refresh && on_line(view, "clock").0).await;
    assert_eq!(on_line(&view, "clock"), (true, false));
    let line = std::fs::read_to_string(home.join("mods/bottom-line.toml")).unwrap();
    assert!(line.contains("\"clock\"]"), "{line}");
    // Nothing else about the mod changed, so its own file wasn't written.
    assert_eq!(
        std::fs::read_to_string(home.join("mods/clock.toml")).unwrap(),
        before
    );

    // A built-in card goes on the line without a copy of its file being made.
    driver
        .send(save("spend", "[\"usage\", \"bottom-line\"]"))
        .await;
    mods_view(&mut driver, |view| view.refresh && on_line(view, "spend").0).await;
    assert!(!home.join("mods/spend.toml").exists());

    // Another place and the line's tick taken off in one save: each file gets its part.
    driver.send(save("clock", "[\"above\", \"usage\"]")).await;
    let view = mods_view(&mut driver, |view| {
        view.refresh && !on_line(view, "clock").0
    })
    .await;
    let ModRowKind::Card { file, .. } = &view.row("clock").unwrap().kind else {
        panic!("a card");
    };
    assert_eq!(file.places, [ModPlace::Above, ModPlace::Usage]);
    let clock = std::fs::read_to_string(home.join("mods/clock.toml")).unwrap();
    assert!(clock.contains("places = [\"above\", \"usage\"]"), "{clock}");
    let line = std::fs::read_to_string(home.join("mods/bottom-line.toml")).unwrap();
    assert!(
        line.contains("right = [\"plan-usage\", \"spend\"]"),
        "{line}"
    );
    driver.exit().await;
}

#[tokio::test]
async fn a_built_in_card_s_places_are_the_place_lists_in_your_settings() {
    use argonauts_core::settings::ui::{ModName, Place};

    let mut driver = driver(|_| {}).await;
    let home = driver.repo.home().to_path_buf();
    let places = |view: &ModsView, name: &str| match &view.row(name).unwrap().kind {
        ModRowKind::Card { file, .. } => file.places.clone(),
        ModRowKind::Own { .. } => panic!("a card"),
    };
    // A place list as your settings file has it now.
    let config = home.join("config.toml");
    let list = |place: Place| -> Vec<ModName> {
        let text = std::fs::read_to_string(&config).unwrap();
        let settings: argonauts_core::settings::Settings = toml::from_str(&text).unwrap();
        settings.ui.place(place).to_vec()
    };
    let view = open(&mut driver).await;
    assert!(!places(&view, "plan-usage").contains(&ModPlace::Above));
    // As it ships it is where the lists and the line put it, so its page shows no change.
    let ModRowKind::Card { file, built_in, .. } = &view.row("plan-usage").unwrap().kind else {
        panic!("a card");
    };
    assert_eq!(built_in.as_deref(), Some(file.as_ref()));
    assert_eq!(
        file.places,
        [ModPlace::Usage, ModPlace::BottomLine, ModPlace::Status]
    );

    // Its page ticks it above the input and off the Usage tab.
    let ticked: Vec<&str> = places(&view, "plan-usage")
        .iter()
        .map(|place| place.word())
        .filter(|place| *place != "usage")
        .chain(["above"])
        .collect();
    driver
        .send(UiCommand::Mod {
            change: ModChange::Save {
                name: "plan-usage".to_owned(),
                pairs: vec![("places".to_owned(), format!("{ticked:?}"))],
                origin: ModOrigin::Yours,
            },
        })
        .await;
    let view = mods_view(&mut driver, |view| {
        view.refresh && places(view, "plan-usage").contains(&ModPlace::Above)
    })
    .await;
    assert!(!places(&view, "plan-usage").contains(&ModPlace::Usage));
    // The lists took it, which is where the band and the tab read it. No file was written.
    assert_eq!(list(Place::AboveInput), [ModName::PlanUsage]);
    assert!(!list(Place::Usage).contains(&ModName::PlanUsage));
    assert!(!list(Place::Usage).is_empty());
    assert!(!home.join("mods/plan-usage.toml").exists());

    // Typed, the same list changes.
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::ModFile {
                name: "plan-usage".to_owned(),
                words: vec!["unpin".to_owned()],
            },
        })
        .await;
    mods_view(&mut driver, |view| {
        view.refresh && !places(view, "plan-usage").contains(&ModPlace::Above)
    })
    .await;
    assert!(list(Place::AboveInput).is_empty());
    assert!(!home.join("mods/plan-usage.toml").exists());
    driver.exit().await;
}

#[tokio::test]
async fn a_place_for_a_hidden_mod_is_kept_once_and_says_it_is_hidden() {
    let mut driver = driver(|repo| {
        let mods = repo.home().join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::write(
            mods.join("clock.toml"),
            CLOCK.replace("body", "shown = false\nbody"),
        )
        .unwrap();
    })
    .await;
    let send = |words: &[&str]| UiCommand::Slash {
        command: SlashCommand::ModFile {
            name: "clock".to_owned(),
            words: words.iter().map(|word| (*word).to_owned()).collect(),
        },
    };
    // It has the place already, hidden or not.
    driver.send(send(&["pin"])).await;
    driver
        .until(
            |event| matches!(event, AppEvent::Notice { text, .. } if text.contains("Already so")),
        )
        .await;
    driver.send(send(&["usage"])).await;
    driver
        .until(|event| {
            matches!(event, AppEvent::Notice { text, .. }
                if text == "clock now shows on the Usage tab, once /clock shows it again")
        })
        .await;
    let text = std::fs::read_to_string(driver.repo.home().join("mods/clock.toml")).unwrap();
    assert!(text.contains("places = [\"above\", \"usage\"]"), "{text}");
    driver.exit().await;
}

#[tokio::test]
async fn a_mod_s_own_command_hides_it_and_shows_it_again_where_it_was() {
    let mut driver = driver(|repo| {
        let mods = repo.home().join("mods");
        script(&mods, "clock.sh", "echo 12:00");
        std::fs::write(mods.join("clock.toml"), CLOCK).unwrap();
    })
    .await;
    let toggle = || UiCommand::Slash {
        command: SlashCommand::ModFile {
            name: "clock".to_owned(),
            words: vec!["toggle".to_owned()],
        },
    };
    let shown = |view: &ModsView| match &view.row("clock").unwrap().kind {
        ModRowKind::Card { file, .. } => (file.shown, file.places.clone()),
        ModRowKind::Own { .. } => panic!("a card"),
    };
    driver.send(toggle()).await;
    let view = mods_view(&mut driver, |view| view.refresh && !shown(view).0).await;
    assert_eq!(shown(&view), (false, vec![ModPlace::Above]));
    let text = std::fs::read_to_string(driver.repo.home().join("mods/clock.toml")).unwrap();
    assert!(text.contains("shown = false"), "{text}");
    driver.send(toggle()).await;
    let view = mods_view(&mut driver, |view| view.refresh && shown(view).0).await;
    assert_eq!(shown(&view), (true, vec![ModPlace::Above]));
    driver.exit().await;
}

#[tokio::test]
async fn doctor_names_a_file_that_didnt_load() {
    let driver = driver(|repo| {
        let mods = repo.home().join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::write(mods.join("broken.toml"), "body = \"pie\"\n").unwrap();
    })
    .await;
    let lines = super::doctor_lines(&driver.env);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines[0].starts_with("mod "), "{lines:?}");
    assert!(lines[0].ends_with("mods/broken.toml · didn't load, and the rest work without it"));
    assert!(lines[1].starts_with("  body is \"pie\""), "{lines:?}");
    // The command line's doctor marks the file as it marks its other problems.
    let marked = super::marked(lines);
    assert!(marked[0].starts_with("! mod "), "{marked:?}");
    assert!(marked[1].starts_with("  body is"), "{marked:?}");
    driver.exit().await;
}

/// A mod of yours that asks for the bottom line by the name of a part the line draws itself
/// loads, but the line never draws it. Doctor and `/mods` say so.
#[tokio::test]
async fn a_mod_the_bottom_line_hides_is_named_in_doctor_and_mods() {
    let mut driver = driver(|repo| {
        let mods = repo.home().join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        let file = "body = \"text\"\nplaces = [\"bottom-line\"]\n[data]\ncommand = [\"date\"]\n";
        std::fs::write(mods.join("artifact.toml"), file).unwrap();
        std::fs::write(mods.join("clock.toml"), file).unwrap();
    })
    .await;
    let lines = super::doctor_lines(&driver.env);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[0].ends_with("mods/artifact.toml · never shows on the bottom line"),
        "{lines:?}"
    );
    assert!(lines[1].contains("its own artifact part"), "{lines:?}");

    let view = open(&mut driver).await;
    let hidden = |name: &str| match &view.row(name).unwrap().kind {
        ModRowKind::Card { hidden_by_line, .. } => *hidden_by_line,
        ModRowKind::Own { .. } => false,
    };
    assert!(hidden("artifact"));
    assert!(!hidden("clock"));
    driver.exit().await;
}

/// What the bottom line's page sends when you pick a CLI's color: one key of `[ui.colors]`,
/// saved in your own settings and on screen at once, and gone again with a reset.
#[tokio::test]
async fn a_picked_color_is_saved_in_your_settings_and_reaches_the_screen() {
    use argonauts_core::settings::SettingScope;
    let mut driver = driver(|_| {}).await;
    let key = "ui.colors.agent-orange".to_owned();
    driver
        .send(UiCommand::SetSetting {
            key: key.clone(),
            value: "\"agent-pink\"".to_owned(),
            scope: SettingScope::Global,
        })
        .await;
    let colors = |event: &AppEvent| match event {
        AppEvent::UiSettings { ui: Some(ui), .. } => Some(
            ui.colors
                .iter()
                .map(|(role, color)| format!("{role}={color}"))
                .collect::<Vec<_>>(),
        ),
        _ => None,
    };
    driver
        .until(|event| colors(event).is_some_and(|have| have == ["agent-orange=agent-pink"]))
        .await;
    let config = driver.repo.home().join("config.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.contains("agent-orange = \"agent-pink\""), "{text}");
    driver
        .send(UiCommand::ResetSetting {
            key,
            scope: SettingScope::Global,
        })
        .await;
    driver
        .until(|event| colors(event).is_some_and(|have| have.is_empty()))
        .await;
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(!text.contains("agent-orange"), "{text}");
    driver.exit().await;
}
