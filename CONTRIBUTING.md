# Contributing to Argonauts Code

Thanks for helping. Bug reports, ideas and pull requests are all welcome.

- **A bug or a feature:** open an issue. For anything bigger than a small fix, open the issue before the pull request, so we can agree on the approach before you spend time on it.
- **A question:** ask in [Discussions](https://github.com/sheng-tse/argonauts-code/discussions).
- **A security problem:** never in a public issue. See [SECURITY.md](SECURITY.md).

Everyone taking part follows the [code of conduct](CODE_OF_CONDUCT.md).

## Set up

You need Rust 1.95 or newer, git, and macOS or Linux. On Linux, the sandbox tests need kernel 6.2 or newer and unprivileged user namespaces; on Ubuntu 24.04, run `sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` first, as CI does. No agent CLI is needed to build Argonauts Code or run its tests.

```sh
git clone https://github.com/sheng-tse/argonauts-code
cd argonauts-code
cargo build
cargo run -p argonauts-code -- doctor
```

Working with a coding agent? It reads [AGENTS.md](AGENTS.md), which points it to the guides in `.agents/skills/`. Codex, agy and OpenCode load those guides as skills on their own. For Claude Code, link them once: `mkdir -p .claude && ln -s ../.agents/skills .claude/skills`.

## Checks

Run these before every pull request:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
CI=true cargo test --workspace
```

CI also runs `cargo deny --locked check licenses advisories sources`, `cargo publish --workspace --dry-run --locked`, the install-script tests (`python3 -m unittest discover -s packaging/tests`) and the workflow linters, and checks that the pull request title is a Conventional Commit.

Screens are tested with [insta](https://insta.rs) snapshots. When you change a screen on purpose, review the new snapshots with `cargo insta review` (install it with `cargo install cargo-insta`) and commit the updated `.snap` files with your change. Some screens are also compared cell for cell with pictures in `crates/*/tests/golden/` (a `.txt` for the characters, an `.html` for the colours), which `cargo insta review` doesn't touch. When one changes on purpose, run its test with `ARGONAUTS_PICTURE_DUMP=<a folder>` to write what the code draws, and update the picture in the same pull request.

Tests never touch your own `~/.argonauts`: everything cargo runs here uses a scratch home instead.

## How the tests work

- **No test calls a model.** Each CLI's output is recorded once and replayed. Recordings live in `crates/adapters/<id>/fixtures/` (and OpenCode's run recordings in `crates/conductor/fixtures/opencode/`), a few files per scenario, with the CLI version that produced them in the `<scenario>.meta.json` beside them.
- **Fakes cover the rest:** `crates/testkit` has fake agent CLIs, throwaway git repos and a terminal driver.
- **When a CLI changes its output,** re-record with `crates/conductor/scripts/rerecord-fixtures.sh --live --scratch <a folder outside this repo>`. It makes three live calls on the cheapest model of Claude Code, Codex and agy each (OpenCode is recorded by hand), in throwaway repos in that folder, and says which recordings changed shape. Before you commit a recording, take out anything about you: your account's plan, email, home path, or anything else you wouldn't post publicly.

## Code

- `crates/core` holds the shared types. Every other crate is one part: `tui`, `app`, `cli`, `crew`, `conductor`, `runner`, `learning`, the adapters (one per CLI, plus `acp` and `manifest` for other agents), `policy`, `mcp`, `registry`, `endpoints`, `scenarios` and `testkit`.
- Readable first, fast where it counts. Match the code around yours, and keep comments for the why, not the what.
- New behavior comes with a test. A screen change comes with a before and after screenshot in the pull request.

## Commits and pull requests

- Titles follow [Conventional Commits](https://www.conventionalcommits.org): `feat:`, `fix:`, `perf:`, `docs:`, `refactor:`, `test:`, `ci:`, `build:` or `chore:`, lowercase, in the imperative, with no period at the end, for example `fix(codex): read the new usage line`. Pull requests are squash-merged, so the title becomes the commit message.
- Keep each pull request to one change. Small ones get reviewed sooner.
- AI-assisted pull requests are welcome. You're responsible for the change all the same: run the checks, read every line, and be ready to explain it in review.
- By contributing, you agree that your contribution is licensed under [Apache-2.0](LICENSE), the project's license.
