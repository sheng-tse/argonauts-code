# Security

## Reporting a vulnerability

Please report security problems privately, never in a public issue.

- On GitHub, open this repository's **Security** tab and choose **Report a vulnerability**.
- Or email shengtselin@gmail.com with "Argonauts security" in the subject.

Say what you ran, what happened and what you expected, and include `argonauts --version`, your OS, and the CLI versions `argonauts doctor` prints. Leave out your keys, tokens and private code, since a report never needs them.

You'll get a reply within a week. When the fix ships, its release notes thank you by name, unless you'd rather not be named.

## What counts

Argonauts Code itself, its installers and its release builds. For example:

- an agent writing where its approval mode doesn't allow (outside its own worktree in Supervised and Auto preview, or into the `[outside] never` list in Auto and Autopilot), or getting past the sandbox or an approval you set;
- a change merged without the reviews and approval your settings require;
- credentials, tokens, prompts or code reaching telemetry, logs or another program;
- a project's mod or settings running a command before you trusted it;
- the MCP server (`argonauts mcp`) doing more than its tools allow;
- a release download or installer that someone else could swap.

On macOS a Claude Code or Codex worker can still reach your login keychain through `/usr/bin/security`, since the sandbox leaves that program and the keychain open for those two CLIs, which keep their sign-ins there.

## What goes elsewhere

- Problems in Claude Code, Codex, agy or OpenCode themselves go to their makers. Argonauts Code starts those programs and doesn't change them.
- Bad code a model writes isn't a vulnerability in Argonauts Code, unless it was merged without the reviews and approval your settings require.

## Copies that claim to be Argonauts Code

The official sources are this repository's releases and installer, the `sheng-tse/tap` Homebrew tap, the npm package `argonauts-code`, and the crate `argonauts-code` on crates.io with the `argonauts-*` crates it is built from. If you find a copy elsewhere that claims to be Argonauts Code, please report it the same way.

## Supported versions

While Argonauts Code is 0.x, security fixes go into the latest release only.
