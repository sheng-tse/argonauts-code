//! Owned paths as the task graph checks them: a file, a directory, or a glob, relative to the
//! repository root. The checks are conservative, since a glob covers everything under its fixed
//! directory as far as disjointness goes.

use globset::{GlobBuilder, GlobMatcher};

const GLOB_CHARS: &[char] = &['*', '?', '[', '{'];

/// Why `path` can't be owned, if it can't.
pub fn malformed(path: &str) -> Option<String> {
    if path.trim().is_empty() {
        return Some("an owned path is empty".to_owned());
    }
    if path.starts_with('/') || path.starts_with('~') || path.contains('\\') {
        return Some(format!("`{path}` isn't relative to the repository"));
    }
    if components(path).any(|part| part == "..") {
        return Some(format!("`{path}` leaves the repository"));
    }
    if fixed_part(path).is_empty() {
        return Some(format!(
            "`{path}` names the whole repository; own a directory or a file instead"
        ));
    }
    None
}

/// Whether two owned paths may name the same file.
pub fn overlap(a: &str, b: &str) -> bool {
    let (a, b) = (fixed_part(a), fixed_part(b));
    a.starts_with(&b[..]) || b.starts_with(&a[..])
}

/// Whether an owned path reaches into `area`, a path or glob such as `.argonauts/**`: it is in
/// the area, or its whole subtree is.
pub fn inside(owned: &str, area: &str) -> bool {
    let fixed = fixed_part(owned).join("/");
    let Some(matcher) = matcher(area) else {
        return overlap(owned, area);
    };
    let literal_area = !is_glob(area);
    (!is_glob(owned) && matcher.is_match(owned))
        || (!fixed.is_empty()
            && (matcher.is_match(&fixed) || matcher.is_match(format!("{fixed}/x"))))
        || (literal_area && fixed_part(owned).starts_with(&fixed_part(area)[..]))
}

/// Whether an owned path holds `area` inside it, as `src/**` holds `src/secrets/**`. An area
/// with no fixed directory, such as `**/*.lock`, is held by nothing.
pub fn contains(owned: &str, area: &str) -> bool {
    let (owned_fixed, area_fixed) = (fixed_part(owned), fixed_part(area));
    if owned_fixed.is_empty() || !area_fixed.starts_with(&owned_fixed[..]) {
        return false;
    }
    if !is_glob(owned) {
        return true;
    }
    let area_dir = area_fixed.join("/");
    matcher(owned)
        .is_some_and(|glob| glob.is_match(&area_dir) || glob.is_match(format!("{area_dir}/x")))
}

fn is_glob(path: &str) -> bool {
    path.contains(GLOB_CHARS)
}

fn components(path: &str) -> impl Iterator<Item = &str> {
    path.split('/')
        .filter(|part| !part.is_empty() && *part != ".")
}

/// The components before the first one holding a glob character.
fn fixed_part(path: &str) -> Vec<&str> {
    components(path)
        .take_while(|part| !part.contains(GLOB_CHARS))
        .collect()
}

/// `*` stays inside one component and `**` spans them, as in `.gitignore`.
fn matcher(glob: &str) -> Option<GlobMatcher> {
    GlobBuilder::new(glob)
        .literal_separator(true)
        .build()
        .ok()
        .map(|glob| glob.compile_matcher())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_is_by_directory() {
        assert!(overlap("src/store/**", "src/store/redis.rs"));
        assert!(overlap("src/store", "src/store/redis.rs"));
        assert!(overlap("src/a.rs", "./src/a.rs"));
        assert!(!overlap("src/store/**", "src/store.rs"));
        assert!(!overlap("src/a.rs", "src/b.rs"));
        assert!(!overlap("src/api/**", "tests/acceptance/task-002/**"));
    }

    #[test]
    fn inside_an_area() {
        assert!(inside(".argonauts/notes/a.md", ".argonauts/**"));
        assert!(inside(".argonauts/**", ".argonauts/**"));
        assert!(inside("Cargo.lock", "**/*.lock"));
        assert!(inside("web/package-lock.json", "**/package-lock.json"));
        assert!(inside("migrations/001.sql", "migrations"));
        assert!(inside("src/secrets/**", "src/secrets/**"));
        assert!(!inside("src/**", "**/*.lock"));
        assert!(!inside("src/app.rs", ".argonauts/**"));
        assert!(!inside("src/store/**", "src/*.rs"));
    }

    #[test]
    fn a_directory_holding_an_area() {
        assert!(contains("src", "src/secrets/**"));
        assert!(contains("src/**", "src/secrets/**"));
        assert!(contains(".argonauts", ".argonauts/**"));
        assert!(!contains("src/*.rs", "src/secrets/**"));
        assert!(!contains("src/**", "**/*.lock"));
        assert!(!contains("src/app", "src/secrets/**"));
    }

    #[test]
    fn malformed_paths() {
        assert!(malformed(".").is_some());
        assert!(malformed("./").is_some());
        assert!(malformed("/etc/passwd").is_some());
        assert!(malformed("../other/x").is_some());
        assert!(malformed("**/*.rs").is_some());
        assert!(malformed("").is_some());
        assert!(malformed("src/**").is_none());
        assert!(malformed("README.md").is_none());
    }
}
