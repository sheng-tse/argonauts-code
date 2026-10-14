//! The raw log's rows: one event per row, its type first and then the CLI's own text with the
//! keys coloured, cut to one line. An unfolded event shows pretty-printed beneath. The text is
//! never reworded, only cut to fit and stripped of control sequences.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::text::{hard_wrap_spans, sanitize_line, str_width, truncate_spans};
use crate::theme::{Role, Theme};

/// The type column is never wider than this, so a long type can't push the text away.
const TYPE_CELLS: usize = 22;

/// `system/init`, `assistant`, `item.completed`: the event's `type`, and its `subtype` when it
/// has one. `None` for a line that isn't a JSON object with a type.
pub(super) fn type_of(line: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    let kind = value.get("type")?.as_str()?;
    Some(match value.get("subtype").and_then(Value::as_str) {
        Some(subtype) => format!("{kind}/{subtype}"),
        None => kind.to_owned(),
    })
}

/// How wide the type column is for these types.
pub(super) fn type_column(types: &[Option<String>]) -> usize {
    types
        .iter()
        .flatten()
        .map(|kind| str_width(kind))
        .max()
        .unwrap_or(0)
        .min(TYPE_CELLS)
}

/// One event on one row, filled with the selection's background when the cursor is on it.
pub(super) fn event_row(
    line: &str,
    kind: Option<&str>,
    column: usize,
    width: usize,
    selected: bool,
    theme: &Theme,
) -> Line<'static> {
    let mut spans = Vec::new();
    if column > 0 {
        let label = kind.unwrap_or_default();
        let label = truncate_spans(&[Span::raw(label.to_owned())], column)
            .into_iter()
            .map(|span| span.content.into_owned())
            .collect::<String>();
        let pad = column.saturating_sub(str_width(&label)) + 2;
        spans.push(Span::styled(label, theme.style(Role::Accent)));
        spans.push(Span::raw(" ".repeat(pad)));
    }
    spans.extend(json_spans(&sanitize_line(line), theme));
    let mut spans = truncate_spans(&spans, width);
    if selected {
        let fill = theme.style(Role::Selection);
        let used: usize = spans.iter().map(|span| str_width(&span.content)).sum();
        spans = spans
            .into_iter()
            .map(|span| {
                let style = span.style.patch(fill);
                Span::styled(span.content, style)
            })
            .collect();
        if used < width {
            spans.push(Span::styled(" ".repeat(width - used), fill));
        }
    }
    Line::from(spans)
}

/// The event pretty-printed, indented under its row, long lines wrapped. A line that isn't
/// JSON shows whole, wrapped.
pub(super) fn unfolded(line: &str, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let pretty = serde_json::from_str::<Value>(line.trim())
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok());
    let text = pretty.unwrap_or_else(|| line.to_owned());
    let indent = Span::styled("  ", Style::default());
    let room = width.saturating_sub(2).max(1);
    let mut rows = Vec::new();
    for row in text.lines() {
        let spans = json_spans(&sanitize_line(row), theme);
        for wrapped in hard_wrap_spans(&spans, room) {
            let mut spans = vec![indent.clone()];
            spans.extend(wrapped);
            rows.push(Line::from(spans));
        }
    }
    rows
}

/// Whether the line holds `query`, ignoring case.
pub(super) fn holds(line: &str, query: &str) -> bool {
    !query.is_empty() && line.to_lowercase().contains(&query.to_lowercase())
}

/// The text in the text color, with each object key in the label color.
fn json_spans(text: &str, theme: &Theme) -> Vec<Span<'static>> {
    let plain = theme.style(Role::Text);
    let key = theme.style(Role::Label);
    let mut spans = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut end = text.len();
        let mut escaped = false;
        for (i, c) in chars.by_ref() {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                end = i + 1;
                break;
            }
        }
        let is_key = text[end..].trim_start().starts_with(':');
        if is_key {
            if start < at {
                spans.push(Span::styled(text[start..at].to_owned(), plain));
            }
            spans.push(Span::styled(text[at..end].to_owned(), key));
            start = end;
        }
    }
    if start < text.len() {
        spans.push(Span::styled(text[start..].to_owned(), plain));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Glyphs;

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    fn text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn an_event_is_one_row_with_its_type_first_and_its_text_as_written() {
        let line = r#"{"type":"system","subtype":"init","cwd":"/tmp/x","tools":["Bash","Edit"]}"#;
        let kind = type_of(line);
        assert_eq!(kind.as_deref(), Some("system/init"));
        let row = event_row(line, kind.as_deref(), 11, 200, false, &theme());
        assert_eq!(text(&row), format!("system/init  {line}"));
        let narrow = event_row(line, kind.as_deref(), 11, 40, false, &theme());
        assert!(str_width(&text(&narrow)) <= 40);
    }

    #[test]
    fn keys_are_coloured_and_values_are_not() {
        let theme = theme();
        let spans = json_spans(r#"{"a":"b:c","d\"e":1}"#, &theme);
        let keys: Vec<&str> = spans
            .iter()
            .filter(|span| span.style == theme.style(Role::Label))
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(keys, [r#""a""#, r#""d\"e""#]);
    }

    #[test]
    fn an_unfolded_event_is_pretty_printed_and_plain_text_stays_whole() {
        let rows = unfolded(r#"{"type":"result","is_error":false}"#, 80, &theme());
        let texts: Vec<String> = rows.iter().map(text).collect();
        assert!(
            texts
                .iter()
                .any(|row| row.trim() == r#""type": "result""#
                    || row.trim() == r#""type": "result","#)
        );
        assert!(texts.len() >= 3);
        let plain = unfolded("not json at all", 80, &theme());
        assert_eq!(plain.len(), 1);
        assert_eq!(type_of("not json at all"), None);
    }

    #[test]
    fn search_ignores_case() {
        assert!(holds(r#"{"type":"Result"}"#, "result"));
        assert!(!holds("anything", ""));
    }
}
