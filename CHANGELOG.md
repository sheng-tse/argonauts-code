# Changelog

Notable changes to Argonauts Code, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). While the version starts with 0, a minor release may change settings or
commands; the notes say how to move over.

## [Unreleased]

## [0.1.0] - unreleased

The first public release.

### Added

- Run Claude Code, Codex, agy and OpenCode as one crew, each through its own CLI, sign-in and plan.
- Workflows: Captain, Orchestrator, Council, spec-first, and your own as TOML files. Ultracode puts a workflow on
  every request, and autopilot lets the crew answer its own questions.
- A git worktree and branch for every task, a sandbox that lets each agent write only there, review by another
  provider when your crew has one, your checks on the reviewed commit, and a merge that waits for you.
- Open-weight models: OpenCode on a local model writes code, and Ollama, LM Studio and other OpenAI-compatible
  servers review, judge and summarize.
- The workflow view, the agent panel, the bottom line with your plans' usage, Overview (`/usage`, `/stats`,
  `/context`) and `/config`.
- `argonauts run` for scripts and CI, `argonauts resume`, `argonauts doctor`, and an MCP server (`argonauts mcp`).
- One anonymous usage report a day, announced on first start and stopped with `argonauts telemetry off` or
  `DO_NOT_TRACK=1` (see [PRIVACY.md](PRIVACY.md)).

[Unreleased]: https://github.com/sheng-tse/argonauts-code/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/sheng-tse/argonauts-code/releases/tag/v0.1.0
