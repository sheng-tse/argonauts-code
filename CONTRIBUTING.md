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

When a CI job fails for a reason that isn't your change, comment `/bot rerun failed` on the pull request to rerun the failed jobs of its latest commit. As its author you can ask twice for each commit. Maintainers can rerun jobs any time, and stop the runs still going with `/bot stop`.

Screens follow the approved drawings in `approved/`: one text grid per panel, state and window size (`approved/<panel>/<state>@<cols>x<rows>.txt`), each listed with its hash in `approved/MANIFEST.toml`. A screen is right when it matches its drawing at 120 × 38 and 177 × 40, not when it matches what the code drew before. The acceptance runner in `crates/scenarios/tests/it/app_drawings.rs` brings the app to the screen of each drawing whose manifest row lists the steps that reach it, and compares it cell for cell. Running it with `ARGONAUTS_SHOTS_DIR=<a folder>` saves each screen that doesn't match its drawing, in the drawings' format, under `<a folder>/approved/`, to set beside the approved one or to attach to the issue that proposes a new look.

The [insta](https://insta.rs) snapshots (`.snap`) and the pictures in `crates/*/tests/golden/` (a `.txt` for the characters, an `.html` for the colours) only record what the code drew. When one of them stops matching, treat it as a finding. If your change wasn't meant to change the screen, fix the code. If it was, open an issue first with a before and after screenshot: a new look lands once a maintainer approves it and changes its drawing in `approved/`, and a snapshot or picture changes only to match a drawing, never just to make a test pass. To see the difference, run the test with `ARGONAUTS_PICTURE_DUMP=<a folder>`, which writes what the code draws, or use `cargo insta review` (install it with `cargo install cargo-insta`).

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
