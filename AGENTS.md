# Working on Argonauts Code

Instructions for coding agents, and anyone else, changing this repository. People should start with [CONTRIBUTING.md](CONTRIBUTING.md).

Argonauts Code is a Rust terminal app that runs coding-agent CLIs (Claude Code, Codex, agy, OpenCode) and model endpoints as one crew. It looks and feels like Claude Code, and runs on macOS and Linux.

## Where things are

- `crates/core`: the shared types. Every other crate is one part: `tui` (the screen), `app` (the controller), `cli` (the `argonauts` command), `crew` and `conductor` (workflows and runs), `runner` (workflow files and scripts), `learning` (track records and learned workflows), `adapters/*` (one per CLI, plus `acp` and `manifest` for other agents), `policy`, `mcp`, `registry` (settings and models), `endpoints` (model servers), `scenarios` (the real binary in a terminal) and `testkit` (fakes for tests).
- `crates/adapters/<id>/fixtures/` and `crates/conductor/fixtures/opencode/`: recorded CLI output, a few files per scenario, with the CLI version in the `<scenario>.meta.json` beside them. Tests replay these.
- `crates/core/templates/`: the prompts, roles, workflows and mods Argonauts Code ships.

## Skills

Step-by-step guides for jobs that come up often. Read the one that fits before you start:

- `.agents/skills/recording-a-cli/SKILL.md`: when a CLI's output changed or a scenario needs a new recording.
- `.agents/skills/live-runs/SKILL.md`: before anything starts a real agent CLI or model outside the tests.

Codex, agy and OpenCode also find these on their own. Claude Code reads skills from `.claude/skills/`, so link it once with `mkdir -p .claude && ln -s ../.agents/skills .claude/skills` (git ignores `.claude/`).

## Before you say a change is done

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
CI=true cargo test --workspace
```

When a screen changes on purpose, review the snapshots with `cargo insta review` and keep the updated `.snap` files, and update any picture in `crates/*/tests/golden/` it is compared with (`ARGONAUTS_PICTURE_DUMP=<folder>` writes what the code draws).

## Always

- Add a test for new behavior. Use the fakes in `testkit` and the recorded fixtures.
- Match the code around yours: readable first, fast where it counts, comments only where they explain why.
- Keep a change to what was asked. Say what you couldn't finish rather than leaving a stub.
- Write commit messages and pull request titles as [Conventional Commits](https://www.conventionalcommits.org) (`fix(codex): read the new usage line`), describing the change itself.
- When you adapt code from another project, say so in the file's header with its license, and add its attribution to `NOTICE` if that project has a NOTICE file.

## Never

- Call a real model or agent CLI from a test.
- Read an agent CLI's credentials, tokens or session files (`~/.claude/.credentials*`, `~/.codex/auth.json`, and the like), or point `ARGONAUTS_HOME` at a real `~/.argonauts`. Tests get a scratch home on their own.
- Re-record fixtures other than as `.agents/skills/recording-a-cli` says, or start a real CLI other than as `.agents/skills/live-runs` says.
- Commit a recording, picture or snapshot that says whose machine or account made it: a home folder path, a user or machine name, an account plan, an email, or an account or project id. A test in `testkit` fails on them.
- Paste a conversation with an assistant into code or comments. Comments say why the code is the way it is.
- Change `LICENSE`.
