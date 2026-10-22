//! Builds the table of the crate's `templates/` folder that `template!` reads, so a file added
//! there is built in with no list to keep in step.

use std::path::{Path, PathBuf};
use std::{env, fs, io};

fn main() -> io::Result<()> {
    let manifest_dir = env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it");
    let root = Path::new(&manifest_dir).join("templates");
    // Cargo scans a folder named here for a change to any file under it.
    println!("cargo:rerun-if-changed={}", root.display());

    let mut files = Vec::new();
    collect(&root, &mut files)?;
    files.sort();

    let mut table = String::from("&[\n");
    for path in &files {
        let under = path
            .strip_prefix(&root)
            .expect("collected under the root")
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let full = path.display().to_string();
        table.push_str(&format!("    ({under:?}, include_str!({full:?})),\n"));
    }
    table.push(']');

    let out_dir = env::var_os("OUT_DIR").expect("cargo sets it");
    fs::write(Path::new(&out_dir).join("templates.rs"), table)
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}
