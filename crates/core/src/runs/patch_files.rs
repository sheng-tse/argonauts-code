//! The files a patch names, in the form Codex's and OpenCode's patch tools take:
//! `*** Add File: path`, `*** Update File: path`, `*** Delete File: path`, and
//! `*** Move to: path` for a file an update moves.

/// Each file the patch changes, with how: `add`, `update` or `delete`, as the live streams say
/// it. A moved file counts at its new path as added.
pub fn patched_files(patch: &str) -> Vec<(String, &'static str)> {
    const MARKERS: [(&str, &str); 4] = [
        ("*** Add File: ", "add"),
        ("*** Update File: ", "update"),
        ("*** Delete File: ", "delete"),
        ("*** Move to: ", "add"),
    ];
    patch
        .lines()
        .filter_map(|line| {
            MARKERS.iter().find_map(|(marker, kind)| {
                let path = line.strip_prefix(marker)?.trim();
                (!path.is_empty()).then(|| (path.to_owned(), *kind))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_file_a_patch_names_is_found_with_how_it_changes() {
        let patch = "*** Begin Patch\n*** Update File: slug.py\n@@ def slugify\n-    return s\n\
                     +    return s.lower()\n*** Add File: tests/test_slug.py\n+import slug\n\
                     *** Delete File: old.py\n*** Update File: a.py\n*** Move to: b.py\n\
                     *** End Patch\n";
        assert_eq!(
            patched_files(patch),
            [
                ("slug.py".to_owned(), "update"),
                ("tests/test_slug.py".to_owned(), "add"),
                ("old.py".to_owned(), "delete"),
                ("a.py".to_owned(), "update"),
                ("b.py".to_owned(), "add"),
            ]
        );
        assert!(patched_files("no patch here").is_empty());
    }
}
