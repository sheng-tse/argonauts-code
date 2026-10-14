//! Fails when a retired name comes back.
//!
//! One table, `retired-names.toml`, holds every retired word, phrase and pattern with the name
//! that replaced it. Every tracked file and file name is read against it, and a failing run
//! lists every place at once with its replacement. The same file keeps real home folders and
//! mail addresses out of the code, and checks the ties a word list can't see: a key spelled
//! unlike its field, a slash command unlike its variant, a prompt template without its file,
//! a role outside the one list, and a recordings folder named by version.
//!
//! Two checks need more than the files. The crew's catalog test compares each template's
//! text with the file its id names. A snapshot no test draws shows only once every test has
//! run, so that check takes two commands. The first has insta list each snapshot it compares
//! (it only adds to the list, so name a file that isn't there yet), the second reads the list:
//!
//! ```sh
//! INSTA_SNAPSHOT_REFERENCES_FILE=/tmp/drawn CI=true cargo test --workspace
//! ARGONAUTS_DRAWN_SNAPSHOTS=/tmp/drawn cargo test -p argonauts-testkit --test retired_names
//! ```
//!
//! `cargo insta test --unreferenced=reject` does both in one where cargo-insta is installed.

#[path = "retired_names/private.rs"]
mod private;
#[path = "retired_names/report.rs"]
mod report;
#[path = "retired_names/snapshots.rs"]
mod snapshots;
#[path = "retired_names/structure.rs"]
mod structure;
#[path = "retired_names/table.rs"]
mod table;
#[path = "retired_names/tree.rs"]
mod tree;
#[path = "retired_names/words.rs"]
mod words;

use report::{Look, Place};

#[test]
fn no_retired_name_is_left() {
    let table = table::load();
    let mut used = words::Used::default();
    let places = words::places(&table, tree::files(), &mut used);
    report::fail_on("retired names", &places);

    // An entry that excuses nothing would let its text come back somewhere else unseen.
    let stale: Vec<String> = table
        .allow
        .iter()
        .zip(&used.0)
        .filter(|(_, used)| !**used)
        .map(|(allow, _)| format!("{}: {:?} ({})", allow.path, allow.text, allow.why))
        .collect();
    assert!(
        stale.is_empty(),
        "these [[allow]] entries no longer match anything, so take them out of {}:\n  {}",
        table::TABLE,
        stale.join("\n  ")
    );
    for unread in &table.unread {
        assert!(
            tree::files().iter().any(|file| file.path == unread.path),
            "{}: [[unread]] names {}, which isn't a tracked file ({})",
            table::TABLE,
            unread.path,
            unread.why
        );
    }
    for pattern in &table.pattern {
        for only in &pattern.only_in {
            assert!(
                tree::files()
                    .iter()
                    .any(|file| table::fits(only, &file.path)),
                "{}: the pattern {:?} is read only in {only}, which no file fits",
                table::TABLE,
                pattern.old
            );
        }
    }
}

#[test]
fn no_real_home_folder_or_address_is_in_a_file() {
    let table = table::load();
    report::fail_on("real names", &private::places(&table, tree::files()));
}

#[test]
fn a_key_is_spelled_like_its_field() {
    let table = table::load();
    report::fail_on(
        "keys unlike their fields",
        &structure::serde_keys(&table, tree::files()),
    );
}

#[test]
fn a_slash_command_is_named_like_its_variant() {
    let table = table::load();
    let places = structure::slash_commands(&table, tree::files());
    report::fail_on("slash commands unlike their variants", &places);
}

#[test]
fn a_prompt_template_sits_at_the_path_its_id_names() {
    report::fail_on("prompt templates", &structure::templates(tree::files()));
}

#[test]
fn a_role_comes_from_the_one_role_list() {
    report::fail_on("roles", &structure::roles(tree::files()));
}

#[test]
fn a_role_outside_the_list_is_found_in_a_workflows_file_and_in_a_file_of_phases() {
    let file = |path: &str, text: &str| tree::File {
        path: path.to_owned(),
        text: Some(text.to_owned()),
    };
    let files = [
        file(
            "templates/workflows/sample.toml",
            "[[step]]\nname = \"Plan\"\nrole = \"planner\"   # who plans\n\n[[step]]\nrole = \"worker\"\n\n  [[phases.steps]]\n  role  = \"scribe\"\n",
        ),
        file(
            ".argonauts/workflows/sample.yaml",
            "steps:\n  - kind: read\n    role: reader\n  - role: \"scribe\"\n",
        ),
        // A `role` key in any other file isn't a workflow's.
        file("crates/core/sample.toml", "role = \"planner\"\n"),
    ];
    let found: Vec<(String, Option<usize>, String)> = structure::roles(&files)
        .into_iter()
        .map(|place| (place.path, place.line, place.found))
        .collect();
    let place =
        |path: &str, line: usize, role: &str| (path.to_owned(), Some(line), role.to_owned());
    assert_eq!(
        found,
        [
            place("templates/workflows/sample.toml", 3, "planner"),
            place("templates/workflows/sample.toml", 9, "scribe"),
            place(".argonauts/workflows/sample.yaml", 4, "scribe"),
        ]
    );
}

#[test]
fn no_recordings_folder_is_named_by_version() {
    let table = table::load();
    let places = structure::fixture_folders(&table, tree::files());
    report::fail_on("recordings folders", &places);
}

/// Which snapshots are drawn is known only after every test has run, so this reads the list
/// of such a run when `ARGONAUTS_DRAWN_SNAPSHOTS` names it.
#[test]
fn no_snapshot_is_left_undrawn() {
    let Some(list) = std::env::var_os("ARGONAUTS_DRAWN_SNAPSHOTS") else {
        return;
    };
    let list = std::path::PathBuf::from(list);
    let drawn =
        std::fs::read_to_string(&list).unwrap_or_else(|e| panic!("{}: {e}", list.display()));
    let places = snapshots::undrawn(&table::workspace(), &drawn, tree::files());
    let snapshots = tree::files()
        .iter()
        .filter(|file| file.path.ends_with(".snap"))
        .count();
    assert!(
        places.len() < snapshots,
        "{}: names none of this workspace's {snapshots} snapshots, so it isn't the list of a \
         run of its tests",
        list.display()
    );
    report::fail_on("snapshots no test draws", &places);
}

#[test]
fn a_snapshot_missing_from_the_list_is_found() {
    let workspace = tempfile::tempdir().unwrap();
    let folder = "crates/tui/src/views/snapshots";
    std::fs::create_dir_all(workspace.path().join(folder)).unwrap();
    let file = |name: &str| {
        std::fs::write(workspace.path().join(folder).join(name), "").unwrap();
        tree::File {
            path: format!("{folder}/{name}"),
            text: Some(String::new()),
        }
    };
    let files = [
        file("argonauts_tui__views__tests__resume_at_100.snap"),
        file("argonauts_tui__views__tests__resume_at_90.snap"),
        file("notes.md"),
    ];
    // The list names one snapshot, and one that was deleted since.
    let listed = |name: &str| workspace.path().join(folder).join(name);
    let drawn = format!(
        "{}\n{}\n",
        listed("argonauts_tui__views__tests__resume_at_100.snap").display(),
        listed("argonauts_tui__views__tests__resume_at_80.snap").display(),
    );

    let found: Vec<(String, String)> = snapshots::undrawn(workspace.path(), &drawn, &files)
        .into_iter()
        .map(|place| (place.path, place.found))
        .collect();
    assert_eq!(
        found,
        [(
            format!("{folder}/argonauts_tui__views__tests__resume_at_90.snap"),
            "resume_at_90".to_owned()
        )]
    );
}

/// The docs in the folder `ARGONAUTS_DOCS` names are read only when it is set.
#[test]
fn the_living_docs_hold_no_retired_name() {
    let Some(docs) = std::env::var_os("ARGONAUTS_DOCS") else {
        return;
    };
    let table = table::load();
    let docs = std::path::PathBuf::from(docs);
    let files: Vec<tree::File> = table
        .living_docs
        .files
        .iter()
        .filter_map(|name| {
            let text = std::fs::read_to_string(docs.join(name)).ok()?;
            Some(tree::File {
                path: name.clone(),
                text: Some(text),
            })
        })
        .collect();
    assert!(
        !files.is_empty(),
        "{}: none of the living docs is there",
        docs.display()
    );
    let lookup = words::Lookup::new(&table);
    let allowed = &table.living_docs.allow;
    let mut used = vec![false; allowed.len()];
    let mut places = Vec::new();
    for file in &files {
        for (number, line) in file.text.as_deref().unwrap_or_default().lines().enumerate() {
            let mut line = std::borrow::Cow::Borrowed(line);
            for (allow, used) in allowed.iter().zip(&mut used) {
                if let Some(without) = words::blanked(&line, &allow.text) {
                    *used = true;
                    line = without.into();
                }
            }
            for (found, new) in lookup.in_text(&line, &file.path) {
                places.push(Place::new(&file.path, Some(number + 1), &found, new));
            }
        }
    }
    report::fail_on("retired names in the living docs", &places);

    // As with the code's own entries: one that excuses nothing is taken out.
    let stale: Vec<String> = allowed
        .iter()
        .zip(&used)
        .filter(|(_, used)| !**used)
        .map(|(allow, _)| format!("{:?} ({})", allow.text, allow.why))
        .collect();
    assert!(
        stale.is_empty(),
        "these [[living_docs.allow]] entries no longer match anything, so take them out of {}:\n  {}",
        table::TABLE,
        stale.join("\n  ")
    );
}

/// Each text the docs may keep holds a retired name, or its entry would excuse nothing, and
/// is passed over whole.
#[test]
fn what_the_docs_may_keep_is_passed_over_whole() {
    let table = table::load();
    let lookup = words::Lookup::new(&table);
    let doc = "doc.md";
    for allow in &table.living_docs.allow {
        let line = format!("see {} for it", allow.text);
        assert!(
            !lookup.in_text(&line, doc).is_empty(),
            "{:?} holds no retired name",
            allow.text
        );
        let without = words::blanked(&line, &allow.text).expect("the text is in the line");
        assert!(lookup.in_text(&without, doc).is_empty(), "{without}");
    }
}

#[test]
fn the_report_reads_as_its_picture() {
    let table = table::load();
    let example = &table.report_example;
    let places: Vec<Place> = example
        .places
        .iter()
        .map(|place| {
            let new = table
                .replacement(&place.word)
                .unwrap_or_else(|| panic!("{} isn't in the table", place.word));
            Place::new(&place.path, Some(place.line), &place.word, new)
        })
        .collect();
    let lines = report::lines("retired names", &places);
    assert_eq!(report::plain(&lines), example.prints.trim_end_matches('\n'));

    // The mark is red and the count bold. A place is dim, the word plain, its replacement
    // green. The two closing notes are dim.
    let looks: Vec<Vec<Look>> = lines
        .iter()
        .map(|line| line.iter().map(|(look, _)| *look).collect())
        .collect();
    let place = vec![Look::Dim, Look::Plain, Look::Green];
    assert_eq!(
        looks,
        [
            vec![Look::Red, Look::Bold],
            place.clone(),
            place.clone(),
            place,
            vec![],
            vec![Look::Dim],
            vec![Look::Dim],
        ]
    );
}

#[test]
fn words_are_matched_whole() {
    let table = table::load();
    let lookup = words::Lookup::new(&table);
    let found = |text: &str| -> Vec<String> {
        let found = lookup.in_text(text, "crates/app/src/sample.rs");
        found.into_iter().map(|(word, _)| word).collect()
    };
    // A longer word that holds a retired one is its own word.
    for plain in [
        "seatbelt",
        "discard",
        "aggregate",
        "delegate",
        "cardinal",
        "shadowing",
        "leading",
        "companion",
        "#[derive(Clone, Copy)]",
        "handed",
        "standing",
    ] {
        assert_eq!(found(plain), Vec::<String>::new(), "{plain}");
    }
    // Every spelling of a part is the same part.
    let table_words: Vec<&str> = table.word.iter().map(|word| word.old[0].as_str()).collect();
    for word in table_words {
        let capital = format!("{}{}", word[..1].to_uppercase(), &word[1..]);
        for spelled in [
            word.to_owned(),
            word.to_uppercase(),
            format!("the_{word}_id"),
            format!("my{capital}Id"),
            format!("{word}-7"),
        ] {
            assert_eq!(found(&spelled), [word], "{spelled}");
        }
    }
    // A phrase is consecutive parts, however they are joined.
    let phrase = &table.phrase[0].old[0];
    let (first, second) = phrase.split_once(' ').expect("two words");
    let capital = |word: &str| format!("{}{}", word[..1].to_uppercase(), &word[1..]);
    for spelled in [
        phrase.clone(),
        format!("{first}-{second}"),
        format!("{first}_{second}"),
        format!("{}{}", capital(first), capital(second)),
    ] {
        assert_eq!(found(&spelled), [phrase.as_str()], "{spelled}");
    }
    assert_eq!(
        found(&format!("{first} and {second}")),
        Vec::<String>::new()
    );
}

#[test]
fn a_banned_text_is_found_whatever_its_case() {
    let mut table = table::load();
    let sample = "Ada-Lovelace";
    table.private.banned = vec![table::BannedText {
        what: "a sample name".to_owned(),
        fix: "a neutral name".to_owned(),
        length: sample.len(),
        hash: format!("{:#x}", private::hash_of(sample.as_bytes())),
    }];
    let file = |text: &str| {
        vec![tree::File {
            path: "notes.md".to_owned(),
            text: Some(text.to_owned()),
        }]
    };
    let found = |text: &str| -> Vec<String> {
        let places = private::places(&table, &file(text));
        places.into_iter().map(|place| place.found).collect()
    };
    assert_eq!(found("made on ada-lovelace's laptop"), ["ada-lovelace"]);
    assert_eq!(found("ADA-LOVELACE"), ["ADA-LOVELACE"]);
    assert_eq!(found("ada-lovelaces"), Vec::<String>::new());
    // The samples are put together here, so this file holds no name the check would find.
    let ada = "ada";
    assert_eq!(
        found(&format!("/Users/{ada}/code")),
        [format!("/Users/{ada}")]
    );
    assert_eq!(
        found("/Users/you/code and {tmp}/home/skills"),
        Vec::<String>::new()
    );
    assert_eq!(
        found(&format!("-Users-{ada}-code-app")),
        [format!("-Users-{ada}")]
    );
    assert_eq!(
        found(&format!("write to {ada}@lovelace.dev")),
        [format!("{ada}@lovelace.dev")]
    );
    assert_eq!(
        found("you@example.com, serde@1.0, @notes.txt"),
        Vec::<String>::new()
    );
}
