# Changelog

Notable changes to Argonauts Code, newest first. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/). While the version starts with 0, a minor release may change settings or commands; the notes say how to move over.

## [Unreleased]

## [0.2.0] - 2026-10-29

Council is a preview in this release.

### Added

- Council as a group conversation: each seat answers alone, then the seats talk one at a time until they agree on a plan, without knowing which model or CLI the others run on. You can write to the whole council or to one seat while it talks, and each task is checked by a fresh session from another maker when one is set up. `[[council.seat]]` sets the seats.
- Each task of a plan is done in its worker's own session, by a workflow, or by 2 to 4 agents that each take a part. The plan picks the way, a way named in your request binds, and `[crew.ways]` and `[council.ways]` say which ways are allowed.
- `/captain`, `/orchestrator`, `/council` and your own workflows' commands switch to that workflow, or run one request with it when you add words. A workflow that isn't ready opens its setup sheet in place of the input.
- Behaviors is drawn as a world grown from your crew's records: a map whose mountains, climate and weather come from what the agents do, a planet, and a universe with a galaxy for each model maker and a system for each model. Rare things your crew does raise wonders on it. Terminals that show pictures, such as Ghostty and kitty, draw it in full, and others in braille (`behaviors.view`).
- Logos before each CLI's and model's name in Ghostty and kitty, and the coloured dot elsewhere. `ui.logos = "off"` keeps the dots.
- Who paid for the tokens: a run's end line and Overview's Tokens split them between your plans, free use such as local models, and paid use with what it cost (`ui.paid_by`).
- When you set no checks, the main agent can name the checks a run should pass. They hold for that run alone, run on each task and on the merge, and are saved if you ask to keep them. A recap says the checks pass only when every one ran and passed.
- `/resume` names the CLI that holds each session and opens it there with `o`, and quitting after a turn prints that CLI's own command to carry on. `argonauts resume <id>` finds any CLI's session by its id.
- `/bottom-line` opens a picker to choose and order the CLIs under the input, with the line as a live preview, and `/bottom-line add`, `remove` and `reset` change it at once.
- Stats' legends are aligned tables. Point at a model to light it in every bar, change the sort, filter with `/`, and press Enter to open its details.
- `/model` lines models up in columns, and suggests the local models that fit your computer, ranked on your computer.
- In Supervised and Auto preview, the agent you talk to can write outside the project once you allow it. Claude Code asks as it always does, and when Codex, OpenCode or agy is refused a write there, "Let <CLI> write outside the project?" asks you. `outside.ask` lists these modes, and emptying it keeps them to the project.
- A shell command's writes outside the project get a "wrote …, outside the project" line too, and a read or write the never-list refused says "kept out".
- Keeping the computer awake works on Linux too, through `systemd-inhibit`, and a bare `/awake` switches the hold on and off.
- When an update can't be put in place, the next start says why, once, and `argonauts doctor` opens with the version running and what the last update check found.

### Changed

- Changes you make in the app save at once, with no question about where. `save_changes_to` picks the place, your global file by default, and only your global file may set it. `--session`, `--local`, `--project` or `--global` after a command puts that one change there, and ctrl+s in a picker keeps it for this session. A safety change still asks you to confirm.
- Words that set the crew hold for that request, or save at once with "from now on". "Keep this crew?" is gone.
- A reviewer the app picks is never the task's own builder, and an approval only from a reviewer of the builder's own provider that you didn't choose no longer merges the work by itself.
- The bottom line shows the CLI the main session is on and the one you used most over the last 30 days, instead of every CLI. Another CLI joins it while a running task waits on it and it is out, down or near a limit.
- Each CLI keeps one colour everywhere, and its models are told apart by shape. OpenCode moves from green to magenta, so green, amber and red only ever mean a state.
- Run rows follow Claude Code's columns, and each state has its own mark, so none is told apart by colour alone.
- The never-list keeps agents from writing `/bin`, `/sbin`, `/lib`, `/lib32`, `/lib64` and `/libx32` too. `.env.example`, `.env.sample`, `.env.template` and `.env.dist`, the templates projects commit, are off it; other `.env` files stay on it.
- Claude Code and Codex sessions in a run start with the CLI's own memory off. The session you talk to keeps yours.
- Copies you build yourself, with `cargo install argonauts-code` or `cargo build --release`, now send the daily usage report too, with the same notice on first start and the same ways to stop it. A debug build never sends (see [PRIVACY.md](PRIVACY.md)).
- The daily model catalog now comes from beside the update manifests, signed with the keys that sign updates.
- The deeper Behaviors map is now called Summaries: `argonauts behaviors --deep` is now `--summaries`, and `behaviors.map = "deeper"` is now `"summaries"`, which the app offers to change at start.
- `ui.motion = "normal"` is now `"full"`, and the app offers to change it at start. `"off"` joins `"reduced"`.

### Removed

- `ui.panel.bar`, since each run row's bar now counts what sits under it. The app offers to take the line out at start.

### Fixed

- A message posted to an agent while its session was starting reached no one. It now arrives at the session's next turn.
- An agent's program starts only once it is registered as an agent, so a command it runs at once can no longer pass for yours to answer an approval or change a setting.
- The "wrote …, outside the project" line shows only once the file really changed, not for a write the CLI reported but its own rules refused.
- OpenCode's context estimate counts the project's `AGENTS.md`, and a task's prompt no longer points OpenCode at a file it already read.
- A session that names no model is shown and priced as the model it runs, so Stats and Behaviors no longer list a CLI as a model, and its spend is no longer estimated at an older model's price.
- The crew table for a request names each model in full, as the request's start line does.
- A merge you answered before its commit message was written no longer leaves an empty "Stopped" row.
- A Ctrl+C meant for another program no longer ends the hold that keeps the computer awake.
- On Linux, the npm package says how to install `xz` when it is missing, instead of failing to unpack.

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

[Unreleased]: https://github.com/sheng-tse/argonauts-code/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/sheng-tse/argonauts-code/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/sheng-tse/argonauts-code/releases/tag/v0.1.0
