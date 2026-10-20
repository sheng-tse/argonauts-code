//! One request count per model server, shared by every model on it and by every process: the
//! app, its runs, and the sessions of each. A local server such as Ollama answers one request at
//! a time by default, so two sessions on it wait their turn instead of slowing each other.
//!
//! Each place is a numbered lock file under `~/.argonauts/slots/`, named for the server's
//! address and held with `File::try_lock`, as the CLI slots are. The OS lets go when the file
//! closes or the process dies, so a crashed run never keeps one.

use std::fs::{File, OpenOptions, TryLockError};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::paths::{Paths, create_private_dir_all};
use crate::settings::endpoints::host_and_port;

/// Places free up in other processes, so there's nothing local to wait on.
const RETRY_EVERY: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerRequests {
    dir: PathBuf,
    name: String,
    count: u32,
}

/// A held place. It's given back when dropped, and when the process ends.
#[derive(Debug)]
pub struct RequestSlot {
    path: PathBuf,
    _lock: File,
}

impl RequestSlot {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl ServerRequests {
    /// `count` requests at once to the server at `base_url`, at least one.
    pub fn new(home: &Paths, base_url: &str, count: u32) -> Self {
        Self {
            dir: home.home().join("slots"),
            name: server_name(base_url),
            count: count.max(1),
        }
    }

    pub fn count(&self) -> u32 {
        self.count
    }

    /// Waits until a place is free, then holds it.
    pub async fn take(&self) -> Result<RequestSlot, String> {
        loop {
            if let Some(slot) = self.try_take()? {
                return Ok(slot);
            }
            tokio::time::sleep(RETRY_EVERY).await;
        }
    }

    /// `None` when every place is taken.
    pub fn try_take(&self) -> Result<Option<RequestSlot>, String> {
        let failed = |path: &Path, err: std::io::Error| format!("{}: {err}", path.display());
        create_private_dir_all(&self.dir).map_err(|err| failed(&self.dir, err))?;
        for n in 0..self.count {
            let path = self.dir.join(format!("{}-{n}.lock", self.name));
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(&path)
                .map_err(|err| failed(&path, err))?;
            match file.try_lock() {
                Ok(()) => return Ok(Some(RequestSlot { path, _lock: file })),
                Err(TryLockError::WouldBlock) => continue,
                Err(TryLockError::Error(err)) => return Err(failed(&path, err)),
            }
        }
        Ok(None)
    }
}

/// `server-localhost-11434`: the address, so two tables naming one server share its count.
/// `localhost`, `127.0.0.1` and `::1` are one machine, and count as one.
fn server_name(base_url: &str) -> String {
    let Some((host, port)) = host_and_port(base_url) else {
        let safe: String = base_url
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        return format!("server-{safe}");
    };
    let host = match host.as_str() {
        "127.0.0.1" | "::1" | "0.0.0.0" => "localhost".to_owned(),
        other => other
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect(),
    };
    format!("server-{host}-{port}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_server_is_one_name_whatever_it_is_called() {
        assert_eq!(
            server_name("http://localhost:11434/v1"),
            "server-localhost-11434"
        );
        assert_eq!(
            server_name("http://127.0.0.1:11434"),
            "server-localhost-11434"
        );
        assert_eq!(server_name("http://[::1]:8000/v1"), "server-localhost-8000");
        assert_eq!(
            server_name("http://gpu-box:11434/v1"),
            "server-gpu-box-11434"
        );
        assert_eq!(server_name("not a url"), "server-not_a_url");
    }

    #[tokio::test]
    async fn every_model_and_process_shares_the_servers_count() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        // Two counts stand for two processes, or two models on one server: each opens its own
        // files, so their locks conflict.
        let app = ServerRequests::new(&paths, "http://localhost:11434/v1", 1);
        let run = ServerRequests::new(&paths, "http://127.0.0.1:11434/v1", 1);
        let first = app.take().await.unwrap();
        assert!(run.try_take().unwrap().is_none());
        drop(first);
        let second = tokio::time::timeout(Duration::from_secs(3), run.take())
            .await
            .unwrap()
            .unwrap();
        assert!(second.path().starts_with(home.path().join("slots")));

        // Another server keeps its own count.
        let other = ServerRequests::new(&paths, "http://127.0.0.1:1234/v1", 1);
        assert!(other.try_take().unwrap().is_some());

        let zero = ServerRequests::new(&paths, "http://localhost:8000/v1", 0);
        assert_eq!(zero.count(), 1);
    }
}
