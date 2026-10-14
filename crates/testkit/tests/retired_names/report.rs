//! What a failing run prints: every place at once, each with its fix.

use std::io::IsTerminal;

use super::table::TABLE;

/// One place a retired name is left.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Place {
    /// What was found, as written in the file.
    pub found: String,
    pub path: String,
    /// `None` when the name of the file or of a folder above it holds the word.
    pub line: Option<usize>,
    /// What to write in its place.
    pub fix: String,
}

impl Place {
    pub fn new(path: &str, line: Option<usize>, found: &str, fix: &str) -> Self {
        Self {
            found: found.to_owned(),
            path: path.to_owned(),
            line,
            fix: fix.to_owned(),
        }
    }

    fn at(&self) -> String {
        match self.line {
            Some(line) => format!("{}:{line}", self.path),
            None => self.path.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    Plain,
    Red,
    Bold,
    Dim,
    Green,
}

impl Look {
    fn code(self) -> &'static str {
        match self {
            Look::Plain => "",
            Look::Red => "\x1b[31m",
            Look::Bold => "\x1b[1m",
            Look::Dim => "\x1b[2m",
            Look::Green => "\x1b[32m",
        }
    }
}

pub type Line = Vec<(Look, String)>;

/// The report's lines, each a run of spans. Places come sorted by the word found, so every
/// use of one retired name reads together, then by file and line.
pub fn lines(title: &str, places: &[Place]) -> Vec<Line> {
    let mut places = places.to_vec();
    places.sort();
    places.dedup();
    let count = match places.len() {
        1 => "1 place".to_owned(),
        many => format!("{many} places"),
    };
    let width = places.iter().map(|place| place.at().chars().count()).max();
    let width = width.unwrap_or(0) + 3;

    let mut lines = vec![vec![
        (Look::Red, "✘ ".to_owned()),
        (Look::Bold, format!("{title} · {count}")),
    ]];
    for place in &places {
        lines.push(vec![
            (Look::Dim, format!("  {:<width$}", place.at())),
            (Look::Plain, format!("{:?}", place.found)),
            (Look::Green, format!(" → {}", place.fix)),
        ]);
    }
    lines.push(Vec::new());
    for note in [
        format!("  one table holds every retired name and its replacement ({TABLE});"),
        "  words are matched whole, so seatbelt or discard never trip it; CLI recordings are checked by name only".to_owned(),
    ] {
        lines.push(vec![(Look::Dim, note)]);
    }
    lines
}

pub fn plain(lines: &[Line]) -> String {
    let rows: Vec<String> = lines
        .iter()
        .map(|line| line.iter().map(|(_, text)| text.as_str()).collect())
        .collect();
    rows.join("\n")
}

fn colored(lines: &[Line]) -> String {
    let rows: Vec<String> = lines
        .iter()
        .map(|line| {
            line.iter()
                .map(|(look, text)| match look {
                    Look::Plain => text.clone(),
                    look => format!("{}{text}\x1b[0m", look.code()),
                })
                .collect()
        })
        .collect();
    rows.join("\n")
}

/// Fails with the report when any place is left. Colors only reach a terminal.
pub fn fail_on(title: &str, places: &[Place]) {
    if places.is_empty() {
        return;
    }
    let lines = lines(title, places);
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let report = if std::io::stderr().is_terminal() && !no_color {
        colored(&lines)
    } else {
        plain(&lines)
    };
    panic!("\n\n{report}\n\n");
}
