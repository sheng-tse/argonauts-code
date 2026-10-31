//! Made-up Ollama tags that hold a recorded stock model to a smaller window with `num_ctx`, as
//! anyone can make with a Modelfile. Each one is the stock tag's recorded answers with its name,
//! its parent and its window changed; nothing here was recorded.

use serde_json::Value;

/// A made-up tag made from `stock` with `num_ctx` set.
pub(super) struct Twin {
    pub tag: &'static str,
    pub stock: &'static str,
    pub num_ctx: u64,
    /// The name and window OpenCode's config gives it, if it names it at all.
    pub named_in_opencode: Option<(&'static str, u64)>,
}

pub(super) const TWINS: [Twin; 2] = [
    Twin {
        tag: "sample-coder:30b-64k",
        stock: "qwen3-coder:30b",
        num_ctx: 65_536,
        named_in_opencode: Some(("Sample Coder 30B (64k)", 65_536)),
    },
    Twin {
        tag: "sample-gemma:12b-32k",
        stock: "gemma3:12b",
        num_ctx: 32_768,
        named_in_opencode: None,
    },
];

/// `/api/tags` as recorded, with each twin listed after its stock tag.
pub(super) fn tags(recorded: &str) -> String {
    let mut tags: Value = serde_json::from_str(recorded).unwrap();
    let models = tags["models"].as_array_mut().unwrap();
    for twin in &TWINS {
        let at = position(models, |model| model["name"] == twin.stock);
        let mut made = models[at].clone();
        made["name"] = twin.tag.into();
        made["model"] = twin.tag.into();
        models.insert(at + 1, made);
    }
    tags.to_string()
}

/// The `/api/show` answers as recorded, keyed by tag, with each twin's answer added.
pub(super) fn show(recorded: &Value) -> Value {
    let mut show = recorded.clone();
    for twin in &TWINS {
        let mut made = recorded[twin.stock].clone();
        let parameters = made["parameters"].as_str().unwrap_or_default();
        made["parameters"] = format!("{parameters}\nnum_ctx {}", twin.num_ctx).into();
        made["details"]["parent_model"] = twin.stock.into();
        show[twin.tag] = made;
    }
    show
}

/// OpenCode's `model.list` as recorded, with each twin listed after its stock tag, named and
/// sized by OpenCode's config where it names it, and as OpenCode finds it by itself elsewhere.
pub(super) fn model_list(recorded: &str) -> String {
    let mut list: Value = serde_json::from_str(recorded).unwrap();
    let models = list["data"].as_array_mut().unwrap();
    for twin in &TWINS {
        let at = position(models, |model| {
            model["providerID"] == "ollama" && model["id"] == twin.stock
        });
        let mut made = models[at].clone();
        made["id"] = twin.tag.into();
        made["modelID"] = twin.tag.into();
        made["name"] = twin.tag.into();
        if let Some((name, window)) = twin.named_in_opencode {
            made["name"] = name.into();
            made["limit"]["context"] = window.into();
        }
        models.insert(at + 1, made);
    }
    list.to_string()
}

fn position(models: &[Value], is_it: impl Fn(&Value) -> bool) -> usize {
    models.iter().position(is_it).unwrap()
}
