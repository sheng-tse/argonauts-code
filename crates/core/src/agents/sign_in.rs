//! Whether a CLI's sign-in error says it is signed out, and the command that signs it in again.
//! Conversations and runs read the same words, so one CLI error reads the same in both.

use crate::app::SignedOut;
use crate::runs::events::session::EndReason;

/// What a sign-in error says when the CLI asks to be signed in again.
const ASKS_AGAIN: [&str; 5] = [
    "log in again",
    "login again",
    "sign in again",
    "not logged in",
    "/login",
];

/// Whether `reason` says `agent`'s CLI is signed out: a sign-in error whose own words say its
/// sign-in was revoked or expired, or ask to sign in again. A token revoked on the server passes
/// the CLI's own check, so only its error tells.
pub fn signed_out(reason: &EndReason, agent: &str) -> Option<SignedOut> {
    let message = match reason {
        EndReason::Auth { message } => message,
        EndReason::CliError { message, .. } if reason.is_sign_in_error() => message,
        _ => return None,
    };
    let message = message.to_lowercase();
    let why = if message.contains("revoked") {
        "its sign-in was revoked"
    } else if message.contains("expired") {
        "its sign-in expired"
    } else if ASKS_AGAIN.iter().any(|words| message.contains(words)) {
        ""
    } else {
        return None;
    };
    Some(SignedOut {
        why: why.to_owned(),
        login: sign_in_command(agent).map(str::to_owned),
    })
}

/// The command that signs `agent`'s CLI in again, as each CLI words it to you.
pub fn sign_in_command(agent: &str) -> Option<&'static str> {
    Some(match agent {
        "claude-code" => "claude /login",
        "codex" => "codex login",
        "antigravity-cli" => "agy",
        "opencode" => "opencode auth login",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCategory;

    fn auth(message: &str) -> EndReason {
        EndReason::Auth {
            message: message.to_owned(),
        }
    }

    /// Every phrase that means signed out, and what it says of the sign-in.
    #[test]
    fn every_phrase_that_means_signed_out() {
        let said = |message: &str| signed_out(&auth(message), "claude-code").map(|out| out.why);
        for (message, why) in [
            (
                "OAuth token revoked · Please run /login",
                "its sign-in was revoked",
            ),
            ("Login expired · Please run /login", "its sign-in expired"),
            ("Your session expired", "its sign-in expired"),
            ("Please log in again", ""),
            ("Run codex login again", ""),
            ("Sign in again to go on", ""),
            ("Not logged in · Please run /login", ""),
            ("Invalid API key · Please run /login", ""),
        ] {
            assert_eq!(said(message).as_deref(), Some(why), "{message}");
        }
        assert_eq!(said("401 Unauthorized"), None);
        let out = signed_out(&auth("Not logged in"), "claude-code").unwrap();
        assert_eq!(out.login.as_deref(), Some("claude /login"));
        // Only a sign-in error counts: the same words in another error say nothing of it.
        let other = EndReason::CliError {
            category: ErrorCategory::Server,
            message: "token revoked".to_owned(),
        };
        assert_eq!(signed_out(&other, "codex"), None);
        let refused = EndReason::CliError {
            category: ErrorCategory::Auth,
            message: "token revoked".to_owned(),
        };
        assert_eq!(
            signed_out(&refused, "codex").and_then(|out| out.login),
            Some("codex login".to_owned())
        );
    }
}
