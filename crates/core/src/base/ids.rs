//! Ids for runs, tasks, and repos. They end up in file paths and git branch names, so each one
//! is checked when it is created or deserialized.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::base::hash::sha256_hex;

/// In bytes.
pub const MAX_ID_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {kind} id {value:?}: {reason}")]
pub struct IdError {
    kind: &'static str,
    value: String,
    reason: &'static str,
}

fn validate(kind: &'static str, value: &str) -> Result<(), IdError> {
    let fail = |reason| {
        Err(IdError {
            kind,
            value: value.to_owned(),
            reason,
        })
    };
    let Some(first) = value.chars().next() else {
        return fail("it is empty");
    };
    if value.len() > MAX_ID_LEN {
        return fail("it is longer than 64 bytes");
    }
    if !first.is_ascii_alphanumeric() {
        return fail("it must start with a letter or digit");
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return fail("only ASCII letters, digits, '-', '_' and '.' are allowed");
    }
    // Git rejects these in branch names, and ".." would also escape a path.
    if value.contains("..") || value.ends_with('.') || value.ends_with(".lock") {
        return fail("it is not safe in a git branch name");
    }
    Ok(())
}

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident, $kind:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
                let value = value.into();
                validate($kind, &value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::new(s)
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> String {
                id.0
            }
        }
    };
}

id_type!(
    /// Such as `run-20260929-041503-3fa2`.
    RunId,
    "run"
);

id_type!(
    /// Unique within a run, such as `task-001`.
    TaskId,
    "task"
);

id_type!(
    /// A repository's folder name plus a hash of its absolute path.
    RepoId,
    "repo"
);

impl RunId {
    pub fn generate() -> Self {
        Self::at(
            OffsetDateTime::now_utc(),
            uuid::Uuid::new_v4().as_u128() as u16,
        )
    }

    /// `suffix` tells apart runs started in the same second.
    pub fn at(when: OffsetDateTime, suffix: u16) -> Self {
        Self(format!(
            "run-{:04}{:02}{:02}-{:02}{:02}{:02}-{suffix:04x}",
            when.year(),
            u8::from(when.month()),
            when.day(),
            when.hour(),
            when.minute(),
            when.second(),
        ))
    }
}

impl TaskId {
    pub fn numbered(n: u32) -> Self {
        Self(format!("task-{n:03}"))
    }
}

impl RepoId {
    pub fn for_repo(path: &Path) -> std::io::Result<Self> {
        let canonical = path.canonicalize()?;
        Ok(Self::from_canonical_path(&canonical))
    }

    /// `path` must already be canonical and absolute.
    pub fn from_canonical_path(path: &Path) -> Self {
        let digest = sha256_hex(path.as_os_str().as_encoded_bytes());
        let name = path
            .file_name()
            .map(|n| sanitize_name(&n.to_string_lossy()))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "repo".to_owned());
        Self(format!("{name}-{}", &digest[..12]))
    }
}

/// Turns a folder name into something `validate` accepts, capped at 40 bytes.
fn sanitize_name(name: &str) -> String {
    let mapped: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    mapped
        .trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn accepts_typical_ids() {
        for ok in ["run-01", "task-001", "api-3f9a1c2b4d5e", "a", "v1.2_x"] {
            assert!(TaskId::new(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn rejects_unsafe_ids() {
        let too_long = "a".repeat(MAX_ID_LEN + 1);
        for bad in [
            "",
            "-main-agent",
            ".hidden",
            "a/b",
            "a..b",
            "a b",
            "x.lock",
            "trail.",
            "ü",
            too_long.as_str(),
        ] {
            assert!(RunId::new(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn error_names_the_kind() {
        let err = TaskId::new("a/b").unwrap_err();
        assert!(err.to_string().contains("task id"));
    }

    #[test]
    fn serde_validates_on_the_way_in() {
        let id: TaskId = serde_json::from_str("\"task-002\"").unwrap();
        assert_eq!(id.as_str(), "task-002");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"task-002\"");
        assert!(serde_json::from_str::<TaskId>("\"../etc\"").is_err());
    }

    #[test]
    fn run_id_format() {
        let id = RunId::at(datetime!(2026-09-29 04:15:03 UTC), 0x3fa2);
        assert_eq!(id.as_str(), "run-20260929-041503-3fa2");
        assert!(RunId::new(id.as_str()).is_ok());
        assert!(RunId::new(RunId::generate().as_str()).is_ok());
    }

    #[test]
    fn task_numbering() {
        assert_eq!(TaskId::numbered(1).as_str(), "task-001");
        assert_eq!(TaskId::numbered(1234).as_str(), "task-1234");
    }

    #[test]
    fn repo_id_uses_name_and_path_hash() {
        let a = RepoId::from_canonical_path(Path::new("/home/me/code/api"));
        let b = RepoId::from_canonical_path(Path::new("/home/me/other/api"));
        assert!(a.as_str().starts_with("api-"));
        assert_eq!(a.as_str().len(), "api-".len() + 12);
        assert_ne!(a, b);
        assert_eq!(
            a,
            RepoId::from_canonical_path(Path::new("/home/me/code/api"))
        );
        assert!(RepoId::new(a.as_str()).is_ok());
    }

    #[test]
    fn repo_id_sanitizes_odd_folder_names() {
        let id = RepoId::from_canonical_path(Path::new("/x/.My Repo (v2)"));
        assert!(id.as_str().starts_with("My_Repo__v2_-"), "{id}");
        let root = RepoId::from_canonical_path(Path::new("/"));
        assert!(root.as_str().starts_with("repo-"), "{root}");
        let dots = RepoId::from_canonical_path(Path::new("/x/a..b"));
        assert!(RepoId::new(dots.as_str()).is_ok(), "{dots}");
    }

    #[test]
    fn repo_id_for_existing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let id = RepoId::for_repo(dir.path()).unwrap();
        assert!(RepoId::new(id.as_str()).is_ok());
        assert!(RepoId::for_repo(&dir.path().join("missing")).is_err());
    }
}
