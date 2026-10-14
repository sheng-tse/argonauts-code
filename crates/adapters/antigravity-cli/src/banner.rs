//! Who Antigravity CLI is signed in as. agy has no command that prints the account: it shows
//! the address and the plan only on the screen it draws as it starts, and says so plainly when
//! no one is signed in. This reads that screen as text, once it has been drawn; nothing is
//! ever sent to agy, and its token files are never read.

use std::sync::LazyLock;

use argonauts_core::adapter::BannerAccount;
use regex::Regex;

/// What agy says on its start-up screen when no one is signed in.
const SIGNED_OUT: &str = "not signed in";

static ADDRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}")
        .expect("the address pattern compiles")
});

/// Google's plans as agy names them, such as `Google AI Pro` or `Gemini Code Assist Standard`.
static PLAN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:Your Plan:\s*(?P<named>[^·|\n]+))|(?P<plan>(?:Google AI|Gemini Code Assist|Gemini|Google One AI)\s+(?:Pro|Ultra|Plus|Premium|Standard|Enterprise|Free)\b)",
    )
    .expect("the plan pattern compiles")
});

/// The account and plan the start-up screen shows, or that it says no one is signed in.
/// `None` while the screen says neither, as before it has finished drawing.
pub(crate) fn account(screen: &str) -> Option<BannerAccount> {
    if screen.contains(SIGNED_OUT) {
        return Some(BannerAccount {
            signed_out: true,
            ..BannerAccount::default()
        });
    }
    let account = ADDRESS.find(screen)?.as_str().to_owned();
    let plan = PLAN.captures(screen).and_then(|found| {
        found
            .name("named")
            .or_else(|| found.name("plan"))
            .map(|plan| plan.as_str().trim().to_owned())
            .filter(|plan| !plan.is_empty())
    });
    Some(BannerAccount {
        account: Some(account),
        plan,
        signed_out: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_screen_names_the_address_and_the_plan() {
        let screen = "     ▄▀▀▄\n    ▀▀▀▀▀▀\n Antigravity CLI 1.3.2\n you@example.com · Google AI Pro\n ~/code/app\n\n >";
        assert_eq!(
            account(screen),
            Some(BannerAccount {
                account: Some("you@example.com".into()),
                plan: Some("Google AI Pro".into()),
                signed_out: false,
            })
        );
        let named = "Signed in: me@school.example\nYour Plan: Gemini Code Assist Standard\n";
        let found = account(named).unwrap();
        assert_eq!(found.account.as_deref(), Some("me@school.example"));
        assert_eq!(found.plan.as_deref(), Some("Gemini Code Assist Standard"));
    }

    #[test]
    fn it_says_when_no_one_is_signed_in_and_waits_while_it_says_neither() {
        let signed_out = " Welcome to the Antigravity CLI. You are currently not signed in.\n Select login method:\n > 1. Google OAuth";
        assert_eq!(
            account(signed_out),
            Some(BannerAccount {
                signed_out: true,
                ..BannerAccount::default()
            })
        );
        assert_eq!(account("     ▄▀▀▄\n    ▀▀▀▀▀▀\n"), None);
        // An address without a plan the screen names is still the account.
        let bare = account(" you@example.com\n").unwrap();
        assert_eq!(
            (bare.account.as_deref(), bare.plan),
            (Some("you@example.com"), None)
        );
    }
}
