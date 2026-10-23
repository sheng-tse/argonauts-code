//! A file a CLI ships with itself, such as a skill it bundles, holds its maker's text, which
//! isn't ours to publish. A recorded session that read such a file keeps a placeholder for it:
//! the file's first heading, then a note in place of the rest.

use std::collections::HashMap;

use serde_json::Value;

use super::report::Place;
use super::table::{BundledText, Table};
use super::tree::File;

const FILE_RESULT_KEYS: [&str; 2] = ["tool_use_result", "toolUseResult"];

/// Each text in a file that a Read or a Skill call brought in from a CLI's bundled folder and
/// that holds more than its placeholder. Only files that name the folder are read.
pub fn places(table: &Table, files: &[File]) -> Vec<Place> {
    let bundled = &table.recordings.bundled_text;
    let mut places = Vec::new();
    for file in files {
        let Some(text) = file.text.as_deref() else {
            continue;
        };
        if !text.contains(&bundled.folder) {
            continue;
        }
        let lines: Vec<Option<Value>> = text
            .lines()
            .map(|line| serde_json::from_str(line).ok())
            .collect();
        let reads = read_calls(&lines, bundled);
        for (number, value) in lines.iter().enumerate() {
            let Some(value) = value else {
                continue;
            };
            for (name, text) in bundled_texts(value, &reads, bundled) {
                let kept = kept_lines(text, bundled);
                if !is_placeholder(&kept, bundled) {
                    let found = format!("{name}, {} lines", kept.len());
                    places.push(Place::new(
                        &file.path,
                        Some(number + 1),
                        &found,
                        &bundled.fix,
                    ));
                }
            }
        }
    }
    places
}

/// The Read calls that open a file in the bundled folder, by id, each with that file's path.
fn read_calls<'v>(lines: &'v [Option<Value>], bundled: &BundledText) -> HashMap<&'v str, &'v str> {
    let mut reads = HashMap::new();
    for block in lines.iter().flatten().flat_map(blocks) {
        let is_read = block["type"] == "tool_use"
            && block["name"]
                .as_str()
                .is_some_and(|name| bundled.read_tools.iter().any(|tool| tool == name));
        if let (true, Some(id), Some(path)) = (
            is_read,
            block["id"].as_str(),
            path_in(&block["input"], &bundled.folder),
        ) {
            reads.insert(id, path);
        }
    }
    reads
}

fn blocks(value: &Value) -> impl Iterator<Item = &Value> {
    value["message"]["content"].as_array().into_iter().flatten()
}

/// The first string in a value that names the folder.
fn path_in<'v>(value: &'v Value, folder: &str) -> Option<&'v str> {
    match value {
        Value::String(text) => text.contains(folder).then_some(text.as_str()),
        Value::Array(items) => items.iter().find_map(|item| path_in(item, folder)),
        Value::Object(map) => map.values().find_map(|item| path_in(item, folder)),
        _ => None,
    }
}

/// Every text in one line that came from the bundled folder, each with the path it came from.
fn bundled_texts<'v>(
    value: &'v Value,
    reads: &HashMap<&str, &'v str>,
    bundled: &BundledText,
) -> Vec<(String, &'v str)> {
    let name = |path: &str| {
        path.find(&bundled.folder)
            .map_or(path, |at| &path[at..])
            .to_owned()
    };
    let mut texts = Vec::new();
    // A session's own log and its stream-json output keep the file a Read returned beside the
    // tool result, under one key or the other.
    let mut read_here = None;
    for key in FILE_RESULT_KEYS {
        let file = &value[key]["file"];
        if let (Some(path), Some(content)) = (file["filePath"].as_str(), file["content"].as_str())
            && path.contains(&bundled.folder)
        {
            texts.push((name(path), content));
            read_here = Some(path);
        }
    }
    for block in blocks(value) {
        if block["type"] == "tool_result" {
            let read = block["tool_use_id"]
                .as_str()
                .and_then(|id| reads.get(id).copied())
                .or(read_here);
            if let Some(path) = read {
                texts.extend(result_texts(&block["content"]).map(|text| (name(path), text)));
            }
        } else if let Some(text) = block["text"].as_str() {
            texts.extend(launched_skill(text, bundled).map(|path| (name(path), text)));
        }
    }
    if let Some(text) = value["message"]["content"].as_str() {
        texts.extend(launched_skill(text, bundled).map(|path| (name(path), text)));
    }
    texts
}

/// A tool result's text: one string, or a list of text blocks.
fn result_texts(content: &Value) -> impl Iterator<Item = &str> {
    let blocks = content.as_array().into_iter().flatten();
    content
        .as_str()
        .into_iter()
        .chain(blocks.filter_map(|block| block["text"].as_str()))
}

/// The skill folder a Skill call's launched text names on its first line, when it's bundled.
fn launched_skill<'t>(text: &'t str, bundled: &BundledText) -> Option<&'t str> {
    let first = text.lines().next()?;
    let folder = first.strip_prefix(&bundled.skill_preamble)?.trim();
    folder.contains(&bundled.folder).then_some(folder)
}

/// The text's lines that aren't blank, without a launched skill's first line or the numbers
/// a Read result puts before each line (`1\t`, or `     1→` in older versions).
fn kept_lines<'t>(text: &'t str, bundled: &BundledText) -> Vec<&'t str> {
    let body = if text.starts_with(&bundled.skill_preamble) {
        text.split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        text
    };
    body.lines()
        .map(without_line_number)
        .filter(|line| !line.trim().is_empty())
        .collect()
}

fn without_line_number(line: &str) -> &str {
    let digits = line.trim_start_matches(' ');
    let rest = digits.trim_start_matches(|c: char| c.is_ascii_digit());
    if rest.len() == digits.len() {
        return line;
    }
    rest.strip_prefix('\t')
        .or_else(|| rest.strip_prefix('→'))
        .unwrap_or(line)
}

/// The note alone, or a heading and then the note.
fn is_placeholder(kept: &[&str], bundled: &BundledText) -> bool {
    match kept {
        [note] => *note == bundled.note,
        [heading, note] => heading.starts_with('#') && *note == bundled.note,
        _ => false,
    }
}
