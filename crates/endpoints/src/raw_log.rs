//! The session's raw log: every line the endpoint sent but the blank ones between events, in
//! order, with the key, header values, and anything that looks like a secret replaced.

use std::path::{Path, PathBuf};

use argonauts_core::redact::redact_secrets;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncWriteExt, BufWriter};

pub(crate) struct RawLog {
    path: PathBuf,
    file: BufWriter<File>,
    secrets: Vec<String>,
    /// The first write error, until it's reported.
    error: Option<String>,
    failed: bool,
}

impl RawLog {
    pub(crate) async fn open(path: &Path, secrets: Vec<String>) -> Result<Self, String> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
            .map_err(|err| format!("couldn't open the raw log {}: {err}", path.display()))?;
        Ok(Self {
            path: path.to_owned(),
            file: BufWriter::new(file),
            secrets,
            error: None,
            failed: false,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) async fn write(&mut self, line: &str) {
        if self.failed {
            return;
        }
        let mut clean = line.replace(['\n', '\r'], " ");
        for secret in &self.secrets {
            clean = clean.replace(secret.as_str(), "[redacted]");
        }
        let mut clean = redact_secrets(&clean).into_owned();
        clean.push('\n');
        if let Err(err) = self.file.write_all(clean.as_bytes()).await {
            self.fail(err);
        }
    }

    /// Every line of a body that wasn't a stream, such as an error.
    pub(crate) async fn write_body(&mut self, body: &str) {
        for line in body.lines().filter(|line| !line.trim().is_empty()) {
            self.write(line).await;
        }
    }

    pub(crate) async fn flush(&mut self) {
        if !self.failed
            && let Err(err) = self.file.flush().await
        {
            self.fail(err);
        }
    }

    /// The first write error, once.
    pub(crate) fn take_error(&mut self) -> Option<String> {
        self.error.take()
    }

    fn fail(&mut self, err: std::io::Error) {
        self.failed = true;
        self.error = Some(format!(
            "couldn't write the raw log {}: {err}",
            self.path.display()
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lines_are_appended_without_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("raw.stdout.jsonl");
        let mut log = RawLog::open(&path, vec!["plain-key-9".into()])
            .await
            .unwrap();
        log.write("data: {\"echo\": \"plain-key-9\"}").await;
        log.write("data: sk-abcdefghijklmnopqrstuvwxyz").await;
        log.write_body("{\"error\":\n\"x\"}\n\n").await;
        log.flush().await;
        assert_eq!(log.take_error(), None);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "data: {\"echo\": \"[redacted]\"}\ndata: [redacted openai key]\n{\"error\":\n\"x\"}\n"
        );
    }
}
