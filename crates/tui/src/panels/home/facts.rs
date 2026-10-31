//! The Overview's lines of words: one fun fact, as Claude Code's /stats picks one, and the
//! few things worth knowing that the numbers add up to.

use argonauts_core::app::overview::OverviewData;

use super::look::share;

/// Books and their length in tokens, the list Claude Code's own stats compare with.
const BOOKS: &[(&str, u64)] = &[
    ("The Little Prince", 22_000),
    ("The Old Man and the Sea", 35_000),
    ("A Christmas Carol", 37_000),
    ("Animal Farm", 39_000),
    ("Fahrenheit 451", 60_000),
    ("The Great Gatsby", 62_000),
    ("Slaughterhouse-Five", 64_000),
    ("Brave New World", 83_000),
    ("The Catcher in the Rye", 95_000),
    ("Harry Potter and the Philosopher's Stone", 103_000),
    ("The Hobbit", 123_000),
    ("1984", 123_000),
    ("To Kill a Mockingbird", 130_000),
    ("Pride and Prejudice", 156_000),
    ("Dune", 244_000),
    ("Moby-Dick", 268_000),
    ("Crime and Punishment", 274_000),
    ("A Game of Thrones", 381_000),
    ("Anna Karenina", 468_000),
    ("Don Quixote", 520_000),
    ("The Lord of the Rings", 576_000),
    ("The Count of Monte Cristo", 603_000),
    ("Les Misérables", 689_000),
    ("War and Peace", 730_000),
];

/// Things that take a while, in minutes, the list Claude Code compares the longest session
/// with.
const PASTIMES: &[(&str, u64)] = &[
    ("a TED talk", 18),
    ("an episode of The Office", 22),
    ("listening to Abbey Road", 47),
    ("a yoga class", 60),
    ("a World Cup soccer match", 90),
    ("a half marathon (average time)", 120),
    ("the movie Inception", 148),
    ("watching Titanic", 195),
    ("a transatlantic flight", 420),
    ("a full night of sleep", 480),
];

/// `Your input and output are ~920x the tokens in A Game of Thrones`: one of the comparisons
/// Claude Code's /stats makes, picked by the day so it holds still while you look.
pub fn fun_fact(data: &OverviewData) -> Option<String> {
    let mut facts = Vec::new();
    let tokens = data.total.input + data.total.output;
    for (name, size) in BOOKS.iter().filter(|(_, size)| tokens >= *size) {
        let times = tokens / size;
        facts.push(if times >= 2 {
            format!("Your input and output are ~{times}x the tokens in {name}")
        } else {
            format!("Your input and output are about as many tokens as {name}")
        });
    }
    let minutes = data.longest_session_secs / 60;
    for (name, length) in PASTIMES {
        let times = minutes / length;
        if times >= 2 {
            facts.push(format!(
                "Your longest session is ~{times}x longer than {name}"
            ));
        }
    }
    if facts.is_empty() {
        return None;
    }
    let pick = usize::from(data.today.ordinal()) % facts.len();
    Some(facts.swap_remove(pick))
}

/// `97% of your input came from cache; 64% of your tokens ran in subagents`: what's worth
/// knowing about how the tokens were spent, when anything stands out.
pub fn worth_knowing(data: &OverviewData) -> Option<String> {
    let mut lines = Vec::new();
    let input = data.total.all_input();
    if input > 0 && data.total.cache_read * 2 >= input {
        lines.push(format!(
            "{} of your input came from cache",
            share(data.total.cache_read, input)
        ));
    }
    let total = data.total.total();
    if total > 0 && data.subagent_tokens * 5 >= total {
        lines.push(format!(
            "{} of your tokens ran in subagents",
            share(data.subagent_tokens, total)
        ));
    }
    (!lines.is_empty()).then(|| lines.join("; "))
}
