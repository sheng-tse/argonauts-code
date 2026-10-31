//! What OpenCode can use. OpenCode has no single account: it keeps a list of providers you
//! connected, each billed its own way. This reads that list from what OpenCode's own commands
//! print (`opencode auth list`, `opencode models`, `opencode debug config`). Its credentials
//! file is never opened, and of the config only the default model and where each provider
//! listens are looked at.

use argonauts_core::agents::adapter::CliProviders;
use argonauts_core::agents::runtimes::runtime;
use argonauts_core::app::status_tab::{CliProvider, ProviderBilling};
use serde_json::Value;

use crate::models::private_server::STANDALONE_FLAG;

/// The commands asked, in the order [`providers`] takes their output. On OpenCode 2 a plain
/// `auth list` or `models` goes through the background service it keeps for the whole computer,
/// and starts it with Argonauts' environment when it isn't running, so both ask a private
/// server instead (2.0.32's `--help` lists the flag on each). `auth list` answers in JSON, since
/// 2.x's own text is columns without a frame. `debug config` starts no server and refuses the
/// flag. OpenCode 1.18 has neither the service nor the flags.
pub(crate) const CHECKS: &[&[&str]] = &[
    &["auth", "list", STANDALONE_FLAG, "--format", "json"],
    &["models", STANDALONE_FLAG],
    &["debug", "config"],
];

/// OpenCode's own free models, which need no provider of yours.
const OWN_PROVIDER: &str = "opencode";

/// The providers and the default model, from what [`CHECKS`] printed. `None` when the first
/// command printed nothing to read, as when this OpenCode has no such command.
pub(crate) fn providers(outputs: &[String]) -> Option<CliProviders> {
    let connected = connected(outputs.first()?)?;
    let models = outputs.get(1).map(String::as_str).unwrap_or_default();
    let config: Value = outputs
        .get(2)
        .and_then(|printed| config_in(printed))
        .unwrap_or(Value::Null);

    let mut providers = connected;
    for id in routed_providers(models) {
        let Some(name) = local_name(&id, &config) else {
            continue;
        };
        if !providers
            .iter()
            .any(|known| same_provider(&known.name, &id))
        {
            providers.push(CliProvider {
                name,
                billing: ProviderBilling::Local,
            });
        }
    }
    let default_model = config
        .get("model")
        .and_then(Value::as_str)
        .and_then(|route| {
            let (provider, _) = route.split_once('/')?;
            let name = providers
                .iter()
                .find(|known| same_provider(&known.name, provider))
                .map_or_else(|| provider.to_owned(), |known| known.name.clone());
            Some((route.to_owned(), name))
        });
    Some(CliProviders {
        providers,
        default_model,
    })
}

/// Each credential `opencode auth list` shows: its name, and how it was connected. A key in
/// the environment counts as an API key. `None` when nothing in the output is that list: not
/// 2.x's JSON, no section's frame, which 1.18 draws around its rows, and no row of a credential
/// either.
fn connected(printed: &str) -> Option<Vec<CliProvider>> {
    if let Some(listed) = connected_json(printed) {
        return Some(listed);
    }
    let text = without_colour(printed);
    let mut providers: Vec<CliProvider> = Vec::new();
    let mut listed = false;
    for line in text.lines().map(str::trim) {
        // A section opens with `┌`, closes with `└`, and holds one `●` row per credential.
        if line.starts_with('┌') || line.starts_with('└') {
            listed = true;
            continue;
        }
        let Some(row) = line.strip_prefix('●') else {
            continue;
        };
        let Some((name, how)) = row.trim().rsplit_once(char::is_whitespace) else {
            continue;
        };
        let billing = match how {
            "oauth" | "wellknown" => ProviderBilling::Plan,
            _ => ProviderBilling::ApiKey,
        };
        let name = name.trim();
        if !providers.iter().any(|known| known.name == name) {
            providers.push(CliProvider {
                name: name.to_owned(),
                billing,
            });
        }
    }
    (listed || !providers.is_empty()).then_some(providers)
}

/// OpenCode 2's `auth list --format json`: one entry per provider, each with the ways it is
/// connected, such as `env` for a key in the environment or `oauth` for a plan's sign-in.
fn connected_json(printed: &str) -> Option<Vec<CliProvider>> {
    let Ok(Value::Array(entries)) = serde_json::from_str::<Value>(printed.trim()) else {
        return None;
    };
    let mut providers: Vec<CliProvider> = Vec::new();
    for entry in &entries {
        let Some(name) = entry["name"].as_str().or_else(|| entry["id"].as_str()) else {
            continue;
        };
        let signed_in = entry["connections"].as_array().is_some_and(|connections| {
            connections.iter().any(|connection| {
                matches!(connection["type"].as_str(), Some("oauth" | "wellknown"))
            })
        });
        let billing = if signed_in {
            ProviderBilling::Plan
        } else {
            ProviderBilling::ApiKey
        };
        if !providers.iter().any(|known| known.name == name) {
            providers.push(CliProvider {
                name: name.to_owned(),
                billing,
            });
        }
    }
    Some(providers)
}

/// The provider ids `opencode models` routes, in the order first seen, without OpenCode's own.
fn routed_providers(printed: &str) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for line in without_colour(printed).lines() {
        let Some((id, _)) = line.trim().split_once('/') else {
            continue;
        };
        if id != OWN_PROVIDER && !id.contains(char::is_whitespace) && !ids.iter().any(|i| i == id) {
            ids.push(id.to_owned());
        }
    }
    ids
}

/// The name of provider `id` when it is a runtime on this computer: one OpenCode's docs name,
/// or one your config points at this computer.
fn local_name(id: &str, config: &Value) -> Option<String> {
    let declared = &config["provider"][id];
    let listens_here = declared["options"]["baseURL"].as_str().is_some_and(|url| {
        ["://localhost", "://127.0.0.1", "://[::1]"]
            .iter()
            .any(|host| url.contains(host))
    });
    // A provider of yours under another id, such as OpenCode's `llama.cpp`, is found by its
    // address instead.
    let known = runtime(id).map(|runtime| runtime.name.to_owned());
    if !listens_here && known.is_none() {
        return None;
    }
    declared["name"]
        .as_str()
        .map(str::to_owned)
        .or(known)
        .or_else(|| Some(id.to_owned()))
}

/// `OpenRouter` and `openrouter`, or `LM Studio` and `lmstudio`, are one provider.
fn same_provider(name: &str, id: &str) -> bool {
    let plain = |text: &str| -> String {
        text.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    plain(name) == plain(id)
}

/// The config `debug config` printed, in 1.x's shape, whatever notices come before or after
/// it. OpenCode 1.18 prints one object. OpenCode 2 prints each source it read in the order it
/// merges them, every one in 2.x's shape, so the documents are merged here, later ones winning,
/// into the default model and each provider's name and address.
fn config_in(printed: &str) -> Option<Value> {
    let start = printed.find(['{', '['])?;
    let value: Value = serde_json::Deserializer::from_str(&printed[start..])
        .into_iter()
        .next()?
        .ok()?;
    let Value::Array(sources) = value else {
        return Some(value);
    };
    let mut merged = serde_json::json!({ "provider": {} });
    for info in sources
        .iter()
        .filter(|source| source["type"] == "document")
        .map(|source| &source["info"])
    {
        if let Some(model) = model_route(&info["model"]) {
            merged["model"] = Value::String(model);
        }
        let Some(providers) = info["providers"].as_object() else {
            continue;
        };
        for (id, provider) in providers {
            let kept = &mut merged["provider"][id];
            if let Some(name) = provider["name"].as_str() {
                kept["name"] = Value::String(name.to_owned());
            }
            if let Some(url) = provider["settings"]["baseURL"].as_str() {
                kept["options"]["baseURL"] = Value::String(url.to_owned());
            }
        }
    }
    Some(merged)
}

/// A model as `provider/model`, written that way or as 2.x's `{providerID, model}`.
fn model_route(model: &Value) -> Option<String> {
    if let Some(route) = model.as_str() {
        return Some(route.to_owned());
    }
    let provider = model["providerID"].as_str()?;
    let id = model["model"].as_str()?;
    Some(format!("{provider}/{id}"))
}

/// `printed` without the escape sequences a CLI colours its lists with.
pub(crate) fn without_colour(printed: &str) -> String {
    let mut plain = String::with_capacity(printed.len());
    let mut chars = printed.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            plain.push(c);
            continue;
        }
        // `ESC [ … letter`
        if chars.next() == Some('[') {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    plain
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
    }

    fn provider(name: &str, billing: ProviderBilling) -> CliProvider {
        CliProvider {
            name: name.to_owned(),
            billing,
        }
    }

    #[test]
    fn the_fixtures_say_they_are_made_up() {
        for name in ["providers.meta.json", "providers_none.meta.json"] {
            let meta: Value = serde_json::from_str(&fixture(name)).unwrap();
            assert_eq!(meta["synthetic"], true, "{name}");
        }
    }

    #[test]
    fn providers_come_with_how_each_bills_and_the_default_model() {
        let found = providers(&[
            fixture("providers_auth_list.stdout.txt"),
            fixture("providers_models.stdout.txt"),
            fixture("providers_config.stdout.json"),
        ])
        .unwrap();
        assert_eq!(
            found.providers,
            [
                provider("OpenRouter", ProviderBilling::ApiKey),
                provider("Ollama", ProviderBilling::Local),
            ]
        );
        assert_eq!(
            found.default_model,
            Some(("openrouter/z-ai/glm-5".to_owned(), "OpenRouter".to_owned()))
        );
    }

    #[test]
    fn nothing_connected_is_an_empty_list_not_an_unknown_one() {
        let found = providers(&[
            fixture("providers_none_auth_list.stdout.txt"),
            fixture("providers_none_models.stdout.txt"),
        ])
        .unwrap();
        assert_eq!(found.providers, []);
        assert_eq!(found.default_model, None);
        // Output that isn't the list at all, as from a version without the command, is unknown.
        assert_eq!(providers(&["error: unknown command auth".to_owned()]), None);
        assert_eq!(providers(&[]), None);
    }

    #[test]
    fn rows_read_without_the_frame_a_version_may_not_draw() {
        let found = providers(&["●  OpenRouter api\n●  Anthropic oauth\n".to_owned()]).unwrap();
        assert_eq!(
            found.providers,
            [
                provider("OpenRouter", ProviderBilling::ApiKey),
                provider("Anthropic", ProviderBilling::Plan),
            ]
        );
        // Neither a frame nor a row: what was printed is something else, and stays unknown.
        assert_eq!(providers(&["Pick a provider to continue".to_owned()]), None);
        assert_eq!(providers(&[String::new()]), None);
    }

    #[test]
    fn a_plan_sign_in_a_key_in_the_environment_and_colours_all_read() {
        let listed = "\u{1b}[90m┌\u{1b}[39m  Credentials \u{1b}[2m~/.local/share/opencode/auth.json\u{1b}[22m\n│\n●  Anthropic \u{1b}[2moauth\u{1b}[22m\n│\n●  Z.AI Coding Plan api\n│\n└  2 credentials\n\n┌  Environment\n│\n●  OpenAI OPENAI_API_KEY\n│\n└  1 environment variable\n";
        let found = providers(&[listed.to_owned()]).unwrap();
        assert_eq!(
            found.providers,
            [
                provider("Anthropic", ProviderBilling::Plan),
                provider("Z.AI Coding Plan", ProviderBilling::ApiKey),
                provider("OpenAI", ProviderBilling::ApiKey),
            ]
        );
    }

    #[test]
    fn a_runtime_your_config_points_at_this_computer_is_local_whatever_its_id() {
        let config = r#"{"provider": {"box": {"name": "My box", "options": {"baseURL": "http://127.0.0.1:8080/v1"}}, "far": {"options": {"baseURL": "https://api.example.com/v1"}}}}"#;
        let found = providers(&[
            "┌  Credentials\n└  0 credentials\n".to_owned(),
            "box/qwen3\nfar/model\nlmstudio/qwen/qwen3-8b\n".to_owned(),
            config.to_owned(),
        ])
        .unwrap();
        assert_eq!(
            found.providers,
            [
                provider("My box", ProviderBilling::Local),
                provider("LM Studio", ProviderBilling::Local),
            ]
        );
    }

    /// OpenCode 2.0.26's `debug config`: each source it read, every file turned into 2.x's
    /// shape, merged in order.
    #[test]
    fn opencode_2s_list_of_sources_reads_as_one_config() {
        let printed = r#"[
          {"type": "document", "path": "/home/me/.config/opencode/opencode.json",
           "info": {"model": {"providerID": "ollama", "model": "qwen3-coder:30b"},
             "providers": {"gpubox": {"name": "GPU box",
               "settings": {"baseURL": "http://127.0.0.1:8080/v1"}},
               "ollama": {"settings": {"baseURL": "http://localhost:11434/v1"}}}}},
          {"type": "directory", "path": "/home/me/.config/opencode"},
          {"type": "document", "path": "/work/repo/opencode.json",
           "info": {"providers": {"gpubox": {"name": "The GPU box"}}}}
        ]"#;
        let config = config_in(printed).unwrap();
        assert_eq!(config["model"], "ollama/qwen3-coder:30b");
        assert_eq!(config["provider"]["gpubox"]["name"], "The GPU box");
        assert_eq!(
            config["provider"]["gpubox"]["options"]["baseURL"],
            "http://127.0.0.1:8080/v1"
        );
        assert_eq!(
            local_name("gpubox", &config).as_deref(),
            Some("The GPU box")
        );
        // 1.18's single object reads as it is, past a notice before it.
        let old = config_in("notice\n{\"model\": \"ollama/llama3.2\"}").unwrap();
        assert_eq!(old["model"], "ollama/llama3.2");
        assert_eq!(config_in("nothing here"), None);
    }

    /// OpenCode 2.0.32's own answers: a key in the environment, and nothing connected. It no
    /// longer draws 1.18's frame, so its list is asked for as JSON.
    #[test]
    fn opencode_2s_list_reads_from_its_json() {
        let meta: Value = serde_json::from_str(&fixture("providers_2.meta.json")).unwrap();
        assert_eq!(meta["cli_version"], "2.0.32");
        assert_eq!(meta["synthetic"], false);
        let found = providers(&[fixture("providers_2_auth_list.stdout.json")]).unwrap();
        assert_eq!(
            found.providers,
            [provider("OpenRouter", ProviderBilling::ApiKey)]
        );
        let none = providers(&[fixture("providers_2_none_auth_list.stdout.json")]).unwrap();
        assert_eq!(none.providers, []);
        let signed_in =
            r#"[{"id": "anthropic", "name": "Anthropic", "connections": [{"type": "oauth"}]}]"#;
        assert_eq!(
            providers(&[signed_in.to_owned()]).unwrap().providers,
            [provider("Anthropic", ProviderBilling::Plan)]
        );
        // 2.x's own columns, which this never asks for, aren't read as a list.
        let columns = "OpenRouter  OPENROUTER_API_KEY          environment\n";
        assert_eq!(providers(&[columns.to_owned()]), None);
        assert_eq!(
            CHECKS[0],
            ["auth", "list", STANDALONE_FLAG, "--format", "json"]
        );
    }

    #[test]
    fn the_credentials_file_is_named_by_opencode_and_never_opened() {
        // The list prints the file's path; nothing here turns a path into a read.
        let source = include_str!("providers.rs");
        let code = source.split("#[cfg(test)]").next().unwrap();
        assert!(!code.contains("read_to_string") && !code.contains("File::open"));
    }
}
