//! A component tree from `ui_render` read into rows of text and buttons. Argonauts draws it in
//! its own terminal style, so only what the tree says matters: `Box` lays children out in a
//! row or a column, `Text` carries text and a look, and `Button` carries a label and what a
//! press sends back. Any other element lays its children out as a `Box` does, so a button
//! under one a later version adds still works, or else shows its text plainly. One with
//! neither is left out.

use argonauts_core::screen::{Drawn, DrawnButton, DrawnLook, DrawnRow, DrawnSpan};
use serde_json::Value;

use crate::json::{str_at, string_at};

/// Deeper than any tree a mod draws, so a reply that loops can't run away.
const MAX_DEPTH: usize = 64;

pub fn read(tree: &Value) -> Result<Drawn, String> {
    if !tree.is_object() {
        return Err("the tree isn't an element".to_owned());
    }
    let mut drawn = Drawn::default();
    let rows = element(tree, DrawnLook::Plain, 0, &mut drawn)?;
    drawn.rows = rows
        .into_iter()
        .filter(|row| !row.text().trim().is_empty())
        .collect();
    Ok(drawn)
}

fn element(
    node: &Value,
    look: DrawnLook,
    depth: usize,
    drawn: &mut Drawn,
) -> Result<Vec<DrawnRow>, String> {
    if depth > MAX_DEPTH {
        return Err("the tree is nested too deep".to_owned());
    }
    if let Some(text) = node.as_str() {
        return Ok(vec![row_of(text, look)]);
    }
    let kind = str_at(node, "type").unwrap_or_default();
    let props = node.get("props").unwrap_or(&Value::Null);
    match kind {
        "Button" => {
            let label = string_at(props, "label").unwrap_or_else(|| text_of(node));
            match node.get("press") {
                Some(press) => drawn.buttons.push(DrawnButton {
                    label,
                    plugin: string_at(press, "plugin").unwrap_or_default(),
                    handle: press.get("handle").and_then(Value::as_u64).unwrap_or(0),
                }),
                None if !label.is_empty() => return Ok(vec![row_of(&label, look)]),
                None => {}
            }
            Ok(Vec::new())
        }
        "Text" => {
            let look = look_of(props).unwrap_or(look);
            let mut row = DrawnRow::default();
            for child in children(node) {
                for part in element(child, look, depth + 1, drawn)? {
                    row.spans.extend(part.spans);
                }
            }
            Ok(vec![row])
        }
        "Box" => laid_out(node, look, depth, drawn),
        _ if !children(node).is_empty() => laid_out(node, look, depth, drawn),
        other => {
            let text = string_at(props, "label")
                .or_else(|| string_at(props, "text"))
                .unwrap_or_default();
            if text.trim().is_empty() {
                let other = if other.is_empty() { "untyped" } else { other };
                if !drawn.left_out.iter().any(|known| known == other) {
                    drawn.left_out.push(other.to_owned());
                }
                return Ok(Vec::new());
            }
            Ok(vec![row_of(&text, look)])
        }
    }
}

/// A `Box`'s children in a row, side by side, or in a column.
fn laid_out(
    node: &Value,
    look: DrawnLook,
    depth: usize,
    drawn: &mut Drawn,
) -> Result<Vec<DrawnRow>, String> {
    let props = node.get("props").unwrap_or(&Value::Null);
    let column = str_at(props, "flexDirection") == Some("column");
    let mut rows = Vec::new();
    for child in children(node) {
        let parts = element(child, look, depth + 1, drawn)?;
        if column {
            rows.extend(parts);
        } else {
            if rows.is_empty() {
                rows.push(DrawnRow::default());
            }
            for part in parts {
                rows[0].spans.extend(part.spans);
            }
        }
    }
    Ok(rows)
}

fn children(node: &Value) -> &[Value] {
    node.get("children")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// Every string under `node`, in order.
fn text_of(node: &Value) -> String {
    match node {
        Value::String(text) => text.clone(),
        _ => children(node).iter().map(text_of).collect(),
    }
}

fn row_of(text: &str, look: DrawnLook) -> DrawnRow {
    DrawnRow {
        spans: vec![DrawnSpan {
            text: text.to_owned(),
            look,
        }],
    }
}

/// Claude Code's theme names, in the few looks Argonauts draws.
fn look_of(props: &Value) -> Option<DrawnLook> {
    if props.get("dimColor").and_then(Value::as_bool) == Some(true) {
        return Some(DrawnLook::Dim);
    }
    let by_color = str_at(props, "color").map(|color| match color {
        "subtle" | "inactive" | "secondaryText" | "gray" | "grey" => DrawnLook::Dim,
        "suggestion" | "claude" | "permission" | "ide" | "autoAccept" => DrawnLook::Accent,
        "warning" | "yellow" => DrawnLook::Warning,
        "error" | "red" => DrawnLook::Error,
        _ => DrawnLook::Plain,
    });
    let bold = props.get("bold").and_then(Value::as_bool) == Some(true);
    match by_color {
        Some(DrawnLook::Plain) | None if bold => Some(DrawnLook::Bold),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// You should know's tree as Claude Code 2.1.289 drew it in a recorded session.
    fn heads_up() -> Value {
        json!({"type": "Box", "props": {"flexDirection": "column", "marginTop": 1}, "children": [
            {"type": "Box", "props": {"flexDirection": "row", "alignItems": "flex-start"},
             "children": [
                {"type": "Box", "props": {"key": "about", "flexShrink": 0}, "children": [
                    {"type": "Text", "children": [
                        {"type": "Text", "props": {"color": "subtle"}, "children": ["✦"]},
                        {"type": "Text", "props": {"color": "subtle"},
                         "children": [" ", "Heads up", " · "]}
                    ]}
                ]},
                {"type": "Text", "props": {"wrap": "wrap"}, "children": [
                    "Release 1.4.0 requires Dana's security sign-off, and Dana is away until Monday."
                ]}
            ]},
            {"type": "Box", "props": {"marginLeft": 2, "flexWrap": "wrap"}, "children": [
                {"type": "Box", "props": {"marginRight": 3}, "children": [
                    {"type": "Button", "props": {"key": "Learn more", "label": "Learn more"},
                     "press": {"plugin": "cc-plugin-you-should-know", "handle": 725_020_656}}
                ]},
                {"type": "Box", "props": {"marginRight": 3}, "children": [
                    {"type": "Button", "props": {"label": "Knew this already"},
                     "press": {"plugin": "cc-plugin-you-should-know", "handle": 11}}
                ]},
                {"type": "Button", "props": {"label": "Dismiss"},
                 "press": {"plugin": "cc-plugin-you-should-know", "handle": 12}}
            ]}
        ]})
    }

    #[test]
    fn a_heads_up_reads_as_one_line_and_three_buttons() {
        let drawn = read(&heads_up()).unwrap();
        assert_eq!(drawn.rows.len(), 1, "{drawn:?}");
        assert_eq!(
            drawn.rows[0].text(),
            "✦ Heads up · Release 1.4.0 requires Dana's security sign-off, and Dana is away \
             until Monday."
        );
        assert_eq!(drawn.rows[0].spans[0].look, DrawnLook::Dim);
        let labels: Vec<_> = drawn.buttons.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(labels, ["Learn more", "Knew this already", "Dismiss"]);
        assert_eq!(drawn.buttons[0].handle, 725_020_656);
        assert_eq!(drawn.buttons[0].plugin, "cc-plugin-you-should-know");
    }

    #[test]
    fn an_unknown_element_shows_its_text_and_one_without_is_left_out() {
        let tree = json!({"type": "Box", "props": {"flexDirection": "column"}, "children": [
            {"type": "Link", "props": {"href": "https://example.com"}, "children": ["the doc"]},
            {"type": "Spinner"},
        ]});
        let drawn = read(&tree).unwrap();
        assert_eq!(drawn.rows[0].text(), "the doc");
        assert_eq!(drawn.left_out, ["Spinner"]);
        // The first render of a session, before any mod drew.
        let nothing = read(&json!({"type": "engine", "ref": 1})).unwrap();
        assert!(nothing.is_empty());
    }

    /// A later version may wrap the buttons in an element Argonauts doesn't know. They stay
    /// buttons you can press, Dismiss among them, and never show as text.
    #[test]
    fn buttons_under_an_unknown_container_stay_buttons() {
        let button = |label: &str, handle: u64| {
            json!({"type": "Button", "props": {"label": label},
                   "press": {"plugin": "cc-plugin-you-should-know", "handle": handle}})
        };
        let tree = json!({"type": "Box", "props": {"flexDirection": "column"}, "children": [
            {"type": "Text", "children": ["Heads up · the release waits for a sign-off"]},
            {"type": "ButtonRow", "children": [
                button("Learn more", 1),
                {"type": "Fragment", "children": [button("Dismiss", 2)]},
            ]},
        ]});
        let drawn = read(&tree).unwrap();
        let rows: Vec<String> = drawn.rows.iter().map(DrawnRow::text).collect();
        assert_eq!(rows, ["Heads up · the release waits for a sign-off"]);
        let buttons: Vec<(&str, u64)> = drawn
            .buttons
            .iter()
            .map(|b| (b.label.as_str(), b.handle))
            .collect();
        assert_eq!(buttons, [("Learn more", 1), ("Dismiss", 2)]);
        assert!(drawn.left_out.is_empty(), "{:?}", drawn.left_out);
    }

    #[test]
    fn a_reply_that_isnt_a_tree_doesnt_read() {
        assert!(read(&json!("text")).is_err());
        assert!(read(&json!(null)).is_err());
    }
}
