//! What OpenCode can run, as `opencode api model.list` lists it on a private server, and the
//! model its Default runs, from `opencode api model.default`. Neither runs a model.
//!
//! OpenCode 2 answers each with `{"location": …, "data": …}`: `data` is a list of
//! `Model.Info` for the first, one `Model.Info` or `null` for the second. A `Model.Info` holds
//! the id, the provider, OpenCode's name for it, `limit.context`, `capabilities.tools`, the
//! variants a launch names after the model as `model#variant`, `time.released`, the cost of
//! each tier and a status. An OpenCode without `opencode api`, as 1.18 may be, still prints its
//! ids one a line with `opencode models`.

use std::path::Path;

use argonauts_core::registry::{CostModel, CostUnit};
use argonauts_core::runtimes::runtime;
use argonauts_core::settings::endpoints::host_and_port;
use serde_json::Value;

use crate::private_server::{ApiCall, ColdStart, KeptServer, Listing, PrivateServer};
use crate::providers::without_colour;

pub const MODEL_LIST: &str = "model.list";
pub const MODEL_DEFAULT: &str = "model.default";
/// OpenCode's own hosted provider, OpenCode Zen.
pub const ZEN: &str = "opencode";

/// One model OpenCode can run.
#[derive(Debug, Clone, PartialEq)]
pub struct ListedModel {
    /// As `--model` takes it: `ollama/qwen3-coder:30b`.
    pub route: String,
    pub provider: String,
    /// OpenCode's own name. A tag OpenCode found by itself has its id here again.
    pub name: String,
    /// `limit.context`, in tokens.
    pub context: Option<u64>,
    /// `capabilities.tools`: whether it can read and edit files on OpenCode.
    pub tools: bool,
    /// The variant ids, in OpenCode's order. Empty for a model that takes none.
    pub variants: Vec<String>,
    /// `time.released`, in milliseconds; 0 when OpenCode doesn't know.
    pub released: i64,
    /// Every tier costs nothing, as OpenCode Zen's free models do. A model with no cost at
    /// all, as a local tag has, isn't counted as free.
    pub free: bool,
    /// The first tier's price, in dollars per million tokens.
    pub price: Option<CostModel>,
    /// `settings.baseURL`: where OpenCode sends its requests.
    pub base_url: Option<String>,
    pub deprecated: bool,
}

impl ListedModel {
    /// The id within its provider: `qwen3-coder:30b`.
    pub fn model_id(&self) -> &str {
        self.route
            .split_once('/')
            .map_or(self.route.as_str(), |(_, id)| id)
    }

    /// OpenCode's name when it gives one of its own, else `None`, as for a tag OpenCode only
    /// found, whose name just repeats its id.
    pub fn own_name(&self) -> Option<&str> {
        let name = self.name.trim();
        (!name.is_empty() && name != self.model_id() && name != self.route).then_some(name)
    }

    /// Runs on this computer: through a runtime such as Ollama, or a provider of yours whose
    /// address is on this computer.
    pub fn local(&self) -> bool {
        is_local(&self.provider)
            || self
                .base_url
                .as_deref()
                .and_then(host_and_port)
                .is_some_and(|(host, _)| matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"))
    }
}

/// Whether OpenCode routes `provider` to a runtime on this computer.
pub fn is_local(provider: &str) -> bool {
    runtime(provider).is_some()
}

/// What the picker calls a provider over its group of models: the runtime's own name, OpenCode
/// Zen for OpenCode's own models, else OpenCode's provider id as it is.
pub fn provider_heading(provider: &str) -> String {
    if provider == ZEN {
        return "OpenCode Zen".to_owned();
    }
    runtime(provider).map_or_else(|| provider.to_owned(), |runtime| runtime.name.to_owned())
}

/// `opencode api model.list`'s answer, in its order. A blank answer is an empty list.
pub fn parse_model_list(printed: &str) -> Result<Vec<ListedModel>, String> {
    if printed.trim().is_empty() {
        return Ok(Vec::new());
    }
    let answer: Value = serde_json::from_str(printed.trim())
        .map_err(|e| format!("OpenCode's model list isn't JSON: {e}"))?;
    let list = match &answer {
        Value::Array(list) => list,
        Value::Object(fields) => match fields.get("data") {
            Some(Value::Array(list)) => list,
            _ => return Err("OpenCode's model list has no `data` list".to_owned()),
        },
        _ => return Err("OpenCode's model list is neither a list nor an object".to_owned()),
    };
    Ok(list.iter().filter_map(listed).collect())
}

/// `opencode api model.default`'s answer: the route Default runs, or `None` when OpenCode
/// names none.
pub fn parse_model_default(printed: &str) -> Result<Option<String>, String> {
    if printed.trim().is_empty() {
        return Ok(None);
    }
    let answer: Value = serde_json::from_str(printed.trim())
        .map_err(|e| format!("OpenCode's default model isn't JSON: {e}"))?;
    let info = match &answer {
        Value::Object(fields) if fields.contains_key("data") => &fields["data"],
        other => other,
    };
    Ok(listed(info).map(|model| model.route))
}

/// `opencode models`: one `provider/model` a line, with nothing but the ids.
pub fn parse_model_lines(printed: &str) -> Vec<ListedModel> {
    let mut models: Vec<ListedModel> = Vec::new();
    for line in without_colour(printed).lines() {
        let route = line.trim();
        let Some((provider, id)) = route.split_once('/') else {
            continue;
        };
        if provider.is_empty() || id.is_empty() || route.contains(char::is_whitespace) {
            continue;
        }
        if models.iter().any(|model| model.route == route) {
            continue;
        }
        models.push(ListedModel {
            route: route.to_owned(),
            provider: provider.to_owned(),
            name: id.to_owned(),
            context: None,
            // Nothing says otherwise, and OpenCode's own default is that a model has tools.
            tools: true,
            variants: Vec::new(),
            released: 0,
            free: false,
            price: None,
            base_url: None,
            deprecated: false,
        });
    }
    models
}

fn listed(info: &Value) -> Option<ListedModel> {
    let provider = info["providerID"].as_str()?.trim();
    let id = info["id"]
        .as_str()
        .or_else(|| info["modelID"].as_str())?
        .trim();
    if provider.is_empty() || id.is_empty() || info["enabled"] == Value::Bool(false) {
        return None;
    }
    let costs = info["cost"].as_array();
    let free = costs.is_some_and(|tiers| {
        !tiers.is_empty()
            && tiers.iter().all(|tier| {
                ["input", "output"]
                    .iter()
                    .all(|side| tier[*side].as_f64().unwrap_or(0.0) == 0.0)
            })
    });
    let variants = info["variants"]
        .as_array()
        .map(|variants| {
            variants
                .iter()
                .filter_map(|variant| variant["id"].as_str().or_else(|| variant.as_str()))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Some(ListedModel {
        route: format!("{provider}/{id}"),
        provider: provider.to_owned(),
        name: info["name"].as_str().unwrap_or(id).trim().to_owned(),
        context: info["limit"]["context"]
            .as_u64()
            .filter(|tokens| *tokens > 0),
        tools: info["capabilities"]["tools"].as_bool().unwrap_or(true),
        variants,
        released: info["time"]["released"].as_i64().unwrap_or(0),
        free,
        price: price(&info["cost"]),
        base_url: info["settings"]["baseURL"].as_str().map(str::to_owned),
        deprecated: info["status"].as_str() == Some("deprecated"),
    })
}

/// OpenCode's price, in dollars per million tokens, from the first of its tiers.
fn price(tiers: &Value) -> Option<CostModel> {
    let tier = match tiers {
        Value::Array(tiers) => tiers.first()?,
        tier => tier,
    };
    let per_mtok = |value: &Value| value.as_f64().filter(|p| p.is_finite() && *p >= 0.0);
    let input = per_mtok(&tier["input"])?;
    Some(CostModel {
        unit: CostUnit::Usd,
        input_per_mtok: Some(input),
        output_per_mtok: per_mtok(&tier["output"]),
        cached_input_per_mtok: per_mtok(&tier["cache"]["read"]),
    })
}

/// The list and the model Default runs, as OpenCode answered them.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenCodeModels {
    pub listing: Listing<ListedModel>,
    pub default: Option<String>,
}

/// Asks a private OpenCode server in `repo` for its models, one server kept for the wait while
/// its providers load, as [`ColdStart`] says, then for the model Default runs. Where
/// `opencode api` fails, the line form of `opencode models` gives the ids alone. Whatever
/// fails, `last` stands.
pub async fn ask(
    server: &PrivateServer,
    repo: &Path,
    wait: ColdStart,
    last: Vec<ListedModel>,
) -> OpenCodeModels {
    let kept = match server.keep(wait.up_to).await {
        Ok(kept) => kept,
        Err(why) => {
            return OpenCodeModels {
                listing: Listing::Failed { kept: last, why },
                default: None,
            };
        }
    };
    let location = repo.display().to_string();
    let call = ApiCall::operation(MODEL_LIST).param("location[directory]", &location);
    let listing = kept.list(&call, parse_model_list, wait, last).await;
    let listing = match listing {
        Listing::Failed { kept: last, why } => match lines_fallback(&kept, wait).await {
            Some(models) if !models.is_empty() => Listing::Listed(models),
            _ => Listing::Failed { kept: last, why },
        },
        other => other,
    };
    let default = if listing.is_fresh() {
        let call = ApiCall::operation(MODEL_DEFAULT).param("location[directory]", &location);
        kept.ask(&call, wait.up_to)
            .await
            .ok()
            .and_then(|printed| parse_model_default(&printed).ok().flatten())
    } else {
        None
    };
    kept.stop().await;
    OpenCodeModels { listing, default }
}

async fn lines_fallback(server: &KeptServer<'_>, wait: ColdStart) -> Option<Vec<ListedModel>> {
    let printed = server.command(&["models"], wait.up_to).await.ok()?;
    Some(parse_model_lines(&printed))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of `opencode api model.list` on 2.0.32: a Zen model with variants and a free
    /// tier, a tag OpenCode found by itself, and one your config names.
    const LISTED: &str = r#"{"location":{"directory":"/repo"},"data":[
      {"id":"deepseek-v4.1-flash-free","modelID":"deepseek-v4.1-flash-free","providerID":"opencode",
       "name":"DeepSeek V4.1 Flash Free","capabilities":{"tools":true,"input":["text"],"output":["text"]},
       "variants":[{"id":"low"},{"id":"high"},{"id":"max"}],"time":{"released":1788998400000},
       "cost":[{"input":0,"output":0,"cache":{"read":0,"write":0}}],"status":"active","enabled":true,
       "limit":{"context":200000,"output":32000}},
      {"id":"gemma3:12b-32k","modelID":"gemma3:12b-32k","providerID":"ollama","name":"gemma3:12b-32k",
       "capabilities":{"tools":false,"input":["text"],"output":["text"]},"variants":[],
       "time":{"released":0},"cost":[],"status":"active","enabled":true,
       "limit":{"context":131072,"output":32000}},
      {"id":"qwen3-coder:30b-64k","modelID":"qwen3-coder:30b-64k","providerID":"ollama",
       "name":"Qwen3 Coder 30B (64k)","capabilities":{"tools":true,"input":["text"],"output":["text"]},
       "variants":[],"time":{"released":0},"cost":[],"status":"active","enabled":true,
       "limit":{"context":65536,"output":32000}}
    ]}"#;

    #[test]
    fn a_listed_model_keeps_its_name_window_tools_variants_and_cost() {
        let models = parse_model_list(LISTED).unwrap();
        assert_eq!(models.len(), 3);
        let zen = &models[0];
        assert_eq!(zen.route, "opencode/deepseek-v4.1-flash-free");
        assert_eq!(zen.own_name(), Some("DeepSeek V4.1 Flash Free"));
        assert_eq!(zen.context, Some(200_000));
        assert_eq!(zen.variants, ["low", "high", "max"]);
        assert!(zen.free && zen.tools && !zen.local());
        assert_eq!(zen.released, 1_788_998_400_000);

        let found = &models[1];
        assert_eq!(
            found.own_name(),
            None,
            "a tag OpenCode found has only its id"
        );
        assert!(!found.tools && found.local() && !found.free);
        assert!(found.variants.is_empty());
        assert_eq!(models[2].own_name(), Some("Qwen3 Coder 30B (64k)"));
    }

    #[test]
    fn a_provider_of_yours_on_this_computer_is_local_and_a_priced_one_keeps_its_price() {
        let answer = r#"{"data":[
          {"id":"qwen3-8b","providerID":"my-box","name":"Qwen3 8B",
           "settings":{"baseURL":"http://127.0.0.1:8080/v1"},"cost":[]},
          {"id":"glm-5","providerID":"openrouter","name":"GLM 5",
           "settings":{"baseURL":"https://openrouter.ai/api/v1"},
           "cost":[{"input":0.6,"output":2.2,"cache":{"read":0.11}}]}
        ]}"#;
        let models = parse_model_list(answer).unwrap();
        assert!(models[0].local() && models[0].price.is_none());
        assert!(!models[1].local() && !models[1].free);
        let price = models[1].price.as_ref().unwrap();
        assert_eq!(price.unit, CostUnit::Usd);
        assert_eq!(price.input_per_mtok, Some(0.6));
        assert_eq!(price.output_per_mtok, Some(2.2));
        assert_eq!(price.cached_input_per_mtok, Some(0.11));
    }

    #[test]
    fn an_empty_answer_lists_nothing() {
        assert_eq!(parse_model_list("").unwrap(), Vec::new());
        assert_eq!(
            parse_model_list(r#"{"location":{},"data":[]}"#).unwrap(),
            Vec::new()
        );
        assert!(parse_model_list("not json").is_err());
    }

    #[test]
    fn the_default_is_the_route_of_the_model_it_names() {
        let answer = r#"{"location":{},"data":{"id":"nemotron-3.5-super-vl-free",
            "providerID":"opencode","name":"Nemotron 3.5 Super VL Free"}}"#;
        assert_eq!(
            parse_model_default(answer).unwrap().as_deref(),
            Some("opencode/nemotron-3.5-super-vl-free")
        );
        assert_eq!(
            parse_model_default(r#"{"location":{},"data":null}"#).unwrap(),
            None
        );
    }

    #[test]
    fn the_line_form_gives_the_ids_alone() {
        let printed =
            "\u{1b}[1mopencode/big-pickle\u{1b}[0m\nollama/llama3.2:latest\n\nnot a route\n";
        let models = parse_model_lines(printed);
        let routes: Vec<&str> = models.iter().map(|model| model.route.as_str()).collect();
        assert_eq!(routes, ["opencode/big-pickle", "ollama/llama3.2:latest"]);
        assert_eq!(models[1].own_name(), None);
    }

    /// A fake `opencode` that answers `api model.list` with an empty list `empty` times and
    /// then with [`LISTED`], `api model.default` with Nemotron, and `models` with two ids.
    /// With `no_api`, every `api` call fails, as on an OpenCode without the command.
    fn fake_opencode(dir: &Path, empty: u32, no_api: bool) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(dir.join("listed.json"), LISTED).unwrap();
        let program = dir.join("opencode");
        let fail = if no_api { "1" } else { "0" };
        let script = format!(
            "#!/bin/sh\n\
             echo \"$*\" >> calls.txt\n\
             case \"$*\" in\n\
             'serve --stdio --port 0') echo '{{\"url\":\"http://127.0.0.1:9\"}}'; cat > /dev/null ;;\n\
             'api --server http://127.0.0.1:9 model.list'*)\n\
               [ {fail} = 1 ] && {{ echo 'unknown command' >&2; exit 1; }}\n\
               n=$(grep -c model.list calls.txt)\n\
               if [ \"$n\" -gt {empty} ]; then cat listed.json; else echo '{{\"data\":[]}}'; fi ;;\n\
             'api --server http://127.0.0.1:9 model.default'*)\n\
               echo '{{\"data\":{{\"id\":\"nemotron-3.5-super-vl-free\",\"providerID\":\"opencode\"}}}}' ;;\n\
             'models --server http://127.0.0.1:9')\n\
               printf 'ollama/llama3.2:latest\\nopencode/big-pickle\\n' ;;\n\
             *) exit 2 ;;\n\
             esac\n"
        );
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        program
    }

    const SOON: ColdStart = ColdStart {
        every: std::time::Duration::from_millis(5),
        up_to: std::time::Duration::from_secs(5),
    };

    #[tokio::test]
    async fn a_cold_server_is_asked_until_it_lists_then_for_its_default() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_opencode(dir.path(), 2, false);
        let server = PrivateServer::new(&program, Some("2.0.32"), dir.path())
            .confined(std::sync::Arc::new(argonauts_core::confine::Unconfined));
        let answer = ask(&server, Path::new("/repo"), SOON, Vec::new()).await;
        let Listing::Listed(models) = &answer.listing else {
            panic!("{answer:?}");
        };
        assert_eq!(models.len(), 3);
        assert_eq!(
            answer.default.as_deref(),
            Some("opencode/nemotron-3.5-super-vl-free")
        );
        let calls = std::fs::read_to_string(dir.path().join("calls.txt")).unwrap();
        assert!(
            calls.contains(
                "api --server http://127.0.0.1:9 model.list --param location[directory]=/repo"
            ),
            "{calls}"
        );
        assert_eq!(calls.matches("model.list").count(), 4, "{calls}");
        assert_eq!(calls.matches("serve --stdio").count(), 1, "{calls}");
    }

    #[tokio::test]
    async fn without_opencode_api_the_line_form_gives_the_ids() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_opencode(dir.path(), 0, true);
        let server = PrivateServer::new(&program, Some("2.0.32"), dir.path())
            .confined(std::sync::Arc::new(argonauts_core::confine::Unconfined));
        let answer = ask(&server, Path::new("/repo"), SOON, Vec::new()).await;
        let routes: Vec<String> = answer
            .listing
            .into_list()
            .into_iter()
            .map(|model| model.route)
            .collect();
        assert_eq!(routes, ["ollama/llama3.2:latest", "opencode/big-pickle"]);
    }

    #[test]
    fn providers_are_headed_by_their_own_names() {
        assert_eq!(provider_heading("ollama"), "Ollama");
        assert_eq!(provider_heading("opencode"), "OpenCode Zen");
        assert_eq!(provider_heading("lmstudio"), "LM Studio");
        assert_eq!(provider_heading("openrouter"), "openrouter");
    }
}
