//! The JSON answer inside a model's reply. A prompted model often wraps its answer in prose or
//! a Markdown fence, and the answer is still what it said.

use serde_json::Value;

/// The whole reply as JSON, else the last fenced block that holds JSON (``` or ~~~, anywhere in
/// the text), else the last JSON object in the text.
pub fn json_in(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return Some(value);
    }
    fenced_blocks(text)
        .into_iter()
        .rev()
        .find_map(|block| serde_json::from_str::<Value>(block).ok())
        .or_else(|| last_object(text))
}

/// The inside of each Markdown code fence in `text`, in order. A fence opens with three or more
/// backticks or tildes at the start of a line (up to three spaces in) and closes with a line of
/// at least as many of the same mark. A fence left open runs to the end, as a reply cut short
/// leaves it.
fn fenced_blocks(text: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut open: Option<(char, usize, usize)> = None;
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let line_start = at;
        at += line.len();
        let Some((mark, run, rest)) = fence_mark(line) else {
            continue;
        };
        match open {
            None => open = Some((mark, run, at)),
            Some((opened, length, body)) if opened == mark && run >= length && rest.is_empty() => {
                blocks.push(text[body..line_start].trim());
                open = None;
            }
            Some(_) => {}
        }
    }
    if let Some((_, _, body)) = open {
        blocks.push(text[body..].trim());
    }
    blocks
}

/// A fence line's mark, how many of it, and what follows on the line.
fn fence_mark(line: &str) -> Option<(char, usize, &str)> {
    let content = line.trim_end_matches(['\n', '\r']);
    let unindented = content.trim_start_matches(' ');
    if content.len() - unindented.len() > 3 {
        return None;
    }
    let mark = unindented
        .chars()
        .next()
        .filter(|c| matches!(c, '`' | '~'))?;
    let run = unindented.chars().take_while(|c| *c == mark).count();
    // The marks are one byte each, so the run's length in bytes is its count.
    (run >= 3).then(|| (mark, run, unindented[run..].trim()))
}

fn last_object(text: &str) -> Option<Value> {
    let mut last = None;
    let mut at = 0;
    while let Some(offset) = text[at..].find('{') {
        let start = at + offset;
        let mut values = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
        match values.next() {
            Some(Ok(value)) if value.is_object() => {
                at = start + values.byte_offset();
                last = Some(value);
            }
            _ => at = start + 1,
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::json_in;

    #[test]
    fn a_reply_that_is_only_json_is_read_whole() {
        assert_eq!(json_in(" {\"plan\": 1}\n"), Some(json!({"plan": 1})));
        assert_eq!(json_in("[1, 2]"), Some(json!([1, 2])));
    }

    #[test]
    fn a_fence_is_read_with_words_before_after_or_around_it() {
        let fence = "```json\n{\"plan\": \"ship\"}\n```";
        for reply in [
            fence.to_owned(),
            format!("Here is my plan:\n{fence}"),
            format!("{fence}\nTell me if you want changes."),
            format!("Here is my plan:\n\n{fence}\n\nIt keeps the tests green."),
        ] {
            assert_eq!(json_in(&reply), Some(json!({"plan": "ship"})), "{reply}");
        }
    }

    #[test]
    fn a_tilde_fence_or_a_longer_one_is_a_fence() {
        assert_eq!(
            json_in("Answer:\n~~~\n{\"ok\": true}\n~~~\n"),
            Some(json!({"ok": true}))
        );
        assert_eq!(
            json_in("Answer:\n````json\n{\"ok\": [\"```\"]}\n````\nDone."),
            Some(json!({"ok": ["```"]}))
        );
    }

    #[test]
    fn the_last_fence_that_holds_json_is_the_answer() {
        let reply = "For example:\n```json\n{\"plan\": \"example\"}\n```\nThe command:\n\
                     ```sh\nnpm test\n```\nMy answer:\n```json\n{\"plan\": \"mine\"}\n```\n";
        assert_eq!(json_in(reply), Some(json!({"plan": "mine"})));
    }

    #[test]
    fn a_fence_left_open_runs_to_the_end() {
        assert_eq!(
            json_in("Plan:\n```json\n{\"plan\": 2}\n"),
            Some(json!({"plan": 2}))
        );
    }

    #[test]
    fn a_bare_object_inside_prose_is_read() {
        assert_eq!(
            json_in("I looked at it. {\"verdict\": \"approve\"} That's all."),
            Some(json!({"verdict": "approve"}))
        );
        assert_eq!(json_in("no answer here"), None);
    }
}
