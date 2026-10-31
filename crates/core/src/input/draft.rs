//! What a draft that starts with `/` is while you type it: a real command, with the arguments
//! it still waits for, or no command at all, with the nearest real one. The input draws from
//! this and Enter obeys it, so what looks like a command is one.

use super::{SHARED_COMMANDS, SLASH_COMMANDS, slash_spec};
use crate::agents::cli_commands::CliCommandSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftCommand {
    Known {
        /// The command as typed, slash and all, such as `/workflow`.
        typed: String,
        /// The arguments it still waits for, such as `<request>`. Empty when it waits for
        /// none, or while a word is half typed.
        hint: String,
        /// What Enter does beyond running it, where its name doesn't say.
        note: Option<&'static str>,
    },
    Unknown {
        typed: String,
        /// The nearest real command, slash and all.
        nearest: Option<String>,
    },
}

impl DraftCommand {
    /// The command as typed, slash and all.
    pub fn typed(&self) -> &str {
        match self {
            DraftCommand::Known { typed, .. } | DraftCommand::Unknown { typed, .. } => typed,
        }
    }

    /// What the bottom line says Enter does with the draft.
    pub fn enter_does(&self) -> String {
        match self {
            DraftCommand::Known {
                typed,
                note: Some(note),
                ..
            } => format!("enter runs {typed} · {note}"),
            DraftCommand::Known { typed, .. } => format!("enter runs {typed}"),
            DraftCommand::Unknown { .. } => "enter sends nothing".to_owned(),
        }
    }

    /// The dim words after the draft: the arguments still to come, or why Enter does nothing.
    pub fn after_draft(&self) -> Option<String> {
        match self {
            DraftCommand::Known { hint, .. } => (!hint.is_empty()).then(|| hint.clone()),
            DraftCommand::Unknown { .. } => Some(match self.fix() {
                Some(fix) => format!("{NOT_A_COMMAND} · {fix}"),
                None => NOT_A_COMMAND.to_owned(),
            }),
        }
    }

    /// How to mend what isn't a command, when a real one is near.
    pub fn fix(&self) -> Option<String> {
        match self {
            DraftCommand::Unknown {
                nearest: Some(nearest),
                ..
            } => Some(format!("did you mean {nearest}? · tab fixes it")),
            DraftCommand::Unknown { nearest: None, .. } | DraftCommand::Known { .. } => None,
        }
    }

    /// What the bottom line says when the draft's row has no room for the fix: what Enter
    /// does, then the fix.
    pub fn enter_does_and_fix(&self) -> String {
        match self.fix() {
            Some(fix) => format!("{} · {fix}", self.enter_does()),
            None => self.enter_does(),
        }
    }
}

/// Why Enter does nothing with a draft that names no command, said after the draft.
pub const NOT_A_COMMAND: &str = "isn't a command";

/// Whether `word` can name a command: a letter, then letters, digits, and `. : - _`, as Claude
/// Code reads one. A path such as `Users/me/notes.md` can't, so a prompt may start with one.
pub fn is_command_word(word: &str) -> bool {
    let mut chars = word.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-' | '_'))
}

/// What `text` is as a command. `None` for anything that isn't one being typed: plain text, a
/// path, a lone `/`, or the start of a real command's name still being typed. `cli` is the
/// main agent CLI's own commands, and `more` any further names that run, such as your mods'.
pub fn draft_command(
    text: &str,
    cli: Option<&CliCommandSet>,
    more: &[String],
) -> Option<DraftCommand> {
    let rest = text.strip_prefix('/')?;
    let name_len = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let (name, args) = rest.split_at(name_len);
    if !is_command_word(name) {
        return None;
    }
    let typed = format!("/{name}");
    let cli_command = cli.and_then(|set| set.find(name));
    let spec = slash_spec(name);
    if spec.is_some() || cli_command.is_some() || more.iter().any(|known| known == name) {
        let usage = spec
            .map(|spec| spec.args)
            .or(cli_command.map(|command| command.args.as_str()))
            .unwrap_or_default();
        let (hint, note) = still_to_come(name, usage, args);
        return Some(DraftCommand::Known { typed, hint, note });
    }
    let names = || {
        let own = SLASH_COMMANDS
            .iter()
            .chain(SHARED_COMMANDS)
            .map(|spec| spec.name);
        let of_cli = cli
            .into_iter()
            .flat_map(|set| set.commands.iter().map(|command| command.name.as_str()));
        own.chain(of_cli).chain(more.iter().map(String::as_str))
    };
    // A name still being typed may yet become a real one, and the menu shows which.
    if args.is_empty() && names().any(|known| known.starts_with(name)) {
        return None;
    }
    Some(DraftCommand::Unknown {
        typed,
        nearest: nearest(name, names()).map(|nearest| format!("/{nearest}")),
    })
}

/// A command's forms that start with a word of its own: the word, what follows it, and what
/// Enter does where that isn't plain.
type Forms = &'static [(&'static str, &'static str, Option<&'static str>)];

const WORKFLOW_FORMS: Forms = &[
    ("new", "<name> [from <built-in>]", None),
    (
        "try",
        "<name> <request>",
        Some("shows what a workflow would run, runs nothing"),
    ),
    ("edit", "<name>", None),
    ("show", "<name>", None),
    ("diff", "<name>", None),
    ("copy", "<name> [--project]", None),
];

fn forms(command: &str) -> Forms {
    match command {
        "workflow" => WORKFLOW_FORMS,
        _ => &[],
    }
}

/// The arguments of `usage` that `args` hasn't reached yet, and the note of the form typed.
/// Nothing while a word is half typed, as Claude Code hides its hint then.
fn still_to_come(command: &str, usage: &str, args: &str) -> (String, Option<&'static str>) {
    let words: Vec<&str> = args.split_whitespace().collect();
    let form = words
        .first()
        .and_then(|first| forms(command).iter().find(|(word, ..)| word == first));
    let (usage, typed, note) = match form {
        Some((_, usage, note)) => (*usage, words.len() - 1, *note),
        None => (usage, words.len(), None),
    };
    let half_typed = !words.is_empty() && !args.ends_with(char::is_whitespace);
    if half_typed {
        return (String::new(), note);
    }
    let left: Vec<&str> = usage_parts(usage).into_iter().skip(typed).collect();
    (left.join(" "), note)
}

/// `usage` split at spaces, keeping a bracketed part such as `[from <built-in>]` whole.
fn usage_parts(usage: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0_usize, None);
    for (at, c) in usage.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            _ => {}
        }
        match (c.is_whitespace() && depth == 0, start) {
            (true, Some(from)) => {
                parts.push(&usage[from..at]);
                start = None;
            }
            (false, None) => start = Some(at),
            _ => {}
        }
    }
    parts.extend(start.map(|from| &usage[from..]));
    parts
}

/// The name closest to `typed`, when one is close enough to be what was meant: at most two
/// slips away, and fewer than half the letters typed. The first of equally close names wins.
fn nearest<'a>(typed: &str, names: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let most = 2.min(typed.chars().count().saturating_sub(1) / 2);
    names
        .map(|name| (slips(typed, name), name))
        .filter(|(slips, _)| (1..=most).contains(slips))
        .min_by_key(|(slips, _)| *slips)
        .map(|(_, name)| name)
}

/// How many slips of the hand turn `a` into `b`: a letter missed, added, or wrong, or two next
/// to each other swapped.
fn slips(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut rows = vec![vec![0_usize; b.len() + 1]; a.len() + 1];
    for (i, row) in rows.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in rows[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let wrong = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (rows[i - 1][j] + 1)
                .min(rows[i][j - 1] + 1)
                .min(rows[i - 1][j - 1] + wrong);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(rows[i - 2][j - 2] + 1);
            }
            rows[i][j] = best;
        }
    }
    rows[a.len()][b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(text: &str) -> Option<DraftCommand> {
        draft_command(text, None, &[])
    }

    fn hint(text: &str) -> String {
        match draft(text) {
            Some(DraftCommand::Known { hint, .. }) => hint,
            other => panic!("{text}: {other:?}"),
        }
    }

    #[test]
    fn a_real_command_names_the_arguments_still_to_come() {
        assert_eq!(hint("/workflow"), "[name|new|try|edit]");
        assert_eq!(hint("/workflow "), "[name|new|try|edit]");
        assert_eq!(hint("/workflow try "), "<name> <request>");
        assert_eq!(hint("/workflow try spec-first "), "<request>");
        assert_eq!(hint("/workflow try spec-first add a burst limit "), "");
        assert_eq!(hint("/workflow new mine "), "[from <built-in>]");
        assert_eq!(hint("/workflow council "), "");
        assert_eq!(hint("/ask "), "<question>");
        assert_eq!(hint("/follow-up "), "[run] <request>");
        assert_eq!(hint("/clear"), "");
        // A word half typed hides the hint, so it never trails the letters being typed.
        assert_eq!(hint("/workflow tr"), "");
        assert_eq!(hint("/workflow try spec"), "");
    }

    #[test]
    fn the_bottom_line_says_what_enter_does() {
        let enter = |text: &str| draft(text).unwrap().enter_does();
        assert_eq!(enter("/clear"), "enter runs /clear");
        assert_eq!(
            enter("/workflow try spec-first "),
            "enter runs /workflow · shows what a workflow would run, runs nothing"
        );
        assert_eq!(enter("/workflow council"), "enter runs /workflow");
        assert_eq!(enter("/wokflow try x"), "enter sends nothing");
        let with_fix = |text: &str| draft(text).unwrap().enter_does_and_fix();
        assert_eq!(
            with_fix("/wokflow try x"),
            "enter sends nothing · did you mean /workflow? · tab fixes it"
        );
        assert_eq!(with_fix("/zzzzzz "), "enter sends nothing");
        assert_eq!(with_fix("/clear"), "enter runs /clear");
    }

    #[test]
    fn an_unknown_command_names_the_nearest_real_one() {
        let after = |text: &str| draft(text).unwrap().after_draft().unwrap();
        assert_eq!(
            after("/wokflow try spec-first add a burst limit"),
            "isn't a command · did you mean /workflow? · tab fixes it"
        );
        assert_eq!(
            after("/staus"),
            "isn't a command · did you mean /status? · tab fixes it"
        );
        assert_eq!(after("/zzzzzz "), "isn't a command");
        // Two letters away from everything short is no suggestion at all.
        assert_eq!(after("/qq "), "isn't a command");
    }

    #[test]
    fn a_name_still_being_typed_is_not_yet_wrong() {
        assert_eq!(draft("/wor"), None);
        assert_eq!(draft("/"), None);
        // Once a space follows it, or nothing starts with it, it's judged.
        assert!(matches!(draft("/wor "), Some(DraftCommand::Unknown { .. })));
        assert!(matches!(draft("/wokf"), Some(DraftCommand::Unknown { .. })));
    }

    #[test]
    fn a_path_and_plain_text_are_no_commands() {
        assert_eq!(draft("/Users/me/shot.png what is this"), None);
        assert_eq!(draft("/tmp/x.log"), None);
        assert_eq!(draft("/tmp/"), None);
        assert_eq!(draft("fix /workflow"), None);
        assert_eq!(draft("/3d"), None);
        assert!(is_command_word("anthropic-skills:example-a"));
        assert!(!is_command_word("Users/me"));
        assert!(!is_command_word(""));
    }

    #[test]
    fn your_mods_and_the_cli_s_commands_are_real_too() {
        let more = ["tides".to_owned()];
        assert!(matches!(
            draft_command("/tides", None, &more),
            Some(DraftCommand::Known { .. })
        ));
        assert!(matches!(
            draft_command("/tidse ", None, &more),
            Some(DraftCommand::Unknown { nearest: Some(nearest), .. }) if nearest == "/tides"
        ));
    }

    #[test]
    fn slips_count_a_swap_as_one() {
        assert_eq!(slips("wokflow", "workflow"), 1);
        assert_eq!(slips("stauts", "status"), 1);
        assert_eq!(slips("modle", "model"), 1);
        assert_eq!(slips("help", "help"), 0);
        assert_eq!(slips("abc", "xyz"), 3);
    }

    #[test]
    fn usage_keeps_a_bracketed_part_whole() {
        assert_eq!(
            usage_parts("<name> [from <built-in>]"),
            ["<name>", "[from <built-in>]"]
        );
        assert_eq!(usage_parts("[run] <request>"), ["[run]", "<request>"]);
        assert!(usage_parts("").is_empty());
    }
}
