//! Fails when a retired name comes back.
//!
//! One table, `retired-names.toml`, holds every retired word, phrase and pattern with the name
//! that replaced it. Every tracked file and file name is read against it, and a failing run
//! lists every place at once with its replacement. The same file keeps real home folders,
//! mail addresses, plans and ids out of the code, and checks the ties a word list can't see: a key
//! spelled unlike its field, a slash command unlike its variant, a prompt template without its
//! file, a role outside the one list, and a recordings folder named by version. A recorded
//! session keeps only a placeholder for a file its CLI bundles, since that text is its maker's.
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

#[path = "retired_names/account.rs"]
mod account;
#[path = "retired_names/binary.rs"]
mod binary;
#[path = "retired_names/bundled.rs"]
mod bundled;
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
            tree::files()
                .iter()
                .any(|file| table::fits(&unread.path, &file.path)),
            "{}: [[unread]] names {}, which fits no tracked file ({})",
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
fn no_account_or_machine_detail_is_in_a_file() {
    let table = table::load();
    report::fail_on("real names", &private::places(&table, tree::files()));
}

fn file(path: &str, text: String) -> tree::File {
    tree::File {
        path: path.to_owned(),
        text: Some(text),
    }
}

/// What the real-names check finds in each file, with its line.
fn found_in(table: &table::Table, files: &[tree::File]) -> Vec<(String, Option<usize>, String)> {
    private::places(table, files)
        .into_iter()
        .map(|place| (place.path, place.line, place.found))
        .collect()
}

/// A made-up home folder, address, temp folder and plan, put together here so this file holds
/// none of them. An address that takes no mail, a neutral folder and the neutral plan pass.
#[test]
fn a_home_folder_an_address_a_temp_folder_and_a_plan_are_found() {
    let table = table::load();
    let (name, plan, key, temp) = ("mallory", "pro", "planType", "k7/q2w9zr1mt5b8c_d3fh60000gn");
    let placeholder = format!("xx/{}", "x".repeat(30));
    let sample = [
        format!("let repo = \"/home/{name}/limiter\";"),
        format!("// asked by {name}@sails.dev"),
        format!(r#"json!({{"{key}": "{plan}", "email": "noreply@github.com"}})"#),
        format!(r#"{{"{key}":"team","{key}":null}} 1+{name}@users.noreply.github.com /Users/you"#),
        format!("open file:///Users/{name}/notes.md and /private/var/folders/{temp}/T/a.txt"),
        "wrote /var/folders/xx/xxxxxxxx/T/hello.txt".to_owned(),
        format!("wrote /var/folders/{placeholder}/T/hello.txt as 1+you@users.noreply.github.com"),
    ];
    let files = [
        file("crates/adapters/codex/src/sample.rs", sample.join("\n")),
        // A recording holds JSON in its strings too, and Claude Code's plan is read there.
        file(
            "crates/adapters/codex/fixtures/sample.meta.json",
            [
                format!(r#"{{"stdin": "{{\"{key}\":\"{plan}\"}}","#),
                format!(r#""usage": {{"subscription_type":"{plan}"}}}}"#),
            ]
            .join("\n"),
        ),
        // The code's own samples of Claude Code's plans tell one plan from another.
        file(
            "crates/adapters/claude-code/src/sample.rs",
            format!(r#"json!({{"subscription_type": "{plan}"}})"#),
        ),
    ];
    let place = |path: &str, line: usize, found: String| (path.to_owned(), Some(line), found);
    let code = "crates/adapters/codex/src/sample.rs";
    let recording = "crates/adapters/codex/fixtures/sample.meta.json";
    assert_eq!(
        found_in(&table, &files),
        [
            place(code, 1, format!("/home/{name}")),
            place(code, 2, format!("{name}@sails.dev")),
            place(code, 3, format!("{key}: {plan}")),
            place(code, 4, format!("1+{name}@users.noreply.github.com")),
            place(code, 5, format!("/Users/{name}")),
            place(code, 5, format!("/var/folders/{temp}")),
            place(recording, 1, format!("{key}: {plan}")),
            place(recording, 2, format!("subscription_type: {plan}")),
        ]
    );
}

/// Each line of the sample is one way to write a made-up home or temp folder, and the last
/// lines are what only looks like one.
#[test]
fn every_spelling_of_a_home_or_temp_folder_is_found() {
    let table = table::load();
    let (name, bird) = ("mallory", "\u{f1}and\u{fa}");
    let (group, id) = ("k7", "q2w9zr1mt5b8c_d3fh60000gn");
    let dashed_id = id.replace('_', "-");
    let x = "x".repeat(30);
    let sample = [
        format!(r#"{{"cwd":"\/Users\/{name}\/limiter"}}"#),
        format!(r#"{{"cwd":"C:\\Users\\{name}\\limiter"}}"#),
        format!(r"/users/{name}/limiter and c:\users\{name}"),
        format!("/System/Volumes/Data/Users/{name} and /Volumes/Macintosh HD/Users/{name}"),
        format!("/var/home/{name}/limiter"),
        format!("open %2FUsers%2F{name}%2Flimiter"),
        format!("cd ~{name}/limiter or ~{name}"),
        format!("/home/{bird}/limiter"),
        format!(r#"{{"tmp":"\/var\/folders\/{group}\/{id}\/T"}}"#),
        format!("~/.claude/projects/-home-{name}-limiter/"),
        format!("~/.claude/projects/-Users-{name}-limiter/ and C--Users-{name}-limiter"),
        format!("projects/-private-var-folders-{group}-{dashed_id}-T-limiter"),
        format!(r#"{{"text":"see\n/Users/{name}"}}"#),
        // None of these is anyone's.
        format!(
            "--home-dir -Users-you-limiter {{tmp}}/home/skills https://example.com/users/{name}"
        ),
        format!("/var/folders/xx/{x}/T and -var-folders-xx-{x}-T-limiter"),
        "<<~EOS, takes ~ten minutes, GET /users/me and /users/{id}".to_owned(),
    ];
    let path = "crates/tui/src/sample.rs";
    let place = |line: usize, found: String| (path.to_owned(), Some(line), found);
    assert_eq!(
        found_in(&table, &[file(path, sample.join("\n"))]),
        [
            place(1, format!("/Users/{name}")),
            place(2, format!(r"\Users\{name}")),
            place(3, format!("/users/{name}")),
            place(3, format!(r"\users\{name}")),
            place(4, format!("/Users/{name}")),
            place(5, format!("/home/{name}")),
            place(6, format!("/Users/{name}")),
            place(7, format!("~{name}")),
            place(8, format!("/home/{bird}")),
            place(9, format!("/var/folders/{group}/{id}")),
            place(10, format!("-home-{name}")),
            place(11, format!("-Users-{name}")),
            place(12, format!("-var-folders-{group}-{dashed_id}")),
            place(13, format!("/Users/{name}")),
        ]
    );
}

/// An address names its account unless its whole name is one that takes no mail. GitHub's
/// address for a hidden login names the login, so only a neutral one passes.
#[test]
fn an_address_that_names_its_account_is_found() {
    let table = table::load();
    let (name, domain) = ("mallory", "sails.dev");
    let sample = [
        format!("4821+{name}@users.noreply.github.com and {name}@users.noreply.github.com"),
        format!("{name}@noreply.{domain}"),
        format!(r"{name}\u0040{domain} and {name}%40{domain}"),
        format!("noreply@{domain}, no-reply@{domain}, 1+you@users.noreply.github.com"),
    ];
    let path = "crates/app/src/sample.rs";
    let place = |line: usize, found: String| (path.to_owned(), Some(line), found);
    assert_eq!(
        found_in(&table, &[file(path, sample.join("\n"))]),
        [
            place(1, format!("4821+{name}@users.noreply.github.com")),
            place(1, format!("{name}@users.noreply.github.com")),
            place(2, format!("{name}@noreply.{domain}")),
            place(3, format!("{name}@{domain}")),
        ]
    );
}

/// The maintainer's address passes only in a file that gives it for reports, and any other
/// address there is still found. A made-up address takes the real one's place here, so this
/// file holds none.
#[test]
fn the_contact_address_passes_only_where_it_is_given() {
    let mut table = table::load();
    let domain = "sails.dev";
    let (contact, other) = (format!("mallory@{domain}"), format!("trent@{domain}"));
    table.private.contact = table::Contact {
        length: contact.len(),
        hash: format!("{:#x}", private::hash_of(contact.as_bytes())),
        given_in: vec!["SECURITY.md".to_owned()],
    };
    let (given, elsewhere) = ("SECURITY.md", "crates/app/src/sample.rs");
    let text = format!("Email {contact}, or {other}.");
    let place = |path: &str, found: &str| (path.to_owned(), Some(1), found.to_owned());
    assert_eq!(
        found_in(&table, &[file(given, text.clone()), file(elsewhere, text)]),
        [
            place(given, &other),
            place(elsewhere, &contact),
            place(elsewhere, &other),
        ]
    );
}

/// Codex's plan is read in every file and in every way it can be written, and any CLI's plan
/// in a recording, its own words included. An agent's plan in plan mode is text, not a plan.
#[test]
fn every_spelling_of_a_plan_is_found() {
    let table = table::load();
    let (plan, key) = ("pro", "planType");
    let codex = [key, "plan_type", "chatgpt_plan_type", "chatgptPlanType"];
    let recorded = [
        format!(r#"{{"loggedIn":true,"subscriptionType":"{plan}","orgName":"<organization>"}}"#),
        format!(r#"{{"result":"{{\"account\":{{\"subscriptionType\":\"{plan}\"}}}}"}}"#),
        format!(r#"{{"stdin":"{{\"line\":\"{{\\\"{key}\\\":\\\"{plan}\\\"}}\"}}"}}"#),
        format!(r#"{{"data":{{"plan":"{plan}","tier":"{plan}","currentTier":"{plan}","userTier":"{plan}"}}}}"#),
        r#"{"plan":"Add a test, then fix it","tier":null,"subscriptionType":"<plan>","next":{"plan":{"mode":"auto"}}}"#.to_owned(),
    ]
    .join("\n");
    let files = [
        file(
            "crates/adapters/codex/src/sample.rs",
            codex
                .iter()
                .map(|key| format!(r#"json!({{"{key}": "{plan}"}}) or {key}: String,"#))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        file(
            "crates/adapters/codex/tests/sample.toml",
            format!("plan_type = \"{plan}\""),
        ),
        file(
            "crates/adapters/codex/tests/sample.yaml",
            format!("planType: {plan}"),
        ),
        file(
            "crates/adapters/claude-code/fixtures/sample.stdout.jsonl",
            recorded,
        ),
        // A pretty-printed meta file may put a value on the line after its key.
        file(
            "crates/adapters/codex/fixtures/sample.meta.json",
            format!("{{\n  \"{key}\":\n    \"{plan}\"\n}}"),
        ),
        file(
            "crates/adapters/antigravity-cli/fixtures/sample.stdout.txt",
            [
                " Your Plan: Sails Pro",
                " Login method: Claude Pro Account",
                " Login method: Claude Team Account, Your Plan: <plan>",
            ]
            .join("\n"),
        ),
        // The code's own samples of the other plans tell one from another.
        file(
            "crates/adapters/antigravity-cli/src/sample.rs",
            format!(" Your Plan: Sails Pro {{\"plan\": \"{plan}\"}}"),
        ),
    ];
    let place =
        |path: &str, line: Option<usize>, found: &str| (path.to_owned(), line, found.to_owned());
    let code = "crates/adapters/codex/src/sample.rs";
    let recording = "crates/adapters/claude-code/fixtures/sample.stdout.jsonl";
    let words = "crates/adapters/antigravity-cli/fixtures/sample.stdout.txt";
    assert_eq!(
        found_in(&table, &files),
        [
            place(code, Some(1), "planType: pro"),
            place(code, Some(2), "plan_type: pro"),
            place(code, Some(3), "chatgpt_plan_type: pro"),
            place(code, Some(4), "chatgptPlanType: pro"),
            place(
                "crates/adapters/codex/tests/sample.toml",
                Some(1),
                "plan_type: pro"
            ),
            place(
                "crates/adapters/codex/tests/sample.yaml",
                Some(1),
                "planType: pro"
            ),
            place(recording, Some(1), "subscriptionType: pro"),
            place(recording, Some(2), "subscriptionType: pro"),
            place(recording, Some(3), "planType: pro"),
            place(recording, Some(4), "plan: pro"),
            place(recording, Some(4), "tier: pro"),
            place(recording, Some(4), "currentTier: pro"),
            place(recording, Some(4), "userTier: pro"),
            place(
                "crates/adapters/codex/fixtures/sample.meta.json",
                Some(3),
                "planType: pro"
            ),
            place(words, Some(1), "Your Plan: Sails Pro"),
            place(words, Some(2), "Login method: Claude Pro Account"),
        ]
    );
}

/// An id of the account, its organization or the install, and a time with the recording
/// machine's offset, in a recording. The code's own samples are made up, so they pass.
#[test]
fn an_accounts_ids_and_the_machines_time_zone_are_found() {
    let table = table::load();
    let ids = r#"{"accountId":"acct-8f3k2","installationId":"<redacted>","environmentId":null}
{"user_id":"user-41x","organization_uuid":"00000000-0000-0000-0000-000000000000","orgId":4417}
{"result":"{\"account_uuid\":\"5c1d-77\"}"}
{"modified_at":"2026-01-02T03:04:05.6789+05:30","expires_at":"2026-01-02T03:04:05+00:00","at":"2026-01-02T03:04:05Z"}"#;
    let files = [
        file(
            "crates/registry/tests/fixtures/sails/sample.stdout.jsonl",
            ids.to_owned(),
        ),
        file("crates/registry/src/sample.rs", ids.to_owned()),
    ];
    let recording = "crates/registry/tests/fixtures/sails/sample.stdout.jsonl";
    let place = |line: usize, found: &str| (recording.to_owned(), Some(line), found.to_owned());
    assert_eq!(
        found_in(&table, &files),
        [
            place(1, "accountId: acct-8f3k2"),
            place(2, "orgId: 4417"),
            place(2, "user_id: user-41x"),
            place(3, "account_uuid: 5c1d-77"),
            place(4, "2026-01-02T03:04:05.6789+05:30"),
        ]
    );
}

/// The name Codex gives the machine beside its install's id, and an organization anywhere in a
/// recording, a JSON string's included. An MCP server's name is no machine's.
#[test]
fn a_machines_name_and_an_organization_are_found() {
    let table = table::load();
    let recorded = r#"{"method":"remoteControl/status/changed","params":{"installationId":"<redacted>","serverName":"sails-laptop"}}
{"params":{"installationId":"<redacted>","serverName":"Mac"},"tools":[{"name":"mcp__docs__read","serverName":"docs"}]}
{"loggedIn":true,"orgName":"Sails Ltd"}
{"result":"{\"account\":{\"organization\":\"Sails Ltd\"}}"}"#;
    let files = [file(
        "crates/adapters/codex/fixtures/sample.app-server.jsonl",
        recorded.to_owned(),
    )];
    let found: Vec<(Option<usize>, String)> = private::setup_places(&table, &files)
        .into_iter()
        .map(|place| (place.line, place.found))
        .collect();
    let place = |line: usize, found: &str| (Some(line), found.to_owned());
    assert_eq!(
        found,
        [
            place(1, "serverName: sails-laptop"),
            place(3, "orgName: Sails Ltd"),
            place(4, "organization: Sails Ltd"),
        ]
    );
}

#[test]
fn every_fixture_is_a_recording_or_made_here() {
    let table = table::load();
    let unsorted: Vec<&str> = tree::files()
        .iter()
        .map(|file| file.path.as_str())
        .filter(|path| path.contains("/fixtures/") || path.contains("/tests/data/"))
        .filter(|path| {
            !table.is_recording(path) && !table::fits_any(&table.recordings.made_here, path)
        })
        .collect();
    assert!(
        unsorted.is_empty(),
        "each of these is a recording, so give it a meta file beside it, or was written here, \
         so list it under [recordings] made_here in {}:\n  {}",
        table::TABLE,
        unsorted.join("\n  ")
    );
    for made_here in &table.recordings.made_here {
        assert!(
            tree::files()
                .iter()
                .any(|file| table::fits(made_here, &file.path)),
            "{}: made_here names {made_here}, which fits no tracked file",
            table::TABLE
        );
    }
}

#[test]
fn a_file_beside_its_meta_is_a_recording() {
    let tracked = [
        "crates/sails/fixtures/opencode/providers.meta.json",
        "crates/sails/fixtures/ollama/meta.json",
        "crates/sails/src/providers.meta.json",
    ];
    let beside = |path: &str| tree::has_a_meta_beside(path, |meta| tracked.contains(&meta));
    assert!(beside(
        "crates/sails/fixtures/opencode/providers_models.stdout.txt"
    ));
    assert!(beside(
        "crates/sails/fixtures/opencode/providers.stdout.json"
    ));
    assert!(beside("crates/sails/fixtures/ollama/tags.json"));
    assert!(!beside(
        "crates/sails/fixtures/opencode/build_hello.stdout.jsonl"
    ));
    assert!(!beside("crates/sails/fixtures/opencode/providersx.json"));
    assert!(!beside("crates/sails/src/providers_models.txt"));
}

#[test]
fn no_file_keeps_an_os_file_or_a_pictures_text() {
    let table = table::load();
    let paths: Vec<&str> = tree::files()
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    let root = table::workspace();
    let read = |path: &str| std::fs::read(root.join(path)).unwrap_or_default();
    report::fail_on(
        "the machine's own files",
        &binary::places(&table.private, &paths, read),
    );
}

#[test]
fn an_os_file_and_a_pictures_text_are_found() {
    let table = table::load();
    let chunk = |kind: &[u8; 4], data: &[u8]| {
        let mut bytes = u32::try_from(data.len()).unwrap().to_be_bytes().to_vec();
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(data);
        bytes.extend_from_slice(&[0; 4]);
        bytes
    };
    let png = |chunks: &[Vec<u8>]| {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        chunks
            .iter()
            .for_each(|chunk| bytes.extend_from_slice(chunk));
        bytes
    };
    let header = chunk(b"IHDR", &[0; 13]);
    let end = chunk(b"IEND", &[]);
    let with_text = png(&[
        header.clone(),
        chunk(b"tEXt", b"Author\0mallory"),
        chunk(b"eXIf", b"MM\0*"),
        end.clone(),
    ]);
    let clean = png(&[header, end]);
    let paths = [
        "assets/.DS_Store",
        "assets/._logo.png",
        "assets/logo.png",
        "assets/clean.png",
    ];
    let read = |path: &str| match path {
        "assets/logo.png" => with_text.clone(),
        "assets/clean.png" => clean.clone(),
        _ => b"\0\0\0\x01Bud1".to_vec(),
    };
    let found: Vec<(String, String)> = binary::places(&table.private, &paths, read)
        .into_iter()
        .map(|place| (place.path, place.found))
        .collect();
    let place = |path: &str, found: &str| (path.to_owned(), found.to_owned());
    assert_eq!(
        found,
        [
            place("assets/.DS_Store", ".DS_Store"),
            place("assets/._logo.png", "._logo.png"),
            place("assets/logo.png", "tEXt"),
            place("assets/logo.png", "eXIf"),
        ]
    );
}

#[test]
fn no_recording_keeps_the_machines_own_setup() {
    let table = table::load();
    report::fail_on(
        "the recording machine's setup",
        &private::setup_places(&table, tree::files()),
    );
}

/// Made-up names take the place of a machine's own: the check needs only their shape.
#[test]
fn a_recordings_setup_and_a_scratch_folder_are_found() {
    let table = table::load();
    let file = |path: &str, text: String| tree::File {
        path: path.to_owned(),
        text: Some(text),
    };
    let init = r#"{"type":"system","subtype":"init","skills":["debug","anthropic-skills:field-notes","anthropic-skills:example-a"],"slash_commands":["compact","standup-notes","probe-cmd"],"agents":["Explore","release-captain"],"plugins":[{"name":"lint-helper","path":"~/.claude/plugins/cache/claude-plugins-official/lint-helper/1.0.0","source":"lint-helper@claude-plugins-official"},{"name":"example-plugin-a","source":"example-plugin-a@claude-plugins-official"},{"name":"cc-plugin-telemetry","path":"builtin"},{"name":"probe","source":"probe@inline"}],"mcp_servers":[{"name":"claude.ai Trip Planner","source":"claudeai"},{"name":"claude.ai Example A","source":"claudeai"},{"name":"argonauts","source":"dynamic"}],"tools":["Read","mcp__claude_ai_Trip_Planner__plan","mcp__claude_ai_Example_A__tool_a","mcp__claude_ai_Example_A__find_trips"]}"#;
    let account = r#"{"response":{"account":{"email":"<email>","organization":"Sails Ltd","subscriptionType":"<plan>"}}}"#;
    let commands = r#"{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"commands":[{"name":"field-notes","description":"Field notes (claude.ai sync)","argumentHint":"","aliases":["anthropic-skills:example-a"]},{"name":"example-b","description":"An example skill (claude.ai sync)","argumentHint":"","aliases":["anthropic-skills:example-b"]},{"name":"clear","description":"Clear","argumentHint":"","aliases":["reset"]}]}}}"#;
    let usage = r#"{"skills":{"skillFrontmatter":[{"name":"trip-planner","source":"syncedSkills","tokens":6},{"name":"example-c","source":"syncedSkills","tokens":6},{"name":"debug","source":"built-in","tokens":5}]}}"#;
    let files = [
        file(
            "crates/adapters/claude-code/fixtures/sample.stdout.jsonl",
            format!("{init}\n{account}\n{commands}\n{usage}\n"),
        ),
        // The code's own examples are made up, so only recordings are read.
        file("crates/adapters/claude-code/src/sample.rs", init.to_owned()),
    ];
    let found: Vec<String> = private::setup_places(&table, &files)
        .iter()
        .map(|place| place.found.clone())
        .collect();
    assert_eq!(
        found,
        [
            "anthropic-skills:field-notes",
            "mcp__claude_ai_Trip_Planner",
            "mcp__claude_ai_Example_A__find_trips",
            "lint-helper",
            "plugin lint-helper",
            "slash_commands standup-notes",
            "agents release-captain",
            "connector claude.ai Trip Planner",
            "organization: Sails Ltd",
            "synced skill field-notes",
            "synced skill field-notes: Field notes (claude.ai sync)",
            "synced skill trip-planner",
        ]
    );

    let scratch = format!("{}/{}-{}/scratchpad", "/private/tmp", "claude", 501);
    let files = [file(
        "crates/tui/src/sample.rs",
        format!("let repo = \"{scratch}\";"),
    )];
    let found: Vec<String> = private::places(&table, &files)
        .iter()
        .map(|place| place.found.clone())
        .collect();
    assert_eq!(found, [format!("{}-{}", "claude", 501)]);
}

#[test]
fn no_recording_keeps_a_file_its_cli_bundles() {
    let table = table::load();
    report::fail_on(
        "a CLI's bundled files",
        &bundled::places(&table, tree::files()),
    );
}

/// A made-up skill a CLI might bundle, put together here so this file holds none of a real
/// one's text: read in full, cut to its placeholder, launched by a Skill call, and kept in a
/// session's own log. A file read from the project is the test's own and passes.
#[test]
fn a_bundled_files_text_beyond_its_placeholder_is_found() {
    let table = table::load();
    let rule = &table.recordings.bundled_text;
    let folder = format!("<tmp>/{}9.9.9/abc123/field-notes", rule.folder);
    let path = format!("{folder}/{}.md", "guide");
    let heading = ["#", "Field", "notes"].join(" ");
    let steps: Vec<String> = (1..=3).map(|n| format!("Step {n} of the notes.")).collect();
    let full = format!("{heading}\n\n{}\n", steps.join("\n"));
    let placeholder = format!("{heading}\n{}\n", rule.note);
    let numbered = |text: &str| {
        let lines: Vec<String> = (1..)
            .zip(text.lines())
            .map(|(n, line)| format!("{n}\t{line}"))
            .collect();
        lines.join("\n")
    };
    let read = |id: &str, path: &str| {
        serde_json::json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": id, "name": "Read", "input": {"file_path": path}},
        ]}})
    };
    let result = |id: &str, path: &str, text: &str, key: &str| {
        serde_json::json!({
            "type": "user",
            "message": {"content": [{"tool_use_id": id, "type": "tool_result", "content": numbered(text)}]},
            key: {"type": "text", "file": {"filePath": path, "content": text}},
        })
    };
    let launched = |text: &str| {
        let launch = format!("{} {folder}\n\n{text}", rule.skill_preamble);
        serde_json::json!({"type": "user", "message": {"content": [{"type": "text", "text": launch}]}})
    };
    let own = "<workdir>/NOTES.md";
    let lines = [
        read("t1", &path),
        result("t1", &path, &full, "tool_use_result"),
        read("t2", &path),
        result("t2", &path, &placeholder, "tool_use_result"),
        launched(&full),
        launched(&placeholder),
        result("t3", &path, &full, "toolUseResult"),
        read("t4", own),
        result("t4", own, &full, "tool_use_result"),
    ];
    let lines: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
    let files = [file(
        "crates/adapters/claude-code/fixtures/sample.stdout.jsonl",
        lines.join("\n"),
    )];

    let found: Vec<(Option<usize>, String)> = bundled::places(&table, &files)
        .into_iter()
        .map(|place| (place.line, place.found))
        .collect();
    let file_name = format!("{}9.9.9/abc123/field-notes/guide.md, 4 lines", rule.folder);
    let skill_name = format!("{}9.9.9/abc123/field-notes, 4 lines", rule.folder);
    assert_eq!(
        found,
        [
            (Some(2), file_name.clone()),
            (Some(2), file_name.clone()),
            (Some(5), skill_name),
            (Some(7), file_name.clone()),
            (Some(7), file_name),
        ]
    );
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
            "crates/core/templates/workflows/sample.toml",
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
            place("crates/core/templates/workflows/sample.toml", 3, "planner"),
            place("crates/core/templates/workflows/sample.toml", 9, "scribe"),
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

/// The docs in the folder `ARGONAUTS_DOCS` names are read only when it is set, with
/// `living-docs.toml` there listing which ones and what they may keep.
#[test]
fn the_living_docs_hold_no_retired_name() {
    let Some(docs) = docs_folder() else {
        return;
    };
    let table = table::load();
    let living = table::living_docs(&docs);
    let files: Vec<tree::File> = living
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
    let allowed = &living.allow;
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
        "these [[allow]] entries no longer match anything, so take them out of {}:\n  {}",
        docs.join(table::LIVING_DOCS).display(),
        stale.join("\n  ")
    );
}

/// Each text the docs may keep holds a retired name, or its entry would excuse nothing, and
/// is passed over whole.
#[test]
fn what_the_docs_may_keep_is_passed_over_whole() {
    let Some(docs) = docs_folder() else {
        return;
    };
    let table = table::load();
    let lookup = words::Lookup::new(&table);
    let doc = "doc.md";
    for allow in &table::living_docs(&docs).allow {
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

/// The folder `ARGONAUTS_DOCS` names. A relative one is read from the workspace's top, where
/// the command is typed, since cargo runs the test from the crate's own folder.
fn docs_folder() -> Option<std::path::PathBuf> {
    std::env::var_os("ARGONAUTS_DOCS").map(|docs| table::workspace().join(docs))
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
