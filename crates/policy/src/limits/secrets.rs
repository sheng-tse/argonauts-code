//! The secret scan: core's `redact` patterns, the same ones the journal hides, run on added lines
//! only. It covers private key headers, common token formats, and long strings assigned to names
//! like `password`, `secret`, or `token`. A hit stops the task.
//!
//! A binary file the diff doesn't show can't be scanned, so adding or changing one stops the task
//! too, unless it's an image, audio, video, or font file. The checks diff build output such as
//! `.pyc` files as text (`argonauts_core::build_output`), so its bytes are scanned instead.

use std::collections::BTreeSet;

use argonauts_core::policy::{HardLimit, Violation};
use argonauts_core::redact::find_secrets;

use super::diff::{FileChange, FileStatus, LineKind};
use super::violation;

/// Key stores a text scan can't read, found by name instead.
const KEY_STORE_EXTENSIONS: &[&str] = &[".p12", ".pfx", ".jks", ".keystore"];
const PRIVATE_KEY_FILES: &[&str] = &["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"];
/// Binary formats that don't hold credentials. Archives, databases, and certificates aren't
/// here, since any of them can.
const MEDIA_EXTENSIONS: &[&str] = &[
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".avif", ".bmp", ".ico", ".mp3", ".wav", ".ogg",
    ".flac", ".mp4", ".webm", ".mov", ".woff", ".woff2", ".ttf", ".otf",
];

pub fn check(changes: &[FileChange]) -> Vec<Violation> {
    let mut found: Vec<(String, BTreeSet<&'static str>)> = Vec::new();
    let mut unscanned: Vec<String> = Vec::new();
    for change in changes {
        let kinds = secrets_in(change);
        if !kinds.is_empty() {
            found.push((change.path.clone(), kinds));
        } else if unscannable(change) {
            unscanned.push(change.path.clone());
        }
    }
    let mut violations = Vec::new();
    if !found.is_empty() {
        violations.push(secrets_found(found));
    }
    if !unscanned.is_empty() {
        violations.push(violation(
            HardLimit::Secret,
            format!(
                "adds or changes binary files the secret scan can't read, so the task stops: {}",
                unscanned.join(", ")
            ),
            unscanned,
        ));
    }
    violations
}

fn secrets_found(found: Vec<(String, BTreeSet<&'static str>)>) -> Violation {
    // Names the kind of secret, never the text, so the violation is safe to journal and show.
    let listed: Vec<String> = found
        .iter()
        .map(|(path, kinds)| {
            let kinds: Vec<&str> = kinds.iter().copied().collect();
            format!("{path} ({})", kinds.join(", "))
        })
        .collect();
    violation(
        HardLimit::Secret,
        format!("adds secrets, so the task stops: {}", listed.join("; ")),
        found.into_iter().map(|(path, _)| path).collect(),
    )
}

fn unscannable(change: &FileChange) -> bool {
    let name = change.file_name().to_ascii_lowercase();
    change.binary
        && change.status != FileStatus::Deleted
        && !MEDIA_EXTENSIONS.iter().any(|ext| name.ends_with(ext))
}

fn secrets_in(change: &FileChange) -> BTreeSet<&'static str> {
    let mut kinds = BTreeSet::new();
    if change.status == FileStatus::Deleted {
        return kinds;
    }
    let name = change.file_name().to_ascii_lowercase();
    if KEY_STORE_EXTENSIONS.iter().any(|ext| name.ends_with(ext))
        || PRIVATE_KEY_FILES.contains(&name.as_str())
    {
        kinds.insert("key file");
    }
    for hunk in &change.hunks {
        // Joined per hunk so a private key's header and body read as one block.
        let added: Vec<&str> = hunk
            .iter()
            .filter(|line| line.kind == LineKind::Added)
            .map(|line| line.text.as_str())
            .collect();
        if added.is_empty() {
            continue;
        }
        kinds.extend(
            find_secrets(&added.join("\n"))
                .into_iter()
                .map(|hit| hit.kind),
        );
    }
    kinds
}
