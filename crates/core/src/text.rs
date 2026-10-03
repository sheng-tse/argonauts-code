//! Prose helpers for what Argonauts says. A message often starts with something you typed, such
//! as `budget.run_usd`, a file, or a worker, and that keeps its exact spelling.

/// Starts `text` with a capital, as a sentence does, unless its first word is written as code:
/// a dotted key, a path, a flag, a worker, or anything with a digit, `_`, or backtick in it.
pub fn sentence_case(text: &str) -> String {
    let first_word = text.split_whitespace().next().unwrap_or_default();
    if is_code(first_word) {
        return text.to_owned();
    }
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Whether a word is a name to keep as written rather than an English word. Closing
/// punctuation and a possessive don't count, so "done," and "it's" read as words.
fn is_code(word: &str) -> bool {
    let word = word.trim_end_matches([',', ';', ':', '.', '!', '?', ')', '"']);
    let word = word
        .strip_suffix("'s")
        .or_else(|| word.strip_suffix("’s"))
        .unwrap_or(word);
    word.chars().any(|c| {
        c.is_ascii_digit()
            || matches!(
                c,
                '.' | '_' | '/' | '\\' | '-' | '`' | '=' | '[' | '~' | '@' | '<' | ':' | '*'
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
            "budget.run_usd in .argonauts/config.local.toml is ignored",
            "default_agent: invalid worker",
            ".argonauts/config.toml changed",
            "~/.argonauts/config.toml is yours",
            "--set ui.keys=vim is refused",
            "claude-code/haiku's model family is unknown",
            "`cargo test` failed",
            "[[gates.run]] needs a name",
            "gpt5 is gone",
        ] {
            assert_eq!(sentence_case(text), text);
        }
    }
}
