//! Argonauts' record of the local models it uses, in `~/.argonauts/local-models.json`, and the
//! servers' own calls behind it. The app and every run host share the one file, under a lock.
//!
//! Each entry is one model on one server: whether Argonauts loaded it (the server didn't hold
//! it before the first request), who uses it now (each run, and each app's conversation, with
//! the process that will let go), and what the server said of it after the last session on it.
//! When the last holder ends, a model Argonauts loaded is freed, unless someone used it since or
//! it's pinned in memory. A holder whose process died no longer counts, and a model it alone
//! held is left to the server's own timer.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::PathBuf;
use std::time::Duration;

use argonauts_core::Paths;
use argonauts_core::agents::runtimes::{Runtime, RuntimeKind, runtime};
use argonauts_core::base::clock;
use argonauts_core::base::future::BoxFuture;
use argonauts_core::base::names::ModelRef;
use argonauts_core::base::paths::create_private_dir_all;
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::models::model_memory::{Freed, Hold, Holder, ModelMemory, Server};
use argonauts_endpoints::session::runtime_api::{self, Held};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::startup::ancestry::Processes;

const RECORD: &str = "local-models.json";

/// Each ask of a server waits at most this long. A load waits for the model to be read in.
const ASK_TIMEOUT: Duration = Duration::from_secs(3);
const LOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// How long a closing app waits for the servers to let go.
const CLOSING_WAIT: Duration = Duration::from_secs(3);

/// Ollama sets a model's expiry again on each request, from that request's own keep-alive.
/// Further than this from what it said right after Argonauts' last session, earlier or later,
/// someone else used the model since.
const SAME_EXPIRY_WITHIN: time::Duration = time::Duration::seconds(2);

/// An expiry this far off is Ollama's `keep_alive: -1`: you pinned the model.
const PINNED_AFTER: time::Duration = time::Duration::days(365);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Record {
    #[serde(default)]
    models: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Entry {
    server: Server,
    model: String,
    /// The server didn't hold it before Argonauts' first request for it.
    ours: bool,
    #[serde(default)]
    holders: Vec<Holding>,
    /// When the server would let it go by itself, as it said after the last session on it, in
    /// seconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Holding {
    key: String,
    /// 0 while a run waits for you with no host, which the next host of the run takes up.
    pid: u32,
    /// So a reused pid is never taken for the holder.
    start_time: u64,
}

impl Holding {
    fn is(&self, holder: &Holder) -> bool {
        self.key == holder.key && (self.pid == holder.pid || self.waiting())
    }

    fn waiting(&self) -> bool {
        self.pid == 0
    }
}

impl Entry {
    fn on(&self, server: &Server, model: &str) -> bool {
        self.server.base_url == server.base_url && same_model(server, &self.model, model)
    }
}

/// Ollama lists `llama3.2` as `llama3.2:latest`.
fn same_model(server: &Server, a: &str, b: &str) -> bool {
    let is_ollama = runtime(&server.endpoint).is_some_and(|r| r.kind == RuntimeKind::Ollama);
    let tagged = |name: &str| {
        if is_ollama && !name.contains(':') {
            format!("{name}:latest")
        } else {
            name.to_owned()
        }
    };
    tagged(a) == tagged(b)
}

pub(crate) struct ServerMemory {
    path: PathBuf,
}

impl ServerMemory {
    pub(crate) fn new(home: &Paths) -> Self {
        Self {
            path: home.home().join(RECORD),
        }
    }

    /// Reads, drops holders whose process ended, applies `change`, and writes back, all under
    /// an exclusive lock. A torn or foreign file reads as empty.
    fn update<T>(&self, change: impl FnOnce(&mut Record) -> T) -> std::io::Result<T> {
        if let Some(dir) = self.path.parent() {
            create_private_dir_all(dir)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&self.path)?;
        file.lock()?;
        let mut record = read(&mut file);
        let mut processes = Processes::new();
        for entry in &mut record.models {
            entry.holders.retain(|holding| {
                holding.waiting() || processes.start_time(holding.pid) == Some(holding.start_time)
            });
        }
        let changed = change(&mut record);
        let mut text = serde_json::to_vec_pretty(&record)?;
        text.push(b'\n');
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&text)?;
        Ok(changed)
    }

    /// `holder`, a run, waits for you and its host goes: it keeps its models for whichever
    /// host of the run carries on, which takes them up as it holds them or lets them go as the
    /// run ends.
    pub(crate) fn wait(&self, holder: &Holder) {
        let kept = self.update(|record| {
            for entry in &mut record.models {
                for holding in entry
                    .holders
                    .iter_mut()
                    .filter(|holding| holding.is(holder))
                {
                    holding.pid = 0;
                    holding.start_time = 0;
                }
            }
        });
        if let Err(err) = kept {
            tracing::warn!("reading {}: {err}", self.path.display());
        }
    }

    /// `holder` ended. It lets go of its models at once, so a holder that starts next never
    /// finds them half freed; then the ones nobody else uses that Argonauts loaded are freed.
    pub(crate) fn release(&self, holder: &Holder) -> BoxFuture<'static, Vec<Freed>> {
        let letting = self
            .update(|record| {
                let mut letting = Vec::new();
                record.models.retain_mut(|entry| {
                    let had = entry.holders.iter().any(|holding| holding.is(holder));
                    entry.holders.retain(|holding| !holding.is(holder));
                    if !entry.holders.is_empty() {
                        return true;
                    }
                    if had && entry.ours && entry.server.free_memory {
                        letting.push(entry.clone());
                    }
                    false
                });
                letting
            })
            .unwrap_or_else(|err| {
                tracing::warn!("reading {}: {err}", self.path.display());
                Vec::new()
            });
        Box::pin(async move {
            let mut freed = Vec::new();
            for entry in letting {
                if let Some(gone) = free_if_unused(&entry).await {
                    freed.push(gone);
                }
            }
            freed
        })
    }
}

/// This app's conversation ended, as `/clear` and `/resume` of another end it. It lets go of
/// its models at once, and those it loaded that nothing else uses are freed after, each said
/// with `say`.
pub(crate) fn conversation_ended(
    home: &Paths,
    names: DisplayNames,
    say: impl Fn(String) + Send + 'static,
) {
    let release = ServerMemory::new(home).release(&Holder::conversation());
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    runtime.spawn(async move {
        for freed in release.await {
            say(freed_line(&names, &freed));
        }
    });
}

/// The app is closing. Its conversation's models go as at `/clear`, but a run that carries on
/// after the app keeps the ones it uses, and nothing is said, since the screen has gone.
pub(crate) async fn app_closing(home: &Paths) {
    let release = ServerMemory::new(home).release(&Holder::conversation());
    if tokio::time::timeout(CLOSING_WAIT, release).await.is_err() {
        tracing::warn!("a local server was still freeing a model when the app closed");
    }
}

/// `Freed Sample Coder 30B (64k) from Ollama (25 GB of RAM)`, naming the model as the screens
/// do: by its endpoint's entry, else by OpenCode's route to it, else by its id.
pub(crate) fn freed_line(names: &DisplayNames, freed: &Freed) -> String {
    let endpoint = freed.server.endpoint.as_str();
    let route = format!("{endpoint}/{}", freed.model);
    let name = [
        (endpoint, freed.model.as_str()),
        ("opencode", route.as_str()),
    ]
    .into_iter()
    .find(|(cli, model)| {
        let id = ModelRef::new(*cli, Some((*model).to_owned()));
        names
            .catalog
            .entry(&id)
            .is_some_and(|entry| entry.name.is_some())
    })
    .and_then(|(cli, model)| names.model(cli, Some(model)));
    freed.words(name.as_deref())
}

fn read(file: &mut File) -> Record {
    let mut text = String::new();
    if file.read_to_string(&mut text).is_err() || text.trim().is_empty() {
        return Record::default();
    }
    serde_json::from_str(&text).unwrap_or_default()
}

fn token(server: &Server) -> Option<String> {
    let variable = server.api_key_env.as_deref()?;
    std::env::var(variable).ok().filter(|key| !key.is_empty())
}

fn client(timeout: Duration) -> Option<reqwest::Client> {
    runtime_api::local_client(timeout)
}

async fn held_now(runtime: &Runtime, server: &Server) -> Option<Vec<Held>> {
    let client = client(ASK_TIMEOUT)?;
    runtime_api::held(&client, runtime, &server.base_url, token(server).as_deref()).await
}

/// Asks the server again before freeing: a model already gone, pinned, or used by someone since
/// stays as it is.
async fn free_if_unused(entry: &Entry) -> Option<Freed> {
    let runtime = runtime(&entry.server.endpoint)?;
    let now = held_now(runtime, &entry.server).await?;
    let found = now
        .into_iter()
        .find(|held| same_model(&entry.server, &held.model, &entry.model))?;
    if let Some(expires) = found.expires_at {
        let pinned = expires - clock::now_utc() > PINNED_AFTER;
        let used_since = entry
            .expires_at
            .and_then(|seen| OffsetDateTime::from_unix_timestamp(seen).ok())
            .is_some_and(|seen| (expires - seen).abs() > SAME_EXPIRY_WITHIN);
        if pinned || used_since {
            return None;
        }
    }
    let client = client(ASK_TIMEOUT)?;
    let token = token(&entry.server);
    match runtime_api::free(
        &client,
        runtime,
        &entry.server.base_url,
        &found,
        token.as_deref(),
    )
    .await
    {
        Ok(true) => Some(Freed {
            server: entry.server.clone(),
            model: entry.model.clone(),
            bytes: found.bytes.or(entry.bytes),
        }),
        Ok(false) => None,
        Err(err) => {
            tracing::warn!("freeing {} on {}: {err}", entry.model, entry.server.name());
            None
        }
    }
}

impl ModelMemory for ServerMemory {
    fn hold<'a>(
        &'a self,
        server: &'a Server,
        model: &'a str,
        holder: &'a Holder,
    ) -> BoxFuture<'a, Hold> {
        Box::pin(async move {
            let Some(runtime) = runtime(&server.endpoint) else {
                return Hold::default();
            };
            let token = token(server);
            let slots = match (runtime.kind, client(ASK_TIMEOUT)) {
                (RuntimeKind::LlamaServer, Some(client)) => {
                    runtime_api::identify(&client, runtime, &server.base_url, token.as_deref())
                        .await
                        .and_then(|served| served.slots)
                }
                _ => None,
            };
            // What the server held before, noted ahead of the load, which would hide it.
            let before = held_now(runtime, server).await;
            if server.free_memory
                && let Some(before) = &before
            {
                let ours = !before
                    .iter()
                    .any(|held| same_model(server, &held.model, model));
                let start_time = Processes::new().start_time(holder.pid).unwrap_or(0);
                let holding = Holding {
                    key: holder.key.clone(),
                    pid: holder.pid,
                    start_time,
                };
                let noted = self.update(|record| {
                    match record
                        .models
                        .iter_mut()
                        .find(|entry| entry.on(server, model))
                    {
                        Some(entry) => {
                            entry
                                .holders
                                .retain(|held| !(held.waiting() && held.key == holding.key));
                            if !entry.holders.contains(&holding) {
                                entry.holders.push(holding);
                            }
                        }
                        None => record.models.push(Entry {
                            server: server.clone(),
                            model: model.to_owned(),
                            ours,
                            holders: vec![holding],
                            expires_at: None,
                            bytes: None,
                        }),
                    }
                });
                if let Err(err) = noted {
                    tracing::warn!("noting {model} in {}: {err}", self.path.display());
                }
            }
            let window = before
                .iter()
                .flatten()
                .find(|held| same_model(server, &held.model, model))
                .and_then(|held| held.window);
            Hold { window, slots }
        })
    }

    fn load<'a>(&'a self, server: &'a Server, model: &'a str) -> BoxFuture<'a, Option<u64>> {
        Box::pin(async move {
            let runtime = runtime(&server.endpoint)?;
            let client = client(LOAD_TIMEOUT)?;
            match runtime_api::load(&client, runtime, &server.base_url, model).await {
                Ok(loaded) => loaded.and_then(|held| held.window),
                Err(err) => {
                    tracing::warn!("loading {model} on {}: {err}", server.name());
                    None
                }
            }
        })
    }

    fn touch<'a>(&'a self, server: &'a Server, model: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let Some(runtime) = runtime(&server.endpoint) else {
                return;
            };
            if !server.free_memory {
                return;
            }
            let Some(now) = held_now(runtime, server).await else {
                return;
            };
            let found = now
                .into_iter()
                .find(|held| same_model(server, &held.model, model));
            let noted = self.update(|record| {
                if let Some(entry) = record
                    .models
                    .iter_mut()
                    .find(|entry| entry.on(server, model))
                {
                    entry.expires_at = found
                        .as_ref()
                        .and_then(|held| held.expires_at)
                        .map(OffsetDateTime::unix_timestamp);
                    entry.bytes = found.as_ref().and_then(|held| held.bytes).or(entry.bytes);
                }
            });
            if let Err(err) = noted {
                tracing::warn!("noting {model} in {}: {err}", self.path.display());
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::RunId;
    use argonauts_testkit::{FakeKind, FakeModelServer};

    use super::*;

    const QWEN: &str = "sample-coder:30b-64k";

    fn ollama(fake: &FakeModelServer) -> Server {
        Server {
            endpoint: "ollama".to_owned(),
            base_url: fake.base_url(),
            api_key_env: None,
            free_memory: true,
        }
    }

    fn memory() -> (tempfile::TempDir, ServerMemory) {
        let home = tempfile::tempdir().unwrap();
        let memory = ServerMemory::new(&Paths::new(home.path()));
        (home, memory)
    }

    fn run(id: &str) -> Holder {
        Holder::run(&RunId::new(id).unwrap())
    }

    fn qwen_server() -> FakeModelServer {
        let fake = FakeModelServer::start(FakeKind::Ollama, &[QWEN, "llama3.2:latest"]).unwrap();
        fake.set_load(25_311_000_000, 65_536);
        fake
    }

    #[tokio::test]
    async fn a_model_argonauts_loaded_is_freed_when_its_last_holder_ends() {
        let fake = qwen_server();
        let (_home, memory) = memory();
        let server = ollama(&fake);
        let (first, second) = (run("r-0001"), run("r-0002"));

        memory.hold(&server, QWEN, &first).await;
        assert_eq!(memory.load(&server, QWEN).await, Some(65_536));
        let again = memory.hold(&server, QWEN, &second).await;
        assert_eq!(again.window, Some(65_536));
        memory.touch(&server, QWEN).await;

        // Another run still uses it.
        assert!(memory.release(&first).await.is_empty());
        assert_eq!(fake.loaded().len(), 1);

        let freed = memory.release(&second).await;
        assert_eq!(freed.len(), 1);
        assert_eq!(
            freed[0].words(Some("Sample Coder 30B (64k)")),
            "Freed Sample Coder 30B (64k) from Ollama (25 GB of RAM)"
        );
        assert!(fake.loaded().is_empty());
        let unload = fake.requests().pop().unwrap();
        assert_eq!(unload.body["keep_alive"], 0);
    }

    #[tokio::test]
    async fn a_model_the_server_held_before_is_never_freed() {
        let fake = qwen_server();
        let (_home, memory) = memory();
        let server = ollama(&fake);
        let at = clock::now_utc() + time::Duration::minutes(5);
        fake.hold("llama3.2:latest", 23_779_979_264, at);
        let holder = run("r-0003");
        // Named without its tag, as a route may.
        memory.hold(&server, "llama3.2", &holder).await;
        memory.touch(&server, "llama3.2").await;
        assert!(memory.release(&holder).await.is_empty());
        assert_eq!(fake.loaded().len(), 1);
    }

    #[tokio::test]
    async fn a_model_used_since_or_pinned_is_left_to_its_timer() {
        let fake = qwen_server();
        let (_home, memory) = memory();
        let server = ollama(&fake);

        let holder = run("r-0004");
        memory.hold(&server, QWEN, &holder).await;
        memory.load(&server, QWEN).await;
        memory.touch(&server, QWEN).await;
        // Someone asked it something after Argonauts' last session: its expiry moved on.
        let later = clock::now_utc() + time::Duration::minutes(9);
        fake.hold(QWEN, 25_311_000_000, later);
        assert!(memory.release(&holder).await.is_empty());
        assert_eq!(fake.loaded().len(), 1);

        let fake = qwen_server();
        let server = ollama(&fake);
        let holder = run("r-0005");
        memory.hold(&server, QWEN, &holder).await;
        memory.load(&server, QWEN).await;
        let pinned = clock::now_utc() + time::Duration::days(100_000);
        fake.hold(QWEN, 25_311_000_000, pinned);
        assert!(memory.release(&holder).await.is_empty());
    }

    /// A program that used the model moments after Argonauts' last session, or asked with a
    /// shorter keep-alive, moved its expiry a little, or earlier: still used since.
    #[tokio::test]
    async fn any_move_of_the_expiry_is_someone_elses_use() {
        for moved in [time::Duration::seconds(5), time::Duration::minutes(-3)] {
            let fake = qwen_server();
            let (_home, memory) = memory();
            let server = ollama(&fake);
            let holder = run("r-0006");
            memory.hold(&server, QWEN, &holder).await;
            memory.load(&server, QWEN).await;
            let noted = clock::now_utc() + time::Duration::minutes(5);
            fake.hold(QWEN, 25_311_000_000, noted);
            memory.touch(&server, QWEN).await;
            fake.hold(QWEN, 25_311_000_000, noted + moved);
            assert!(memory.release(&holder).await.is_empty(), "{moved}");
            assert_eq!(fake.loaded().len(), 1, "{moved}");
        }
    }

    /// A run that waits for you keeps its model when its host goes, so another holder's end
    /// leaves it loaded, and the host that carries the run on to its end frees it.
    #[tokio::test]
    async fn a_run_that_waits_keeps_its_model_for_the_host_that_ends_it() {
        let fake = qwen_server();
        let (_home, memory) = memory();
        let server = ollama(&fake);
        let first_host = run("r-0010");
        memory.hold(&server, QWEN, &first_host).await;
        memory.load(&server, QWEN).await;
        memory.touch(&server, QWEN).await;
        memory.wait(&first_host);

        let conversation = Holder::conversation();
        memory.hold(&server, QWEN, &conversation).await;
        assert!(memory.release(&conversation).await.is_empty());
        assert_eq!(fake.loaded().len(), 1);

        // The run's next host is another process.
        let next_host = Holder {
            key: first_host.key.clone(),
            pid: u32::MAX - 2,
        };
        let freed = memory.release(&next_host).await;
        assert_eq!(freed.len(), 1, "{freed:?}");
        assert!(fake.loaded().is_empty());
    }

    #[tokio::test]
    async fn free_memory_off_or_a_dead_holder_frees_nothing() {
        let fake = qwen_server();
        let (_home, memory) = memory();
        let mut server = ollama(&fake);
        server.free_memory = false;
        let holder = run("r-0006");
        memory.hold(&server, QWEN, &holder).await;
        memory.load(&server, QWEN).await;
        assert!(memory.release(&holder).await.is_empty());
        assert_eq!(fake.loaded().len(), 1);

        // A run host that died never lets go itself, and its model waits for Ollama's timer.
        let fake = qwen_server();
        let server = ollama(&fake);
        let gone = Holder {
            key: "run r-0007".to_owned(),
            pid: u32::MAX - 1,
        };
        memory.hold(&server, QWEN, &gone).await;
        memory.load(&server, QWEN).await;
        let other = run("r-0008");
        memory.hold(&server, "llama3.2:latest", &other).await;
        memory.load(&server, "llama3.2:latest").await;
        let freed = memory.release(&other).await;
        assert_eq!(freed.len(), 1);
        assert_eq!(freed[0].model, "llama3.2:latest");
        assert_eq!(fake.loaded().len(), 1);
    }

    #[test]
    fn the_freed_line_names_the_model_as_opencode_lists_it() {
        use argonauts_core::models::model_catalog::{CatalogEntry, ModelCatalog};

        let mut named = CatalogEntry::new("opencode/ollama/sample-coder:30b-64k".parse().unwrap());
        named.name = Some("Sample Coder 30B (64k)".to_owned());
        let names = DisplayNames {
            catalog: ModelCatalog {
                models: vec![named],
                ..ModelCatalog::default()
            },
            ..DisplayNames::default()
        };
        let freed = |model: &str| Freed {
            server: Server {
                endpoint: "ollama".to_owned(),
                base_url: "http://localhost:11434/v1".to_owned(),
                api_key_env: None,
                free_memory: true,
            },
            model: model.to_owned(),
            bytes: Some(25_311_000_000),
        };
        assert_eq!(
            freed_line(&names, &freed(QWEN)),
            "Freed Sample Coder 30B (64k) from Ollama (25 GB of RAM)"
        );
        assert_eq!(
            freed_line(&names, &freed("sample-gemma:12b-32k")),
            "Freed Sample Gemma 12B 32K from Ollama (25 GB of RAM)"
        );
    }

    #[tokio::test]
    async fn a_vllm_server_keeps_its_model() {
        let fake = FakeModelServer::start(
            FakeKind::Vllm {
                max_model_len: 32768,
            },
            &["Qwen/Q"],
        )
        .unwrap();
        let (home, memory) = memory();
        let server = Server {
            endpoint: "vllm".to_owned(),
            ..ollama(&fake)
        };
        let holder = run("r-0009");
        memory.hold(&server, "Qwen/Q", &holder).await;
        assert!(memory.release(&holder).await.is_empty());
        let record = std::fs::read_to_string(home.path().join(RECORD)).unwrap_or_default();
        assert!(!record.contains("Qwen/Q"));
    }
}
