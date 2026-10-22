# Prompts

Every prompt Argonauts Code sends an agent starts as one of the files in this folder. Nothing is locked: edit a file and the next agent that starts reads your text. Delete a file and the built-in one is in use again.

A file you never edit is a copy: it keeps following the built-in as newer versions of Argonauts Code reword it. Once you edit one it is yours, and it stays as you wrote it.

## What an agent reads, in order

1. **The frame**, `frames/<role>.md`: who the agent is, what it may touch, and the shape of its answer.
2. **Its instructions.** By default the orchestrator writes them for each agent and each request. Where it wrote none, the agent gets the file for its step, such as `review/review.md`.
3. **Your file for the role**, `.argonauts/roles/<role>.md`. It is added after the instructions. With `mode: replace` at its top it takes their place.
4. **What your request said for the role**, such as "tell the reviewers to focus on date parsing".

Open an agent and press Tab for its Input tab: it shows each part and where it came from.

## Who writes the instructions

```toml
[prompts]
writer = "orchestrator"   # orchestrator · built-in · or a model, such as "codex/gpt-5.6"

[prompts.judge]
writer = "built-in"       # one role can be different
```

- `orchestrator`: the orchestrator's own words for that agent.
- `built-in`: the files here, as they are or as you changed them.
- a model: that model writes each agent's instructions before the agent starts.

## Roles

There are five: `main`, `orchestrator`, `worker`, `reviewer`, `judge`. Every finer role follows one of them. A critic reads the reviewers' frame and file, unless you add `frames/critic.md` or `roles/critic.md`: `/prompts copy frames/critic` and `/prompts role critic` write them, and `/prompts` then lists the critic after the reviewers.

## Words a frame may use

- `{{others}}` in `frames/worker.md` says whether other workers build beside this one.
- `{{author}}` names whoever made the work. The reviewers' and the judge's frames say "You aren't told who wrote it" instead. To tell them, replace that line with one that uses `{{author}}`.

## Where files are read from

`.argonauts/prompts/` in the project first, then `~/.argonauts/prompts/`.

- `/prompts` lists every part, who writes it, and which files you changed. In the list, `e` copies the selected one out to edit, `w` picks who writes for its role, `r` goes back to the built-in, `d` compares yours with it, and Enter shows the text in use.
- The same typed: `/prompts copy <name>` (or `copy all`), `/prompts writer [role] <who>`, `/prompts reset <name>`, `/prompts diff <name>`, `/prompts show <name>`.
- `/prompts role <role>` writes a starter `.argonauts/roles/<role>.md` for your own instructions. It changes nothing until you write in it. `/prompts reset role <role>` deletes your file for the role, as `r` does on its instructions row.

Or ask the main agent: "make the reviewers stricter about missing tests".
