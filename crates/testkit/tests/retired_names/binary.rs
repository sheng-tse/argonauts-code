//! What a file that isn't text keeps of the machine that made it: a file an OS writes into a
//! folder, with the names of what's in it, and a picture's text and Exif, where an editor
//! writes its user's name, the file's path or the camera's.

use super::report::Place;
use super::table::Private;

/// `read` gives a file's bytes. Only pictures are read.
pub fn places(private: &Private, paths: &[&str], read: impl Fn(&str) -> Vec<u8>) -> Vec<Place> {
    let mut places = Vec::new();
    for path in paths {
        let name = path.rsplit('/').next().unwrap_or(path);
        if private.machine_files.iter().any(|file| file == name) || name.starts_with("._") {
            places.push(Place::new(path, None, name, &private.machine_file_fix));
        }
        if name.ends_with(".png") {
            for chunk in png_text(&read(path), &private.picture_text_chunks) {
                places.push(Place::new(path, None, &chunk, &private.picture_text_fix));
            }
        }
    }
    places
}

/// The listed chunks a PNG holds, each named once.
fn png_text(bytes: &[u8], listed: &[String]) -> Vec<String> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    let mut found: Vec<String> = Vec::new();
    let Some(mut rest) = bytes.strip_prefix(SIGNATURE) else {
        return found;
    };
    // Each chunk is its length, its four-letter kind, its data and a checksum.
    while let (Some(length), Some(kind)) = (rest.get(..4), rest.get(4..8)) {
        let length = u32::from_be_bytes([length[0], length[1], length[2], length[3]]) as usize;
        let kind = String::from_utf8_lossy(kind).into_owned();
        if listed.contains(&kind) && !found.contains(&kind) {
            found.push(kind);
        }
        let Some(next) = length.checked_add(12).and_then(|end| rest.get(end..)) else {
            break;
        };
        rest = next;
    }
    found
}
