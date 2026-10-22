//! The kinds of thing a shell command did. A command line splits on `&&`, `||`, `;` and new
//! lines, each part is read by its program, and only the first command of a pipeline counts,
//! since the rest only filter what it printed.

use argonauts_core::behaviors::Label;

use super::files::{self, FileRole};

/// How many shells inside shells are read: `timeout 60 bash -lc "env X=1 sh -c 'pytest'"`.
const NESTED: u8 = 3;

/// Programs that run a script fed to them, as `cat <<'EOF' | python3 -` feeds one.
const INTERPRETERS: [&str; 11] = [
    "python", "python3", "py", "node", "deno", "bun", "ruby", "perl", "sh", "bash", "zsh",
];

/// Each part's kind, in the order they ran, with files judged below `root`, the project's
/// folder, where it's known. Parts that only set the scene, such as `cd` or `export`, have
/// none.
pub(super) fn kinds(command: &str, root: Option<&str>) -> Vec<Label> {
    let mut found = Vec::new();
    read_script(&unwrapped(command), root, 0, &mut found);
    found
}

/// A shell a command starts, after whatever wraps it, is read by the script it's handed.
fn read_script(script: &str, root: Option<&str>, depth: u8, found: &mut Vec<Label>) {
    for words in commands(script) {
        match shell_script(without_wrappers(&words)) {
            Some(inner) if depth < NESTED => read_script(inner, root, depth + 1, found),
            _ => found.extend(kind_of(&words, root)),
        }
    }
}

/// The script `sh -c`, `bash -lc` and the like are handed.
fn shell_script(words: &[String]) -> Option<&str> {
    match words {
        [shell, flag, script, ..]
            if ["sh", "bash", "zsh", "dash"].contains(&program(shell))
                && flag.starts_with('-')
                && !flag.starts_with("--")
                && flag.ends_with('c') =>
        {
            Some(script)
        }
        _ => None,
    }
}

/// The script a shell was handed, as Codex wraps each command: `/bin/zsh -lc '…'`.
pub(crate) fn unwrapped(command: &str) -> String {
    let parts = commands(command);
    if let [words] = parts.as_slice()
        && let Some(script) = shell_script(words)
    {
        return script.to_owned();
    }
    command.to_owned()
}

fn program(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// The script's simple commands as words, quotes taken off, leaving out every command after
/// a pipe and the bodies of here-documents.
fn commands(script: &str) -> Vec<Vec<String>> {
    let mut lexer = Lexer::default();
    let mut chars = script.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(quote) = lexer.quote {
            match c {
                _ if c == quote => lexer.quote = None,
                '\\' if quote == '"' => {
                    if let Some(next) = chars.next() {
                        lexer.word.push(next);
                    }
                }
                _ => lexer.word.push(c),
            }
            lexer.quoted = true;
            continue;
        }
        if lexer.depth > 0 {
            lexer.word.push(c);
            match c {
                '(' => lexer.depth += 1,
                ')' => lexer.depth -= 1,
                _ => {}
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                lexer.quote = Some(c);
                lexer.quoted = true;
            }
            '\\' => match chars.next() {
                Some('\n') | None => {}
                Some(next) => lexer.word.push(next),
            },
            '$' if chars.peek() == Some(&'(') => {
                lexer.word.push(c);
                lexer.word.push('(');
                chars.next();
                lexer.depth = 1;
            }
            '#' if lexer.word.is_empty() && !lexer.quoted => {
                while chars.next_if(|next| *next != '\n').is_some() {}
            }
            ' ' | '\t' => lexer.end_word(),
            '\n' => {
                lexer.end_command(false);
                if let Some(end) = lexer.heredoc.take() {
                    skip_heredoc(&mut chars, &end);
                }
            }
            ';' => lexer.end_command(false),
            '&' if chars.next_if_eq(&'&').is_some() => lexer.end_command(false),
            '&' if lexer.word.ends_with(['>', '<']) => lexer.word.push(c),
            // `&>` and `&>>` send both outputs to a file; only a bare `&` ends a command.
            '&' if chars.peek() == Some(&'>') => lexer.word.push(c),
            '&' => lexer.end_command(false),
            '|' if chars.next_if_eq(&'|').is_some() => lexer.end_command(false),
            '|' => lexer.end_command(true),
            '(' | '{' | '}' if lexer.word.is_empty() => {}
            ')' => lexer.end_word(),
            _ => lexer.word.push(c),
        }
    }
    lexer.end_command(false);
    lexer.commands
}

fn skip_heredoc(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, end: &str) {
    loop {
        let line: String = std::iter::from_fn(|| chars.next_if(|c| *c != '\n')).collect();
        let more = chars.next().is_some();
        if line.trim() == end || !more {
            return;
        }
    }
}

#[derive(Default)]
struct Lexer {
    commands: Vec<Vec<String>>,
    words: Vec<String>,
    word: String,
    quote: Option<char>,
    /// The word had a quote, so an empty one still counts.
    quoted: bool,
    /// Inside `$( … )`, which stays one word.
    depth: u32,
    /// How many pipes the current command comes after.
    stage: u32,
    /// The word that ends the here-document the current line opened.
    heredoc: Option<String>,
}

impl Lexer {
    fn end_word(&mut self) {
        if self.word.is_empty() && !self.quoted {
            return;
        }
        let word = std::mem::take(&mut self.word);
        self.quoted = false;
        let opens = self
            .words
            .last()
            .is_some_and(|last| last == "<<" || last == "<<-");
        if let Some(end) = word.strip_prefix("<<").filter(|end| !end.starts_with('<')) {
            let end = end.trim_start_matches('-');
            if !end.is_empty() {
                self.heredoc = Some(end.to_owned());
            }
        } else if opens {
            self.heredoc = Some(word.clone());
        }
        self.words.push(word);
    }

    /// Ends the current command; `pipe` says the next one reads its output.
    fn end_command(&mut self, pipe: bool) {
        self.end_word();
        let words = std::mem::take(&mut self.words);
        if !words.is_empty() {
            match self.stage {
                0 => self.commands.push(words),
                1 => self.fed_a_here_document(words),
                _ => {}
            }
        }
        self.stage = if pipe { self.stage + 1 } else { 0 };
    }

    /// `cat <<'EOF' | python3 -` runs the here-document as `python3 - <<'EOF'` would, so the
    /// command that counts is the interpreter's.
    fn fed_a_here_document(&mut self, mut words: Vec<String>) {
        let Some(last) = self.commands.last_mut() else {
            return;
        };
        let opener = last.iter().find(|word| word.starts_with("<<")).cloned();
        let only_a_here_document = program(&last[0]) == "cat" && last.len() <= 3;
        let interpreter = without_wrappers(&words)
            .first()
            .is_some_and(|first| INTERPRETERS.contains(&program(first)));
        if let Some(opener) = opener.filter(|_| only_a_here_document && interpreter) {
            words.push(opener);
            *last = words;
        }
    }
}

/// Words before the program that only change how it runs: `FOO=1`, `sudo`, `timeout 60`.
fn without_wrappers(mut words: &[String]) -> &[String] {
    loop {
        let Some(first) = words.first() else {
            return words;
        };
        let assignment = first.split_once('=').is_some_and(|(name, _)| {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
        words = match program(first) {
            _ if assignment => &words[1..],
            // A loop's or a condition's keyword comes before the command it runs.
            "if" | "then" | "else" | "elif" | "while" | "until" | "do" | "!" => &words[1..],
            "time" | "nohup" | "command" | "exec" | "stdbuf" => skip_options(&words[1..]),
            "nice" => skip_options_with(&words[1..], &["-n", "--adjustment"]),
            "env" => skip_options_with(&words[1..], &["-u", "-C", "-S", "--unset", "--chdir"]),
            "sudo" => skip_options_with(&words[1..], &["-u", "-g", "-C", "-h", "-p", "-U"]),
            "xargs" => skip_options_with(&words[1..], &["-I", "-n", "-P", "-L", "-d", "-E", "-s"]),
            "timeout" | "gtimeout" => {
                let rest =
                    skip_options_with(&words[1..], &["-s", "-k", "--signal", "--kill-after"]);
                rest.get(1..).unwrap_or_default()
            }
            _ => return words,
        };
    }
}

/// Past a wrapper's options, `takes_value` naming those that take the next word as theirs.
fn skip_options_with<'a>(mut words: &'a [String], takes_value: &[&str]) -> &'a [String] {
    while let Some(first) = words.first() {
        if !first.starts_with('-') {
            break;
        }
        let skip = if takes_value.contains(&first.as_str()) {
            2
        } else {
            1
        };
        words = words.get(skip..).unwrap_or_default();
    }
    words
}

fn skip_options(words: &[String]) -> &[String] {
    let start = words
        .iter()
        .position(|word| !word.starts_with('-'))
        .unwrap_or(words.len());
    &words[start..]
}

fn kind_of(words: &[String], root: Option<&str>) -> Option<Label> {
    let words = without_wrappers(words);
    let first = words.first()?;
    let args = &words[1..];
    let sub = || {
        args.iter()
            .find(|arg| !arg.starts_with('-'))
            .map(String::as_str)
    };
    let has = |flag: &str| args.iter().any(|arg| arg == flag);
    let touches_records = args.iter().any(|arg| files::is_records(arg));
    if let Some(target) = written_to(words) {
        return Some(files::wrote(target, root));
    }
    let removes_tests = || {
        args.iter()
            .filter(|arg| !arg.starts_with('-'))
            .any(|arg| files::holds_tests(arg, root))
    };
    let label = match program(first) {
        "cd" | "pushd" | "popd" | "export" | "set" | "unset" | "source" | "." | "true"
        | "false" | ":" | "echo" | "printf" | "sleep" | "exit" | "wait" | "local" | "read"
        | "trap" | "alias" | "shopt" | "ulimit" | "umask" | "test" | "[" | "[[" | "pwd"
        | "clear" | "which" | "type" | "for" | "case" | "select" | "function" | "done" | "fi"
        | "esac" => return None,
        "git" => git(args, root),
        "pytest" | "py.test" | "jest" | "vitest" | "mocha" | "rspec" | "ctest" | "tox"
        | "phpunit" | "nextest" | "ava" | "nox" => Label::RunTests,
        "python" | "python3" | "py" => python(args, words, root),
        "node" | "deno" | "bun" => match sub() {
            _ if has("--test") => Label::RunTests,
            Some("test") => Label::RunTests,
            Some(file) if files::project_role(file, root) == FileRole::Test => Label::RunTests,
            _ => Label::Shell,
        },
        "npm" | "yarn" | "pnpm" => package_script(args),
        "npx" | "bunx" | "pnpx" => return kind_of(skip_options(args), root),
        "cargo" => match sub() {
            Some("test" | "nextest") => Label::RunTests,
            Some("build" | "check" | "doc") => Label::Build,
            Some("clippy" | "fmt") => Label::Lint,
            Some("add" | "install" | "update" | "fetch") => Label::Install,
            _ => Label::Shell,
        },
        "go" => match sub() {
            Some("test") => Label::RunTests,
            Some("build") => Label::Build,
            Some("vet" | "fmt") => Label::Lint,
            Some("get" | "install") => Label::Install,
            Some("mod") if has("tidy") || has("download") => Label::Install,
            _ => Label::Shell,
        },
        "make" | "just" => match sub() {
            Some(target) if target.contains("test") || target == "check" => Label::RunTests,
            Some("lint" | "fmt" | "format") => Label::Lint,
            Some("install") => Label::Install,
            _ => Label::Build,
        },
        "mvn" | "gradle" | "gradlew" | "dotnet" | "swift" | "bazel" | "bazelisk" => {
            if args
                .iter()
                .any(|arg| arg.contains("test") || arg == "verify")
            {
                Label::RunTests
            } else {
                Label::Build
            }
        }
        "tsc" | "cmake" | "ninja" | "webpack" | "esbuild" | "rustc" | "gcc" | "clang" | "javac" => {
            Label::Build
        }
        "rustfmt" | "eslint" | "prettier" | "ruff" | "black" | "flake8" | "mypy" | "pylint"
        | "isort" | "gofmt" | "goimports" | "golangci-lint" | "shellcheck" | "biome"
        | "stylelint" | "pyright" => Label::Lint,
        "pip" | "pip3" | "uv" | "poetry" | "brew" | "apt" | "apt-get" | "gem" | "bundle"
        | "composer" | "conda" | "pipx" => match sub() {
            Some("install" | "add" | "sync" | "uninstall" | "remove" | "require" | "upgrade") => {
                Label::Install
            }
            Some("pip") if has("install") => Label::Install,
            Some("run") => return kind_of(skip_options(&args[1..]), root),
            _ => Label::Shell,
        },
        "curl" | "wget" | "http" | "https" | "xh" | "aria2c" => Label::WebFetch,
        "cat" | "head" | "tail" | "less" | "more" | "nl" | "bat" | "wc" | "awk" | "jq" | "od"
        | "xxd" | "hexdump" | "strings" | "column" | "sort" | "uniq" | "diff" => {
            printed(args, touches_records, root)
        }
        "sed" | "perl" if in_place(args) => edited_by_shell(args, root),
        "sed" => printed(args, touches_records, root),
        "ls" | "find" | "tree" | "fd" | "du" | "stat" | "file" | "realpath" if touches_records => {
            Label::ListRecords
        }
        "ls" | "find" | "tree" | "fd" | "du" | "stat" | "file" | "realpath" => Label::ListFiles,
        "grep" | "egrep" | "fgrep" | "rg" | "ag" | "ack" if touches_records => Label::ListRecords,
        "rg" if has("--files") => Label::ListFiles,
        "grep" | "egrep" | "fgrep" | "rg" | "ag" | "ack" => Label::SearchCode,
        "rm" | "rmdir" | "unlink" if removes_tests() => Label::EditTestRemoved,
        "mkdir" | "touch" | "rm" | "rmdir" | "unlink" | "mv" | "cp" | "ln" | "chmod" | "chown"
        | "tar" | "unzip" | "zip" | "mktemp" => Label::FileOps,
        "argonauts" => Label::RunStatus,
        _ => Label::Shell,
    };
    Some(label)
}

/// Where `cat`, `echo`, `printf` or `tee` wrote a file: the target of `>` or `>>`, or
/// `tee`'s file.
fn written_to(words: &[String]) -> Option<&str> {
    let writer = ["cat", "echo", "printf", "tee"].contains(&program(&words[0]));
    if !writer {
        return None;
    }
    let mut targets = words.iter().enumerate().filter_map(|(at, word)| {
        if [">", ">>", "&>", "&>>"].contains(&word.as_str()) {
            return words.get(at + 1).map(String::as_str);
        }
        let inline = word
            .strip_prefix("&>>")
            .or_else(|| word.strip_prefix("&>"))
            .or_else(|| word.strip_prefix(">>"))
            .or_else(|| word.strip_prefix('>'));
        inline.filter(|target| !target.is_empty() && !target.starts_with('&'))
    });
    let redirected = targets.next();
    let teed = || {
        (program(&words[0]) == "tee")
            .then(|| skip_options(&words[1..]).first().map(String::as_str))
            .flatten()
    };
    redirected
        .or_else(teed)
        .filter(|target| *target != "/dev/null" && !target.starts_with("/dev/"))
}

fn printed(args: &[String], touches_records: bool, root: Option<&str>) -> Label {
    if touches_records {
        return Label::ReadRecords;
    }
    let test_file = args
        .iter()
        .filter(|arg| !arg.starts_with('-'))
        .any(|arg| files::project_role(arg, root) == FileRole::Test);
    if test_file {
        Label::PrintTestFiles
    } else {
        Label::PrintFiles
    }
}

fn in_place(args: &[String]) -> bool {
    args.iter().any(|arg| {
        arg == "--in-place" || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains('i'))
    })
}

/// `sed -i` and `perl -pi` edit their last file in place.
fn edited_by_shell(args: &[String], root: Option<&str>) -> Label {
    let file = args.iter().rev().find(|arg| !arg.starts_with('-'));
    match file.map(|file| files::project_role(file, root)) {
        Some(FileRole::Test) => Label::EditTest,
        Some(FileRole::Config) => Label::EditConfig,
        Some(FileRole::Doc) => Label::EditDoc,
        _ => Label::EditSource,
    }
}

fn git(args: &[String], root: Option<&str>) -> Label {
    // `-C <dir>` and `-c <key=value>` take a word each before the subcommand.
    let mut rest = args;
    while let Some(first) = rest.first() {
        rest = match first.as_str() {
            "-C" | "-c" | "--git-dir" | "--work-tree" => rest.get(2..).unwrap_or_default(),
            option if option.starts_with('-') => &rest[1..],
            _ => break,
        };
    }
    let Some(sub) = rest.first() else {
        return Label::Shell;
    };
    let after = &rest[1..];
    match sub.as_str() {
        "log" | "show" | "blame" | "reflog" | "shortlog" | "whatchanged" | "annotate"
        | "rev-list" | "cat-file" => Label::GitHistory,
        "status" | "diff" | "diff-index" | "diff-tree" | "rev-parse" | "describe" | "remote" => {
            Label::GitStatus
        }
        "branch"
            if after.iter().all(|arg| arg.starts_with('-'))
                && !after
                    .iter()
                    .any(|arg| ["-d", "-D", "-m", "-M", "--delete"].contains(&arg.as_str())) =>
        {
            Label::GitStatus
        }
        "grep" => Label::SearchCode,
        "ls-files" | "ls-tree" => Label::ListFiles,
        "stash" if matches!(after.first().map(String::as_str), Some("list" | "show")) => {
            Label::GitHistory
        }
        "rm" if after
            .iter()
            .filter(|arg| !arg.starts_with('-'))
            .any(|arg| files::holds_tests(arg, root)) =>
        {
            Label::EditTestRemoved
        }
        "add" | "commit" | "stash" | "checkout" | "reset" | "merge" | "rebase" | "switch"
        | "restore" | "cherry-pick" | "revert" | "am" | "apply" | "rm" | "mv" | "tag" | "push"
        | "pull" | "fetch" | "clean" | "worktree" | "init" | "clone" | "branch" => Label::GitChange,
        _ => Label::Shell,
    }
}

fn python(args: &[String], words: &[String], root: Option<&str>) -> Label {
    let module = args
        .iter()
        .position(|arg| arg == "-m")
        .and_then(|at| args.get(at + 1));
    if let Some(module) = module {
        return match module.as_str() {
            "pytest" | "unittest" | "nose2" | "tox" | "nox" => Label::RunTests,
            "pip" if args.iter().any(|arg| arg == "install") => Label::Install,
            "mypy" | "ruff" | "black" | "flake8" | "pylint" | "isort" => Label::Lint,
            _ => Label::Shell,
        };
    }
    let options = args.iter().take_while(|arg| arg.starts_with('-'));
    let snippet = options
        .into_iter()
        .any(|arg| arg == "-" || (arg.starts_with("-c") && !arg.starts_with("--")))
        || words.iter().any(|word| word.starts_with("<<"));
    if snippet {
        return Label::PythonSnippet;
    }
    match args.iter().find(|arg| !arg.starts_with('-')) {
        Some(script) if files::project_role(script, root) == FileRole::Test => Label::RunTests,
        Some(_) => Label::Shell,
        None => Label::PythonSnippet,
    }
}

/// `npm test`, `yarn run build`, `pnpm install` and the like.
fn package_script(args: &[String]) -> Label {
    let mut words = args.iter().filter(|arg| !arg.starts_with('-'));
    let script = match words.next().map(String::as_str) {
        Some("run" | "run-script") => words.next().map(String::as_str),
        other => other,
    };
    match script {
        Some("install" | "i" | "ci" | "add" | "remove" | "uninstall" | "update") => Label::Install,
        Some("exec" | "dlx") => Label::Shell,
        Some(script) if script == "t" || script.starts_with("test") => Label::RunTests,
        Some(script) if script.starts_with("build") || script.starts_with("compile") => {
            Label::Build
        }
        Some(script)
            if ["lint", "format", "fmt", "prettier", "typecheck", "check"]
                .iter()
                .any(|name| script.starts_with(name)) =>
        {
            Label::Lint
        }
        None => Label::Install,
        _ => Label::Shell,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds_of(command: &str) -> Vec<Label> {
        kinds(command, None)
    }

    #[test]
    fn a_command_line_splits_into_its_parts() {
        assert_eq!(
            commands("git status --short && git diff -- a.py; ls -la || echo 'a && b'"),
            [
                vec!["git", "status", "--short"],
                vec!["git", "diff", "--", "a.py"],
                vec!["ls", "-la"],
                vec!["echo", "a && b"],
            ]
        );
    }

    #[test]
    fn only_the_first_command_of_a_pipeline_counts() {
        assert_eq!(
            kinds_of("git log --oneline | head -5 && cat src/a.py | grep foo"),
            [Label::GitHistory, Label::PrintFiles]
        );
    }

    #[test]
    fn a_here_document_is_part_of_its_command() {
        let script = "python3 - <<'EOF'\nimport os; os.listdir('.')\nls\nEOF\ngit status";
        assert_eq!(kinds_of(script), [Label::PythonSnippet, Label::GitStatus]);
        let written = "mkdir -p tests && cat > tests/test_a.py <<EOF\nassert 1\nEOF";
        assert_eq!(kinds_of(written), [Label::FileOps, Label::WriteTest]);
    }

    #[test]
    fn a_wrapped_command_is_read_inside_its_shell() {
        let codex = "/bin/zsh -lc 'git status --short && git diff -- src/store.js && node --test'";
        assert_eq!(
            kinds_of(codex),
            [Label::GitStatus, Label::GitStatus, Label::RunTests]
        );
        let quoted = r#"/bin/bash -lc "sed -n '1,240p' tests/test_money.py""#;
        assert_eq!(kinds_of(quoted), [Label::PrintTestFiles]);
    }

    #[test]
    fn scene_setting_parts_have_no_kind() {
        assert_eq!(
            kinds_of("cd /w/task && export CI=1 && FOO=1 timeout 60 cargo test -p core"),
            [Label::RunTests]
        );
        assert_eq!(kinds_of("cd /w && echo done"), []);
    }

    #[test]
    fn git_subcommands() {
        let cases = [
            ("git log -1 --oneline", Label::GitHistory),
            ("git show ef3f187 -- ratelimit/bucket.py", Label::GitHistory),
            ("git -C /w/task blame a.py", Label::GitHistory),
            ("git --no-pager diff --stat", Label::GitStatus),
            ("git branch --show-current", Label::GitStatus),
            ("git branch fix-1", Label::GitChange),
            ("git commit -m 'x'", Label::GitChange),
            ("git stash pop", Label::GitChange),
            ("git stash list", Label::GitHistory),
            ("git stash show -p stash@{0}", Label::GitHistory),
            ("git revert --no-edit HEAD", Label::GitChange),
            ("git grep -n slug", Label::SearchCode),
            ("git ls-files", Label::ListFiles),
            ("git bisect start", Label::Shell),
        ];
        for (command, label) in cases {
            assert_eq!(kinds_of(command), [label], "{command}");
        }
    }

    #[test]
    fn programs() {
        let cases = [
            ("pytest -q tests/test_money.py", Label::RunTests),
            ("python3 -m pytest", Label::RunTests),
            ("python -m unittest discover", Label::RunTests),
            ("node --test", Label::RunTests),
            ("npm test", Label::RunTests),
            ("npm run test:unit", Label::RunTests),
            ("npx vitest run", Label::RunTests),
            ("go test ./...", Label::RunTests),
            ("make check", Label::RunTests),
            ("uv run pytest", Label::RunTests),
            ("python3 tests/run_all.py", Label::RunTests),
            ("python3 -c 'print(1)'", Label::PythonSnippet),
            ("python3 scripts/gen.py", Label::Shell),
            ("cargo build --release", Label::Build),
            ("npm run build", Label::Build),
            ("cargo clippy --all-targets", Label::Lint),
            ("ruff check .", Label::Lint),
            ("pip install requests", Label::Install),
            ("npm install", Label::Install),
            ("uv add httpx", Label::Install),
            ("cargo add serde", Label::Install),
            ("curl -s https://example.com", Label::WebFetch),
            ("cat src/store.js src/cli.js", Label::PrintFiles),
            ("sed -n '1,80p' src/cli.js", Label::PrintFiles),
            ("head -50 test/store.test.js", Label::PrintTestFiles),
            ("sed -i '' 's/a/b/' src/a.py", Label::EditSource),
            ("perl -pi -e 's/a/b/' tests/test_a.py", Label::EditTest),
            ("ls -R src test package.json", Label::ListFiles),
            ("find . -name '*.py'", Label::ListFiles),
            ("rg -n 'rate.?limit' src", Label::SearchCode),
            ("grep -rn TODO .", Label::SearchCode),
            ("rm -rf build", Label::FileOps),
            ("rm -rf tests/test_flaky.py", Label::EditTestRemoved),
            ("rm -r tests", Label::EditTestRemoved),
            ("git rm -q tests/test_flaky.py", Label::EditTestRemoved),
            ("git rm src/old.py", Label::GitChange),
            (
                "rm -rf ratelimit/__pycache__ tests/__pycache__",
                Label::FileOps,
            ),
            ("rm tests/fixtures/data.json", Label::FileOps),
            ("sudo -u app nice -n 5 pytest", Label::RunTests),
            ("pytest &> out.log", Label::RunTests),
            ("cargo test &>> out.log", Label::RunTests),
            ("echo hi &> notes.md", Label::WriteDoc),
            ("echo hi > notes.md", Label::WriteDoc),
            ("tee src/a.py", Label::WriteSource),
            ("ls /u/.argonauts/state/repo-1/runs", Label::ListRecords),
            (
                "cat /u/.argonauts/state/repo-1/runs/run-1/events.jsonl",
                Label::ReadRecords,
            ),
            ("ollama list", Label::Shell),
        ];
        for (command, label) in cases {
            assert_eq!(kinds_of(command), [label], "{command}");
        }
        assert_eq!(kinds_of("echo hi >/dev/null"), []);
        assert_eq!(kinds_of("(cd src && make)"), [Label::Build]);
        assert_eq!(kinds_of("python3 -I -c 'print(1)'"), [Label::PythonSnippet]);
        assert_eq!(kinds_of("rg --files -g '*.py'"), [Label::ListFiles]);
    }

    #[test]
    fn a_shell_after_its_wrappers_is_read_by_its_script() {
        assert_eq!(
            kinds_of("timeout 600 bash -lc \"cargo test\""),
            [Label::RunTests]
        );
        assert_eq!(kinds_of("env FOO=1 bash -c 'pytest'"), [Label::RunTests]);
        assert_eq!(
            kinds_of("nice -n 5 sh -c 'git status && git log -1'"),
            [Label::GitStatus, Label::GitHistory]
        );
    }

    #[test]
    fn a_here_document_fed_to_an_interpreter_is_a_snippet() {
        let fed = "cat <<'EOF' | python3 -\nimport json\nprint(1)\nEOF";
        assert_eq!(kinds_of(fed), [Label::PythonSnippet]);
        let spaced = "cat << EOF | python3\nprint(1)\nEOF\ngit status";
        assert_eq!(kinds_of(spaced), [Label::PythonSnippet, Label::GitStatus]);
        assert_eq!(kinds_of("cat <<EOF | bash\nls\nEOF"), [Label::Shell]);
        assert_eq!(
            kinds_of("cat a.py | python3 -"),
            [Label::PrintFiles],
            "a file printed is still a file printed"
        );
    }

    #[test]
    fn test_files_are_judged_inside_the_project() {
        let root = Some("/Users/you/test/proj");
        assert_eq!(
            kinds("cat /Users/you/test/proj/src/app.py", root),
            [Label::PrintFiles]
        );
        assert_eq!(
            kinds("cat /Users/you/test/proj/tests/test_a.py", root),
            [Label::PrintTestFiles]
        );
    }

    #[test]
    fn loops_and_conditions_are_read_by_the_commands_inside() {
        assert_eq!(
            kinds_of("for f in src/a.py tests/b.py; do echo \"== $f\"; cat -n $f; done"),
            [Label::PrintFiles]
        );
        assert_eq!(
            kinds_of("if grep -q x a.py; then git diff; fi"),
            [Label::SearchCode, Label::GitStatus]
        );
    }

    #[test]
    fn substitutions_and_comments_stay_whole() {
        assert_eq!(
            kinds_of("tmpdir=$(mktemp -d; echo x) && cp -R src \"$tmpdir\"/ # copy it\ngit status"),
            [Label::FileOps, Label::GitStatus]
        );
    }
}
