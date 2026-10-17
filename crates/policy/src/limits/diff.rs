//! Reads a unified diff (`git diff` output) into one change per file. A line it can't place is
//! an error, so a malformed diff fails the checks instead of hiding a change.
//!
//! It also tells where a changed line sits in a TOML, INI, JSON, or `go.mod` file, as far as
//! the hunk's own lines show it, because the dependency and test-config checks see only hunks.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Deleted,
    Modified,
    Renamed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub kind: LineKind,
    /// Without the leading marker.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// The new path, or the old one for a deleted file.
    pub path: String,
    /// Set for a rename or a copy, or when a plain diff names two different files.
    pub old_path: Option<String>,
    pub status: FileStatus,
    /// The diff doesn't show its content.
    pub binary: bool,
    /// Each hunk's lines, in order.
    pub hunks: Vec<Vec<Line>>,
}

impl FileChange {
    pub fn added(&self) -> impl Iterator<Item = &str> {
        self.lines_of(LineKind::Added)
    }

    pub fn removed(&self) -> impl Iterator<Item = &str> {
        self.lines_of(LineKind::Removed)
    }

    /// The path and, for a rename, the old one too: moving a file away changes both places.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.path.as_str()).chain(self.old_path.as_deref())
    }

    pub fn file_name(&self) -> &str {
        file_name(&self.path)
    }

    fn lines_of(&self, kind: LineKind) -> impl Iterator<Item = &str> {
        self.hunks
            .iter()
            .flatten()
            .filter(move |line| line.kind == kind)
            .map(|line| line.text.as_str())
    }
}

pub fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Errors name the line number only, since a line could hold a secret.
pub fn parse(unified_diff: &str) -> Result<Vec<FileChange>, String> {
    let mut parser = Parser::default();
    for (index, line) in unified_diff.lines().enumerate() {
        parser
            .line(line)
            .map_err(|err| format!("line {}: {err}", index + 1))?;
    }
    parser.finish()
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum Side {
    #[default]
    Unknown,
    DevNull,
    Path(String),
}

#[derive(Default)]
struct Pending {
    old: Side,
    new: Side,
    new_file: bool,
    deleted_file: bool,
    renamed: bool,
    copied: bool,
    binary: bool,
    /// Inside a `GIT binary patch` body, which runs until the next file.
    binary_patch: bool,
    saw_old: bool,
    saw_new: bool,
    hunks: Vec<Vec<Line>>,
}

#[derive(Default)]
struct Parser {
    files: Vec<FileChange>,
    current: Option<Pending>,
    old_left: u64,
    new_left: u64,
}

impl Parser {
    fn line(&mut self, line: &str) -> Result<(), String> {
        if self.old_left > 0 || self.new_left > 0 {
            return self.hunk_line(line);
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            self.finish_file()?;
            let (old, new) = git_header_paths(rest)?;
            self.current = Some(Pending {
                old,
                new,
                ..Pending::default()
            });
            return Ok(());
        }
        if line.starts_with("diff ") {
            return Err("only plain two-file diffs can be read".to_owned());
        }
        if self.current.as_ref().is_some_and(|file| file.binary_patch) {
            return Ok(());
        }
        if let Some(rest) = line.strip_prefix("--- ") {
            let starts_file = self
                .current
                .as_ref()
                .is_none_or(|file| file.saw_old || !file.hunks.is_empty());
            if starts_file {
                self.finish_file()?;
                self.current = Some(Pending::default());
            }
            let file = self.pending()?;
            file.old = diff_path(rest, "a/")?;
            file.saw_old = true;
            return Ok(());
        }
        if let Some(rest) = line.strip_prefix("+++ ") {
            let file = self.pending()?;
            if !file.saw_old || file.saw_new {
                return Err("a `+++` line without its `---` line".to_owned());
            }
            file.new = diff_path(rest, "b/")?;
            file.saw_new = true;
            return Ok(());
        }
        if let Some(rest) = line.strip_prefix("@@ ") {
            let (old, new) = hunk_counts(rest)?;
            let file = self.pending()?;
            if !(file.saw_old && file.saw_new) {
                return Err("a hunk before its file's `---` and `+++` lines".to_owned());
            }
            file.hunks.push(Vec::new());
            self.old_left = old;
            self.new_left = new;
            return Ok(());
        }
        if line.starts_with('\\') {
            // "\ No newline at end of file" after a hunk's last line.
            return match &self.current {
                Some(file) if !file.hunks.is_empty() => Ok(()),
                _ => Err("a no-newline marker outside a hunk".to_owned()),
            };
        }
        if line.trim().is_empty() {
            return Ok(());
        }
        self.extended_header(line)
    }

    /// Git's lines between `diff --git` and the hunks.
    fn extended_header(&mut self, line: &str) -> Result<(), String> {
        let file = self.pending()?;
        if file.saw_old {
            return Err("a file header after the file's hunks began".to_owned());
        }
        if let Some(rest) = line.strip_prefix("rename from ") {
            file.old = Side::Path(header_path(rest)?);
            file.renamed = true;
        } else if let Some(rest) = line.strip_prefix("rename to ") {
            file.new = Side::Path(header_path(rest)?);
            file.renamed = true;
        } else if let Some(rest) = line.strip_prefix("copy from ") {
            file.old = Side::Path(header_path(rest)?);
            file.copied = true;
        } else if let Some(rest) = line.strip_prefix("copy to ") {
            file.new = Side::Path(header_path(rest)?);
            file.copied = true;
        } else if line.starts_with("new file mode ") {
            file.new_file = true;
        } else if line.starts_with("deleted file mode ") {
            file.deleted_file = true;
        } else if line.starts_with("Binary files ") {
            file.binary = true;
        } else if line == "GIT binary patch" {
            file.binary = true;
            file.binary_patch = true;
        } else if ![
            "old mode ",
            "new mode ",
            "index ",
            "similarity index ",
            "dissimilarity index ",
            // `git show --remerge-diff` names the conflict a merge settled, as in
            // `remerge CONFLICT (content): Merge conflict in src/lib.rs`.
            "remerge ",
        ]
        .iter()
        .any(|header| line.starts_with(header))
        {
            return Err("a line that isn't part of a unified diff".to_owned());
        }
        Ok(())
    }

    fn hunk_line(&mut self, line: &str) -> Result<(), String> {
        let (kind, text) = match line.as_bytes().first() {
            Some(b' ') => (LineKind::Context, &line[1..]),
            Some(b'+') => (LineKind::Added, &line[1..]),
            Some(b'-') => (LineKind::Removed, &line[1..]),
            Some(b'\\') => return Ok(()),
            // Some tools strip the space from an empty context line.
            None => (LineKind::Context, ""),
            Some(_) => return Err("a hunk ended before its header's line count".to_owned()),
        };
        let fits = match kind {
            LineKind::Context => self.old_left > 0 && self.new_left > 0,
            LineKind::Added => self.new_left > 0,
            LineKind::Removed => self.old_left > 0,
        };
        if !fits {
            return Err("a hunk has more lines than its header says".to_owned());
        }
        if kind != LineKind::Added {
            self.old_left -= 1;
        }
        if kind != LineKind::Removed {
            self.new_left -= 1;
        }
        let hunk = self
            .current
            .as_mut()
            .and_then(|file| file.hunks.last_mut())
            .ok_or_else(|| "a hunk line outside a file".to_owned())?;
        hunk.push(Line {
            kind,
            text: text.to_owned(),
        });
        Ok(())
    }

    fn pending(&mut self) -> Result<&mut Pending, String> {
        self.current
            .as_mut()
            .ok_or_else(|| "a line before the first file header".to_owned())
    }

    fn finish_file(&mut self) -> Result<(), String> {
        if let Some(file) = self.current.take() {
            self.files.push(file.into_change()?);
        }
        Ok(())
    }

    fn finish(mut self) -> Result<Vec<FileChange>, String> {
        if self.old_left > 0 || self.new_left > 0 {
            return Err("the diff ends inside a hunk".to_owned());
        }
        self.finish_file()?;
        Ok(self.files)
    }
}

impl Pending {
    fn into_change(self) -> Result<FileChange, String> {
        let path = |side: Side| match side {
            Side::Path(path) => Some(path),
            Side::Unknown | Side::DevNull => None,
        };
        let deleted = self.deleted_file || self.new == Side::DevNull;
        let added = self.new_file || self.old == Side::DevNull;
        let (status, path, old_path) = match (deleted, added) {
            (true, true) => return Err("a file both added and deleted".to_owned()),
            (true, false) => (FileStatus::Deleted, path(self.old), None),
            (false, true) => (FileStatus::Added, path(self.new), None),
            (false, false) => {
                let old = path(self.old);
                let new = path(self.new);
                let status = if self.renamed {
                    FileStatus::Renamed
                } else if self.copied {
                    FileStatus::Added
                } else {
                    FileStatus::Modified
                };
                let old = old.filter(|old| Some(old) != new.as_ref());
                (status, new, old)
            }
        };
        let path = path.ok_or_else(|| "a file without a path".to_owned())?;
        Ok(FileChange {
            path,
            old_path,
            status,
            binary: self.binary,
            hunks: self.hunks,
        })
    }
}

/// `@@ -12,3 +12,4 @@ context` gives (3, 4); a missing count is 1.
fn hunk_counts(rest: &str) -> Result<(u64, u64), String> {
    let bad = || "a hunk header that can't be read".to_owned();
    let (ranges, _) = rest.split_once(" @@").ok_or_else(bad)?;
    let (old, new) = ranges.split_once(' ').ok_or_else(bad)?;
    let count = |range: &str, sign: char| -> Result<u64, String> {
        let range = range.strip_prefix(sign).ok_or_else(bad)?;
        let (start, count) = range.split_once(',').unwrap_or((range, "1"));
        start.parse::<u64>().map_err(|_| bad())?;
        count.parse::<u64>().map_err(|_| bad())
    };
    let counts = (count(old, '-')?, count(new, '+')?);
    if counts == (0, 0) {
        return Err(bad());
    }
    Ok(counts)
}

/// `a/x b/y`, either side possibly quoted. Unquoted names with spaces are split where both
/// halves name the same file, which is the only case git leaves ambiguous.
fn git_header_paths(rest: &str) -> Result<(Side, Side), String> {
    let bad = || "a `diff --git` line that can't be read".to_owned();
    let (old, new) = if rest.starts_with('"') {
        let (_, after) = unquote(rest)?;
        let old = &rest[..rest.len() - after.len()];
        (old, after.strip_prefix(' ').ok_or_else(bad)?)
    } else if let Some(at) = rest.find(" \"") {
        (&rest[..at], &rest[at + 1..])
    } else {
        let half = rest.len().saturating_sub(1) / 2;
        match (rest.get(..half), rest.get(half..)) {
            (Some(old), Some(new))
                if new.starts_with(' ') && old.len() > 2 && old.get(2..) == new.get(3..) =>
            {
                (old, &new[1..])
            }
            _ => {
                let at = rest.find(" b/").ok_or_else(bad)?;
                (&rest[..at], &rest[at + 1..])
            }
        }
    };
    Ok((diff_path(old, "a/")?, diff_path(new, "b/")?))
}

/// A `---`/`+++` path: quoted or not, with any trailing tab (a timestamp) dropped.
fn diff_path(raw: &str, prefix: &str) -> Result<Side, String> {
    let raw = if raw.starts_with('"') {
        unquote(raw)?.0
    } else {
        raw.split('\t').next().unwrap_or(raw).to_owned()
    };
    if raw == "/dev/null" {
        return Ok(Side::DevNull);
    }
    Ok(Side::Path(clean_path(&raw, prefix)?))
}

/// A `rename from` or `copy to` path, which git quotes like any other.
fn header_path(raw: &str) -> Result<String, String> {
    if raw.starts_with('"') {
        clean_path(&unquote(raw)?.0, "")
    } else {
        clean_path(raw, "")
    }
}

/// Refuses paths that leave the worktree, since a check matching `.argonauts/**` must never
/// be walked around with `x/../.argonauts`.
fn clean_path(raw: &str, prefix: &str) -> Result<String, String> {
    let path = raw.strip_prefix(prefix).unwrap_or(raw);
    let path = path.trim_start_matches("./");
    let leaves = path.is_empty()
        || path.starts_with('/')
        || path.contains('\0')
        || path.split('/').any(|part| part == "..");
    if leaves {
        return Err("a path outside the worktree".to_owned());
    }
    Ok(path.to_owned())
}

/// Git's C-style quoting: returns the name and whatever follows the closing quote.
fn unquote(raw: &str) -> Result<(String, &str), String> {
    let bad = || "a quoted path that can't be read".to_owned();
    let body = raw.strip_prefix('"').ok_or_else(bad)?;
    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'"' => {
                let name = String::from_utf8(out).map_err(|_| bad())?;
                return Ok((name, &body[at + 1..]));
            }
            b'\\' => {
                let escape = *bytes.get(at + 1).ok_or_else(bad)?;
                at += 2;
                out.push(match escape {
                    b'n' => b'\n',
                    b't' => b'\t',
                    b'r' => b'\r',
                    b'a' => 0x07,
                    b'b' => 0x08,
                    b'f' => 0x0c,
                    b'v' => 0x0b,
                    b'0'..=b'7' => {
                        let digits = body.get(at - 1..at + 2).ok_or_else(bad)?;
                        at += 2;
                        u8::from_str_radix(digits, 8).map_err(|_| bad())?
                    }
                    other => other,
                });
            }
            byte => {
                out.push(byte);
                at += 1;
            }
        }
    }
    Err(bad())
}

/// Where a line sits in a TOML or INI file: the table, and the array it's inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TomlPlace<'a> {
    /// `None` when the hunk starts inside a table whose header it doesn't show.
    pub table: Option<&'a str>,
    pub array: Array<'a>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Array<'a> {
    /// The hunk may start inside an array.
    Unknown,
    Outside,
    Inside(&'a str),
}

/// Each line of a hunk with its place. Added lines follow the new file, removed lines the old.
pub fn toml_places(hunk: &[Line]) -> Vec<(TomlPlace<'_>, &Line)> {
    let start = TomlPlace {
        table: None,
        array: Array::Unknown,
    };
    let (mut old, mut new) = (start, start);
    let mut placed = Vec::with_capacity(hunk.len());
    for line in hunk {
        let side = if line.kind == LineKind::Removed {
            old
        } else {
            new
        };
        let after = next_toml_place(side, &line.text);
        match line.kind {
            LineKind::Context => (old, new) = (after, after),
            LineKind::Added => new = after,
            LineKind::Removed => old = after,
        }
        // A header belongs to its own table; any other line to the place it opens in.
        let own = if toml_table_header(&line.text).is_some() {
            after
        } else {
            side
        };
        placed.push((own, line));
    }
    placed
}

fn next_toml_place<'a>(place: TomlPlace<'a>, text: &'a str) -> TomlPlace<'a> {
    if let Some(table) = toml_table_header(text) {
        return TomlPlace {
            table: Some(table),
            array: Array::Outside,
        };
    }
    if let Some((key, value)) = toml_entry(text) {
        if value.starts_with('[') && !value.ends_with(']') {
            return TomlPlace {
                array: Array::Inside(key),
                ..place
            };
        }
        return place;
    }
    if strip_comment(text).trim().starts_with(']') {
        return TomlPlace {
            array: Array::Outside,
            ..place
        };
    }
    place
}

/// `[table]` or `[[array.of.tables]]` (also INI's `[tool:pytest]`), as `table`.
pub fn toml_table_header(text: &str) -> Option<&str> {
    let code = strip_comment(text).trim();
    let inner = code
        .strip_prefix("[[")
        .and_then(|rest| rest.strip_suffix("]]"))
        .or_else(|| {
            code.strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
        })?;
    let inner = inner.trim();
    // A nested array value like `["a", "b"]` starts with a value and has a comma; a table name
    // starts with a key.
    let starts_with_key =
        inner.starts_with(|c: char| c.is_ascii_alphanumeric() || "_-".contains(c));
    (!inner.is_empty() && (starts_with_key || !inner.contains(','))).then_some(inner)
}

/// The key of a `key = value` line, unquoted, or `None` for anything else, such as an array
/// item like `"requests==2.0",`.
pub fn toml_key(text: &str) -> Option<&str> {
    toml_entry(text).map(|(key, _)| key)
}

/// The value of a `key = value` line, trimmed.
pub fn toml_value(text: &str) -> Option<&str> {
    toml_entry(text).map(|(_, value)| value)
}

fn toml_entry(text: &str) -> Option<(&str, &str)> {
    let code = strip_comment(text).trim();
    let key_end = match code.chars().next()? {
        '[' => return None,
        quote @ ('"' | '\'') => {
            let close = code[1..].find(quote)? + 2;
            let after = code[close..].trim_start();
            if !after.starts_with(['=', '.']) {
                return None;
            }
            close + code[close..].find('=')?
        }
        _ => code.find('=')?,
    };
    let key = unquote_key(code[..key_end].trim());
    let value = code[key_end + 1..].trim();
    (!key.is_empty()).then_some((key, value))
}

fn unquote_key(key: &str) -> &str {
    key.trim_matches(|c| c == '"' || c == '\'')
}

/// Drops a `#` comment that isn't inside a string.
fn strip_comment(text: &str) -> &str {
    let mut quote = None;
    for (at, c) in text.char_indices() {
        match (quote, c) {
            (None, '#') => return &text[..at],
            (None, '"' | '\'') => quote = Some(c),
            (Some(open), _) if c == open => quote = None,
            _ => {}
        }
    }
    text
}

/// The keys of the JSON objects a line is in, outermost first, as far as the hunk shows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonPlace<'a>(Vec<Option<&'a str>>);

impl<'a> JsonPlace<'a> {
    /// `Some("")` for the top level, `None` when the hunk starts inside an object it doesn't
    /// open.
    pub fn innermost(&self) -> Option<&'a str> {
        self.0.last().copied().flatten()
    }

    pub fn within(&self, keys: &[&str]) -> bool {
        self.0.iter().flatten().any(|key| keys.contains(key))
    }
}

/// Each line of a hunk with the JSON objects it's in.
pub fn json_objects(hunk: &[Line]) -> Vec<(JsonPlace<'_>, &Line)> {
    let mut old: Vec<Option<&str>> = vec![None];
    let mut new: Vec<Option<&str>> = vec![None];
    let mut placed = Vec::with_capacity(hunk.len());
    for line in hunk {
        let side = if line.kind == LineKind::Removed {
            &old
        } else {
            &new
        };
        placed.push((JsonPlace(side.clone()), line));
        let step = json_step(&line.text);
        match line.kind {
            LineKind::Context => {
                apply_json_step(&mut old, step);
                apply_json_step(&mut new, step);
            }
            LineKind::Added => apply_json_step(&mut new, step),
            LineKind::Removed => apply_json_step(&mut old, step),
        }
    }
    placed
}

#[derive(Clone, Copy)]
enum JsonStep<'a> {
    Open(&'a str),
    Close,
    Stay,
}

fn json_step(text: &str) -> JsonStep<'_> {
    let code = text.trim();
    if code == "{" {
        return JsonStep::Open("");
    }
    if code.starts_with('}') {
        return JsonStep::Close;
    }
    match json_entry(code) {
        Some((key, rest)) if rest.trim_end() == "{" => JsonStep::Open(key),
        _ => JsonStep::Stay,
    }
}

fn apply_json_step<'a>(stack: &mut Vec<Option<&'a str>>, step: JsonStep<'a>) {
    match step {
        JsonStep::Open(key) => stack.push(Some(key)),
        JsonStep::Close if stack.len() > 1 => {
            stack.pop();
        }
        // Leaving an object the hunk never opened lands somewhere it doesn't show.
        JsonStep::Close => *stack = vec![None],
        JsonStep::Stay => {}
    }
}

/// `"key": rest` as (`key`, `rest`).
pub fn json_entry(text: &str) -> Option<(&str, &str)> {
    let (key, rest) = json_string(text.trim())?;
    let rest = rest.trim_start().strip_prefix(':')?;
    Some((key, rest.trim()))
}

/// `"key": "value"` as (`key`, `value`).
pub fn json_string_entry(text: &str) -> Option<(&str, &str)> {
    let (key, rest) = json_entry(text)?;
    let (value, _) = json_string(rest)?;
    Some((key, value))
}

/// A leading JSON string (escapes kept as written) and what follows it.
fn json_string(text: &str) -> Option<(&str, &str)> {
    let body = text.strip_prefix('"')?;
    let mut escaped = false;
    for (at, c) in body.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '"' => return Some((&body[..at], &body[at + 1..])),
            _ => {}
        }
    }
    None
}

/// Where a line sits in `go.mod`: the directive block it's inside, if the hunk shows it.
pub fn go_blocks(hunk: &[Line]) -> Vec<(GoBlock<'_>, &Line)> {
    let (mut old, mut new) = (GoBlock::Unknown, GoBlock::Unknown);
    let mut placed = Vec::with_capacity(hunk.len());
    for line in hunk {
        let side = if line.kind == LineKind::Removed {
            old
        } else {
            new
        };
        placed.push((side, line));
        let code = line.text.split("//").next().unwrap_or("").trim();
        let after = if let Some(directive) = code.strip_suffix('(') {
            GoBlock::Inside(directive.trim())
        } else if code == ")" {
            GoBlock::Outside
        } else {
            side
        };
        match line.kind {
            LineKind::Context => (old, new) = (after, after),
            LineKind::Added => new = after,
            LineKind::Removed => old = after,
        }
    }
    placed
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoBlock<'a> {
    Unknown,
    Outside,
    /// `require`, `replace`, `exclude`, or `retract`.
    Inside(&'a str),
}
