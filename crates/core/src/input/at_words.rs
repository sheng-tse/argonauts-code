//! The `@` words in a message, such as `@codex` or `@src/main.rs`, found the same way by the
//! screen that highlights them and the app that hands their files to the agent.

use std::borrow::Cow;
use std::ops::Range;

/// One `@` word: where it sits in the text, `@` and any quotes included, and the name after
/// the `@`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtWord<'t> {
    pub range: Range<usize>,
    pub name: Cow<'t, str>,
}

/// Every `@` that starts a word, alone or after an opening bracket or quote, so an email
/// address isn't one. A name with spaces is written in quotes, `@"my notes.md"`. Punctuation
/// that ends a sentence, such as the comma in `@README.md, then`, isn't part of the name.
pub fn at_words(text: &str) -> Vec<AtWord<'_>> {
    let mut words = Vec::new();
    let mut at = 0;
    while let Some(found) = text[at..].find('@') {
        let start = at + found;
        at = start + 1;
        let starts_word = text[..start]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace() || matches!(c, '(' | '[' | '{' | '"' | '\''));
        if !starts_word {
            continue;
        }
        let rest = &text[start + 1..];
        if let Some(quoted) = rest.strip_prefix('"')
            && let Some(close) = quoted.find('"')
            && close > 0
            && !quoted[..close].contains('\n')
        {
            let end = start + 2 + close + 1;
            words.push(AtWord {
                range: start..end,
                name: Cow::Borrowed(&quoted[..close]),
            });
            at = end;
            continue;
        }
        let word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let name = rest[..word_end].trim_end_matches(ends_sentence);
        if name.is_empty() {
            continue;
        }
        let end = start + 1 + name.len();
        words.push(AtWord {
            range: start..end,
            name: Cow::Borrowed(name),
        });
        at = end;
    }
    words
}

/// How a name goes after the `@` so [`at_words`] reads it back whole.
pub fn at_word(name: &str) -> String {
    let needs_quotes = name.contains(char::is_whitespace)
        || name.ends_with(ends_sentence)
        || name.starts_with('"');
    if needs_quotes {
        format!("@\"{name}\"")
    } else {
        format!("@{name}")
    }
}

fn ends_sentence(c: char) -> bool {
    matches!(
        c,
        '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '\'' | '"'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(text: &str) -> Vec<String> {
        at_words(text)
            .into_iter()
            .map(|word| word.name.into_owned())
            .collect()
    }

    #[test]
    fn words_that_start_with_an_at_sign() {
        assert_eq!(
            names("@codex look at @src/main.rs and @docs/"),
            ["codex", "src/main.rs", "docs/"]
        );
        assert_eq!(names("mail me at me@example.com"), Vec::<String>::new());
        assert_eq!(names("@ alone, and @@twice"), ["@twice"]);
        assert_eq!(names("line\n@notes.txt"), ["notes.txt"]);
    }

    #[test]
    fn sentence_punctuation_stays_outside_the_name() {
        assert_eq!(
            names("see @README.md, then @src/lib.rs. Also (@Cargo.toml)?"),
            ["README.md", "src/lib.rs", "Cargo.toml"]
        );
        let text = "read @notes.txt.";
        let word = &at_words(text)[0];
        assert_eq!(&text[word.range.clone()], "@notes.txt");
    }

    #[test]
    fn quotes_hold_a_name_with_spaces() {
        let text = "read @\"my notes.md\" first";
        let words = at_words(text);
        assert_eq!(words[0].name, "my notes.md");
        assert_eq!(&text[words[0].range.clone()], "@\"my notes.md\"");
        assert_eq!(names("an open @\"quote"), ["\"quote"]);
    }

    #[test]
    fn a_written_name_reads_back_whole() {
        for name in ["src/main.rs", "docs/", "my notes.md", "odd.", "codex"] {
            let text = format!("see {} now", at_word(name));
            assert_eq!(names(&text), [name], "{text}");
        }
        assert_eq!(at_word("src/main.rs"), "@src/main.rs");
        assert_eq!(at_word("my notes.md"), "@\"my notes.md\"");
    }
}
