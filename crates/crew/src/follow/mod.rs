//! Runs that work on a GitHub issue, and the pull requests Argonauts Code opened for them.

pub mod draft;
mod followed;
mod link;
pub mod round;
pub mod watch;

pub use followed::{Due, Followed, Heading, Reply, Work};
pub use link::IssueLink;

use crate::engine::host::Host;

/// The issue the run works on, when `/issue` started it.
pub fn link_of(host: &Host) -> Option<IssueLink> {
    let request = host.read(|state| {
        state
            .run
            .started
            .as_ref()
            .map(|started| started.request.clone())
    })?;
    IssueLink::for_request(host.repo().state_dir(), &request)
}

/// The remote a run on an issue pushes to: the one it names, else `origin`, else the first.
fn remote_of<'r>(remotes: &'r [String], named: Option<&str>) -> Option<&'r String> {
    match named {
        Some(named) => remotes.iter().find(|remote| *remote == named),
        None => remotes
            .iter()
            .find(|remote| *remote == "origin")
            .or(remotes.first()),
    }
}
