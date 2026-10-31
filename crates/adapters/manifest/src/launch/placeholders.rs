//! `{name}` placeholders in a manifest's arguments, filled in one pass so a value that holds
//! placeholder text, such as a prompt quoting `{workdir}`, is never filled again.

/// The placeholders a manifest may use. The registry's check keeps the same list.
pub(crate) const PLACEHOLDERS: &[&str] = &[
    "model",
    "effort",
    "workdir",
    "session",
    "home",
    "schema_file",
    "prompt_file",
    "prompt",
];

/// What one placeholder becomes in this launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fill {
    Value(String),
    /// An optional value that isn't set. Its argument is left out, and so is the flag just
    /// before it when the placeholder is the whole argument, so `["--model", "{model}"]`
    /// without a model passes nothing and the CLI uses its default.
    Omit,
    /// Can't be filled here. The reason is shown to you.
    Refuse(String),
}

/// The names of the placeholders in `arg`, in order. A name is ASCII letters, digits, and
/// underscores, so JSON such as `{"a":1}` in an argument isn't mistaken for one.
pub(crate) fn placeholders_in(arg: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut rest = arg;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        match placeholder_at(after) {
            Some(name) => {
                names.push(name);
                rest = &after[name.len() + 1..];
            }
            None => rest = after,
        }
    }
    names
}

/// The name at the start of `text` if a `}` closes it.
fn placeholder_at(text: &str) -> Option<&str> {
    let end = text.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))?;
    (end > 0 && text[end..].starts_with('}')).then(|| &text[..end])
}

/// Fills every argument. An unknown placeholder or a refused one is an error naming it.
pub(crate) fn fill_args(
    args: &[String],
    lookup: impl Fn(&str) -> Fill,
) -> Result<Vec<String>, String> {
    let mut filled: Vec<String> = Vec::with_capacity(args.len());
    // Whether the last argument pushed was a plain flag from the manifest, which an omitted
    // value right after it takes along.
    let mut last_was_flag = false;
    for arg in args {
        match fill_one(arg, &lookup)? {
            Some(text) => {
                last_was_flag = text == *arg && arg.starts_with('-');
                filled.push(text);
            }
            None => {
                let whole = matches!(placeholders_in(arg).as_slice(), [name] if arg.len() == name.len() + 2);
                if whole && last_was_flag {
                    filled.pop();
                }
                last_was_flag = false;
            }
        }
    }
    Ok(filled)
}

/// `None` when an omitted value leaves the argument out.
fn fill_one(arg: &str, lookup: &impl Fn(&str) -> Fill) -> Result<Option<String>, String> {
    let mut out = String::with_capacity(arg.len());
    let mut rest = arg;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(name) = placeholder_at(after) else {
            out.push('{');
            rest = after;
            continue;
        };
        if !PLACEHOLDERS.contains(&name) {
            return Err(format!(
                "{{{name}}} isn't a placeholder Argonauts Code knows"
            ));
        }
        match lookup(name) {
            Fill::Value(value) => out.push_str(&value),
            Fill::Omit => return Ok(None),
            Fill::Refuse(reason) => return Err(format!("{{{name}}}: {reason}")),
        }
        rest = &after[name.len() + 1..];
    }
    out.push_str(rest);
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    fn lookup(name: &str) -> Fill {
        match name {
            "model" => Fill::Value("m-1".into()),
            "workdir" => Fill::Value("/work".into()),
            "prompt" => Fill::Value("say {workdir}".into()),
            "session" => Fill::Refuse("only resume_args get a session".into()),
            _ => Fill::Omit,
        }
    }

    #[test]
    fn finds_names_and_ignores_json_braces() {
        assert_eq!(
            placeholders_in("--cwd={workdir}/{model}"),
            ["workdir", "model"]
        );
        assert!(placeholders_in(r#"{"a":1}"#).is_empty());
        assert!(placeholders_in("{}").is_empty());
        assert!(placeholders_in("{unclosed").is_empty());
        assert_eq!(placeholders_in("{{model}}"), ["model"]);
    }

    #[test]
    fn fills_values_once() {
        let filled = fill_args(&args(&["-p", "{prompt}", "--cwd={workdir}"]), lookup);
        assert_eq!(filled, Ok(args(&["-p", "say {workdir}", "--cwd=/work"])));
    }

    #[test]
    fn omitted_values_take_their_flag_along() {
        let filled = fill_args(
            &args(&[
                "run",
                "--effort",
                "{effort}",
                "--schema={schema_file}",
                "--model",
                "{model}",
            ]),
            lookup,
        );
        assert_eq!(filled, Ok(args(&["run", "--model", "m-1"])));
        // A positional before the omitted value stays.
        let filled = fill_args(&args(&["run", "{effort}"]), lookup);
        assert_eq!(filled, Ok(args(&["run"])));
    }

    #[test]
    fn unknown_and_refused_placeholders_are_errors() {
        let err = fill_args(&args(&["{nope}"]), lookup).unwrap_err();
        assert!(err.contains("{nope}"), "{err}");
        let err = fill_args(&args(&["--resume", "{session}"]), lookup).unwrap_err();
        assert!(err.contains("only resume_args"), "{err}");
    }
}
