# Roles

The crew has five roles: `main`, `orchestrator`, `worker`, `reviewer` and `judge`. Which
model fills a role is `[crew]` in `config.toml`. What you want a role to do is a file here,
named after it.

| Folder | Whose |
|---|---|
| `.argonauts/roles/` | this project's |
| `~/.argonauts/roles/` | yours, in every repository |

## A role's file

```markdown
---
mode: add            # add (default) · replace
---
Check every public function for missing error handling.
Treat a missing test as a required change, never as a note.
```

Saved as `.argonauts/roles/reviewer.md`, that text reaches every reviewer.

| Line | What it is | Default |
|---|---|---|
| `mode: add` | Your text is added after the built-in instructions, under "Your instructions". | yes |
| `mode: replace` | Your text takes the place of the built-in instructions for the role. Reviewers and the judge take added text only. | |

The lines between the two `---` are optional. A file with only your text is `mode: add`.

## How your text reaches a role

There are four ways, from the most lasting to the most passing.

| When | How | Example |
|---|---|---|
| Always | A file here. | `.argonauts/roles/reviewer.md` |
| For one workflow | The step's `does` line in the workflow file. | `does = "Review the change against the spec."` |
| For one request | Say it. The screen shows what was passed on, and to whom. | "add due dates, and tell the reviewers to focus on date parsing" |
| While it runs | `@` the role and your message. | `@reviewer the parser moved to dates.rs` |

The text travels inside the prompt, so it works the same on every CLI. Open an agent and its
Input tab shows exactly what it was sent, part by part: the frame, the orchestrator's
instructions for that agent, your file for the role, and your words in the request.

## What a reviewer is never told

A reviewer isn't told who wrote the work it reviews: no model's name and no CLI's. That is
what keeps a review independent, since a model judges its own provider's work more kindly.

So reviewers and the judge take added text, never a replacement: their built-in part stays.
Every other role may be replaced. And a `reviewer.md` that names a model or a CLI gets a
warning, since it would tell the reviewer what Argonauts Code leaves out.

That rule is one line of the reviewer's own prompt. If you do want to change it, `/prompts`
writes the prompt out as a file in `.argonauts/prompts/`, and deleting that file goes back.

## A file that came with a clone

A roles file changes what an agent is told. One that arrived with a cloned repository asks
for your trust once before it is used, and again when it changes.

The fields here are those of Argonauts Code {version}.
