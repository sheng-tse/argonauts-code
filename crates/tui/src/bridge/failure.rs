//! A failure in one plain line: what failed, in words. The CLI's or git's own text, often JSON
//! a few layers deep, stays behind it for `r`.

use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::cell::{Cell, HistoryCell};
use crate::render::RenderCtx;
use crate::text::{fit_lines, sanitize_line, wrap_spans};

/// Plain lines are cut here, so one never wraps a narrow screen twice.
const PLAIN_CHARS: usize = 120;

/// What follows an agent's plain error line when the CLI said more.
pub const RAW_ERROR_HINT: &str = "r shows the raw error";
/// What follows a failed run's line when git or the CLI said more.
pub const RAW_ALL_HINT: &str = "r shows it all";

/// The whole text behind the newest failure in the conversation. `r` opens it full screen until
/// something is typed, so the letter is only taken while its hint is the latest thing said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFailure {
    pub title: String,
    pub text: String,
}

impl RawFailure {
    /// The text behind `plain_line`, or nothing when the line already says all of it.
    pub fn behind(plain_line: &str, title: impl Into<String>, text: &str) -> Option<Self> {
        let text = text.trim();
        (!text.is_empty() && !plain_line.contains(text)).then(|| Self {
            title: title.into(),
            text: text.to_owned(),
        })
    }

    /// What the full-screen view shows: JSON laid out by its nesting, any other text as it came.
    pub fn cells(&self) -> Vec<Cell> {
        let text = serde_json::from_str::<Value>(&self.text)
            .ok()
            .filter(|value| value.is_object() || value.is_array())
            .and_then(|value| serde_json::to_string_pretty(&value).ok())
            .unwrap_or_else(|| self.text.clone());
        vec![Cell::Custom(Box::new(RawText { text }))]
    }
}

#[derive(Debug, Clone)]
struct RawText {
    text: String,
}

impl HistoryCell for RawText {
    /// Each line wraps under its own indent, so laid-out JSON keeps its shape on a narrow screen.
    fn lines(&self, ctx: &RenderCtx<'_>) -> Vec<Line<'static>> {
        let width = usize::from(ctx.width);
        let mut lines = Vec::new();
        for line in self.text.lines() {
            let body = line.trim_start();
            if body.is_empty() {
                lines.push(Line::default());
                continue;
            }
            let indent = [Span::raw(
                line[..line.len() - body.len()].replace('\t', "    "),
            )];
            let spans = [Span::raw(sanitize_line(body).into_owned())];
            lines.extend(wrap_spans(&spans, width, &indent, &indent));
        }
        fit_lines(&mut lines, width);
        lines
    }
}

/// What failed, in words, for `cli` (its display name, such as `Codex`), from the text it
/// failed with.
pub fn plain(cli: &str, message: &str) -> String {
    let said = innermost(message);
    let lower = said.to_lowercase();
    let words = if refused_format(&lower) {
        format!("{cli} refused the answer format Argonauts Code asked for")
    } else if lower.contains("not logged in")
        || lower.contains("please run /login")
        || lower.contains("authentication")
        || lower.contains("unauthorized")
    {
        format!("{cli} isn't signed in")
    } else if lower.contains("rate limit") {
        format!("{cli} hit a rate limit")
    } else {
        match first_line(&said) {
            "" => format!("{cli} failed"),
            line => format!("{cli} failed: {line}"),
        }
    };
    cut(&words)
}

/// Any failure's text in one line: the message inside its JSON, its first line, cut to fit.
pub fn one_line(text: &str) -> String {
    cut(first_line(&innermost(text)))
}

/// What git said, in one line, after what Argonauts Code was doing: `git couldn't add the run's
/// files: <its first line>`.
pub fn plain_git(doing: &str, message: &str) -> String {
    match first_line(message) {
        "" => cut(&format!("git couldn't {doing}")),
        line => cut(&format!("git couldn't {doing}: {line}")),
    }
}

/// The message a JSON error carries, however deep, as `{"error":{"message":"…"}}` or a string
/// that is itself JSON. Text that isn't JSON is the message.
fn innermost(text: &str) -> String {
    let mut current = text.trim().to_owned();
    for _ in 0..4 {
        let Ok(value) = serde_json::from_str::<Value>(&current) else {
            break;
        };
        let inner =
            ["error", "message", "detail"]
                .iter()
                .find_map(|key| match value.get(*key)? {
                    Value::String(text) => Some(text.clone()),
                    nested @ Value::Object(_) => Some(nested.to_string()),
                    _ => None,
                });
        match inner {
            Some(inner) => current = inner.trim().to_owned(),
            None => break,
        }
    }
    current
}

fn refused_format(lower: &str) -> bool {
    lower.contains("invalid_json_schema")
        || lower.contains("invalid schema for response_format")
        || lower.contains("output_schema")
}

fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
}

fn cut(text: &str) -> String {
    match text.char_indices().nth(PLAIN_CHARS) {
        Some((at, _)) => format!("{}…", text[..at].trim_end()),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape in which Codex passes on the service turning down an answer format.
    const CODEX_SCHEMA: &str = r#"{"type":"error","message":"{\"error\":{\"message\":\"Invalid schema for response_format 'codex_output_schema': In context=(), 'required' is required to be supplied and to be an array including every key in properties. Missing 'summary'.\",\"type\":\"invalid_request_error\",\"param\":\"text.format.schema\",\"code\":\"invalid_json_schema\"}}"}"#;

    #[test]
    fn a_refused_answer_format_reads_in_words() {
        assert_eq!(
            plain("Codex", CODEX_SCHEMA),
            "Codex refused the answer format Argonauts Code asked for"
        );
    }

    #[test]
    fn other_errors_keep_their_own_first_line_and_never_their_json() {
        let api = r#"{"error":{"message":"Overloaded\nretry later","type":"overloaded_error"}}"#;
        assert_eq!(plain("Claude Code", api), "Claude Code failed: Overloaded");
        assert_eq!(plain("Codex", ""), "Codex failed");
        let long = "x".repeat(400);
        assert!(plain("Codex", &long).chars().count() <= PLAIN_CHARS + 1);
    }

    #[test]
    fn the_raw_text_is_kept_only_when_it_says_more_than_the_line() {
        let line = plain("Codex", CODEX_SCHEMA);
        let raw = RawFailure::behind(&line, "review", CODEX_SCHEMA).expect("the JSON says more");
        assert_eq!(raw.text, CODEX_SCHEMA);
        assert_eq!(
            RawFailure::behind("Codex failed: gone", "review", " gone\n"),
            None
        );
        assert_eq!(RawFailure::behind("Codex failed", "review", "  "), None);
    }

    #[test]
    fn the_raw_view_lays_json_out_and_keeps_other_text_as_it_came() {
        let theme = crate::theme::Theme::dark();
        let clock = crate::render::Clock::still(std::time::Instant::now());
        let ctx = RenderCtx::new(40, &theme, clock);
        let shown = |text: &str| -> Vec<String> {
            let raw = RawFailure {
                title: "t".into(),
                text: text.into(),
            };
            raw.cells()[0]
                .lines(&ctx)
                .iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect()
                })
                .collect()
        };
        let json = shown(r#"{"error":{"message":"bad","code":"x"}}"#);
        assert_eq!(json.first().map(String::as_str), Some("{"));
        // Keys keep their order when a dependency asks serde_json to, so a line may end in a comma.
        assert!(
            json.iter()
                .any(|line| line.trim_end_matches(',') == "    \"message\": \"bad\""),
            "{json:?}"
        );
        let git = shown("fatal: one\n\nhint: two");
        assert_eq!(git, ["fatal: one", "", "hint: two"]);
    }

    #[test]
    fn git_says_what_it_refused_in_one_line() {
        let message = "The following paths are ignored by one of your .gitignore files:\n.argonauts\nhint: Use -f";
        assert_eq!(
            plain_git("add the run's files", message),
            "git couldn't add the run's files: The following paths are ignored by one of your \
             .gitignore files:"
        );
    }
}
