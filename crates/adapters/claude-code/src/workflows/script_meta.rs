//! The `meta` a Claude Code workflow script exports (`export const meta = { name, description,
//! phases: [{ title, detail }] }`), read without running anything. It's a JavaScript object
//! literal, so this reads the subset such a literal uses: objects, arrays, quoted strings,
//! numbers, `true`, `false`, `null`, bare keys, trailing commas, and comments. Anything else, a
//! function call or a template string with `${}`, ends the read, and the progress reports fill
//! in what's missing.

use serde_json::{Map, Number, Value};

/// The script's `meta` object, or `None` when it has none this can read.
pub fn meta(script: &str) -> Option<Value> {
    let at = find_meta(script)?;
    let mut reader = Reader {
        chars: script[at..].chars().collect(),
        at: 0,
    };
    let value = reader.value(0)?;
    value.is_object().then_some(value)
}

/// Where `meta`'s object literal starts: after `meta =` or `meta:`.
fn find_meta(script: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(found) = script[from..].find("meta") {
        let start = from + found;
        let before = script[..start].chars().next_back();
        let after = script[start + 4..].trim_start();
        let word_starts = before.is_none_or(|c| !c.is_alphanumeric() && c != '_' && c != '.');
        if word_starts && let Some(rest) = after.strip_prefix(['=', ':']) {
            let rest = rest.trim_start();
            if rest.starts_with('{') {
                return Some(script.len() - rest.len());
            }
        }
        from = start + 4;
    }
    None
}

/// Deeper than any real `meta`, so a hostile script can't run the stack out.
const MAX_DEPTH: usize = 16;

struct Reader {
    chars: Vec<char>,
    at: usize,
}

impl Reader {
    fn value(&mut self, depth: usize) -> Option<Value> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.skip_blank();
        match self.peek()? {
            '{' => self.object(depth),
            '[' => self.array(depth),
            '\'' | '"' | '`' => self.string().map(Value::String),
            c if c == '-' || c.is_ascii_digit() => self.number(),
            _ => match self.word()?.as_str() {
                "true" => Some(Value::Bool(true)),
                "false" => Some(Value::Bool(false)),
                "null" | "undefined" => Some(Value::Null),
                _ => None,
            },
        }
    }

    fn object(&mut self, depth: usize) -> Option<Value> {
        self.at += 1;
        let mut map = Map::new();
        loop {
            self.skip_blank();
            match self.peek()? {
                '}' => {
                    self.at += 1;
                    return Some(Value::Object(map));
                }
                ',' => self.at += 1,
                '\'' | '"' => {
                    let key = self.string()?;
                    self.entry(&mut map, key, depth)?;
                }
                _ => {
                    let key = self.word()?;
                    self.entry(&mut map, key, depth)?;
                }
            }
        }
    }

    fn entry(&mut self, map: &mut Map<String, Value>, key: String, depth: usize) -> Option<()> {
        self.skip_blank();
        if self.peek()? != ':' {
            return None;
        }
        self.at += 1;
        let value = self.value(depth + 1)?;
        map.insert(key, value);
        Some(())
    }

    fn array(&mut self, depth: usize) -> Option<Value> {
        self.at += 1;
        let mut items = Vec::new();
        loop {
            self.skip_blank();
            match self.peek()? {
                ']' => {
                    self.at += 1;
                    return Some(Value::Array(items));
                }
                ',' => self.at += 1,
                _ => items.push(self.value(depth + 1)?),
            }
        }
    }

    fn string(&mut self) -> Option<String> {
        let quote = self.peek()?;
        self.at += 1;
        let mut out = String::new();
        loop {
            let c = self.next()?;
            match c {
                c if c == quote => return Some(out),
                '$' if quote == '`' && self.peek() == Some('{') => return None,
                '\\' => out.push(match self.next()? {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    other => other,
                }),
                c => out.push(c),
            }
        }
    }

    fn number(&mut self) -> Option<Value> {
        let start = self.at;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E' | '_'))
        {
            self.at += 1;
        }
        let text: String = self.chars[start..self.at]
            .iter()
            .filter(|c| **c != '_')
            .collect();
        let number: f64 = text.parse().ok()?;
        if number.fract() == 0.0 && number.abs() < 9e15 {
            #[allow(clippy::cast_possible_truncation)]
            return Some(Value::from(number as i64));
        }
        Number::from_f64(number).map(Value::Number)
    }

    fn word(&mut self) -> Option<String> {
        let start = self.at;
        while self
            .peek()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$')
        {
            self.at += 1;
        }
        (self.at > start).then(|| self.chars[start..self.at].iter().collect())
    }

    fn skip_blank(&mut self) {
        loop {
            match (self.peek(), self.chars.get(self.at + 1).copied()) {
                (Some(c), _) if c.is_whitespace() => self.at += 1,
                (Some('/'), Some('/')) => while self.next().is_some_and(|c| c != '\n') {},
                (Some('/'), Some('*')) => {
                    self.at += 2;
                    while let Some(c) = self.next() {
                        if c == '*' && self.peek() == Some('/') {
                            self.at += 1;
                            break;
                        }
                    }
                }
                _ => return,
            }
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.at += 1;
        Some(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_the_meta_of_a_workflow_script() {
        let script = "export const meta = {\n  name: 'probe',\n  description: \"Asks, \\\"then\\\" answers\",\n  // the order they run in\n  phases: [\n    { title: 'Ask', detail: `one agent` },\n    { 'title': 'Answer', },\n  ],\n  version: 2,\n}\n\nconst a = await agent('x', { label: 'ask:one' })\n";
        assert_eq!(
            meta(script),
            Some(json!({
                "name": "probe",
                "description": "Asks, \"then\" answers",
                "phases": [{"title": "Ask", "detail": "one agent"}, {"title": "Answer"}],
                "version": 2
            }))
        );
    }

    #[test]
    fn a_meta_it_cant_read_is_none() {
        assert_eq!(meta("const x = 1"), None);
        assert_eq!(meta("export const metadata = { name: 'x' }"), None);
        assert_eq!(meta("export const meta = { name: `run-${id}` }"), None);
        assert_eq!(meta("export const meta = { phases: phases() }"), None);
        assert_eq!(meta("export const meta = { name: 'cut"), None);
        let deep = format!("export const meta = {}", "{a:".repeat(100));
        assert_eq!(meta(&deep), None);
    }
}
