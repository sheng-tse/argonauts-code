//! Fails when the sources lose their shape: a `.rs` file loose in a `src/` beside the entry
//! file, a module written as `x.rs` beside its folder `x/`, a file over 1,500 production lines
//! or a crate over 60,000, or core past its ceiling. The files and crates over the limits today
//! are listed in `source-layout.toml` with their planned split, and may only shrink.
//!
//! A `src/` holds its entry file and folders by area. Cargo's own `src/bin/` is such a folder,
//! and folders of other files (`js/`, insta's `snapshots/`) hold no `.rs` file to check.

#[path = "source_layout/list.rs"]
mod list;
#[path = "source_layout/sources.rs"]
mod sources;

use list::LIST;

/// How many production lines a file or a crate may hold, and what to do past it.
struct Limit {
    lines: usize,
    what: &'static str,
    fix: &'static str,
}

const FILE: Limit = Limit {
    lines: 1_500,
    what: "file",
    fix: "Move each idea in it into a file of its own, in the folder of its area.",
};
const CRATE: Limit = Limit {
    lines: 60_000,
    what: "crate",
    fix: "Move whole areas into a crate of their own, so each crate stays one part.",
};
const CORE: &str = "crates/core";

#[test]
fn every_src_holds_its_entry_file_and_folders() {
    let list = list::load();
    let mut problems = Vec::new();
    for member in sources::members() {
        let beside: Vec<&str> = member.beside_their_folders().collect();
        if !list.exceptions.contains_key(&member.path) {
            for path in member.loose().filter(|path| !beside.contains(path)) {
                problems.push(format!(
                    "{path} sits loose beside {entry}.\n  \
                     Move it into the folder of its area, or name the crate under\n  \
                     [exceptions] in {LIST}.",
                    entry = member.entry(),
                ));
            }
        }
        for path in beside {
            let folder = path.trim_end_matches(".rs");
            problems.push(format!(
                "{path} sits beside its folder {folder}/.\n  \
                 Move it into the folder as {folder}/mod.rs, so the module is one folder."
            ));
        }
    }
    for path in list.exceptions.keys() {
        match sources::member(path) {
            None => problems.push(format!(
                "{path} is under [exceptions], but no member of the workspace has that path.\n  \
                 Take it out of {LIST}, or write the crate's path as Cargo.toml does."
            )),
            Some(member) if member.loose().next().is_none() => problems.push(format!(
                "{path} is under [exceptions], but its src/ holds no loose file.\n  \
                 Take it out of {LIST}, so a loose file can't come back unseen."
            )),
            Some(_) => {}
        }
    }
    fail_on(&problems);
}

#[test]
fn no_file_or_crate_passes_its_size() {
    let list = list::load();
    let mut problems = Vec::new();
    for member in sources::members() {
        for (path, lines) in &member.counted {
            problems.extend(over(path, *lines, list.file(path), &FILE));
        }
        let lines = member.lines();
        problems.extend(over(
            &member.path,
            lines,
            list.crate_at(&member.path),
            &CRATE,
        ));
    }
    for listed in &list.files {
        let counted = sources::members()
            .iter()
            .flat_map(|member| &member.counted)
            .find(|(path, _)| *path == listed.path);
        match counted {
            None => problems.push(format!(
                "{} is on the list, but no file outside the tests has that path.\n  \
                 Take its entry out of {LIST}, or give it the path the file moved to.",
                listed.path
            )),
            Some((_, lines)) => problems.extend(within(&listed.path, *lines, listed, &FILE)),
        }
    }
    for listed in &list.crates {
        match sources::member(&listed.path) {
            None => problems.push(format!(
                "{} is on the list, but no member of the workspace has that path.\n  \
                 Take its entry out of {LIST}, or write the crate's path as Cargo.toml does.",
                listed.path
            )),
            Some(member) => problems.extend(within(&listed.path, member.lines(), listed, &CRATE)),
        }
    }
    let mut paths: Vec<&str> = list
        .files
        .iter()
        .chain(&list.crates)
        .map(|listed| listed.path.as_str())
        .collect();
    paths.sort_unstable();
    for pair in paths.windows(2).filter(|pair| pair[0] == pair[1]) {
        problems.push(format!(
            "{} is on the list twice.\n  Keep one entry in {LIST}.",
            pair[0]
        ));
    }
    fail_on(&problems);
}

#[test]
fn core_only_shrinks() {
    let list = list::load();
    let core = sources::member(CORE).expect("core is a member of the workspace");
    let lines = core.lines();
    let ceiling = list.core.ceiling;
    let mut problems = Vec::new();
    if lines > ceiling {
        problems.push(format!(
            "{CORE} has {} production lines; its ceiling is {}.\n  \
             core may only shrink, and holds only what several crates share. Move\n  \
             what one crate alone uses into that crate.",
            figure(lines),
            figure(ceiling),
        ));
    }
    fail_on(&problems);
}

/// What to do when a file or crate passes its ceiling or, unlisted, the limit.
fn over(path: &str, lines: usize, listed: Option<&list::Listed>, limit: &Limit) -> Option<String> {
    match listed {
        Some(listed) if lines > listed.ceiling => Some(format!(
            "{path} has {} production lines; its ceiling is {}.\n  \
             A {} on the list may only shrink. Move code out, or split it as\n  \
             the list says: \"{}\".",
            figure(lines),
            figure(listed.ceiling),
            limit.what,
            listed.split
        )),
        None if lines > limit.lines => Some(format!(
            "{path} has {} production lines, over the limit of {}.\n  {}",
            figure(lines),
            figure(limit.lines),
            limit.fix
        )),
        _ => None,
    }
}

/// A listed file or crate back within the limit, whose entry would let it grow past it again.
fn within(path: &str, lines: usize, listed: &list::Listed, limit: &Limit) -> Option<String> {
    (lines <= limit.lines).then(|| {
        format!(
            "{path} has {} production lines, within the limit of {}.\n  \
             Take it off the list in {LIST}, so it can't grow back to its\n  \
             ceiling of {}.",
            figure(lines),
            figure(limit.lines),
            figure(listed.ceiling)
        )
    })
}

/// A count with its thousands marked, as the limits are written: 60,000.
fn figure(count: usize) -> String {
    let digits = count.to_string();
    let mut marked = String::with_capacity(digits.len() + digits.len() / 3);
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            marked.push(',');
        }
        marked.push(digit);
    }
    marked
}

fn fail_on(problems: &[String]) {
    assert!(problems.is_empty(), "\n\n{}\n", problems.join("\n\n"));
}
