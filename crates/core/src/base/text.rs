//! Prose helpers for what Argonauts Code says. A message often starts with something you typed,
//! such as `limits.spend_per_run`, a file, or a model, and that keeps its exact spelling.

/// Starts `text` with a capital, as a sentence does, unless its first word is written as code:
/// a dotted key, a path, a flag, a model, anything with a digit, `_`, or backtick in it, a name
/// spelled in lowercase such as agy, or the key of a line quoted from a file, such as
/// `workflow = "council"`.
pub fn sentence_case(text: &str) -> String {
    let mut words = text.split_whitespace();
    let first_word = words.next().unwrap_or_default();
    if is_code(first_word) || words.next() == Some("=") {
        return text.to_owned();
    }
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `a`, `a and b`, `a, b and c`, as a sentence lists things.
pub fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [only] => only.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// A workflow, mode or role as the screen names it, its first letter a capital: `Council`,
/// `Maker-checker`, `Best-of-n`, `Supervised`, `Judge`. What you type and what files hold keep
/// the spelling they have; only what's drawn changes.
pub fn shown_name(name: &str) -> String {
    let name = name.trim();
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// What a run's header calls the workflow it runs, after the run's name: a council is the mode
/// itself, `Council`; any other is `orchestrator workflow`.
pub fn workflow_kind(kind: &str) -> String {
    let kind = kind.trim();
    if kind.eq_ignore_ascii_case("council") {
        shown_name(kind)
    } else {
        format!("{} workflow", kind.to_lowercase())
    }
}

/// Whether `reported`, the name a workflow's script gave itself, names the run: a script that
/// calls itself by its template, a built-in one such as `orchestrator` or `council`, says
/// nothing the run's own name doesn't.
pub fn names_the_run(reported: &str, templates: &[Option<&str>]) -> bool {
    let reported = reported.trim();
    let built_in = crate::workflows::workflow_files::BUILT_IN
        .iter()
        .map(|(name, _)| Some(*name));
    !reported.is_empty()
        && !templates
            .iter()
            .copied()
            .chain(built_in)
            .flatten()
            .any(|template| template.trim().eq_ignore_ascii_case(reported))
}

/// The small words a run's name never ends on when it is drawn as words.
const SMALL_WORDS: &[&str] = &[
    "a", "an", "and", "for", "in", "of", "on", "the", "to", "with",
];

/// A run's name as the screen draws it. A slug stays the run's id, and is drawn as its words:
/// `fix-the-money-rounding` reads `Fix the money rounding`, and `add-due-dates-to` drops the
/// small word it was cut on, `Add due dates`. Any other name keeps its own spelling.
pub fn title_words(name: &str) -> String {
    let name = name.trim();
    let slug = name.contains('-')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !slug {
        return name.to_owned();
    }
    let mut words: Vec<&str> = name.split('-').filter(|word| !word.is_empty()).collect();
    while words.len() > 1 && words.last().is_some_and(|last| SMALL_WORDS.contains(last)) {
        words.pop();
    }
    shown_name(&words.join(" "))
}

/// `a`, `a and b`, or `a, b and c`, as a sentence lists things.
pub fn listed(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Starts `text` in lowercase, for a reason that follows "since" or "because" mid-sentence,
/// unless its first word is code or a name in capitals, such as `README` or `API`, or it starts
/// with a model named on its CLI, such as "Sonnet 5 on Claude Code".
pub fn clause_case(text: &str) -> String {
    let first_word = text.split_whitespace().next().unwrap_or_default();
    let capitals = first_word.chars().filter(|c| c.is_uppercase()).count() > 1;
    if is_code(first_word) || capitals || starts_with_a_model(text) {
        return text.to_owned();
    }
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Whether `text` starts with a model as the app names one on its CLI: a name of a few words,
/// then " on " and the CLI's name, such as "Sonnet 5 on Claude Code," or "Haiku on agy".
fn starts_with_a_model(text: &str) -> bool {
    let clause = text.split(',').next().unwrap_or_default();
    let Some((model, cli)) = clause.split_once(" on ") else {
        return false;
    };
    let capital = |words: &str| words.chars().next().is_some_and(char::is_uppercase);
    let cli_named = capital(cli) || LOWERCASE_NAMES.iter().any(|name| cli.starts_with(name));
    capital(model) && model.split_whitespace().count() <= 4 && cli_named
}

/// Names that are spelled in lowercase, which no sentence capitalizes.
const LOWERCASE_NAMES: &[&str] = &["agy"];

/// Whether a word is a name to keep as written rather than an English word. Closing
/// punctuation and a possessive don't count, so "done," and "it's" read as words.
fn is_code(word: &str) -> bool {
    let word = word.trim_end_matches([',', ';', ':', '.', '!', '?', ')', '"']);
    let word = word
        .strip_suffix("'s")
        .or_else(|| word.strip_suffix("’s"))
        .unwrap_or(word);
    LOWERCASE_NAMES.contains(&word)
        || word.chars().any(|c| {
            c.is_ascii_digit()
                || matches!(
                    c,
                    '.' | '_' | '/' | '\\' | '-' | '`' | '=' | '[' | '~' | '@' | '<' | ':' | '*'
                )
        })
}

/// A state's name in the journal, as the screen says it. Every place that prints a state from
/// its serialized name goes through here.
pub fn state_words(name: &str) -> &str {
    match name {
        "needs_you" => "needs you",
        other => other,
    }
}

/// The last sentence of `words`, the claim a reply most often answers: what a reply to a
/// message quotes.
pub fn last_sentence(words: &str) -> &str {
    let trimmed = words.trim_end();
    let body = trimmed.strip_suffix('.').unwrap_or(trimmed);
    match body.rfind(". ") {
        Some(at) => trimmed[at + 2..].trim_start(),
        None => trimmed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_quotes_the_last_sentence() {
        assert_eq!(
            last_sentence("Keep the window. A bucket needs a burst size."),
            "A bucket needs a burst size."
        );
        assert_eq!(last_sentence("One claim only"), "One claim only");
    }

    #[test]
    fn a_runs_slug_is_drawn_as_its_words() {
        assert_eq!(
            title_words("fix-the-money-rounding"),
            "Fix the money rounding"
        );
        assert_eq!(title_words("add-due-dates-to"), "Add due dates");
        assert_eq!(title_words("per-key-rate-limit"), "Per key rate limit");
        assert_eq!(title_words("Add due dates"), "Add due dates");
        assert_eq!(title_words("council"), "council");
        assert_eq!(title_words("the-"), "The");
    }

    #[test]
    fn a_clause_starts_in_lowercase_unless_it_names_something() {
        assert_eq!(
            clause_case("One task needs no debate"),
            "one task needs no debate"
        );
        assert_eq!(clause_case("README says so"), "README says so");
        assert_eq!(
            clause_case("limits.spend_per_run is spent"),
            "limits.spend_per_run is spent"
        );
        assert_eq!(clause_case(""), "");
        assert_eq!(
            clause_case("Sonnet 5 on Claude Code, a fallback, reviewed task-001"),
            "Sonnet 5 on Claude Code, a fallback, reviewed task-001"
        );
        assert_eq!(
            clause_case("Haiku on agy reviewed it"),
            "Haiku on agy reviewed it"
        );
        assert_eq!(
            clause_case("Every check passed on the merge"),
            "every check passed on the merge"
        );
    }

    #[test]
    fn words_start_with_a_capital() {
        assert_eq!(sentence_case("unknown setting x"), "Unknown setting x");
        assert_eq!(sentence_case("done, then more"), "Done, then more");
        assert_eq!(sentence_case("it's set"), "It's set");
        assert_eq!(sentence_case(""), "");
    }

    #[test]
    fn keys_paths_and_names_keep_their_spelling() {
        for text in [
            "limits.spend_per_run in .argonauts/config.local.toml is ignored",
            "crew.main: invalid model",
            ".argonauts/config.toml changed",
            "~/.argonauts/config.toml is yours",
            "--set ui.keys=vim is refused",
            "claude-code/haiku's provider is unknown",
            "`cargo test` failed",
            "[[checks.run]] needs a name",
            "gpt5 is gone",
            "workflow = \"x\" in config.toml names no workflow",
            "agy worked on this",
            "agy's session ended",
        ] {
            assert_eq!(sentence_case(text), text);
        }
    }
}
