//! Secret patterns shared by the secret check and the event sink, which redacts before anything
//! reaches the journal, a subscriber, a reviewer, or a judge.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretMatch {
    pub kind: &'static str,
    /// In bytes.
    pub range: Range<usize>,
}

/// A pattern and which capture group is the secret (0 for the whole match).
struct Pattern {
    kind: &'static str,
    regex: Regex,
    group: usize,
}

static PATTERNS: LazyLock<Vec<Pattern>> = LazyLock::new(|| {
    let pattern = |kind, source: &str, group| Pattern {
        kind,
        regex: Regex::new(source).unwrap_or_else(|err| panic!("secret pattern {kind}: {err}")),
        group,
    };
    vec![
        pattern(
            "private key",
            r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----(?s:.*?)(?:-----END [A-Z0-9 ]*PRIVATE KEY-----|\z)",
            0,
        ),
        pattern("aws access key", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b", 0),
        pattern("github token", r"\bgh[pousr]_[A-Za-z0-9]{36,}\b", 0),
        pattern("github token", r"\bgithub_pat_[A-Za-z0-9_]{22,}", 0),
        pattern("anthropic key", r"\bsk-ant-[A-Za-z0-9_-]{20,}", 0),
        pattern("openai key", r"\bsk-[A-Za-z0-9_-]{20,}", 0),
        pattern("google api key", r"\bAIza[0-9A-Za-z_-]{35}", 0),
        pattern("slack token", r"\bxox[abposr]-[A-Za-z0-9-]{10,}", 0),
        pattern(
            "assigned secret",
            r#"(?i)(?:password|passwd|secret|token|api[_-]?key)\b["']?\s*[:=]\s*["']?([A-Za-z0-9/+_.=-]{16,})"#,
            1,
        ),
    ]
});

/// Every secret in `text`, in order, with overlapping finds merged.
pub fn find_secrets(text: &str) -> Vec<SecretMatch> {
    let mut found: Vec<SecretMatch> = Vec::new();
    for pattern in PATTERNS.iter() {
        for captures in pattern.regex.captures_iter(text) {
            if let Some(hit) = captures.get(pattern.group) {
                found.push(SecretMatch {
                    kind: pattern.kind,
                    range: hit.range(),
                });
            }
        }
    }
    found.sort_by_key(|hit| (hit.range.start, std::cmp::Reverse(hit.range.end)));
    let mut merged: Vec<SecretMatch> = Vec::new();
    for hit in found {
        match merged.last_mut() {
            Some(last) if hit.range.start < last.range.end => {
                last.range.end = last.range.end.max(hit.range.end);
            }
            _ => merged.push(hit),
        }
    }
    merged
}

/// `text` with every secret replaced by `[redacted <kind>]`. Borrows when there's none.
pub fn redact_secrets(text: &str) -> Cow<'_, str> {
    let found = find_secrets(text);
    if found.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for hit in found {
        out.push_str(&text[at..hit.range.start]);
        out.push_str("[redacted ");
        out.push_str(hit.kind);
        out.push(']');
        at = hit.range.end;
    }
    out.push_str(&text[at..]);
    Cow::Owned(out)
}

pub fn redact_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            if let Cow::Owned(clean) = redact_secrets(text) {
                *text = clean;
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(redact_json),
        serde_json::Value::Object(map) => map.values_mut().for_each(redact_json),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Built at runtime so no string in this file looks like a real credential.
    fn fake(prefix: &str, body: char, len: usize) -> String {
        format!("{prefix}{}", body.to_string().repeat(len))
    }

    #[test]
    fn redact_secrets_patterns() {
        let cases = [
            (fake("AKIA", 'Q', 16), "aws access key"),
            (fake("ghp_", 'a', 36), "github token"),
            (fake("github_pat_", 'b', 30), "github token"),
            (fake("sk-ant-", 'c', 30), "anthropic key"),
            (fake("sk-proj-", 'd', 30), "openai key"),
            (fake("AIza", 'e', 35), "google api key"),
            (fake("xoxb-", '1', 20), "slack token"),
        ];
        for (secret, kind) in cases {
            let text = format!("before {secret} after");
            assert_eq!(
                redact_secrets(&text),
                format!("before [redacted {kind}] after"),
                "{kind}"
            );
        }
    }

    #[test]
    fn private_keys_and_assignments_are_redacted_but_names_stay() {
        let key = format!(
            "-----BEGIN {0} KEY-----\nMIIE\n-----END {0} KEY-----",
            "RSA PRIVATE"
        );
        assert_eq!(
            redact_secrets(&format!("x {key} y")),
            "x [redacted private key] y"
        );
        let assignment = format!("DB_PASSWORD = \"{}\"", fake("", 'z', 20));
        assert_eq!(
            redact_secrets(&assignment),
            "DB_PASSWORD = \"[redacted assigned secret]\""
        );
        assert_eq!(redact_secrets("password = short"), "password = short");
    }

    #[test]
    fn clean_text_is_borrowed_and_overlaps_merge() {
        assert!(matches!(redact_secrets("nothing here"), Cow::Borrowed(_)));
        let text = format!("token: {}", fake("sk-ant-", 'k', 24));
        assert_eq!(find_secrets(&text).len(), 1);
    }

    #[test]
    fn json_strings_are_redacted_everywhere() {
        let secret = fake("ghp_", 'q', 40);
        let mut value = json!({"a": [format!("x {secret}")], "b": {"c": secret}, "d": 3});
        redact_json(&mut value);
        assert_eq!(
            value,
            json!({"a": ["x [redacted github token]"], "b": {"c": "[redacted github token]"}, "d": 3})
        );
    }
}
