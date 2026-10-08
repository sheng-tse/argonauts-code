//! Claude Code's artifacts: pages a session publishes on claude.ai with its `Artifact` tool.
//! This reads one from the tool's call and result as the stream sends them,
//! and words it the way Claude Code 2.1.292's own screen does. Sources are byte offsets into
//! that version's program text.
//!
//! - The result's text starts `Published <file> at <url> (Version 1…)`, or `Updated the
//!   Artifact at <url> …` for one made from a type (@215300300).
//! - The row under the call is `Published <url>` or `Updated <url>`, dim, the URL a link
//!   (@224008353).
//! - The bottom line's part is `⧉ <name>` with one artifact and `⧉ <count>` with more: the
//!   mark in Claude Code's own colour, the rest dim, and `artifact <name>` or `<count>
//!   artifacts` where the mark can't be drawn. The name is the published file's without its
//!   extension, else the title, cut at 24 cells (@183773744 for the glyph, @212816978 for the
//!   part).
//! - `1 feedback draft` beside that part is another part and says nothing of the artifact:
//!   it counts the notes about Claude Code itself that a session left for you to send
//!   (@212867164, @223178338). No stream line carries it.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The tool's name in the stream.
pub const TOOL: &str = "Artifact";

/// The mark Claude Code puts before an artifact's name on its bottom line.
pub const GLYPH: &str = "⧉";

/// Where claude.ai serves artifacts. Only such a link is ever opened or copied, since the
/// address comes out of a tool's result.
const HOST: &str = "https://claude.ai/";

/// One published page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub url: String,
    /// What Claude Code calls it: the published file's name without its extension, else its
    /// title.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The file that was published, as the call named it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// The result said it updated a page that existed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub updated: bool,
}

impl Artifact {
    /// `Published` or `Updated`, the word before the link in the call's row.
    pub fn verb(&self) -> &'static str {
        if self.updated { "Updated" } else { "Published" }
    }
}

/// Whether this call of the tool publishes a page. The tool also reads, lists, and deletes
/// artifacts and uploads their assets, and none of those makes a page to show.
pub fn publishes(name: &str, input: &Value) -> bool {
    let publish = match input.get("action").and_then(Value::as_str) {
        None | Some("publish") => true,
        Some(_) => false,
    };
    let asset = input.get("asset").and_then(Value::as_bool) == Some(true);
    name == TOOL && publish && !asset
}

/// How the tool starts the result of a call that made or changed a page.
const MADE: &[&str] = &["Published", "Updated", "Created"];

/// The page a publishing call made, from its input and the text of its result. `None` when
/// the result isn't one of a page made, as a refused or failed publish isn't, whatever
/// address its words mention.
pub fn published(input: &Value, output: &str) -> Option<Artifact> {
    let output = output.trim_start();
    if !MADE.iter().any(|verb| output.starts_with(verb)) {
        return None;
    }
    let url = link_in(output)?.to_owned();
    let text = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let file = text("file_path");
    let title = text("title");
    let stem = file.as_deref().and_then(|file| {
        let stem = Path::new(file).file_stem()?.to_string_lossy();
        Some(stem.trim().to_owned()).filter(|stem| !stem.is_empty())
    });
    Some(Artifact {
        name: stem
            .or_else(|| title.clone())
            .unwrap_or_else(|| "artifact".to_owned()),
        updated: output.starts_with("Updated"),
        url,
        title,
        file,
    })
}

/// The first claude.ai address in `text`, without the punctuation a sentence puts after it.
pub fn link_in(text: &str) -> Option<&str> {
    let start = text.find(HOST)?;
    let rest = &text[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c.is_control() || matches!(c, '"' | '<' | '>' | '`'))
        .unwrap_or(rest.len());
    let link = rest[..end].trim_end_matches(['.', ',', ';', ':', ')', ']', '\'']);
    (link.len() > HOST.len()).then_some(link)
}

/// An artifact's whole address in `text`: a claude.ai link that ends in `/artifact/` and a
/// full id. The terminal is told a row's link is clickable only for one of these, so a link a
/// narrow row cut short is never offered as if it were whole.
pub fn whole_link_in(text: &str) -> Option<&str> {
    let link = link_in(text)?;
    let path = link.split(['?', '#']).next().unwrap_or(link);
    let (_, id) = path.rsplit_once("/artifact/")?;
    let hex =
        |part: &str, len: usize| part.len() == len && part.chars().all(|c| c.is_ascii_hexdigit());
    let parts: Vec<&str> = id.split('-').collect();
    let whole = matches!(parts.as_slice(), [a, b, c, d, e]
        if hex(a, 8) && hex(b, 4) && hex(c, 4) && hex(d, 4) && hex(e, 12));
    whole.then_some(link)
}

/// Whether `url` is a claude.ai address and nothing else, the only kind handed to a browser.
pub fn is_link(url: &str) -> bool {
    link_in(url) == Some(url)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const URL: &str = "https://claude.ai/code/artifact/0b1e7c9a-4f1d-4c57-9a55-2f6f0d3a8e11";

    fn result(start: &str) -> String {
        format!(
            "{start} (Version 1) Icon: chart. Not previewed this session: the preview catches \
             layout and load errors before viewers see them."
        )
    }

    #[test]
    fn a_publish_names_the_page_by_its_file_and_keeps_its_link() {
        let input = json!({"file_path": "/repo/out/weekly-usage-chart.html", "icon": "chart",
                           "description": "This week's spending"});
        assert!(publishes(TOOL, &input));
        let output = result(&format!(
            "Published /repo/out/weekly-usage-chart.html at {URL}"
        ));
        let artifact = published(&input, &output).unwrap();
        assert_eq!(artifact.url, URL);
        assert_eq!(artifact.name, "weekly-usage-chart");
        assert_eq!(
            artifact.file.as_deref(),
            Some("/repo/out/weekly-usage-chart.html")
        );
        assert_eq!(
            (artifact.verb(), artifact.title.as_deref()),
            ("Published", None)
        );

        // One made from a type is updated in place, and goes by its title when it has no file.
        let typed = json!({"action": "publish", "title": "This week", "url": URL});
        let output =
            format!("Updated the Artifact at {URL} with (unknown) (and any `files` listed).");
        let artifact = published(&typed, &output).unwrap();
        assert_eq!(
            (artifact.name.as_str(), artifact.verb()),
            ("This week", "Updated")
        );
        assert_eq!(artifact.url, URL);
    }

    #[test]
    fn only_a_publish_that_names_a_claude_ai_address_is_an_artifact() {
        for other in ["read", "list", "delete", "open", "quickstart", "pin"] {
            assert!(
                !publishes(TOOL, &json!({"action": other, "url": URL})),
                "{other}"
            );
        }
        assert!(!publishes(
            TOOL,
            &json!({"file_path": "a.png", "asset": true, "url": URL})
        ));
        assert!(!publishes("Write", &json!({"file_path": "a.html"})));
        let input = json!({"file_path": "a.html"});
        for output in [
            "<tool_use_error>content must first be written to a file</tool_use_error>",
            "Your organization's policy blocks publishing. See https://claude.ai/settings/policy",
            "Published a.html at https://example.com/code/artifact/1",
            "Published a.html at https://claude.ai/",
            "",
        ] {
            assert_eq!(published(&input, output), None, "{output}");
        }
    }

    #[test]
    fn a_link_ends_where_the_sentence_goes_on() {
        assert_eq!(link_in(&format!("at {URL}.")), Some(URL));
        assert_eq!(link_in(&format!("see ({URL}), then")), Some(URL));
        assert_eq!(link_in(&format!("{URL}\nnext line")), Some(URL));
        let shared = format!("{URL}?sk=abc123");
        assert_eq!(link_in(&shared), Some(shared.as_str()));
        assert!(is_link(URL));
        // A link a narrow row cut short isn't a whole one.
        assert_eq!(whole_link_in(&format!("Published {URL}")), Some(URL));
        assert_eq!(whole_link_in(&shared), Some(shared.as_str()));
        assert_eq!(whole_link_in(&URL[..URL.len() - 4]), None);
        assert_eq!(whole_link_in("https://claude.ai/settings"), None);
        assert!(!is_link(&format!("{URL} && rm -rf ~")));
        assert!(!is_link("https://claude.ai.evil.example/x"));
        assert!(!is_link("-oProxyCommand=x https://claude.ai/a"));
    }
}
