//! How much of a local route your own OpenCode config already declares, so Argonauts Code leaves
//! yours in charge and only adds what's missing.
//!
//! OpenCode 2 says it itself: `opencode api GET /api/config` lists every source it read, in
//! the order it merges them, each file turned into 2.x's shape (`providers.<id>.settings`) from
//! either shape. OpenCode 1.18 has no such call, so its files are read here. That file list
//! follows `loadInstanceState` in `opencode/src/config/config.ts` at v1.18.33, read from the
//! source, not run. Remote org config, console config, and managed files are left out.

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

/// What your config says about one provider and model, least first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Declared {
    Nothing,
    /// The provider, but not this model under it.
    Provider,
    /// The model, with no window for it.
    Model,
    /// The model and its window (`limit.context`), or a file Argonauts Code can't read, which
    /// is never overridden.
    ModelAndWindow,
}

/// A local runtime's provider as a route names it: Argonauts Code's id for the runtime, where
/// the runtime listens, and the model.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Wanted<'a> {
    pub id: &'a str,
    pub address: &'a str,
    pub model: &'a str,
}

/// Your provider for a local runtime, and how much of the model it declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Yours {
    /// The id your config gives it: the runtime's own, else that of a provider of yours at the
    /// runtime's address, such as the `llama.cpp` OpenCode's docs give llama-server.
    pub id: String,
    pub declared: Declared,
}

/// Your provider for `wanted`'s runtime in any of `files`, in either shape: 1.x's
/// `provider.<id>` or 2.x's `providers.<id>`. A file that can't be read as config stands for
/// all of it.
pub(crate) fn declared(files: &[PathBuf], wanted: Wanted) -> Yours {
    let mut configs = Vec::new();
    for text in files
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
    {
        let Ok(config) = serde_json::from_str::<Value>(&to_json(&text)) else {
            return Yours {
                id: wanted.id.to_owned(),
                declared: Declared::ModelAndWindow,
            };
        };
        configs.push(config);
    }
    yours_in(&configs.iter().collect::<Vec<_>>(), wanted)
}

/// Your provider for `wanted`'s runtime in OpenCode 2's own account of its config
/// (`opencode api GET /api/config`). `None` when the answer isn't that list, such as from a
/// version that doesn't have the call.
pub(crate) fn declared_in_answer(answer: &str, wanted: Wanted) -> Option<Yours> {
    let Value::Array(sources) = serde_json::from_str::<Value>(answer.trim()).ok()? else {
        return None;
    };
    let configs: Vec<&Value> = sources
        .iter()
        .filter(|source| source["type"] == "document")
        .map(|source| &source["info"])
        .collect();
    Some(yours_in(&configs, wanted))
}

/// OpenCode merges its sources key by key, so the provider may be named in one and its model
/// in another. The runtime's own id wins over a provider found by its address.
fn yours_in(configs: &[&Value], wanted: Wanted) -> Yours {
    let id = if configs
        .iter()
        .any(|config| !provider(config, wanted.id).is_null())
    {
        wanted.id.to_owned()
    } else {
        configs
            .iter()
            .find_map(|config| provider_at(config, wanted.address))
            .unwrap_or_else(|| wanted.id.to_owned())
    };
    let declared = configs
        .iter()
        .map(|config| declared_in(config, &id, wanted.model))
        .max()
        .unwrap_or(Declared::Nothing);
    Yours { id, declared }
}

const SHAPES: [&str; 2] = ["provider", "providers"];

fn provider<'a>(config: &'a Value, id: &str) -> &'a Value {
    SHAPES
        .iter()
        .map(|key| &config[*key][id])
        .find(|provider| !provider.is_null())
        .unwrap_or(&Value::Null)
}

/// The id of a provider whose base URL is on the server at `address`: 1.x keeps it in
/// `options`, 2.x in `settings`.
fn provider_at(config: &Value, address: &str) -> Option<String> {
    let at = server(address)?;
    SHAPES
        .iter()
        .filter_map(|key| config[*key].as_object())
        .flatten()
        .find(|(_, provider)| {
            ["options", "settings"].iter().any(|place| {
                provider[*place]["baseURL"]
                    .as_str()
                    .and_then(server)
                    .is_some_and(|server| server == at)
            })
        })
        .map(|(id, _)| id.clone())
}

/// The host and port a URL reaches, with every name of this computer counted as one.
fn server(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.trim().split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !port.contains(']') => (host, Some(port.parse::<u16>().ok()?)),
        _ => (authority, None),
    };
    let port = match (port, scheme.to_ascii_lowercase().as_str()) {
        (Some(port), _) => port,
        (None, "https") => 443,
        (None, _) => 80,
    };
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    let host = match host.as_str() {
        "localhost" | "127.0.0.1" | "::1" => "localhost".to_owned(),
        _ => host,
    };
    (!host.is_empty()).then_some((host, port))
}

fn declared_in(config: &Value, id: &str, model: &str) -> Declared {
    SHAPES
        .iter()
        .map(|key| {
            let provider = &config[*key][id];
            let entry = &provider["models"][model];
            if entry["limit"]["context"].is_u64() {
                Declared::ModelAndWindow
            } else if !entry.is_null() {
                Declared::Model
            } else if !provider.is_null() {
                Declared::Provider
            } else {
                Declared::Nothing
            }
        })
        .max()
        .unwrap_or(Declared::Nothing)
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

    /// A route's provider on the runtime's own address.
    fn on(id: &'static str, model: &'static str) -> Wanted<'static> {
        let address = argonauts_core::agents::runtimes::runtime(id)
            .map_or("http://localhost:1/v1", |runtime| runtime.address());
        Wanted { id, address, model }
    }

    fn level(files: &[PathBuf], id: &'static str, model: &'static str) -> Declared {
        declared(files, on(id, model)).declared
    }

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
        assert_eq!(level(&files, "ollama", "qwen3"), Declared::Provider);
        assert_eq!(level(&files, "lmstudio", "qwen3"), Declared::Nothing);
        assert_eq!(level(&[], "ollama", "qwen3"), Declared::Nothing);

        std::fs::write(&file, "{ \"provider\": ").unwrap();
        assert_eq!(level(&files, "lmstudio", "qwen3"), Declared::ModelAndWindow);
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
        assert_eq!(level(&files, "ollama", "llama3.2"), Declared::Model);
        assert_eq!(level(&files, "ollama", "gemma4:e2b"), Declared::Provider);
    }

    #[test]
    fn a_2x_file_counts_as_a_1x_one_does() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("opencode.json");
        std::fs::write(
            &file,
            r#"{"providers": {"ollama": {"settings": {"baseURL": "http://gpu:11434/v1"},
                "models": {"llama3.2": {"limit": {"context": 8192, "output": 2048}},
                           "qwen3": {"name": "Qwen 3"}}}}}"#,
        )
        .unwrap();
        let files = [file];
        assert_eq!(
            level(&files, "ollama", "llama3.2"),
            Declared::ModelAndWindow
        );
        assert_eq!(level(&files, "ollama", "qwen3"), Declared::Model);
        assert_eq!(level(&files, "ollama", "gemma3"), Declared::Provider);
        assert_eq!(level(&files, "lmstudio", "gemma3"), Declared::Nothing);
    }

    /// OpenCode 2.0.26's answer to `GET /api/config` with your file and an inline document:
    /// each source in turn, every one in 2.x's shape, the folders it read listed as well.
    const ANSWER: &str = r#"[
        {"type": "document", "path": "/home/me/.config/opencode/opencode.json",
         "info": {"providers": {"ollama": {"name": "Ollama (local)",
            "package": "aisdk:@ai-sdk/openai-compatible",
            "settings": {"baseURL": "http://localhost:11434/v1"},
            "models": {"qwen3-coder:30b-64k": {"name": "Qwen3 Coder 30B (64k)",
                "limit": {"context": 65536, "output": 16384}}}}}}},
        {"type": "directory", "path": "/home/me/.config/opencode"},
        {"type": "document", "info": {"providers": {"lmstudio": {
            "models": {"qwen/qwen3-8b": {"name": "qwen/qwen3-8b"}}}}}}
    ]"#;

    #[test]
    fn opencode_2_says_what_it_read_itself() {
        let said =
            |id, model| declared_in_answer(ANSWER, on(id, model)).map(|yours| yours.declared);
        assert_eq!(
            said("ollama", "qwen3-coder:30b-64k"),
            Some(Declared::ModelAndWindow)
        );
        assert_eq!(said("ollama", "gemma3:12b-32k"), Some(Declared::Provider));
        assert_eq!(said("lmstudio", "qwen/qwen3-8b"), Some(Declared::Model));
        assert_eq!(said("vllm", "qwen"), Some(Declared::Nothing));
        assert_eq!(
            declared_in_answer("[]\n", on("ollama", "x")).map(|yours| yours.declared),
            Some(Declared::Nothing)
        );
        // Not the list: an older OpenCode, or an error printed instead.
        for not_a_list in ["", "{\"error\": \"no\"}", "unknown command api"] {
            assert!(declared_in_answer(not_a_list, on("ollama", "x")).is_none());
        }
    }

    #[test]
    fn a_provider_of_yours_at_the_runtimes_address_is_used_under_its_own_id() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("opencode.json");
        std::fs::write(
            &file,
            r#"{"provider": {"llama.cpp": {"npm": "@ai-sdk/openai-compatible",
                "options": {"baseURL": "http://localhost:9931/v1"},
                "models": {"qwen3-8b-q4.gguf": {"name": "Qwen3 8B"}}}}}"#,
        )
        .unwrap();
        let mut files = vec![file];
        let yours = |id: &str, declared| Yours {
            id: id.to_owned(),
            declared,
        };
        assert_eq!(
            declared(&files, on("llamacpp", "qwen3-8b-q4.gguf")),
            yours("llama.cpp", Declared::Model)
        );
        assert_eq!(
            declared(&files, on("llamacpp", "gemma3.gguf")),
            yours("llama.cpp", Declared::Provider)
        );
        // Another port is another server.
        let elsewhere = Wanted {
            address: "http://127.0.0.1:8080/v1",
            ..on("llamacpp", "m.gguf")
        };
        assert_eq!(
            declared(&files, elsewhere),
            yours("llamacpp", Declared::Nothing)
        );

        // A provider under the runtime's own id wins.
        let own = dir.path().join("own.json");
        std::fs::write(&own, r#"{"provider": {"llamacpp": {}}}"#).unwrap();
        files.push(own);
        assert_eq!(
            declared(&files, on("llamacpp", "m.gguf")),
            yours("llamacpp", Declared::Provider)
        );

        // OpenCode 2 says it in 2.x's shape.
        let answer = r#"[{"type": "document", "info": {"providers": {"llama.cpp": {
            "settings": {"baseURL": "http://[::1]:9931"}}}}}]"#;
        assert_eq!(
            declared_in_answer(answer, on("llamacpp", "m.gguf")),
            Some(yours("llama.cpp", Declared::Provider))
        );
    }

    #[test]
    fn a_server_is_its_host_and_port() {
        let at = |host: &str, port| Some((host.to_owned(), port));
        assert_eq!(server("http://localhost:9931/v1"), at("localhost", 9931));
        assert_eq!(server("http://127.0.0.1:9931"), at("localhost", 9931));
        assert_eq!(server("http://[::1]:9931/v1"), at("localhost", 9931));
        assert_eq!(server("https://gpu.lan/v1"), at("gpu.lan", 443));
        assert_eq!(server("http://me@Box:8000/v1?x=1"), at("box", 8000));
        assert_eq!(server("gpu:11434"), None);
        assert_eq!(server("http://gpu:port/v1"), None);
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
