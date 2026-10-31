//! What the history of a folder without git never records: secrets, installed packages and
//! build output. A repository says this in its `.gitignore`; a folder that has none would hand
//! `.env` and `node_modules/` to every agent's copy, so the list starts from defaults, and
//! `history.leave_out` adds to it or takes from it. The patterns are git's ignore patterns,
//! and they join the folder's own `.gitignore` rules where it has any.

use argonauts_core::workflows::build_output::{BUILTIN, BuildOutput};

/// One kind of file on the list, as the first note names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Group {
    pub name: &'static str,
    pub patterns: &'static [&'static str],
    /// The patterns a note names when the group has too many to list, with the words that
    /// stand for the others.
    sample: Option<(&'static [&'static str], &'static str)>,
}

pub const SECRETS: Group = Group {
    name: "secrets",
    patterns: &[".env", ".env.*", "*.pem", "*.key"],
    sample: None,
};

pub const PACKAGES: Group = Group {
    name: "packages",
    patterns: &["node_modules/", ".venv/"],
    sample: None,
};

pub const BUILD_OUTPUT: Group = Group {
    name: "build output",
    patterns: BUILTIN,
    sample: Some((
        &["__pycache__/", ".next/", ".gradle/"],
        "the rest of the built-in list",
    )),
};

/// The default list, in the order a note names it. A pattern in two groups counts for the
/// first.
pub const GROUPS: [Group; 3] = [SECRETS, PACKAGES, BUILD_OUTPUT];

/// Before a pattern in `history.leave_out`, takes it from the list instead of adding it.
const TAKE: char = '!';

/// The list in force: the defaults without the ones `setting` takes, then the ones it adds.
pub fn patterns(setting: &[String]) -> Vec<String> {
    let (taken, added) = split(setting);
    let mut list: Vec<String> = Vec::new();
    let defaults = GROUPS
        .iter()
        .flat_map(|group| group.patterns.iter().copied());
    for pattern in defaults.chain(added) {
        if !taken.contains(&pattern) && !list.iter().any(|have| have == pattern) {
            list.push(pattern.to_owned());
        }
    }
    list
}

fn split(setting: &[String]) -> (Vec<&str>, Vec<&str>) {
    let entries = setting.iter().map(|entry| entry.trim());
    let (taken, added): (Vec<&str>, Vec<&str>) = entries
        .filter(|entry| !entry.is_empty())
        .partition(|entry| entry.starts_with(TAKE));
    let taken = taken
        .into_iter()
        .map(|entry| entry.trim_start_matches(TAKE).trim())
        .collect();
    (taken, added)
}

/// A group as one folder met it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Met {
    pub name: &'static str,
    /// The patterns that kept something of this folder out.
    pub found: Vec<String>,
    /// The group's other patterns in force, as a note lists them.
    pub also: String,
}

/// Each default group against `kept_out`, the paths the list kept out of a record, folders
/// ending in `/`. A group `setting` took whole isn't named.
pub fn met(setting: &[String], kept_out: &[String]) -> Vec<Met> {
    let (taken, _) = split(setting);
    let mut seen: Vec<&str> = Vec::new();
    let mut groups = Vec::new();
    for group in GROUPS {
        let own: Vec<&str> = group
            .patterns
            .iter()
            .copied()
            .filter(|pattern| !taken.contains(pattern) && !seen.contains(pattern))
            .collect();
        seen.extend(&own);
        if own.is_empty() {
            continue;
        }
        let (found, rest): (Vec<&str>, Vec<&str>) = own
            .iter()
            .partition(|pattern| kept_out.iter().any(|path| names(pattern, path)));
        groups.push(Met {
            name: group.name,
            also: also(&group, &rest),
            found: found.into_iter().map(str::to_owned).collect(),
        });
    }
    groups
}

/// The patterns `setting` adds that kept something of this folder out.
pub fn added_and_found(setting: &[String], kept_out: &[String]) -> Vec<String> {
    let (_, added) = split(setting);
    added
        .into_iter()
        .filter(|pattern| kept_out.iter().any(|path| names(pattern, path)))
        .map(str::to_owned)
        .collect()
}

fn also(group: &Group, rest: &[&str]) -> String {
    match group.sample {
        Some((sample, others)) if rest.len() > sample.len() => {
            let named: Vec<&str> = sample
                .iter()
                .copied()
                .filter(|pattern| rest.contains(pattern))
                .collect();
            format!("{} and {others}", named.join(", "))
        }
        _ => rest.join(", "),
    }
}

/// Whether `pattern` is what keeps `path` out. Git lists a folder it leaves out whole as
/// `name/`, so a file is imagined inside for the matcher, which reads files.
fn names(pattern: &str, path: &str) -> bool {
    let one = BuildOutput::of(false, &[pattern.to_owned()]);
    match path.strip_suffix('/') {
        Some(folder) => one.contains(&format!("{folder}/inside")),
        None => one.contains(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|&entry| entry.to_owned()).collect()
    }

    #[test]
    fn the_list_starts_from_the_defaults_and_the_setting_adds_and_takes() {
        let list = patterns(&[]);
        for pattern in [".env", "*.pem", "node_modules/", ".venv/", "__pycache__/"] {
            assert!(list.iter().any(|have| have == pattern), "{pattern}");
        }
        assert_eq!(
            list.iter().filter(|have| *have == "node_modules/").count(),
            1,
            "a pattern in two groups is listed once"
        );

        let list = patterns(&strings(&["!.env.*", " data/ ", "", "!*.key"]));
        assert!(list.iter().any(|have| have == "data/"));
        assert!(list.iter().any(|have| have == ".env"));
        assert!(!list.iter().any(|have| have == ".env.*" || have == "*.key"));
    }

    #[test]
    fn a_note_names_what_each_group_found_here_and_what_else_it_holds() {
        let kept_out = strings(&[".env", "node_modules/", "web/node_modules/"]);
        let groups = met(&[], &kept_out);
        assert_eq!(
            groups,
            vec![
                Met {
                    name: "secrets",
                    found: strings(&[".env"]),
                    also: ".env.*, *.pem, *.key".into(),
                },
                Met {
                    name: "packages",
                    found: strings(&["node_modules/"]),
                    also: ".venv/".into(),
                },
                Met {
                    name: "build output",
                    found: Vec::new(),
                    also: "__pycache__/, .next/, .gradle/ and the rest of the built-in list".into(),
                },
            ]
        );
    }

    #[test]
    fn a_group_the_setting_took_whole_is_not_named() {
        let setting = strings(&["!node_modules/", "!.venv/", "scratch/"]);
        let groups = met(&setting, &strings(&["scratch/", "certs/site.pem"]));
        let names: Vec<&str> = groups.iter().map(|group| group.name).collect();
        assert_eq!(names, ["secrets", "build output"]);
        assert_eq!(groups[0].found, ["*.pem"]);
        assert_eq!(
            added_and_found(&setting, &strings(&["scratch/"])),
            ["scratch/"]
        );
    }
}
