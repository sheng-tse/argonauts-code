//! Every picture a screen is checked against gives the glyphs some fonts draw two cells wide the
//! room they need, so a screen drawn to match it does too.

use std::path::{Path, PathBuf};

use argonauts_testkit::wide_glyphs::crowded;

/// Pictures drawn before the arrow rule, or that draw a line no change has touched since, such
/// as the overview's key line under the Context tab, each fixed when that screen is next
/// changed. One that is fixed may stay listed until then; the check only refuses a picture
/// missing here.
const STILL_CROWDED: [&str; 11] = [
    "tui/golden/resumed_after.txt",
    "tui/tests/golden/nested_orchestrator_folded.txt",
    "tui/tests/golden/no_git_fixes.txt",
    "tui/tests/golden/old_runs.txt",
    "tui/src/bridge/transcript/pictures/question_after.txt",
    "tui/src/overview/pictures/by_cli_after.txt",
    "tui/src/overview/pictures/context_after.txt",
    "tui/src/overview/pictures/context_near_full.txt",
    "tui/src/overview/pictures/context_qwen_after.txt",
    "tui/src/overview/pictures/context_tab.txt",
    "tui/src/overview/stats/screens/by_cli_after.txt",
];

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crates folder")
        .to_path_buf()
}

/// The `.txt` pictures under `dir`: the folders named golden, pictures or screens.
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
            let picture_folder = matches!(name.as_ref(), "golden" | "pictures" | "screens");
            pictures(&path, inside || picture_folder, out);
        } else if inside && name.ends_with(".txt") {
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
        if STILL_CROWDED.contains(&shown.as_str()) {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (row, line) in text.lines().enumerate() {
            for column in crowded(line) {
                crowding.push(format!("{shown}:{}:{} {line:?}", row + 1, column + 1));
            }
        }
    }
    assert!(crowding.is_empty(), "{}", crowding.join("\n"));
}
