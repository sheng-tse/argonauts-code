//! Syntax colors for code blocks, with bat's grammars from two-face (TypeScript, TOML, Go,
//! Dockerfiles, and more beyond syntect's own). Their scopes map onto theme roles, so code takes
//! the theme's palette and its 256, 16, and no-color fallbacks.
//!
//! The size guards follow Codex's `render/highlight.rs` (Apache-2.0): huge or pathological input
//! comes back plain, so a pasted log can't stall a frame.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

use ratatui::text::Span;
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

use crate::theme::{Role, Theme};

const MAX_BYTES: usize = 512 * 1024;
const MAX_LINES: usize = 10_000;
const MAX_LINE_BYTES: usize = 4 * 1024;

/// The shell grammar's name in the bundle.
const SHELL: &str = "Bourne Again Shell (bash)";

/// One span list per source line. An unknown or missing language comes back plain.
pub fn highlight_code(code: &str, lang: Option<&str>, theme: &Theme) -> Vec<Vec<Span<'static>>> {
    // Live cells ask for the same blocks every frame, so the cache comes first.
    let key = cache_key(code, lang, theme);
    if let Some(hit) = cache_get(key) {
        return hit.as_ref().clone();
    }
    let too_big = code.len() > MAX_BYTES
        || code.lines().count() > MAX_LINES
        || code.lines().any(|line| line.len() > MAX_LINE_BYTES);
    let lang = if too_big { None } else { lang };
    let mut highlighter = LineHighlighter::new(lang);
    let lines: Vec<Vec<Span<'static>>> = split_lines(code)
        .map(|line| highlighter.line(line, theme))
        .collect();
    cache_put(key, Arc::new(lines.clone()));
    lines
}

/// The grammar name for a fence's info string, such as `sh`, `bash`, `console`, or
/// `rust,no_run`. Returns `None` when no bundled grammar fits.
pub fn syntax_for(info: &str) -> Option<&'static str> {
    let token = info
        .trim()
        .split([',', ' ', '\t', '{'])
        .next()
        .unwrap_or("")
        .trim_start_matches('.')
        .to_ascii_lowercase();
    if token.is_empty() {
        return None;
    }
    let set = syntax_set();
    let name = match token.as_str() {
        "sh" | "bash" | "zsh" | "shell" | "console" | "shellscript" | "shell-session"
        | "terminal" => SHELL,
        "rs" | "rust" => "Rust",
        "py" | "python" | "python3" => "Python",
        "js" | "javascript" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "ts" | "typescript" | "mts" | "cts" => "TypeScript",
        "tsx" => "TypeScriptReact",
        "toml" => "TOML",
        "dockerfile" | "docker" => "Dockerfile",
        "json" | "jsonc" | "json5" => "JSON",
        "yaml" | "yml" => "YAML",
        "diff" | "patch" | "udiff" => "Diff",
        "md" | "markdown" => "Markdown",
        "c" | "h" => "C",
        "cpp" | "c++" | "cc" | "cxx" | "hpp" => "C++",
        "rb" | "ruby" => "Ruby",
        "golang" => "Go",
        "html" | "xhtml" => "HTML",
        "make" | "makefile" => "Makefile",
        _ => {
            return set
                .find_syntax_by_token(&token)
                .map(|syntax| syntax.name.as_str());
        }
    };
    set.find_syntax_by_name(name)
        .map(|syntax| syntax.name.as_str())
}

/// Carries the parser's state from line to line, so a block that streams in is colored as its
/// lines complete. The state doesn't depend on the theme. Once the code grows past the size
/// guards, the rest of it comes back plain.
#[derive(Debug, Clone)]
pub struct LineHighlighter {
    state: Option<(ParseState, ScopeStack)>,
    shell: bool,
    /// A fence that names no language, whose text takes the inline code color.
    bare: bool,
    lines: usize,
    bytes: usize,
}

impl LineHighlighter {
    /// A highlighter for code in `lang` (a fence info string), plain when no grammar fits.
    pub fn new(lang: Option<&str>) -> Self {
        let syntax = lang
            .and_then(syntax_for)
            .and_then(|name| syntax_set().find_syntax_by_name(name));
        Self::for_syntax(syntax)
    }

    /// A fence that names no language colors its text like inline code, as Claude Code draws it.
    pub fn for_fence(info: &str) -> Self {
        let info = info.trim();
        if info.is_empty() {
            return Self {
                bare: true,
                ..Self::for_syntax(None)
            };
        }
        Self::new(Some(info))
    }

    fn for_syntax(syntax: Option<&'static SyntaxReference>) -> Self {
        Self {
            state: syntax.map(|syntax| (ParseState::new(syntax), ScopeStack::new())),
            shell: syntax.is_some_and(|syntax| syntax.name == SHELL),
            bare: false,
            lines: 0,
            bytes: 0,
        }
    }

    /// Colors one line, given without its newline.
    pub fn line(&mut self, text: &str, theme: &Theme) -> Vec<Span<'static>> {
        if self.bare {
            return bare_line(text, theme);
        }
        let shell = self.shell;
        let mut spans: Vec<Span<'static>> = Vec::new();
        self.parse(text, |piece, scopes| {
            let role = role_for(scopes, piece, shell).unwrap_or(Role::Text);
            let style = theme.style(role);
            match spans.last_mut() {
                Some(last) if last.style == style => last.content.to_mut().push_str(piece),
                _ => spans.push(Span::styled(piece.to_owned(), style)),
            }
        });
        spans
    }

    /// Moves past one line without coloring it, for a line that was already drawn.
    pub fn skip(&mut self, text: &str) {
        self.parse(text, |_, _| {});
    }

    fn parse(&mut self, text: &str, mut piece: impl FnMut(&str, &[Scope])) {
        self.lines += 1;
        self.bytes += text.len() + 1;
        if self.lines > MAX_LINES || self.bytes > MAX_BYTES || text.len() > MAX_LINE_BYTES {
            self.state = None;
        }
        let parsed = self.state.as_mut().and_then(|(parse, _)| {
            let mut with_newline = String::with_capacity(text.len() + 1);
            with_newline.push_str(text);
            with_newline.push('\n');
            parse.parse_line(&with_newline, syntax_set()).ok()
        });
        let (Some(ops), Some((_, stack))) = (parsed, self.state.as_mut()) else {
            // Unknown, too big, or a grammar error: the rest of the block stays plain.
            self.state = None;
            if !text.is_empty() {
                piece(text, &[]);
            }
            return;
        };
        let mut start = 0;
        for (at, op) in ops {
            let end = at.min(text.len());
            if end > start {
                piece(&text[start..end], stack.as_slice());
                start = end;
            }
            if stack.apply(&op).is_err() {
                if start < text.len() {
                    piece(&text[start..], &[]);
                }
                self.state = None;
                return;
            }
        }
        if start < text.len() {
            piece(&text[start..], stack.as_slice());
        }
    }
}

/// A line of a fence that names no language: its text from the first to the last non-space
/// character in the code color, and the indent before it plain.
fn bare_line(text: &str, theme: &Theme) -> Vec<Span<'static>> {
    let body = text.trim_end();
    let content = body.trim_start();
    if content.is_empty() {
        return Vec::new();
    }
    let indent = &body[..body.len() - content.len()];
    let mut spans = Vec::with_capacity(2);
    if !indent.is_empty() {
        spans.push(Span::styled(indent.to_owned(), theme.style(Role::Text)));
    }
    spans.push(Span::styled(content.to_owned(), theme.style(Role::Code)));
    spans
}

/// The source lines of `code`, without their newlines. A trailing newline doesn't start one more
/// line, so `"a\n"` is one line and `""` is none.
pub(super) fn split_lines(code: &str) -> impl Iterator<Item = &str> {
    let body = code.strip_suffix('\n').unwrap_or(code);
    let empty = code.is_empty();
    body.split('\n').filter(move |_| !empty)
}

/// The role for a token, from its innermost scope outwards. `None` keeps the default color.
///
/// Comments win over everything inside them, and a string's own quotes and punctuation take the
/// string color, so `// hi` and `"a"` each read as one piece.
fn role_for(stack: &[Scope], text: &str, shell: bool) -> Option<Role> {
    let rules = scope_rules();
    let rule_of = |scope: &Scope| {
        rules
            .iter()
            .find(|(prefix, _)| prefix.is_prefix_of(*scope))
            .map(|(_, rule)| *rule)
    };
    let is = |scope: &Scope, role: Role| matches!(rule_of(scope), Some(Rule::Role(r)) if r == role);
    if stack.iter().any(|scope| is(scope, Role::SyntaxComment)) {
        return Some(Role::SyntaxComment);
    }
    for (depth, scope) in stack.iter().enumerate().rev() {
        let Some(rule) = rule_of(scope) else {
            continue;
        };
        return match rule {
            Rule::Plain => stack[..depth]
                .iter()
                .any(|outer| is(outer, Role::SyntaxString))
                .then_some(Role::SyntaxString),
            Rule::Role(role) => Some(role),
            // In shell code only built-in commands are colored, as Claude Code's highlighter
            // does: `cd` and `mkdir` stand out, `git` and `cargo` don't.
            Rule::ShellCommand if shell => {
                is_shell_builtin(text.trim()).then_some(Role::SyntaxCommand)
            }
            Rule::ShellCommand => Some(Role::SyntaxCommand),
            Rule::ShellModifier if shell && is_shell_builtin(text.trim()) => {
                Some(Role::SyntaxCommand)
            }
            Rule::ShellModifier => Some(Role::SyntaxKeyword),
        };
    }
    None
}

#[derive(Debug, Clone, Copy)]
enum Rule {
    /// Stop looking and keep the default color, such as for `&&` or `=`.
    Plain,
    Role(Role),
    /// A command or function name: always colored, except shell commands that aren't built in.
    ShellCommand,
    /// `export`, `local`, and friends, which shell code colors as built-ins.
    ShellModifier,
}

/// Scope prefixes and what they mean, most specific first where prefixes overlap.
fn scope_rules() -> &'static [(Scope, Rule)] {
    static RULES: OnceLock<Vec<(Scope, Rule)>> = OnceLock::new();
    RULES.get_or_init(|| {
        let table: &[(&str, Rule)] = &[
            ("comment", Rule::Role(Role::SyntaxComment)),
            ("string", Rule::Role(Role::SyntaxString)),
            ("constant.numeric", Rule::Role(Role::SyntaxNumber)),
            ("constant.language", Rule::Role(Role::SyntaxConstant)),
            ("constant.character", Rule::Role(Role::SyntaxConstant)),
            ("constant.other", Rule::Role(Role::SyntaxConstant)),
            ("keyword.operator", Rule::Plain),
            ("keyword", Rule::Role(Role::SyntaxKeyword)),
            ("storage.modifier", Rule::ShellModifier),
            ("storage", Rule::Role(Role::SyntaxKeyword)),
            ("entity.name.type", Rule::Role(Role::SyntaxType)),
            ("entity.name.class", Rule::Role(Role::SyntaxType)),
            ("entity.other.inherited-class", Rule::Role(Role::SyntaxType)),
            ("support.type", Rule::Role(Role::SyntaxType)),
            ("support.class", Rule::Role(Role::SyntaxType)),
            ("entity.name.function", Rule::ShellCommand),
            ("support.function", Rule::Role(Role::SyntaxCommand)),
            ("support.macro", Rule::Role(Role::SyntaxCommand)),
            ("variable.function", Rule::ShellCommand),
            ("entity.name.tag", Rule::Role(Role::SyntaxKeyword)),
            (
                "entity.other.attribute-name",
                Rule::Role(Role::SyntaxNumber),
            ),
            ("markup.inserted", Rule::Role(Role::Success)),
            ("markup.deleted", Rule::Role(Role::Error)),
            ("meta.diff", Rule::Role(Role::Dim)),
            ("markup.heading", Rule::Role(Role::SyntaxKeyword)),
            ("variable.parameter", Rule::Plain),
            ("punctuation", Rule::Plain),
        ];
        table
            .iter()
            .filter_map(|(name, rule)| Scope::new(name).ok().map(|scope| (scope, *rule)))
            .collect()
    })
}

/// The words highlight.js colors as built-ins in shell code: POSIX and bash built-ins, common zsh
/// ones, and GNU coreutils. Claude Code draws these in the command color. Sorted for a binary
/// search.
const SHELL_BUILTINS: &[&str] = &[
    "alias",
    "arch",
    "autoload",
    "b2sum",
    "base32",
    "base64",
    "basename",
    "bg",
    "bind",
    "bindkey",
    "break",
    "builtin",
    "caller",
    "cat",
    "cd",
    "chcon",
    "chdir",
    "chgrp",
    "chmod",
    "chown",
    "chroot",
    "cksum",
    "comm",
    "command",
    "continue",
    "cp",
    "csplit",
    "cut",
    "date",
    "dd",
    "declare",
    "df",
    "dir",
    "dircolors",
    "dirname",
    "dirs",
    "disown",
    "du",
    "echo",
    "enable",
    "env",
    "eval",
    "exec",
    "exit",
    "expand",
    "export",
    "expr",
    "factor",
    "fc",
    "fg",
    "fmt",
    "fold",
    "functions",
    "getopts",
    "groups",
    "hash",
    "head",
    "help",
    "history",
    "hostid",
    "id",
    "jobs",
    "join",
    "kill",
    "let",
    "link",
    "ln",
    "local",
    "logname",
    "logout",
    "ls",
    "mapfile",
    "md5sum",
    "mkdir",
    "mkfifo",
    "mknod",
    "mktemp",
    "mv",
    "nice",
    "nl",
    "nohup",
    "nproc",
    "numfmt",
    "od",
    "paste",
    "pathchk",
    "pinky",
    "popd",
    "pr",
    "print",
    "printenv",
    "printf",
    "ptx",
    "pushd",
    "pwd",
    "read",
    "readarray",
    "readlink",
    "readonly",
    "realpath",
    "rehash",
    "return",
    "rm",
    "rmdir",
    "runcon",
    "seq",
    "setopt",
    "sha1sum",
    "sha224sum",
    "sha256sum",
    "sha384sum",
    "sha512sum",
    "shift",
    "shred",
    "shuf",
    "sleep",
    "sort",
    "source",
    "split",
    "stat",
    "stdbuf",
    "stty",
    "sum",
    "suspend",
    "sync",
    "tac",
    "tail",
    "tee",
    "test",
    "timeout",
    "times",
    "touch",
    "tr",
    "trap",
    "truncate",
    "tsort",
    "tty",
    "type",
    "typeset",
    "ulimit",
    "umask",
    "unalias",
    "uname",
    "unexpand",
    "uniq",
    "unlink",
    "unset",
    "unsetopt",
    "uptime",
    "users",
    "vdir",
    "wait",
    "wc",
    "whence",
    "where",
    "which",
    "who",
    "whoami",
    "yes",
];

fn is_shell_builtin(word: &str) -> bool {
    SHELL_BUILTINS.binary_search(&word).is_ok()
}

fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

type Cached = Arc<Vec<Vec<Span<'static>>>>;

const CACHE_SIZE: usize = 32;

fn cache() -> &'static Mutex<Vec<(u64, Cached)>> {
    static CACHE: OnceLock<Mutex<Vec<(u64, Cached)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::with_capacity(CACHE_SIZE)))
}

fn cache_key(code: &str, lang: Option<&str>, theme: &Theme) -> u64 {
    let mut hasher = DefaultHasher::new();
    code.hash(&mut hasher);
    lang.hash(&mut hasher);
    for role in SYNTAX_ROLES {
        theme.style(role).hash(&mut hasher);
    }
    hasher.finish()
}

/// Every role a highlighted line can use, so a theme change misses the cache.
const SYNTAX_ROLES: [Role; 11] = [
    Role::Text,
    Role::Dim,
    Role::Success,
    Role::Error,
    Role::SyntaxKeyword,
    Role::SyntaxString,
    Role::SyntaxCommand,
    Role::SyntaxComment,
    Role::SyntaxNumber,
    Role::SyntaxType,
    Role::SyntaxConstant,
];

fn cache_get(key: u64) -> Option<Cached> {
    let mut entries = cache().lock().ok()?;
    let index = entries.iter().position(|(k, _)| *k == key)?;
    // Move the hit to the back, so the oldest entry is the one that goes.
    let entry = entries.remove(index);
    let hit = entry.1.clone();
    entries.push(entry);
    Some(hit)
}

fn cache_put(key: u64, lines: Cached) {
    let Ok(mut entries) = cache().lock() else {
        return;
    };
    if entries.len() >= CACHE_SIZE {
        entries.remove(0);
    }
    entries.push((key, lines));
}

#[cfg(test)]
mod tests {
    use ratatui::style::Style;

    use super::*;

    fn role_of(lines: &[Vec<Span<'static>>], theme: &Theme, word: &str) -> Option<Role> {
        let span = lines
            .iter()
            .flatten()
            .find(|span| span.content.contains(word))?;
        Role::ALL
            .into_iter()
            .find(|role| theme.style(*role) == span.style)
    }

    #[test]
    fn info_strings_pick_grammars() {
        assert_eq!(syntax_for("sh"), Some(SHELL));
        assert_eq!(syntax_for("console"), Some(SHELL));
        assert_eq!(syntax_for("zsh"), Some(SHELL));
        assert_eq!(syntax_for("rust,no_run"), Some("Rust"));
        assert_eq!(syntax_for("rs"), Some("Rust"));
        assert_eq!(syntax_for("py"), Some("Python"));
        assert_eq!(syntax_for("js"), Some("JavaScript"));
        assert_eq!(syntax_for("ts"), Some("TypeScript"));
        assert_eq!(syntax_for("tsx"), Some("TypeScriptReact"));
        assert_eq!(syntax_for("toml"), Some("TOML"));
        assert_eq!(syntax_for("go"), Some("Go"));
        assert_eq!(syntax_for("kotlin"), Some("Kotlin"));
        assert_eq!(syntax_for("Dockerfile"), Some("Dockerfile"));
        assert_eq!(syntax_for("json"), Some("JSON"));
        assert_eq!(syntax_for("yaml"), Some("YAML"));
        assert_eq!(syntax_for("diff"), Some("Diff"));
        assert_eq!(syntax_for("python title=\"x\""), Some("Python"));
        assert_eq!(syntax_for("klingon"), None);
        assert_eq!(syntax_for(""), None);
    }

    #[test]
    fn shell_builtins_are_commands_and_other_programs_are_not() {
        let theme = Theme::dark();
        let code = "mkdir -p /tmp/x && cd /tmp/x\ngit init -q && echo \"# try\" > README.md\n";
        let lines = highlight_code(code, Some("bash"), &theme);
        assert_eq!(lines.len(), 2);
        assert_eq!(role_of(&lines, &theme, "cd"), Some(Role::SyntaxCommand));
        assert_eq!(role_of(&lines, &theme, "mkdir"), Some(Role::SyntaxCommand));
        assert_eq!(role_of(&lines, &theme, "echo"), Some(Role::SyntaxCommand));
        assert_eq!(role_of(&lines, &theme, "try"), Some(Role::SyntaxString));
        assert_eq!(role_of(&lines, &theme, "git"), Some(Role::Text));
        assert_eq!(role_of(&lines, &theme, "&&"), Some(Role::Text));
    }

    #[test]
    fn rust_scopes_map_onto_roles() {
        let theme = Theme::dark();
        let code = "fn main() {\n    let x: u32 = 42; // hi\n    println!(\"{x}\");\n}\n";
        let lines = highlight_code(code, Some("rust"), &theme);
        assert_eq!(lines.len(), 4);
        assert_eq!(role_of(&lines, &theme, "fn"), Some(Role::SyntaxKeyword));
        assert_eq!(role_of(&lines, &theme, "main"), Some(Role::SyntaxCommand));
        assert_eq!(role_of(&lines, &theme, "42"), Some(Role::SyntaxNumber));
        assert_eq!(role_of(&lines, &theme, "// hi"), Some(Role::SyntaxComment));
        assert_eq!(
            role_of(&lines, &theme, "println!"),
            Some(Role::SyntaxCommand)
        );
    }

    #[test]
    fn unknown_languages_and_huge_code_stay_plain() {
        let theme = Theme::dark();
        let lines = highlight_code("hello world\n", Some("klingon"), &theme);
        assert_eq!(lines, vec![vec![Span::styled("hello world", Style::new())]]);
        let long = format!("echo {}\n", "x".repeat(MAX_LINE_BYTES + 1));
        let lines = highlight_code(&long, Some("sh"), &theme);
        assert_eq!(lines[0].len(), 1);
        assert!(highlight_code("", Some("sh"), &theme).is_empty());
        assert_eq!(highlight_code("\n", Some("sh"), &theme), vec![Vec::new()]);
    }

    #[test]
    fn line_by_line_matches_the_whole_block() {
        let theme = Theme::dark();
        let code = "def f(x):\n    '''doc\n    string'''\n    return None # c\n";
        let whole = highlight_code(code, Some("py"), &theme);
        let mut highlighter = LineHighlighter::new(Some("py"));
        let pieces: Vec<_> = split_lines(code)
            .map(|line| highlighter.line(line, &theme))
            .collect();
        assert_eq!(whole, pieces);
        assert_eq!(
            role_of(&whole, &theme, "string'''"),
            role_of(&whole, &theme, "'''doc")
        );
        assert_eq!(role_of(&whole, &theme, "return"), Some(Role::SyntaxKeyword));
    }

    #[test]
    fn the_cache_follows_the_theme() {
        let dark = Theme::dark();
        let light = Theme::light();
        let code = "cd /tmp\n";
        let a = highlight_code(code, Some("sh"), &dark);
        let b = highlight_code(code, Some("sh"), &light);
        assert_ne!(a, b);
        assert_eq!(a, highlight_code(code, Some("sh"), &dark));
    }
}
