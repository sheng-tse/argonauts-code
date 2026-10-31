use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum LearningError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{path}: {message}")]
    Parse { path: PathBuf, message: String },
    /// A rule refused it. The text says which rule and why, in words you can act on.
    #[error("{0}")]
    Refused(String),
    #[error("git {args}: {message}")]
    Git { args: String, message: String },
    /// Its part of the learning crate isn't built yet.
    #[error("{0} isn't built yet")]
    NotYet(&'static str),
}

pub type Result<T> = std::result::Result<T, LearningError>;
