//! A fork of yours for a repository you can't push to. `gh` makes it on GitHub, as you, only
//! once you say so, and it's added here as the remote `fork`. A run on an issue then pushes its
//! branch there, and its draft opens on the repository the issue is in.

use std::path::Path;

use argonauts_conductor::gh::RepoName;
use argonauts_core::NoteLevel;
use argonauts_core::app::{AppChoice, AppEvent, ChoiceKind};
use argonauts_core::events::approval::{AnswerAction, AnswerOption, FreeText};
use argonauts_core::names::ChoiceId;

use super::gh_here;
use crate::controller::Ctx;

/// The answers of the question, which also tell the screen whose question it is.
pub(crate) const FORK: &str = "issue:fork";
pub(crate) const NO: &str = "issue:no-fork";

/// The remote a fork of yours is kept as here.
pub(crate) const REMOTE: &str = "fork";

/// A fork that would let the run on issue `number` push its branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Offer {
    pub upstream: RepoName,
    pub login: String,
    pub number: u64,
}

/// The remote here that is `login`'s fork of `upstream`, if there is one.
pub(crate) fn remote_here(root: &Path, login: &str, upstream: &RepoName) -> Option<String> {
    let out = crate::git::command(root)
        .args(["remote", "get-url", REMOTE])
        .output()
        .ok()?;
    let url = String::from_utf8_lossy(&out.stdout);
    let named = repo_of_remote(url.trim())?;
    let yours = named.account.eq_ignore_ascii_case(login)
        && named.name.eq_ignore_ascii_case(&upstream.name)
        && named.host.eq_ignore_ascii_case(&upstream.host);
    (out.status.success() && yours).then(|| REMOTE.to_owned())
}

/// `https://github.com/you/limiter.git`, or its ssh form, `git@<host>:you/limiter.git`.
fn repo_of_remote(url: &str) -> Option<RepoName> {
    let url = url.trim_end_matches('/').trim_end_matches(".git");
    if let Some(named) = RepoName::of_url(url) {
        return Some(named);
    }
    let (host, path) = url.strip_prefix("git@")?.split_once(':')?;
    let mut named = RepoName::parse(path)?;
    host.clone_into(&mut named.host);
    Some(named)
}

pub(crate) fn question(id: ChoiceId, offer: &Offer) -> AppChoice {
    let Offer {
        upstream,
        login,
        number,
    } = offer;
    let option = |key: char, label: &str, value: &str, detail: String| AnswerOption {
        key,
        label: label.to_owned(),
        action: AnswerAction::Choose {
            value: value.to_owned(),
        },
        recommended: false,
        detail: Some(detail),
    };
    AppChoice {
        id,
        kind: ChoiceKind::GoAhead,
        title: format!("Fork {upstream} to work on #{number}?"),
        body: vec![format!(
            "gh makes the fork on GitHub as you, and it's added here as the remote \"{REMOTE}\". \
             The run pushes its branch there, and its draft opens on {upstream}."
        )],
        options: vec![
            option(
                'f',
                "Fork it",
                FORK,
                format!("a repository of yours: {login}/{}", upstream.name),
            ),
            option(
                'n',
                "No",
                NO,
                "nothing is made, and nothing starts".to_owned(),
            ),
        ],
        free_text: FreeText::None,
        bell: true,
    }
}

/// Your answer: the fork is made and kept as a remote here, or nothing is.
pub(crate) fn answer(ctx: &Ctx, id: ChoiceId, offer: Offer, action: &AnswerAction) {
    let forks = matches!(action, AnswerAction::Choose { value } if value == FORK);
    let number = offer.number;
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: if forks {
            format!("Forking {}.", offer.upstream)
        } else {
            format!("Nothing was made, and nothing started on #{number}.")
        },
    });
    if !forks {
        return;
    }
    let gh = gh_here::gh_for(&ctx.env.repo_root);
    let root = ctx.env.repo_root.clone();
    let out = ctx.out.clone();
    tokio::spawn(async move {
        let made = match gh {
            Ok(gh) => gh.fork(&offer.upstream, &offer.login).await,
            Err(err) => Err(err),
        };
        let (level, text) = match made.map(|fork| keep_as_remote(&root, &fork).map(|()| fork)) {
            Ok(Ok(fork)) => (
                NoteLevel::Info,
                format!(
                    "Forked {} as {fork}, kept here as the remote \"{REMOTE}\". /issue {number} \
                     starts on #{number} from it.",
                    offer.upstream
                ),
            ),
            Ok(Err(err)) => (NoteLevel::Error, err),
            Err(err) => (NoteLevel::Error, format!("No fork was made: {err}")),
        };
        out.send(AppEvent::Notice { level, text });
    });
}

fn keep_as_remote(root: &Path, fork: &RepoName) -> Result<(), String> {
    let url = format!("{}.git", fork.url());
    let out = crate::git::command(root)
        .args(["remote", "add", REMOTE, &url])
        .output()
        .map_err(|err| format!("{fork} was made, but adding it here failed: {err}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!(
        "{fork} was made, but adding it here as \"{REMOTE}\" failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remotes_address_names_its_repository() {
        let named = |url| repo_of_remote(url).map(|repo| (repo.host.clone(), repo.to_string()));
        assert_eq!(
            named("https://github.com/you/limiter.git"),
            Some(("github.com".to_owned(), "you/limiter".to_owned()))
        );
        assert_eq!(
            named("git@github.example.com:you/limiter.git"),
            Some(("github.example.com".to_owned(), "you/limiter".to_owned()))
        );
        assert_eq!(named("/tmp/limiter.git"), None);
    }
}
