//! Errors in what the user asked for, as opposed to failures while doing it.

use std::fmt;

/// The same exit status clap uses.
pub const USAGE_EXIT: u8 = 2;

#[derive(Debug)]
pub struct UsageError(anyhow::Error);

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for UsageError {}

pub fn usage(err: impl Into<anyhow::Error>) -> anyhow::Error {
    anyhow::Error::new(UsageError(err.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_errors_are_recognizable_and_keep_their_message() {
        let err = usage(anyhow::anyhow!("bad --check").context("reading flags"));
        assert!(err.is::<UsageError>());
        assert_eq!(format!("{err:#}"), "reading flags: bad --check");
        assert!(!anyhow::anyhow!("other").is::<UsageError>());
    }
}
