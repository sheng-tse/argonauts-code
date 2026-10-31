//! The pull request a run opened, which the bottom line's right end names as Claude Code's
//! footer does, such as `PR #12`.

use argonauts_core::app::AppEvent;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::task::MergeEvent;

use super::Cx;

/// Keeps the newest pull request a run opened. Replaying an old run names none: it may be
/// closed by now.
pub fn on_event(event: &AppEvent, cx: &mut Cx<'_>) {
    let AppEvent::Run { event } = event else {
        return;
    };
    let RunEventKind::Merge(MergeEvent::PullRequest { url: Some(url), .. }) = &event.kind else {
        return;
    };
    if cx.replaying() {
        return;
    }
    if let Some(label) = label(url) {
        cx.model.bottom.mode.pull_request = Some(label);
    }
}

/// `PR #12` for a GitHub pull request's address, `MR !12` for a GitLab merge request's.
fn label(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let (rest, number) = url.rsplit_once('/')?;
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if rest.ends_with("/pull") {
        Some(format!("PR #{number}"))
    } else if rest.ends_with("/merge_requests") {
        Some(format!("MR !{number}"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::label;

    #[test]
    fn a_pull_request_is_named_by_its_number() {
        assert_eq!(
            label("https://github.com/acme/shop/pull/12").as_deref(),
            Some("PR #12")
        );
        assert_eq!(
            label("https://gitlab.com/acme/shop/-/merge_requests/7/").as_deref(),
            Some("MR !7")
        );
        assert_eq!(label("https://github.com/acme/shop/issues/12"), None);
        assert_eq!(label("https://github.com/acme/shop/pull/new"), None);
    }
}
