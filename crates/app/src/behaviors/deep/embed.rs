//! Turning summaries into vectors, so summaries that mean the same land together: an embedding
//! model through an OpenAI-style endpoint (nomic-embed-text on Ollama by default), or, with no
//! model, word vectors made from the summaries themselves.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use argonauts_core::future::BoxFuture;
use serde_json::json;

use super::linalg::{SparseRow, truncated_svd};

/// The embedding model auto picks, on Ollama.
pub(crate) const NOMIC: &str = "nomic-embed-text";

/// Its download, which is asked about first.
pub(crate) const NOMIC_MEGABYTES: u32 = 274;

/// The dimensions word vectors are reduced to.
pub(crate) const WORD_DIMENSIONS: usize = 60;

/// The embedding model of an OpenAI endpoint with your key, which auto takes without Ollama.
pub(crate) const OPENAI_EMBEDDING: &str = "text-embedding-3-small";

/// An `[endpoints.<id>]` table auto may embed with: OpenAI's API, with its key set and paid use
/// allowed (`allow_api_billing`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyedEndpoint {
    pub id: String,
    pub base_url: String,
    /// The environment variable holding the key.
    pub key_env: String,
}

impl KeyedEndpoint {
    pub fn find(settings: &argonauts_core::settings::Settings) -> Option<KeyedEndpoint> {
        settings.endpoints.iter().find_map(|(id, table)| {
            let base_url = table.base_url.clone()?;
            let key_env = table.api_key_env.clone()?;
            let openai = id == "openai" || base_url.contains("api.openai.com");
            let keyed = std::env::var(&key_env).is_ok_and(|key| !key.trim().is_empty());
            (openai && keyed && table.allow_api_billing && !table.local).then(|| KeyedEndpoint {
                id: id.clone(),
                base_url,
                key_env,
            })
        })
    }

    pub fn embedder(&self) -> Option<EmbeddingEndpoint> {
        let token = std::env::var(&self.key_env).ok();
        EmbeddingEndpoint::new(&self.id, &self.base_url, OPENAI_EMBEDDING, token, false)
    }
}

/// Summaries sent in one embedding request.
pub(crate) const PER_EMBEDDING_REQUEST: usize = 64;

/// A model that turns texts into vectors.
pub(crate) trait Embedder: Send + Sync {
    /// As the store keys its vectors, such as `ollama/nomic-embed-text`.
    fn id(&self) -> String;
    fn embed<'a>(&'a self, texts: &'a [String]) -> BoxFuture<'a, Result<Vec<Vec<f32>>, String>>;
}

/// An embedding model behind an OpenAI-style `/embeddings`.
pub(crate) struct EmbeddingEndpoint {
    /// The endpoint's or runtime's id, such as `ollama`.
    pub endpoint: String,
    pub model: String,
    /// Ends in `/v1`.
    pub base_url: String,
    pub token: Option<String>,
    client: reqwest::Client,
}

impl EmbeddingEndpoint {
    pub fn new(
        endpoint: &str,
        base_url: &str,
        model: &str,
        token: Option<String>,
        local: bool,
    ) -> Option<EmbeddingEndpoint> {
        let timeout = Duration::from_secs(120);
        let client = if local {
            argonauts_endpoints::runtime_api::local_client(timeout)?
        } else {
            reqwest::Client::builder().timeout(timeout).build().ok()?
        };
        Some(EmbeddingEndpoint {
            endpoint: endpoint.to_owned(),
            model: model.to_owned(),
            base_url: base_url.trim_end_matches('/').to_owned(),
            token,
            client,
        })
    }

    /// nomic-embed-text places texts for clustering best when it's told so.
    fn input(&self, text: &str) -> String {
        if self.model.starts_with(NOMIC) {
            format!("clustering: {text}")
        } else {
            text.to_owned()
        }
    }
}

impl Embedder for EmbeddingEndpoint {
    fn id(&self) -> String {
        format!("{}/{}", self.endpoint, self.model)
    }

    fn embed<'a>(&'a self, texts: &'a [String]) -> BoxFuture<'a, Result<Vec<Vec<f32>>, String>> {
        Box::pin(async move {
            let url = format!("{}/embeddings", self.base_url);
            let input: Vec<String> = texts.iter().map(|text| self.input(text)).collect();
            let mut request = self
                .client
                .post(&url)
                .json(&json!({ "model": self.model, "input": input }));
            if let Some(token) = &self.token {
                request = request.bearer_auth(token);
            }
            let response = request
                .send()
                .await
                .map_err(|err| format!("{url}: {err}"))?;
            let status = response.status();
            if !status.is_success() {
                return Err(format!("{url} answered {status}"));
            }
            let reply: serde_json::Value = response
                .json()
                .await
                .map_err(|err| format!("{url}: {err}"))?;
            let mut vectors: Vec<(usize, Vec<f32>)> = reply["data"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .map(|(at, item)| {
                    let index = item["index"].as_u64().map_or(at, |index| index as usize);
                    let vector = item["embedding"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|value| value.as_f64().map(|value| value as f32))
                        .collect();
                    (index, vector)
                })
                .collect();
            vectors.sort_by_key(|(index, _)| *index);
            if vectors.len() != texts.len() || vectors.iter().any(|(_, v)| v.is_empty()) {
                return Err(format!(
                    "{url} sent {} vectors for {} texts",
                    vectors.len(),
                    texts.len()
                ));
            }
            Ok(vectors.into_iter().map(|(_, vector)| vector).collect())
        })
    }
}

/// Downloads the embedding model into the Ollama at `base_url`, once you said yes. It waits for
/// the whole download.
pub(crate) async fn pull_nomic(base_url: &str) -> Result<(), String> {
    let client = argonauts_endpoints::runtime_api::local_client(Duration::from_secs(1800))
        .ok_or("no client for this machine's servers")?;
    let root = base_url.trim_end_matches('/').trim_end_matches("/v1");
    let url = format!("{root}/api/pull");
    let response = client
        .post(&url)
        .json(&json!({ "model": NOMIC, "stream": false }))
        .send()
        .await
        .map_err(|err| format!("{url}: {err}"))?;
    if !response.status().is_success() {
        return Err(format!("{url} answered {}", response.status()));
    }
    Ok(())
}

/// Word vectors for `texts`: each text's words and word pairs weighted by TF-IDF (counts
/// dampened by their log, rare words weighing more), then reduced to [`WORD_DIMENSIONS`] by a
/// truncated SVD. They depend on every text together, so they're made again each time and
/// never kept.
pub(crate) fn word_vectors(texts: &[String]) -> Vec<Vec<f32>> {
    let grams: Vec<Vec<String>> = texts.iter().map(|text| grams(text)).collect();
    let mut columns: BTreeMap<&str, usize> = BTreeMap::new();
    let mut documents: HashMap<usize, usize> = HashMap::new();
    for text in &grams {
        let mut seen: Vec<usize> = Vec::new();
        for gram in text {
            let next = columns.len();
            let column = *columns.entry(gram.as_str()).or_insert(next);
            if !seen.contains(&column) {
                seen.push(column);
                *documents.entry(column).or_insert(0) += 1;
            }
        }
    }
    let n = texts.len() as f64;
    let rows: Vec<SparseRow> = grams
        .iter()
        .map(|text| {
            let mut counts: BTreeMap<usize, f64> = BTreeMap::new();
            for gram in text {
                *counts.entry(columns[gram.as_str()]).or_insert(0.0) += 1.0;
            }
            let mut row: SparseRow = counts
                .into_iter()
                .map(|(column, count)| {
                    let idf = ((1.0 + n) / (1.0 + documents[&column] as f64)).ln() + 1.0;
                    (column, (1.0 + count.ln()) * idf)
                })
                .collect();
            let norm = row.iter().map(|(_, x)| x * x).sum::<f64>().sqrt();
            if norm > 0.0 {
                row.iter_mut().for_each(|(_, x)| *x /= norm);
            }
            row
        })
        .collect();
    let width = columns.len();
    let dimensions = WORD_DIMENSIONS.min(width.saturating_sub(1)).max(1);
    truncated_svd(&rows, width, dimensions, 7)
        .into_iter()
        .map(|row| row.into_iter().map(|x| x as f32).collect())
        .collect()
}

/// A text's words of two letters or more, lowercased, then each pair of neighbours.
fn grams(text: &str) -> Vec<String> {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() >= 2)
        .map(str::to_lowercase)
        .collect();
    let pairs = words
        .windows(2)
        .map(|pair| format!("{} {}", pair[0], pair[1]));
    let mut all = words.clone();
    all.extend(pairs);
    all
}

#[cfg(test)]
pub(crate) mod fakes {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Places each text by the letters it holds, and counts what it was asked.
    pub struct Letters {
        pub texts: AtomicUsize,
    }

    impl Letters {
        pub fn new() -> Letters {
            Letters {
                texts: AtomicUsize::new(0),
            }
        }
    }

    impl Embedder for Letters {
        fn id(&self) -> String {
            "ollama/letters".to_owned()
        }

        fn embed<'a>(
            &'a self,
            texts: &'a [String],
        ) -> BoxFuture<'a, Result<Vec<Vec<f32>>, String>> {
            Box::pin(async move {
                self.texts.fetch_add(texts.len(), Ordering::SeqCst);
                Ok(texts
                    .iter()
                    .map(|text| {
                        let mut vector = vec![0.0f32; 26];
                        for c in text.to_lowercase().chars().filter(char::is_ascii_lowercase) {
                            vector[(c as u8 - b'a') as usize] += 1.0;
                        }
                        vector
                    })
                    .collect())
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn distance(a: &[f32], b: &[f32]) -> f32 {
        a.iter()
            .zip(b)
            .map(|(x, y)| (x - y).powi(2))
            .sum::<f32>()
            .sqrt()
    }

    #[test]
    fn word_vectors_put_alike_summaries_together() {
        let texts: Vec<String> = [
            "Run the full unit test suite",
            "Run the unit test suite again",
            "Read recent git history and log",
            "Read the git history log",
            "Accept task completion",
        ]
        .iter()
        .map(|text| (*text).to_owned())
        .collect();
        let vectors = word_vectors(&texts);
        assert_eq!(vectors.len(), 5);
        assert!(vectors[0].len() <= WORD_DIMENSIONS);
        let tests = distance(&vectors[0], &vectors[1]);
        let apart = distance(&vectors[0], &vectors[2]);
        let git = distance(&vectors[2], &vectors[3]);
        assert!(tests < apart && git < apart, "{tests} {git} {apart}");
    }

    #[test]
    fn many_summaries_reduce_to_sixty_dimensions() {
        let texts: Vec<String> = (0..120)
            .map(|at| format!("summary word{at} and word{} with shared tail", at % 7))
            .collect();
        let vectors = word_vectors(&texts);
        assert!(vectors.iter().all(|vector| vector.len() == WORD_DIMENSIONS));
        assert_eq!(
            word_vectors(&texts),
            vectors,
            "the same summaries, the same vectors"
        );
    }

    /// A server on this machine that answers one embedding request, out of order, and hands
    /// back the body it was sent.
    fn one_answer(reply: &'static str) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{BufRead as _, BufReader, Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let answered = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
                if line == "\r\n" {
                    break;
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{reply}",
                reply.len()
            )
            .unwrap();
            String::from_utf8(body).unwrap()
        });
        (base_url, answered)
    }

    #[tokio::test]
    async fn an_embedding_endpoint_is_read_in_its_own_order_and_told_it_clusters() {
        let (base_url, answered) = one_answer(
            r#"{"data":[{"index":1,"embedding":[0.0,1.0]},{"index":0,"embedding":[1.0,0.0]}]}"#,
        );
        let embedder = EmbeddingEndpoint::new("ollama", &base_url, NOMIC, None, true).unwrap();
        assert_eq!(embedder.id(), "ollama/nomic-embed-text");
        let texts = vec!["Read git history".to_owned(), "Run the tests".to_owned()];
        let vectors = embedder.embed(&texts).await.unwrap();
        assert_eq!(vectors, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
        let sent: serde_json::Value = serde_json::from_str(&answered.join().unwrap()).unwrap();
        assert_eq!(sent["model"], NOMIC);
        assert_eq!(sent["input"][0], "clustering: Read git history");
    }

    #[test]
    fn words_are_two_letters_or_more_and_pairs_follow() {
        assert_eq!(
            grams("Read a git log"),
            vec!["read", "git", "log", "read git", "git log"]
        );
    }
}
