// Included by each CLI adapter's build script. A recording's `.meta.json` names the version
// it was made with and the versions it also matches, so the tested list follows the
// recordings and nothing else has to be kept in step with them.

use std::collections::BTreeSet;
use std::path::Path;
use std::{env, fs};

/// Writes `tested_versions.rs`, a slice of every version the recordings stand for, oldest
/// first. A file written by hand (`"synthetic": true`) stands for no version.
fn write_tested_versions() {
    let fixtures = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("fixtures");
    println!("cargo:rerun-if-changed={}", fixtures.display());

    let mut versions = BTreeSet::new();
    for entry in fs::read_dir(&fixtures).unwrap() {
        let path = entry.unwrap().path();
        let is_meta = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".meta.json"));
        if !is_meta {
            continue;
        }
        let meta: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap())
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        if meta["synthetic"] == true {
            continue;
        }
        let recorded_with = meta["cli_version"].as_str();
        let also_matches = meta["also_matches"].as_array().into_iter().flatten();
        for version in recorded_with
            .into_iter()
            .chain(also_matches.filter_map(|v| v.as_str()))
        {
            versions.insert((numbers(version), version.to_owned()));
        }
    }

    let listed: Vec<String> = versions
        .into_iter()
        .map(|(_, version)| format!("{version:?}"))
        .collect();
    let out = Path::new(&env::var("OUT_DIR").unwrap()).join("tested_versions.rs");
    fs::write(out, format!("&[{}]", listed.join(", "))).unwrap();
}

/// A version's numbers, so 1.2.13 sorts after 1.2.9.
fn numbers(version: &str) -> Vec<u64> {
    version
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|part| part.parse().ok())
        .collect()
}
