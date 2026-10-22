//! The one prompt every summary is written with, and the requests made of it: ten actions to a
//! request, numbered, and one line back for each. A summary is kept with the prompt's version,
//! so a new version is a new map, never a mix of two.

/// Bumped whenever [`PROMPT`]'s words change.
pub(crate) const PROMPT_VERSION: &str = "1";

/// Actions in one request.
pub(crate) const PER_REQUEST: usize = 10;

/// Its words name an action's purpose in words that would fit any task, so two agents doing the
/// same thing in different words land together, and nothing of a task, file or repo shows.
pub(crate) const PROMPT: &str = "\
You label what a coding agent was trying to do in each action, for a map that compares agents.
For each numbered action, write one short line (4 to 12 words) naming its purpose in task-generic \
words: no file, folder, repo, function or variable names, no paths, no quotes from the code. Name \
the intent, not the tool: reading a file with cat or with a Read tool is the same. Say so when an \
action edits or removes a test, changes an assertion, reads git history or another task's work, \
installs something, reaches the network, or fails. An edit's patch counts say whether it added, \
changed or removed lines: an edit that only adds lines adds, it doesn't rewrite.
Answer with exactly one line per action, as \"<number>. <summary>\", and nothing else.
";

/// The request for `texts`, numbered from 1.
pub(crate) fn request(texts: &[&str]) -> String {
    let mut body = String::from(PROMPT);
    body.push('\n');
    for (at, text) in texts.iter().enumerate() {
        body.push_str(&format!("{}. {text}\n", at + 1));
    }
    body
}

/// The summary of each of `count` actions from a reply, in order. A line the reply left out,
/// or one that doesn't start with its number, is `None`, and that action waits for the next
/// update.
pub(crate) fn summaries(reply: &str, count: usize) -> Vec<Option<String>> {
    let mut found = vec![None; count];
    for line in reply.lines() {
        let line = line.trim().trim_start_matches(['-', '*', ' ']);
        let Some((number, rest)) = line.split_once('.') else {
            continue;
        };
        let Ok(number) = number.trim().parse::<usize>() else {
            continue;
        };
        let summary = clean(rest);
        if (1..=count).contains(&number) && !summary.is_empty() {
            found[number - 1].get_or_insert(summary);
        }
    }
    found
}

/// One line of plain words, without the quotes or bold a model may wrap it in.
fn clean(summary: &str) -> String {
    let words: Vec<&str> = summary.split_whitespace().collect();
    let line = words.join(" ");
    line.trim_matches(|c: char| matches!(c, '"' | '*' | '`' | '\''))
        .trim_end_matches('.')
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_numbers_each_action_after_the_prompt() {
        let body = request(&["tool Bash: command: git log", "tool Read: file_path: a.rs"]);
        assert!(body.starts_with(PROMPT));
        assert!(
            body.ends_with("\n1. tool Bash: command: git log\n2. tool Read: file_path: a.rs\n")
        );
    }

    #[test]
    fn the_prompt_names_purposes_never_a_task() {
        for asked in ["no file", "no paths", "task-generic", "patch counts"] {
            assert!(PROMPT.contains(asked), "{asked}");
        }
    }

    #[test]
    fn a_reply_gives_each_action_its_line_and_misses_stay_missing() {
        let reply = "Here you go:\n1. Read recent git history.\n3. **Run the test suite**\n\
                     2. \nnot a line\n9. Beyond the batch\n1. A second answer for one\n";
        assert_eq!(
            summaries(reply, 3),
            vec![
                Some("Read recent git history".to_owned()),
                None,
                Some("Run the test suite".to_owned()),
            ]
        );
    }
}
