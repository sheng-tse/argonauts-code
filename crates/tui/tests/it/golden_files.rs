//! Every file kept in `golden/` is one a test of the screen names, by its own name or, for a
//! picture kept in parts, as its name followed by a part the test fills in. A numbered copy a
//! recording left beside its picture is named by none, so it fails here rather than stay in
//! the folder unchecked.

use std::path::{Path, PathBuf};

fn files(folder: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files(&path, found);
        } else {
            found.push(path);
        }
    }
}

#[test]
fn every_kept_picture_is_one_a_test_names() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    for folder in ["src", "tests"] {
        files(&root.join(folder), &mut sources);
    }
    let text: String = sources
        .iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .collect();
    let mut kept = Vec::new();
    files(&root.join("tests/golden"), &mut kept);
    let unnamed: Vec<String> = kept
        .iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?;
            let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
            let named = text.contains(stem)
                || stem
                    .split_once('.')
                    .is_some_and(|(first, _)| text.contains(&format!("{first}.{{")));
            (!named).then(|| {
                path.strip_prefix(root)
                    .unwrap_or(path)
                    .display()
                    .to_string()
            })
        })
        .collect();
    assert!(unnamed.is_empty(), "no test names {unnamed:#?}");
}
