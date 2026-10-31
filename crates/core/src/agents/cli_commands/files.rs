//! Reading the command and skill files the agent CLIs keep: markdown with a front matter block,
//! such as `.claude/commands/ship.md` or `skills/pdf/SKILL.md`. A folder that isn't there has
//! no commands, and a file that can't be read is skipped, so one bad file hides nothing else.

use std::fs;
use std::path::{Path, PathBuf};

/// One command or skill file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandFile {
    pub path: PathBuf,
    /// The file's stem, or for a skill the `name` it gives, else its folder's name.
    pub name: String,
    fields: Vec<(String, String)>,
    /// What follows the front matter.
    pub body: String,
}

impl CommandFile {
    pub fn read(path: &Path, name: impl Into<String>) -> Option<Self> {
        let text = fs::read_to_string(path).ok()?;
        let (fields, body) = front_matter(&text);
        Some(Self {
            path: path.to_owned(),
            name: name.into(),
            fields,
            body: body.to_owned(),
        })
    }

    /// A front matter value, unquoted and on one line.
    pub fn field(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
            .filter(|value| !value.is_empty())
    }

    /// `description`, else the body's first line of text.
    pub fn help(&self) -> String {
        let text = self.field("description").unwrap_or_else(|| {
            self.body
                .lines()
                .map(|line| line.trim().trim_start_matches('#').trim())
                .find(|line| !line.is_empty())
                .unwrap_or_default()
        });
        one_line(text)
    }

    /// `argument-hint`, such as `[branch]`.
    pub fn args(&self) -> String {
        self.field("argument-hint")
            .map(one_line)
            .unwrap_or_default()
    }

    /// A skill with `user-invocable: false` is for the model alone and stays out of the menu.
    pub fn user_invocable(&self) -> bool {
        self.field("user-invocable") != Some("false")
    }
}

/// `*.md` in `dir`, by name, and in its subfolders too when `nested`. A nested command keeps
/// its own file's stem as its name.
pub fn command_files(dir: &Path, nested: bool) -> Vec<CommandFile> {
    let mut found = Vec::new();
    for path in sorted_entries(dir) {
        if path.is_dir() {
            if nested {
                found.extend(command_files(&path, true));
            }
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        found.extend(CommandFile::read(&path, stem));
    }
    found
}

/// `<dir>/<skill>/SKILL.md`, by folder name.
pub fn skill_files(dir: &Path) -> Vec<CommandFile> {
    sorted_entries(dir)
        .into_iter()
        .filter_map(|folder| {
            let folder_name = folder.file_name()?.to_str()?.to_owned();
            let file = CommandFile::read(&folder.join("SKILL.md"), folder_name)?;
            let name = file.field("name").map(str::to_owned);
            Some(match name {
                Some(name) => CommandFile { name, ..file },
                None => file,
            })
        })
        .collect()
}

/// The folders in `dir`, by name, such as the versions of a plugin.
pub fn folders(dir: &Path) -> Vec<PathBuf> {
    sorted_entries(dir)
        .into_iter()
        .filter(|path| path.is_dir())
        .collect()
}

/// Hidden entries are left out, as the CLIs leave them out.
fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !name.starts_with('.'))
        })
        .collect();
    paths.sort();
    paths
}

/// The `key: value` lines between `---` fences, and the rest. A folded or literal value
/// (`>` or `|`) takes the indented lines under it.
fn front_matter(text: &str) -> (Vec<(String, String)>, &str) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return (Vec::new(), text);
    };
    let Some(end) = rest.find("\n---") else {
        return (Vec::new(), text);
    };
    let block = &rest[..end];
    let body = rest[end + 4..]
        .split_once('\n')
        .map_or("", |(_, body)| body);
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in block.lines() {
        let indented = line.starts_with([' ', '\t']);
        match (indented, fields.last_mut()) {
            (true, Some((_, value))) => {
                if !value.is_empty() {
                    value.push(' ');
                }
                value.push_str(line.trim());
            }
            _ => {
                if let Some((key, value)) = line.split_once(':') {
                    let value = value.trim();
                    let value = if matches!(value, ">" | "|" | ">-" | "|-") {
                        ""
                    } else {
                        unquote(value)
                    };
                    fields.push((key.trim().to_owned(), value.to_owned()));
                }
            }
        }
    }
    (fields, body)
}

fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn front_matter_gives_help_and_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            &root.join("ship.md"),
            "---\ndescription: \"Ship the branch\"\nargument-hint: [branch]\nallowed-tools: Bash(git push:*)\n---\nPush $ARGUMENTS.\n",
        );
        write(&root.join("notes.md"), "\n# Write release notes\n\nMore.\n");
        write(&root.join("frontend/component.md"), "Make a component\n");
        write(&root.join("README.txt"), "not a command");
        write(&root.join(".hidden.md"), "hidden");

        let top: Vec<String> = command_files(root, false)
            .iter()
            .map(|file| file.name.clone())
            .collect();
        assert_eq!(top, ["notes", "ship"]);
        let all = command_files(root, true);
        let names: Vec<&str> = all.iter().map(|file| file.name.as_str()).collect();
        assert_eq!(names, ["component", "notes", "ship"]);

        let ship = &all[2];
        assert_eq!(ship.help(), "Ship the branch");
        assert_eq!(ship.args(), "[branch]");
        assert_eq!(ship.body, "Push $ARGUMENTS.\n");
        assert_eq!(
            all[1].help(),
            "Write release notes",
            "the first line, unmarked"
        );
        assert_eq!(all[1].args(), "");
        assert!(command_files(&root.join("missing"), true).is_empty());
        assert_eq!(folders(root), [root.join("frontend")]);
    }

    #[test]
    fn skills_take_their_given_name_and_folded_descriptions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            &root.join("pdf/SKILL.md"),
            "---\nname: pdf-tools\ndescription: >\n  Work with PDFs,\n  every page.\n---\nBody\n",
        );
        write(
            &root.join("helper/SKILL.md"),
            "---\ndescription: 'For the model only'\nuser-invocable: false\n---\n",
        );
        write(&root.join("empty/notes.md"), "no SKILL.md here");

        let skills = skill_files(root);
        let names: Vec<&str> = skills.iter().map(|file| file.name.as_str()).collect();
        assert_eq!(names, ["helper", "pdf-tools"]);
        assert_eq!(skills[1].help(), "Work with PDFs, every page.");
        assert!(skills[1].user_invocable());
        assert!(!skills[0].user_invocable());
        assert_eq!(skills[0].help(), "For the model only");
    }

    #[test]
    fn a_file_without_front_matter_is_all_body() {
        let (fields, body) = front_matter("---not a fence\ntext");
        assert!(fields.is_empty());
        assert_eq!(body, "---not a fence\ntext");
        let (fields, body) = front_matter("---\ndescription: open\nno end");
        assert!(fields.is_empty(), "an unclosed block is text");
        assert_eq!(body, "---\ndescription: open\nno end");
    }
}
