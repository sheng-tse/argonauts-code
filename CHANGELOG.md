# Changelog

Notable changes to Argonauts Code, newest first. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/). While the version starts with 0, a minor release may change settings or commands; the notes say how to move over.

## [Unreleased]

## [0.1.0] - 2026-10-23

The first public release.

### Added

- Run Claude Code, Codex, agy and OpenCode as one crew, each through its own CLI, sign-in and plan.
- Workflows: Captain, Orchestrator, Council, maker-checker, best-of-n and draft-and-vote, and your own as TOML files or scripts. Ultracode puts a workflow on every request, and autopilot lets the crew answer its own questions.
- A git worktree and branch for every task, review by the reviewers you name or, by default, by another provider when your crew has one, the checks your settings set on the reviewed commit, and nothing merged without you or the rules you set.
- Approval modes from Supervised to Autopilot. In Auto and Autopilot agents read and write outside the project too, and a sandbox keeps your SSH, cloud and GPG keys, `.env` files, browser profiles and other CLIs' logins out of every agent's reach, and system folders read-only.
- Open-weight models: OpenCode on a local model writes code, and Ollama, LM Studio and other OpenAI-compatible servers review, judge and summarize.
- The workflow view, the agent panel, the bottom line with your plans' usage, Overview (`/usage`, `/stats`, `/context`) and `/config`.
- Behaviors (Overview › Behaviors): a map of what each model's agents do, one model compared with the rest, and a deeper map a local or hosted model summarizes once. `argonauts behaviors` prints the ranking and exports Hodoscope's format.
- `/issue` starts work from a GitHub issue, and a pull request's checks and reviews are followed until it can merge.
- `argonauts self-update` and `/update` keep Argonauts Code and your agent CLIs current, with `--rollback`.
- `argonauts run` for scripts and CI, `argonauts resume`, `argonauts doctor`, and an MCP server (`argonauts mcp`).
- One anonymous usage report a day, announced on first start and stopped with `argonauts telemetry off` or `DO_NOT_TRACK=1` (see [PRIVACY.md](PRIVACY.md)).

[Unreleased]: https://github.com/sheng-tse/argonauts-code/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/sheng-tse/argonauts-code/releases/tag/v0.1.0
