//! How much of a local route your own OpenCode config already declares, so Argonauts Code leaves
//! yours in charge and only adds what's missing.
//!
//! The file list follows `loadInstanceState` in `opencode/src/config/config.ts` at v1.18.33, read
//! from the source, not run. Remote org config, console config, and managed files are left out.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Every OpenCode config file that applies in `workdir`, whether or not it exists. `env` reads
/// an environment variable.
pub(crate) fn config_files(workdir: &Path, env: impl Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let path_var = |name: &str| {
        env(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let home = path_var("HOME");
    let global = path_var("XDG_CONFIG_HOME")
        .or_else(|| home.as_ref().map(|home| home.join(".config")))
        .map(|config| config.join("opencode"));

    let mut files = Vec::new();
    if let Some(global) = &global {
        files.push(global.join("config.json"));
        files.extend(json_files(global));
    }
    files.extend(path_var("OPENCODE_CONFIG"));
    for dir in project_dirs(workdir) {
        files.extend(json_files(&dir));
        files.extend(json_files(&dir.join(".opencode")));
    }
    if let Some(home) = &home {
        files.extend(json_files(&home.join(".opencode")));
    }
    if let Some(dir) = path_var("OPENCODE_CONFIG_DIR") {
        files.extend(json_files(&dir));
    }
    files
}

fn json_files(dir: &Path) -> [PathBuf; 2] {
    ["opencode.jsonc", "opencode.json"].map(|name| dir.join(name))
}

/// The working directory and its parents up to the git root, or up to `/` outside a repo.
fn project_dirs(workdir: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for dir in workdir.ancestors() {
        dirs.push(dir.to_path_buf());
        // A linked worktree has a `.git` file, not a folder.
        if dir.join(".git").exists() {
            break;
        }
    }
    dirs
}

/// What your config files say about one provider and model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Declared {
    Nothing,
    /// `provider.<id>`, but not this model under it.
    Provider,
    /// `provider.<id>.models.<model>`, or a file Argonauts Code can't read, which is never
    /// overridden.
    Model,
}

/// How much of `provider.<id>.models.<model>` any of `files` declares.
pub(crate) fn declared(files: &[PathBuf], id: &str, model: &str) -> Declared {
    let mut found = Declared::Nothing;
    for text in files
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
    {
        let Ok(config) = serde_json::from_str::<Value>(&to_json(&text)) else {
            return Declared::Model;
        };
        let provider = &config["provider"][id];
        if !provider["models"][model].is_null() {
            return Declared::Model;
        }
        if !provider.is_null() {
            found = Declared::Provider;
        }
    }
    found
}

/// OpenCode config is JSONC: comments and trailing commas are allowed.
fn to_json(jsonc: &str) -> String {
    drop_trailing_commas(&drop_comments(jsonc))
}

fn drop_comments(jsonc: &str) -> String {
    let mut json = String::with_capacity(jsonc.len());
    let mut strings = StringScan::default();
    let mut chars = jsonc.chars().peekable();
    while let Some(c) = chars.next() {
        if !strings.inside {
            match (c, chars.peek()) {
                ('/', Some('/')) => {
                    while chars.next_if(|&next| next != '\n').is_some() {}
                    continue;
                }
                ('/', Some('*')) => {
                    chars.next();
                    let mut star = false;
                    for next in chars.by_ref() {
                        if star && next == '/' {
                            break;
                        }
                        star = next == '*';
                    }
                    continue;
                }
                _ => {}
            }
        }
        strings.feed(c);
        json.push(c);
    }
    json
}

fn drop_trailing_commas(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    let mut strings = StringScan::default();
    for (at, c) in json.char_indices() {
        let in_string = strings.feed(c);
        if !in_string && c == ',' && json[at + 1..].trim_start().starts_with(['}', ']']) {
            continue;
        }
        out.push(c);
    }
    out
}

/// Whether a left-to-right scan is inside a JSON string.
#[derive(Default)]
struct StringScan {
    inside: bool,
    escaped: bool,
}

impl StringScan {
    /// Takes the next character and says whether it belongs to a string, quotes included.
    fn feed(&mut self, c: char) -> bool {
        if !self.inside {
            self.inside = c == '"';
            return self.inside;
        }
        if self.escaped {
            self.escaped = false;
        } else if c == '\\' {
            self.escaped = true;
        } else if c == '"' {
            self.inside = false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn opencode_config_files() {
        let repo = tempfile::tempdir().unwrap();
        let workdir = repo.path().join("crates/app");
        std::fs::create_dir_all(&workdir).unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();

        let vars = HashMap::from([
            ("HOME", "/home/me"),
            ("OPENCODE_CONFIG", "/etc/mine.json"),
            ("OPENCODE_CONFIG_DIR", ""),
        ]);
        let files = config_files(&workdir, |name| vars.get(name).map(|v| (*v).to_owned()));
        for expected in [
            PathBuf::from("/home/me/.config/opencode/config.json"),
            PathBuf::from("/home/me/.config/opencode/opencode.jsonc"),
            PathBuf::from("/etc/mine.json"),
            workdir.join("opencode.json"),
            workdir.join(".opencode/opencode.json"),
            repo.path().join("opencode.jsonc"),
            PathBuf::from("/home/me/.opencode/opencode.json"),
        ] {
            assert!(files.contains(&expected), "{}", expected.display());
        }
        // The walk stops at the git root, and an empty variable counts as unset.
        let outside = repo.path().parent().unwrap().join("opencode.json");
        assert!(!files.contains(&outside));
        assert!(files.iter().all(|f| f.is_absolute()));

        let xdg = HashMap::from([("XDG_CONFIG_HOME", "/xdg")]);
        let files = config_files(&workdir, |name| xdg.get(name).map(|v| (*v).to_owned()));
        assert!(files.contains(&PathBuf::from("/xdg/opencode/opencode.json")));
    }

    #[test]
    fn provider_declared_in_jsonc() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("opencode.jsonc");
        let missing = dir.path().join("opencode.json");
        std::fs::write(
            &file,
            r#"{
              // my own Ollama box
              "provider": {
                "ollama": { "options": { "baseURL": "http://gpu:11434/v1", }, },
                /* "lmstudio": {} */
              },
              "model": "ollama/qwen3 // not a comment",
            }"#,
        )
        .unwrap();
        let files = [missing, file.clone()];
        assert_eq!(declared(&files, "ollama", "qwen3"), Declared::Provider);
        assert_eq!(declared(&files, "lmstudio", "qwen3"), Declared::Nothing);
        assert_eq!(declared(&[], "ollama", "qwen3"), Declared::Nothing);

        std::fs::write(&file, "{ \"provider\": ").unwrap();
        assert_eq!(declared(&files, "lmstudio", "qwen3"), Declared::Model);
    }

    #[test]
    fn a_model_declared_in_any_file_counts() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.json");
        let project = dir.path().join("opencode.json");
        std::fs::write(
            &global,
            r#"{"provider": {"ollama": {"models": {"llama3.2": {}}}}}"#,
        )
        .unwrap();
        std::fs::write(&project, r#"{"provider": {"ollama": {}}}"#).unwrap();
        let files = [global, project];
        assert_eq!(declared(&files, "ollama", "llama3.2"), Declared::Model);
        assert_eq!(declared(&files, "ollama", "gemma4:e2b"), Declared::Provider);
    }

    #[test]
    fn jsonc_to_json() {
        assert_eq!(
            to_json("{\"a\": \"x // y, }\", /* c */ \"b\": [1, 2, ], } // end"),
            "{\"a\": \"x // y, }\",  \"b\": [1, 2 ] } "
        );
        assert_eq!(
            to_json(r#"{"q": "say \"hi\", // there"}"#),
            r#"{"q": "say \"hi\", // there"}"#
        );
    }
}
