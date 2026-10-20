//! What this machine gives models, and what each local model takes of it, for `/model`. Read
//! from the system's own counts and from Ollama's `/api/ps`, `/api/tags` and `/api/show`,
//! none of which loads or runs a model.
//!
//! The most models may use is a fixed ceiling: on Apple silicon what macOS lets the GPU use,
//! the figure Ollama reads too (51.8 GiB on a 64 GB Mac); on a PC with an NVIDIA graphics
//! board, that board's memory; else the machine's memory. A model takes its weights plus what its window
//! needs, two bytes for each key and value of every layer's heads for every token, until it
//! has loaded once, when Ollama's own figure counts instead. That figure is remembered in
//! `~/.argonauts/model-ram.json`, so it still counts once the model unloads.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use argonauts_core::model_picker::MachineMemory;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The system's counts answer at once.
const ASK_TIMEOUT: Duration = Duration::from_secs(5);

/// One tag Ollama has, as `/api/tags` and `/api/show` describe it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Tag {
    /// Its weights, in bytes: what it takes on disk and in memory before its window.
    pub size: u64,
    /// Bytes each token of its window takes, from its layers and heads.
    pub per_token: Option<u64>,
    /// It can think before it answers, as its capabilities say.
    pub thinking: bool,
}

/// What a loaded tag holds, and the window it was loaded with where Ollama says.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Held {
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
}

/// What Ollama said of the models on this machine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Ollama {
    /// By tag name, such as `qwen3-coder:30b-64k`.
    pub tags: BTreeMap<String, Tag>,
    /// The loaded ones, by tag name, with what each holds.
    pub loaded: BTreeMap<String, Held>,
    /// What tags held the last time they were loaded, from [`MeasuredRam`].
    pub measured: BTreeMap<String, Held>,
}

impl Ollama {
    /// What `tag` takes at `window`: what it holds when loaded, else what it held when it last
    /// loaded at that window, else its weights and what the window needs. `None` for a tag
    /// Ollama doesn't have.
    pub(crate) fn ram(&self, tag: &str, window: Option<u64>) -> Option<u64> {
        if let Some(held) = self.loaded.get(tag) {
            return Some(held.bytes);
        }
        let known = self.tags.get(tag)?;
        let at_this_window =
            |held: &&Held| held.window.is_none() || window.is_none_or(|w| held.window == Some(w));
        if let Some(held) = self.measured.get(tag).filter(at_this_window) {
            return Some(held.bytes);
        }
        let window = window.unwrap_or(0);
        Some(known.size + known.per_token.unwrap_or(0) * window)
    }

    pub(crate) fn in_use(&self) -> u64 {
        self.loaded.values().map(|held| held.bytes).sum()
    }
}

/// What each Ollama tag held the last time it was seen loaded, by server, in
/// `~/.argonauts/model-ram.json`. One reading's loaded tags replace what was kept for them,
/// and a tag the server no longer has is forgotten.
#[derive(Debug, Clone)]
pub(crate) struct MeasuredRam {
    path: PathBuf,
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
struct Measurements {
    #[serde(default)]
    servers: BTreeMap<String, BTreeMap<String, Held>>,
}

impl MeasuredRam {
    pub(crate) fn in_home(home: &Path) -> Self {
        Self {
            path: home.join("model-ram.json"),
        }
    }

    /// Keeps what `ollama`'s loaded tags hold now, and gives it what the others held when they
    /// last loaded. A record that can't be read or written only means estimates, never a
    /// failed reading.
    pub(crate) fn note(&self, server: &str, ollama: &mut Ollama) {
        let mut kept: Measurements = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let before = kept.servers.get(server).cloned().unwrap_or_default();
        let mut now = before.clone();
        now.retain(|tag, _| ollama.tags.contains_key(tag));
        now.extend(ollama.loaded.iter().map(|(tag, held)| (tag.clone(), *held)));
        if now != before {
            kept.servers.insert(server.to_owned(), now.clone());
            kept.servers.retain(|_, tags| !tags.is_empty());
            if let Err(err) = self.write(&kept) {
                tracing::debug!("couldn't keep what the models held: {err}");
            }
        }
        ollama.measured = now;
    }

    fn write(&self, kept: &Measurements) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(kept).map_err(std::io::Error::other)?;
        let fresh = self.path.with_extension("json.tmp");
        std::fs::write(&fresh, text)?;
        std::fs::rename(&fresh, &self.path)
    }
}

/// `/api/tags`: each tag's name and size.
pub(crate) fn parse_tags(json: &str) -> BTreeMap<String, Tag> {
    let Ok(answer) = serde_json::from_str::<Value>(json) else {
        return BTreeMap::new();
    };
    answer["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let name = model["name"].as_str().or_else(|| model["model"].as_str())?;
            let size = model["size"].as_u64()?;
            Some((
                name.to_owned(),
                Tag {
                    size,
                    ..Tag::default()
                },
            ))
        })
        .collect()
}

/// `/api/ps`: the loaded tags, what each holds and the window it was loaded with.
pub(crate) fn parse_ps(json: &str) -> BTreeMap<String, Held> {
    let Ok(answer) = serde_json::from_str::<Value>(json) else {
        return BTreeMap::new();
    };
    answer["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let name = model["name"].as_str().or_else(|| model["model"].as_str())?;
            let held = Held {
                bytes: model["size"].as_u64()?,
                window: model["context_length"].as_u64().filter(|n| *n > 0),
            };
            Some((name.to_owned(), held))
        })
        .collect()
}

/// `/api/show`: the bytes a token of the window takes, from the architecture's layer count,
/// key and value heads and their lengths, and whether the tag thinks.
pub(crate) fn parse_show(json: &str) -> (Option<u64>, bool) {
    let Ok(answer) = serde_json::from_str::<Value>(json) else {
        return (None, false);
    };
    let thinking = answer["capabilities"]
        .as_array()
        .is_some_and(|capabilities| capabilities.iter().any(|c| c == "thinking"));
    (per_token(&answer["model_info"]), thinking)
}

/// The bytes one token of the window takes: a key and a value for every key-value head of
/// every layer, two bytes a number, as Ollama keeps the cache at f16.
fn per_token(info: &Value) -> Option<u64> {
    let arch = info["general.architecture"].as_str()?;
    let field = |name: &str| info[format!("{arch}.{name}")].as_u64();
    let layers = field("block_count")?;
    let heads = field("attention.head_count")?;
    let kv_heads = field("attention.head_count_kv").unwrap_or(heads);
    let head = field("embedding_length").map(|width| width / heads.max(1));
    let key = field("attention.key_length").or(head)?;
    let value = field("attention.value_length").unwrap_or(key);
    Some(layers * kv_heads * (key + value) * 2)
}

/// Asks the Ollama at `base_url` for its tags, the loaded ones and each tag's shape.
pub(crate) async fn ask_ollama(base_url: &str) -> Option<Ollama> {
    let client = reqwest::Client::builder()
        .timeout(ASK_TIMEOUT)
        .build()
        .ok()?;
    let base = base_url.trim_end_matches('/').trim_end_matches("/v1");
    let get = |path: &str| client.get(format!("{base}{path}")).send();
    let tags = get("/api/tags").await.ok()?.text().await.ok()?;
    let mut tags = parse_tags(&tags);
    let loaded = match get("/api/ps").await {
        Ok(answer) => parse_ps(&answer.text().await.unwrap_or_default()),
        Err(_) => BTreeMap::new(),
    };
    for (name, tag) in &mut tags {
        let shown = client
            .post(format!("{base}/api/show"))
            .json(&serde_json::json!({ "model": name }))
            .send()
            .await;
        if let Ok(shown) = shown {
            let (per_token, thinking) = parse_show(&shown.text().await.unwrap_or_default());
            tag.per_token = per_token;
            tag.thinking = thinking;
        }
    }
    Some(Ollama {
        tags,
        loaded,
        ..Ollama::default()
    })
}

/// This machine's memory and the most models may use, with what `in_use` holds now.
pub(crate) async fn machine(in_use: u64) -> Option<MachineMemory> {
    if cfg!(target_os = "macos") {
        let total = sysctl("hw.memsize").await?;
        let wired_mb = sysctl("iogpu.wired_limit_mb").await.filter(|mb| *mb > 0);
        let limit = wired_mb
            .map(|mb| mb * 1024 * 1024)
            .or_else(gpu_working_set)
            .unwrap_or(total);
        Some(MachineMemory {
            limit: limit.min(total),
            total,
            in_use,
            free: vm_free().await.unwrap_or(0),
            mac: true,
        })
    } else {
        let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
        let kib = |field: &str| {
            meminfo
                .lines()
                .find_map(|line| line.strip_prefix(field)?.split_whitespace().next())
                .and_then(|value| value.parse::<u64>().ok())
                .map(|kib| kib * 1024)
        };
        let total = kib("MemTotal:")?;
        Some(MachineMemory {
            limit: nvidia_memory().await.unwrap_or(total),
            total,
            in_use,
            free: kib("MemAvailable:").unwrap_or(0),
            mac: false,
        })
    }
}

async fn output(program: &str, args: &[&str]) -> Option<String> {
    let run = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    let printed = tokio::time::timeout(ASK_TIMEOUT, run).await.ok()?.ok()?;
    printed
        .status
        .success()
        .then(|| String::from_utf8_lossy(&printed.stdout).into_owned())
}

async fn sysctl(name: &str) -> Option<u64> {
    output("/usr/sbin/sysctl", &["-n", name])
        .await?
        .trim()
        .parse()
        .ok()
}

/// What `vm_stat` counts as free, inactive, speculative or purgeable: what other apps leave.
async fn vm_free() -> Option<u64> {
    let printed = output("/usr/bin/vm_stat", &[]).await?;
    let page = printed
        .lines()
        .next()?
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    let pages = |label: &str| {
        printed
            .lines()
            .find_map(|line| line.strip_prefix(label))
            .and_then(|rest| rest.trim().trim_end_matches('.').parse::<u64>().ok())
            .unwrap_or(0)
    };
    let free = [
        "Pages free:",
        "Pages inactive:",
        "Pages speculative:",
        "Pages purgeable:",
    ]
    .iter()
    .map(|label| pages(label))
    .sum::<u64>();
    Some(free * page)
}

/// The memory of the first NVIDIA GPU, where `nvidia-smi` is installed.
async fn nvidia_memory() -> Option<u64> {
    let printed = output(
        "nvidia-smi",
        &["--query-gpu=memory.total", "--format=csv,noheader,nounits"],
    )
    .await?;
    let mib: u64 = printed.lines().next()?.trim().parse().ok()?;
    Some(mib * 1024 * 1024)
}

/// What macOS lets the GPU use, as Metal's `recommendedMaxWorkingSetSize` says. `None` off
/// Apple silicon, or where Metal can't be loaded.
#[cfg(target_os = "macos")]
fn gpu_working_set() -> Option<u64> {
    use std::ffi::{c_char, c_void};

    use nix::libc::{RTLD_LAZY, dlopen, dlsym};

    type Create = unsafe extern "C" fn() -> *mut c_void;
    type Register = unsafe extern "C" fn(*const c_char) -> *const c_void;
    type SendU64 = unsafe extern "C" fn(*mut c_void, *const c_void) -> u64;
    type SendVoid = unsafe extern "C" fn(*mut c_void, *const c_void);

    // SAFETY: the two system libraries are loaded by their fixed paths and only looked up by
    // name; each symbol is checked for null before it's called with the signature Apple
    // documents for it. The device Metal creates is released once its one number is read.
    unsafe {
        let metal = dlopen(
            c"/System/Library/Frameworks/Metal.framework/Metal".as_ptr(),
            RTLD_LAZY,
        );
        let objc = dlopen(c"/usr/lib/libobjc.A.dylib".as_ptr(), RTLD_LAZY);
        if metal.is_null() || objc.is_null() {
            return None;
        }
        let create = dlsym(metal, c"MTLCreateSystemDefaultDevice".as_ptr());
        let register = dlsym(objc, c"sel_registerName".as_ptr());
        let send = dlsym(objc, c"objc_msgSend".as_ptr());
        if create.is_null() || register.is_null() || send.is_null() {
            return None;
        }
        let create: Create = std::mem::transmute(create);
        let register: Register = std::mem::transmute(register);
        let device = create();
        if device.is_null() {
            return None;
        }
        let working_set = register(c"recommendedMaxWorkingSetSize".as_ptr());
        let bytes = std::mem::transmute::<*mut c_void, SendU64>(send)(device, working_set);
        let release = register(c"release".as_ptr());
        std::mem::transmute::<*mut c_void, SendVoid>(send)(device, release);
        (bytes > 0).then_some(bytes)
    }
}

#[cfg(not(target_os = "macos"))]
fn gpu_working_set() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/api/show` for qwen3-coder:30b on Ollama: 48 layers with 4 key and value heads of 128.
    const QWEN_SHOW: &str = r#"{"capabilities":["completion","tools"],"model_info":{
        "general.architecture":"qwen3moe","qwen3moe.block_count":48,
        "qwen3moe.attention.head_count":32,"qwen3moe.attention.head_count_kv":4,
        "qwen3moe.attention.key_length":128,"qwen3moe.attention.value_length":128,
        "qwen3moe.embedding_length":2048}}"#;

    #[test]
    fn a_window_takes_two_bytes_a_key_and_value_of_every_head() {
        let (per_token, thinking) = parse_show(QWEN_SHOW);
        assert_eq!(per_token, Some(48 * 4 * 256 * 2));
        assert!(!thinking);
        let (_, thinks) = parse_show(r#"{"capabilities":["completion","thinking"]}"#);
        assert!(thinks);
    }

    #[test]
    fn a_tag_takes_its_weights_and_its_window_until_it_has_loaded() {
        let tags = parse_tags(
            r#"{"models":[{"name":"qwen3-coder:30b","size":18556699283},
                          {"name":"qwen3-coder:30b-64k","size":18556699283}]}"#,
        );
        let loaded = parse_ps(r#"{"models":[{"name":"qwen3-coder:30b-64k","size":26843545600}]}"#);
        let mut ollama = Ollama {
            tags,
            loaded,
            ..Ollama::default()
        };
        for tag in ollama.tags.values_mut() {
            tag.per_token = parse_show(QWEN_SHOW).0;
        }
        let gib = |bytes: u64| (bytes as f64 / (1u64 << 30) as f64).round() as u64;
        // At 256k: 17.3 GiB of weights and 24 GiB of cache.
        assert_eq!(
            ollama.ram("qwen3-coder:30b", Some(262_144)).map(gib),
            Some(41)
        );
        assert_eq!(
            ollama.ram("qwen3-coder:30b-64k", Some(65_536)).map(gib),
            Some(25),
            "Ollama's own figure once it has loaded"
        );
        assert_eq!(ollama.in_use(), 26_843_545_600);
        assert_eq!(ollama.ram("llama3.2:latest", Some(131_072)), None);
    }

    #[test]
    fn a_tag_keeps_what_it_held_once_it_unloads() {
        let home = tempfile::tempdir().unwrap();
        let measured = MeasuredRam::in_home(home.path());
        let server = "http://127.0.0.1:11434";
        let tags = parse_tags(
            r#"{"models":[{"name":"qwen3-coder:30b-64k","size":18556699283},
                          {"name":"gemma3:12b-32k","size":8149190253}]}"#,
        );
        let reading = |loaded: &str| Ollama {
            tags: tags.clone(),
            loaded: parse_ps(loaded),
            ..Ollama::default()
        };
        let mut loaded = reading(
            r#"{"models":[{"name":"qwen3-coder:30b-64k","size":26843545600,
                           "context_length":65536}]}"#,
        );
        measured.note(server, &mut loaded);

        let mut unloaded = reading(r#"{"models":[]}"#);
        measured.note(server, &mut unloaded);
        assert_eq!(
            unloaded.ram("qwen3-coder:30b-64k", Some(65_536)),
            Some(26_843_545_600),
            "Ollama's own figure, though it isn't loaded now"
        );
        assert_eq!(
            unloaded.ram("qwen3-coder:30b-64k", Some(32_768)),
            Some(18_556_699_283),
            "at another window it's an estimate again"
        );
        assert_eq!(unloaded.in_use(), 0);

        // Another server keeps its own, and a tag its server dropped is forgotten.
        let mut elsewhere = reading(r#"{"models":[]}"#);
        measured.note("http://gpu-box:11434", &mut elsewhere);
        assert!(elsewhere.measured.is_empty());
        let mut removed = Ollama {
            tags: parse_tags(r#"{"models":[{"name":"gemma3:12b-32k","size":8149190253}]}"#),
            ..Ollama::default()
        };
        measured.note(server, &mut removed);
        assert!(removed.measured.is_empty());
        let mut again = reading(r#"{"models":[]}"#);
        measured.note(server, &mut again);
        assert_eq!(
            again.ram("qwen3-coder:30b-64k", Some(65_536)),
            Some(18_556_699_283)
        );
    }

    #[test]
    fn a_record_that_cant_be_read_leaves_estimates() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("model-ram.json"), "not json").unwrap();
        let measured = MeasuredRam::in_home(home.path());
        let mut ollama = Ollama {
            tags: parse_tags(r#"{"models":[{"name":"llama3.2:latest","size":2019393189}]}"#),
            ..Ollama::default()
        };
        measured.note("http://127.0.0.1:11434", &mut ollama);
        assert_eq!(ollama.ram("llama3.2:latest", None), Some(2_019_393_189));
    }

    #[test]
    fn answers_that_cant_be_read_give_nothing() {
        assert!(parse_tags("not json").is_empty());
        assert!(parse_ps("{}").is_empty());
        assert_eq!(parse_show("{}"), (None, false));
    }
}
