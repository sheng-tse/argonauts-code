# Templates

Everything Argonauts Code is built from that you can make your own: the workflows, the prompts
each agent reads, an example of your own instructions for each role, and the starter mods.
The program is built from these files, so what you read here is what runs.

Argonauts Code keeps a copy of this folder in `~/.argonauts/templates/`, written the first time
it starts and again when it updates. Those are reference copies. Edit a copy in one of the
places below, never the reference, which the next update writes over.

## What is here

| Folder | What it holds |
|---|---|
| `workflows/` | How the crew works, one file a workflow: Council, Orchestrator, Maker-checker, Best-of-n and the rest. |
| `prompts/` | Each role's frame and the other prompts, one file each. |
| `roles/` | An example instructions file for each role. |
| `mods/` | The starter mods. |

Each folder has a README that says what its files hold and how they are read.

## Where a copy goes to take effect

| Copy into | Whose |
|---|---|
| `.argonauts/` in a repository | that project's, shared with whoever clones it |
| `~/.argonauts/` | yours, in every folder |

Keep the folder a file sits in: `workflows/council.toml` goes to `.argonauts/workflows/council.toml`,
and `prompts/frames/reviewer.md` to `.argonauts/prompts/frames/reviewer.md`. The project's file
wins over yours, and deleting your file goes back to the built-in.

`/templates` lists the workflows and the prompts, marks the workflow in use and the files you
changed, and copies one into place: `/templates <name>` for this folder, and
`/templates <name> --global` for every folder.
