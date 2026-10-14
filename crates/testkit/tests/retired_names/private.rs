//! Real home folders and email addresses: none belongs in a tracked file, recordings
//! included. Sample text uses a neutral folder and a reserved mail domain.

use super::report::Place;
use super::table::{Private, Table};
use super::tree::File;

pub fn places(table: &Table, files: &[File]) -> Vec<Place> {
    let private = &table.private;
    let banned: Vec<(usize, u64)> = private
        .banned
        .iter()
        .map(|text| {
            let hash = u64::from_str_radix(text.hash.trim_start_matches("0x"), 16)
                .unwrap_or_else(|e| panic!("{}: {e}", text.what));
            (text.length, hash)
        })
        .collect();

    let mut places = Vec::new();
    for file in files {
        let Some(text) = file.text.as_deref() else {
            continue;
        };
        for (number, line) in text.lines().enumerate() {
            let mut found = Vec::new();
            homes(line, private, &mut found);
            mail(line, private, &mut found);
            banned_texts(line, &banned, private, &mut found);
            for (text, fix) in found {
                places.push(Place::new(&file.path, Some(number + 1), &text, fix));
            }
        }
    }
    places
}

/// A home folder that isn't one of the neutral names, spelled as a path or the way Claude
/// Code names a project's folder, with a dash for each slash.
fn homes<'t>(line: &str, private: &'t Private, found: &mut Vec<(String, &'t str)>) {
    for (root, ends_at_dash) in [
        ("/Users/", false),
        ("/home/", false),
        ("\\Users\\", false),
        ("-Users-", true),
    ] {
        for (at, _) in line.match_indices(root) {
            // Part of a longer path (`{dir}/home/skills`) is a folder that happens to be
            // called home, not anyone's.
            let inside_a_path = line[..at]
                .chars()
                .next_back()
                .is_some_and(|before| before.is_alphanumeric() || "_.~}/".contains(before));
            if inside_a_path {
                continue;
            }
            let rest = &line[at + root.len()..];
            let name: String = rest
                .chars()
                .take_while(|c| {
                    c.is_ascii_alphanumeric() || *c == '_' || (!ends_at_dash && "-.".contains(*c))
                })
                .collect();
            let is_a_name = name.chars().next().is_some_and(char::is_alphanumeric);
            if is_a_name && !private.neutral_homes.contains(&name) {
                found.push((format!("{root}{name}"), private.home_fix.as_str()));
            }
        }
    }
}

/// An address whose domain isn't a reserved or listed one. `name@1.2.3` is a package and
/// `@notes.txt` a file, so neither counts.
fn mail<'t>(line: &str, private: &'t Private, found: &mut Vec<(String, &'t str)>) {
    let local = |c: char| c.is_ascii_alphanumeric() || "._%+-".contains(c);
    let domain = |c: char| c.is_ascii_alphanumeric() || ".-".contains(c);
    for (at, _) in line.match_indices('@') {
        let before: String = line[..at].chars().rev().take_while(|c| local(*c)).collect();
        let before: String = before.chars().rev().collect();
        let after: String = line[at + 1..].chars().take_while(|c| domain(*c)).collect();
        let after = after.trim_end_matches(['.', '-']);
        let Some((_, last)) = after.rsplit_once('.') else {
            continue;
        };
        let is_address = before.chars().any(|c| c.is_ascii_alphabetic())
            && last.len() >= 2
            && last.chars().all(|c| c.is_ascii_alphabetic())
            && !private.file_endings.iter().any(|ending| ending == last);
        let lower = after.to_ascii_lowercase();
        let neutral = private
            .neutral_mail_endings
            .iter()
            .any(|ending| lower == *ending || lower.ends_with(&format!(".{ending}")));
        if is_address && !neutral {
            found.push((format!("{before}@{after}"), private.mail_fix.as_str()));
        }
    }
}

/// A banned text anywhere it stands as a word of its own, whatever its case.
fn banned_texts<'t>(
    line: &str,
    banned: &[(usize, u64)],
    private: &'t Private,
    found: &mut Vec<(String, &'t str)>,
) {
    let bytes = line.as_bytes();
    let starts_a_word = |at: usize| {
        bytes[at].is_ascii_alphanumeric() && (at == 0 || !bytes[at - 1].is_ascii_alphanumeric())
    };
    for at in (0..bytes.len()).filter(|at| starts_a_word(*at)) {
        for (which, (length, hash)) in banned.iter().enumerate() {
            let end = at + length;
            let ends_a_word =
                end == bytes.len() || (end < bytes.len() && !bytes[end].is_ascii_alphanumeric());
            if ends_a_word && hash_of(&bytes[at..end]) == *hash {
                let fix = private.banned[which].fix.as_str();
                found.push((String::from_utf8_lossy(&bytes[at..end]).into_owned(), fix));
            }
        }
    }
}

/// FNV-1a over the lowercased bytes. It only has to tell a banned text from other words.
pub fn hash_of(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte.to_ascii_lowercase())).wrapping_mul(0x0100_0000_01b3)
    })
}
