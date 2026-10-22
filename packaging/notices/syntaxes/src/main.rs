//! Prints, as Markdown, the license of every syntax definition two-face bundles whose license asks to be reproduced.
//! The program uses two-face's syntaxes only, never its themes.

fn main() {
    let listing = two_face::acknowledgement::listing();
    let mut markdown = String::new();
    for license in listing.for_syntaxes() {
        if license.needs_acknowledgement() {
            license.write_md(&mut markdown);
        }
    }
    print!("{markdown}");
}
