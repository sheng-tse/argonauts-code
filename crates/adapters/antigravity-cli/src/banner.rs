//! Who Antigravity CLI is signed in as. agy has no command that prints the account: it shows
//! the address and the plan only in the banner it draws as it starts. This reads that screen
//! as text; nothing is ever sent to agy, and its token files are never read.
//!
//! agy draws three other screens first or instead, and each means something else:
//! - while it loads a saved login it says "You are currently not signed in" over a spinner,
//!   for a few milliseconds, so that sentence alone says nothing;
//! - with no saved login it asks for a login method, which is the only sign that no one is
//!   signed in;
//! - in a folder it hasn't trusted it asks about the folder before anything else.

use std::sync::LazyLock;

use argonauts_core::adapter::BannerAccount;
use regex::Regex;

/// The banner's first line, such as `Antigravity CLI 1.3.3`. The account is on the next one.
static TITLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Antigravity CLI\s+v?\d+(?:\.\d+)+").expect("the title pattern compiles")
});

static ADDRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}")
        .expect("the address pattern compiles")
});

/// A plan on a line of its own under the account, as `Your Plan: Gemini Code Assist Standard`.
const PLAN_LABEL: &str = "Your Plan:";
/// What agy asks in a folder it hasn't trusted.
const TRUST_QUESTION: &str = "Do you trust";
/// What agy asks when no login is saved, in any case.
const LOGIN_CHOICE: &str = "login method";
/// The spinner's words while a saved login loads.
const LOADING: &str = "Signing in";

/// What the start-up screen says of who is signed in. `None` while it says nothing yet: before
/// the banner is drawn, and all the while the saved login loads.
pub(crate) fn account(screen: &str) -> Option<BannerAccount> {
    if screen.contains(TRUST_QUESTION) {
        return Some(BannerAccount::UntrustedFolder);
    }
    if let Some(signed_in) = signed_in(screen) {
        return Some(signed_in);
    }
    let asks_how = screen.to_lowercase().contains(LOGIN_CHOICE);
    (asks_how && !screen.contains(LOADING)).then_some(BannerAccount::SignedOut)
}

/// The address on the line right after the banner's title, with the plan beside or under it.
fn signed_in(screen: &str) -> Option<BannerAccount> {
    let mut lines = screen.lines().map(inside_frame);
    lines.by_ref().find(|line| TITLE.is_match(line))?;
    let mut under = lines.filter(|line| !line.is_empty());
    let line = under.next()?;
    let address = ADDRESS.find(line)?;
    let plan = plan_beside(&line[address.end()..]).or_else(|| {
        under
            .find_map(|line| line.split_once(PLAN_LABEL))
            .and_then(|(_, plan)| plan_beside(plan))
    });
    Some(BannerAccount::SignedIn {
        account: address.as_str().to_owned(),
        plan,
    })
}

/// A line without the frame agy may draw around its banner.
fn inside_frame(line: &str) -> &str {
    line.trim_matches(|c: char| c.is_whitespace() || ('\u{2500}'..='\u{257f}').contains(&c))
}

/// The plan as written after an address: `(Google AI Pro)` or `· Google AI Pro`.
fn plan_beside(rest: &str) -> Option<String> {
    let plan = rest
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '·' | '|' | '-'))
        .trim();
    (!plan.is_empty()).then(|| plan.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed_in(account: &str, plan: Option<&str>) -> Option<BannerAccount> {
        Some(BannerAccount::SignedIn {
            account: account.to_owned(),
            plan: plan.map(str::to_owned),
        })
    }

    /// agy's banner as 1.3.3 draws it.
    const BANNER: &str = "\
╭──────────────────────────────────────────╮
│ Antigravity CLI 1.3.3                    │
│ you@example.com (Google AI Pro)          │
│ Gemini 3.8 Flash (High)                  │
│ /                                        │
╰──────────────────────────────────────────╯";

    /// The frame agy draws for a few milliseconds while it loads a saved login.
    const LOADING_FRAME: &str = "\
 Welcome to the Antigravity CLI.
 You are currently not signed in.

 ⣷  Signing in…";

    #[test]
    fn the_banner_names_the_address_and_the_plan_on_its_second_line() {
        assert_eq!(
            account(BANNER),
            signed_in("you@example.com", Some("Google AI Pro"))
        );
        let dotted = "     ▄▀▀▄\n    ▀▀▀▀▀▀\n Antigravity CLI 1.3.2\n you@example.com · Google AI Pro\n ~/code/app\n\n >";
        assert_eq!(
            account(dotted),
            signed_in("you@example.com", Some("Google AI Pro"))
        );
        let under =
            " Antigravity CLI 1.3.3\n me@school.example\n Your Plan: Gemini Code Assist Standard\n";
        assert_eq!(
            account(under),
            signed_in("me@school.example", Some("Gemini Code Assist Standard"))
        );
    }

    #[test]
    fn the_loading_frame_says_nothing_however_it_reads() {
        assert_eq!(account(LOADING_FRAME), None);
        // The spinner gone and the banner not drawn yet is still not an answer.
        assert_eq!(
            account(" Welcome to the Antigravity CLI.\n You are currently not signed in.\n"),
            None
        );
        // Nor is a title whose next line hasn't been drawn.
        assert_eq!(account(" Antigravity CLI 1.3.3\n"), None);
        assert_eq!(account("     ▄▀▀▄\n    ▀▀▀▀▀▀\n"), None);
    }

    #[test]
    fn only_the_login_choice_means_no_one_is_signed_in() {
        let choice = " Welcome to the Antigravity CLI. You are currently not signed in.\n Select login method:\n > 1. Google OAuth";
        assert_eq!(account(choice), Some(BannerAccount::SignedOut));
        // The choice under a spinner is still loading.
        let loading = format!("{choice}\n ⣷  Signing in…");
        assert_eq!(account(&loading), None);
    }

    #[test]
    fn a_folder_agy_has_not_trusted_is_told_apart() {
        let question = " Do you trust the contents of this project?\n > 1. Yes, trust this folder\n   2. No, exit";
        assert_eq!(account(question), Some(BannerAccount::UntrustedFolder));
    }

    #[test]
    fn an_address_anywhere_else_is_not_the_account() {
        // A path or a notice may hold an address; only the banner's own line counts.
        assert_eq!(account(" ~/mail/you@example.com/inbox\n >"), None);
        let no_address = " Antigravity CLI 1.3.3\n API key\n Gemini 3.8 Flash\n";
        assert_eq!(account(no_address), None);
        // An address with no plan beside or under it is the account all the same.
        let bare = " Antigravity CLI 1.3.3\n you@example.com\n";
        assert_eq!(account(bare), signed_in("you@example.com", None));
    }
}
