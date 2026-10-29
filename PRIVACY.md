# Argonauts Code: what it sends, and how to stop it

Argonauts Code sends one small, anonymous report a day about how it is used. The first time it starts, it tells you so, and nothing is sent before then. One command turns it off for good.

## What is sent

Once a day, when Argonauts starts or an `argonauts run` ends, it adds up the days since its last report and sends one event. This is all of it:

- the Argonauts version, your system (such as `macos` or `linux`), and your machine in rough brackets: the kind of chip (such as `apple_silicon`), memory and CPU cores rounded down to a power of two, and the kind of terminal from a fixed list;
- how Argonauts is set up, in fixed words: inline or full screen, the sandbox and approval modes, whether autopilot is on, and whether the folder it started in is a git repository;
- the coding CLIs you have installed and their versions (such as Claude Code 2.1.295);
- for each model you used: its public id, or for a model on your own computer only its series (such as `qwen` or `gemma`), the CLI that ran it, its tokens (input, output and cached), what those tokens are worth at the model's list price, whether you paid through a plan, an API key, or a model on your own computer, the roles it filled (such as main agent, worker or reviewer), and the effort levels your settings run those roles at;
- how the work went, as counts: runs for each built-in workflow and how each ended (merged, opened a pull request, left on its branch, dropped, stopped or failed), turns your main agent answered itself, how many review rounds tasks took, how many review findings were given and fixed and by which models, how checks went, who approved merges, and how often an agent fell back to another model and why (signed out, a usage limit, a service down, and so on);
- counts of features you used: goals you set, ultracode, how many mods you have, the built-in commands you used by name (never what you typed after them), and how often you opened the workflow view;
- health counts: crashes by the part of Argonauts' own code they happened in, how many kinds of CLI output Argonauts didn't understand, and how long starting took, in three brackets;
- what your agents did, as counts: for each model you used and the role it worked in (main agent, orchestrator, worker, reviewer, judge or merger), how many of its actions fell under each label of a fixed list that ships with Argonauts Code (such as `git_history`, reading git history, or `edit_test_added`, adding tests), how many fit no label, and for each label how many of those actions were in work that merged, was sent back, or was dropped;
- a random install id, made on your computer before the first report;
- the version of this list of fields, so a report says which shape it has.

## What is never sent

- your prompts, the answers you get, or any code;
- the names of your files, folders, repositories, or workflows you wrote yourself (they are sent as "custom");
- the names you gave your own model servers (sent as "endpoint"), or the name of a model without a public price (one on your own computer goes by its series alone, such as `qwen`, and any other as "other");
- your accounts, emails, or keys;
- anything read from a CLI's own session files;
- the actions themselves: no command, path, file content, project or run name, and none of the summaries the deeper Behaviors map writes (those stay on your computer).

The install id is not linked to you, your account, your email, or a folder. The reports are marked anonymous, so the receiving service builds no profile of you, and it is set to throw away the address your computer sends from.

## Why

To learn which CLIs and models people run with Argonauts, how much they use them, which versions are out there, and which features are used, so the work goes where it helps; and to publish totals across everyone on a public page, so anyone can see how coding agents and models are used.

## Where it goes

To PostHog Cloud in its US region (`us.i.posthog.com`), a product analytics service, in the Argonauts project. Only the people who run Argonauts can see the reports. PostHog keeps them for one year, then deletes them.

A public page shows totals across everyone, once there are enough reports for totals to mean something, and only for groups of at least 10 installs. It never shows one person's report. The page stores only each day's totals, never the reports themselves.

Questions about the reports, or a request to delete what an install id sent: email the address in [SECURITY.md](SECURITY.md) with "Argonauts privacy" in the subject.

## Other requests it makes

These carry nothing about you or your work beyond what any download shows, such as your computer's address and the Argonauts version, except that Anthropic sees your key when you've set one.

- Once a day it reads `https://argonauts-code.github.io/updates/v1/stable.json` (or `beta.json`) to see whether a new release is out, and a copy from the installer downloads it from this repository's releases. `updates.check = false` in `~/.argonauts/config.toml`, or `ARGONAUTS_DISABLE_UPDATES=1`, stops both.
- Once a day it reads the model catalog served beside the update manifests (`https://argonauts-code.github.io/updates/v1/models.toml`, with its signature beside it) and Anthropic's public model pages (`platform.claude.com`), or, with `ANTHROPIC_API_KEY` set, Anthropic's model list with that key, so `/model` shows the current models and ranks the local ones for your computer. Nothing about your computer is sent to rank them: the ranking happens on it.

## How to see it, and how to turn it off

- `argonauts telemetry show` prints the next report in plain words, one row for each part, and says whether it would be sent.
- `argonauts telemetry off` stops the reports. `argonauts telemetry on` starts them again.
- Setting `DO_NOT_TRACK=1` in your environment stops them, whatever else is set.
- The setting is `telemetry.enabled` in your own `~/.argonauts/config.toml`. Once you turn it off, a repository's settings can't turn it back on, and neither can an agent working for you.
- On CI machines (when `CI` is set), nothing is sent.
- Every optimized build sends the report, wherever it came from: the installer, Homebrew, npm, `cargo install argonauts-code`, or a clone built with `cargo build --release`. A debug build, such as `cargo run` in a clone, never sends anything.
- Whoever builds Argonauts can send its reports to a PostHog project of their own, with `ARGONAUTS_POSTHOG_KEY` and `ARGONAUTS_POSTHOG_HOST` set while it compiles, or build a copy that never sends, with `ARGONAUTS_POSTHOG_KEY` set empty. A build that sets `ARGONAUTS_POSTHOG_HOST` without `ARGONAUTS_POSTHOG_KEY` sends nothing, so Argonauts' key never goes to another host. The key built in is Argonauts' project key, which can add reports and never read them.

If the network is down, the report waits for the next day. It never slows Argonauts down and never shows an error.
