# .argonauts

Everything Argonauts Code reads that you can change is a file in a folder named `.argonauts`.
There are two:

| Folder | Whose | Wins |
|---|---|---|
| `~/.argonauts/` | yours, in every repository | |
| `.argonauts/` in a repository | that project's, shared with whoever clones it | over yours, key by key and file by file |

Every file is optional. Delete one and the built-in behaviour is back. `/setup` writes the
starter files and these READMEs, and never writes over a file you changed.

## What each file and folder is for

```
.argonauts/
  README.md            this page
  config.toml          the settings: the crew, the workflow in use, approvals, limits, fallbacks
  config.local.toml    the same keys, just for you in this repository. Kept out of git
  models.toml          models and CLIs beyond the ones Argonauts Code already knows
  approval-rules.md    your own rules for what may be approved without you
  conventions.md       what every agent should know about this repository
  workflows/
    README.md          every field of a step, with examples
    orchestrator.toml  council.toml  ...    how the crew works, one file a workflow
  roles/
    README.md          how your text reaches a role, and what a reviewer is never told
    reviewer.md  ...   your own instructions for a role
  mods/
    README.md          every field, each body with a picture, the JSON a command prints
    stats.toml  bottom-line.toml  branch.toml  checks.toml
    checks.sh          the program checks.toml runs, waiting for your numbers
  prompts/             the built-in prompts, written out when you want to change one
  learned/             workflows Argonauts Code learned here, with their versions
  goal/  notes/  decisions/    what runs in this repository agreed on and wrote down
```

`~/.argonauts/` also holds what is only ever yours: `state/` with each repository's runs and
conversations, and the files Argonauts Code keeps for itself. Nothing there is for editing.

## Three ways to change something

| How | Good for |
|---|---|
| Say it to the main session: "use Opus for the workers", "make me a mod for CI" | anything, in your own words. It shows what it would write and asks where to keep it |
| A command: `/config`, `/workflow`, `/mods`, `/model`, `/approvals`, `/limits` | one setting, with its choices on screen |
| Edit the file | several things at once, or a file you want to review and commit |

A file that doesn't read is never half used. `/doctor` names it, says what is wrong in plain
words, and everything else keeps working.

## Which file wins

For a setting: built-in, then `~/.argonauts/config.toml`, then the project's `config.toml`,
then your `config.local.toml`, then what you set for this session. `/config` shows every key
with the place its value came from.

For a workflow, a role's instructions or a mod: the project's file, then yours, then the
built-in one of the same name.

A project's file can't loosen a safety setting, run a command, or change what an agent is
told until you trust it once. Argonauts Code asks the first time it sees the file, and again
when the file changes.

The fields on these pages are those of Argonauts Code {version}.
