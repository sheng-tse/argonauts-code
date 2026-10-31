//! Every picture a screen is checked against gives the glyphs some fonts draw two cells wide the
//! room they need, so a screen drawn to match it does too.

use std::path::{Path, PathBuf};

use argonauts_testkit::pictures::wide_glyphs::crowded;

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crates folder")
        .to_path_buf()
}

/// The `.txt` pictures under `dir`, in the folders named golden, pictures or screens, and the
/// snapshots tests keep of drawn screens.
fn pictures(dir: &Path, inside: bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name == "target" || name == "fixtures" {
                continue;
            }
            let picture_folder = matches!(
                name.as_ref(),
                "golden" | "pictures" | "screens" | "snapshots"
            );
            pictures(&path, inside || picture_folder, out);
        } else if inside && (name.ends_with(".txt") || name.ends_with(".snap")) {
            out.push(path);
        }
    }
}

#[test]
fn no_picture_crowds_a_square_or_an_arrow() {
    let root = crates_dir();
    let mut found = Vec::new();
    pictures(&root, false, &mut found);
    assert!(found.len() > 50, "the pictures under {}", root.display());
    let mut crowding = Vec::new();
    for path in found {
        let shown = path.strip_prefix(&root).unwrap_or(&path).to_string_lossy();
        let shown = shown.replace('\\', "/");
        let text = std::fs::read_to_string(&path).unwrap();
        // A snapshot's header names the test, not what it drew.
        let drawn = match text.strip_prefix("---\n") {
            Some(rest) => rest.split_once("\n---\n").map_or(rest, |(_, drawn)| drawn),
            None => &text,
        };
        for (row, line) in drawn.lines().enumerate() {
            for column in crowded(line) {
                crowding.push(format!("{shown}:{}:{} {line:?}", row + 1, column + 1));
            }
        }
    }
    assert!(crowding.is_empty(), "{}", crowding.join("\n"));
}
