//! A manifest server on this computer for tests, a key to sign with, and release archives
//! whose program prints the version it stands for.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

use super::{Download, Manifest, Source};

/// Serves what a test puts at each path, and answers 404 for the rest.
pub(crate) struct FakeServer {
    address: String,
    files: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    asked: Arc<Mutex<Vec<String>>>,
}

impl FakeServer {
    pub(crate) async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let files: Arc<Mutex<BTreeMap<String, Vec<u8>>>> = Arc::default();
        let asked: Arc<Mutex<Vec<String>>> = Arc::default();
        let (served, noted) = (Arc::clone(&files), Arc::clone(&asked));
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let (files, asked) = (Arc::clone(&served), Arc::clone(&noted));
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0u8; 1024];
                    while !request.windows(4).any(|end| end == b"\r\n\r\n") {
                        match stream.read(&mut buffer).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => request.extend_from_slice(&buffer[..read]),
                        }
                    }
                    let text = String::from_utf8_lossy(&request);
                    let path = text.split_whitespace().nth(1).unwrap_or("/").to_owned();
                    asked
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(path.clone());
                    let body = files
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .get(&path)
                        .cloned();
                    let (status, body) = match body {
                        Some(body) => ("200 OK", body),
                        None => ("404 Not Found", b"not here".to_vec()),
                    };
                    let head = format!(
                        "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes()).await;
                    let _ = stream.write_all(&body).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self {
            address,
            files,
            asked,
        }
    }

    /// Serves `body` at `path` from now on, and gives its full address.
    pub(crate) fn put(&self, path: &str, body: Vec<u8>) -> String {
        self.files
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path.to_owned(), body);
        format!("{}{path}", self.address)
    }

    /// Where the manifests are, as a [`Source`] trusting `keys`.
    pub(crate) fn source(&self, keys: Vec<String>) -> Source {
        Source::new(format!("{}/updates/v1", self.address), keys)
    }

    /// Signs `manifest` with `signer` and serves it as `name`, such as `stable` or
    /// `releases/0.1.2`.
    pub(crate) fn publish(&self, name: &str, manifest: &Manifest, signer: &Signer) {
        let json = serde_json::to_vec_pretty(manifest).unwrap();
        let signature = signer.sign(&json);
        self.put(&format!("/updates/v1/{name}.json"), json);
        self.put(
            &format!("/updates/v1/{name}.json.minisig"),
            signature.into_bytes(),
        );
    }

    /// Serves `archive` as a release's download, and the manifest's entry for it.
    pub(crate) fn release(&self, version: &str, archive: Vec<u8>) -> Download {
        use sha2::Digest as _;
        let size = archive.len() as u64;
        let sha256 = super::apply::hex(&sha2::Sha256::digest(&archive));
        let url = self.put(
            &format!("/download/v{version}/argonauts-aarch64-apple-darwin.tar.xz"),
            archive,
        );
        Download { url, sha256, size }
    }

    /// The paths asked for so far.
    pub(crate) fn asked(&self) -> Vec<String> {
        self.asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// A minisign key made for one test.
pub(crate) struct Signer {
    keys: minisign::KeyPair,
}

impl Signer {
    pub(crate) fn new() -> Self {
        Self {
            keys: minisign::KeyPair::generate_unencrypted_keypair().unwrap(),
        }
    }

    /// The public key as a `.pub` file's second line holds it.
    pub(crate) fn public(&self) -> String {
        self.keys.pk.to_base64()
    }

    /// The `.minisig` file for `data`.
    pub(crate) fn sign(&self, data: &[u8]) -> String {
        minisign::sign(
            Some(&self.keys.pk),
            &self.keys.sk,
            Cursor::new(data),
            Some("argonauts-code update manifest"),
            None,
        )
        .unwrap()
        .into_string()
    }
}

/// A manifest for `version` at `serial`, with a build for Apple silicon and one for this
/// computer, everyone in, and nothing held back.
pub(crate) fn manifest(version: &str, serial: u64) -> Manifest {
    Manifest {
        channel: argonauts_core::settings::updates::UpdateChannel::Stable,
        serial,
        version: semver::Version::parse(version).unwrap(),
        notes: None,
        min_version: None,
        rollout: Vec::new(),
        halted: false,
        yanked: Vec::new(),
        // The examples' build, and the one for the computer the tests run on.
        platforms: ["aarch64-apple-darwin"]
            .into_iter()
            .chain(super::platform())
            .map(|target| {
                let download = Download {
                    url: format!("https://example.com/v{version}/argonauts-{target}.tar.xz"),
                    sha256: "0".repeat(64),
                    size: 1,
                };
                (target.to_owned(), download)
            })
            .collect(),
    }
}

/// A `.tar.xz` laid out as dist builds one, whose `argonauts` prints `argonauts-code <version>`.
pub(crate) fn release_archive(version: &str) -> Vec<u8> {
    let program = format!("#!/bin/sh\necho \"argonauts-code {version}\"\n");
    let mut tar = tar::Builder::new(Vec::new());
    for (name, body, mode) in [
        (
            "argonauts-code-aarch64-apple-darwin/argonauts",
            program.as_bytes(),
            0o755,
        ),
        (
            "argonauts-code-aarch64-apple-darwin/README.md",
            b"read me".as_slice(),
            0o644,
        ),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        tar.append_data(&mut header, name, body).unwrap();
    }
    let tar = tar.into_inner().unwrap();
    let mut xz = xz2::write::XzEncoder::new(Vec::new(), 6);
    std::io::Write::write_all(&mut xz, &tar).unwrap();
    xz.finish().unwrap()
}
