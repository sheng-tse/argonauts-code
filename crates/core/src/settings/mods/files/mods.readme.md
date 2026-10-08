# Mods

A mod is one drawn card: colour, bars, meters, marks, small charts. It is never a block of
text or a page of options. A mod is a file, and this folder holds them:

| Folder | Whose |
|---|---|
| `~/.argonauts/mods/` | yours, in every repository |
| `.argonauts/mods/` | this project's, shared with whoever clones it |

The built-in cards (context, plan-usage, spend, tokens, runs and waiting) are mods too, drawn
from Argonauts' own numbers. Where each shows is yours: `/mods <name> <place>` moves it. How
it draws is in your settings, under `ui.mods.<name>` in `/config`. A file here with one of
their names changes neither. When both folders hold a mod of the same name, the project's
wins.

`/mods` lists every mod with a live preview. `/mods <name>` opens one mod's page, where each
change shows in the preview as you make it and `s` saves it into the file. Editing the file
by hand does the same: a saved file loads within a second, with no restart. A built-in card's
page offers only where it shows, and `s` saves that into your place lists.

An Argonauts mod is data: a file that says what to draw and where the numbers come from. A
Claude Code mod is code that runs inside Claude Code. Yours still load in every Claude Code
session Argonauts starts, and their toasts, status lines, log lines and commands show here.
Their bands and panes don't.

## A mod file

```toml
# ~/.argonauts/mods/checks.toml
name   = "checks"              # also its command: /checks shows or hides it
title  = "checks"
body   = "stack"               # stack · meters · list · columns · sparkline · marks · text
places = ["above"]             # above · mode-line · overview · status · usage · context · limits · stats
form   = "full"                # full · compact · line
key    = "ctrl+x k"            # optional

[data]
command = ["./checks.sh"]      # a program of yours that prints the JSON below
every   = "60s"
timeout = "2s"
# source = "plan_usage"        # or a built-in source, in place of the command
```

## Every field

| Field | What it is | Default |
|---|---|---|
| `name` | Lowercase letters, digits, `-` and `_`, starting with a letter. It must match the file's name, and it can't be a command's name such as `model`. | the file's name |
| `title` | The bold word in the card's header. | the name |
| `body` | What is drawn under the header. See the pictures below. | none: a mod needs one |
| `places` | Where it shows, in the order written. An empty list shows it nowhere but `/mods`. The bottom line keeps its own list: see below. | `[]` |
| `shown` | `false` hides it everywhere and keeps its places. Its own command flips this. | `true` |
| `form` | `full`: frame, header, body and legend. `compact`: frame, header and the bar. `line`: one row. | `full` |
| `key` | Two keys in a row that hide it or show it again: `ctrl+x k`. | none |
| `[data] source` | A built-in source. See the list below. | one of `source` and `command` is needed |
| `[data] command` | A program and its arguments, as a list. `./name` is a program beside this file. | |
| `[data] every` | How often the command runs: `10s`, `5m`, `1h`. At least `1s`. | `60s` |
| `[data] timeout` | How long it may take before it is stopped. At most `30s`. | `2s` |
| `[levels] warn` | The percent from which a meter is amber. | `70` |
| `[levels] danger` | The percent from which a meter is red. | `90` |
| `[colors]` | A colour for a part, by its label: `passed = "green"`. It wins over the colour the data gives. | none |

The places:

| Place | Where |
|---|---|
| `above` | above the input, as a card |
| `mode-line` | on the bottom line, as a few words |
| `overview`, `status`, `usage`, `context`, `limits`, `stats` | on that tab of the Overview, as a card |

The bottom line is the one place a mod's own file doesn't decide. That line shows what its own
file lists, `bottom-line.toml` in this folder, so a mod gets there by being ticked on: on its
own page, with `/mods <name> mode-line`, or on `/mods bottom-line`, which also gives it a side
and an order. `mode-line` written into `places` by hand only asks, and `/mods` says so beside
the mod until one of those ticks it on.

The built-in cards keep their places in your settings, in order: `ui.above_input.mods` for
above the input, and `ui.views.<tab>.mods` for the Status, Usage, Context and Limits tabs.
Their pages, `/pin` and `/mods <name> <place>` write those lists, and `/mods` shows what they
hold.

The colours: `green`, `amber`, `red`, `blue`, `purple`, `cyan`, `orange`, `pink`, `gold` and
`grey`. Each is a colour of your theme, so a mod looks right in the dark and in the light.

## Each body, with a picture

The pictures are drawn without colour. On screen each part takes its own.

`stack`: a stacked bar and its legend. Each part is a share of the whole.

```
╭────────────────────────────────────────────────────────────╮
│ ◆ stack                           7 of 9 passed [1 failed] │
│ ██████████████████████████████████████████████████████████ │
│ ▌ passed      7  78%   ▌ running     1  11%                │
│ ▌ failed      1  11%                                       │
╰────────────────────────────────────────────────────────────╯
```

`meters`: one meter a row. Each part is measured against its own `max`, or against the
largest part when it gives none. A part's `note` goes at the right.

```
╭────────────────────────────────────────────────────────────╮
│ ◆ meters                                   56% of the week │
│ 5h ╸───────────────────────────────────  1%  resets 4:40pm │
│ 7d ━━━━━━━━━━━━━━━━━━━━──────────────── 56%  resets Tue    │
╰────────────────────────────────────────────────────────────╯
```

`list`: a mark, a name and a number a row. The mark follows the part's colour: green is done,
amber waits for you, red failed.

```
╭────────────────────────────────────────────────────────────╮
│ ◆ list                                              3 open │
│ ◆ review the plan                                       2m │
│ ✔ merge add-due                                        now │
│ ✗ fix the flaky test                                    1h │
╰────────────────────────────────────────────────────────────╯
```

`columns`: a column chart, one column a part, with the first letter of its label under it.

```
╭────────────────────────────────────────────────────────────╮
│ ◆ columns                            5.0M tokens this week │
│ 7   ▃   ▁ █                                                │
│   ▇ █ ▅ █ █ ▂ ▇                                            │
│   M T W T F S S                                            │
╰────────────────────────────────────────────────────────────╯
```

`sparkline`: the same numbers in one row, as a trend.

```
╭────────────────────────────────────────────────────────────╮
│ ◆ sparkline                          5.0M tokens this week │
│ ▃ ▆ ▂ ▅ █ ▁ ▃                                              │
╰────────────────────────────────────────────────────────────╯
```

`marks`: a square per item, in its part's colour. A part with value 7 is seven squares.

```
╭────────────────────────────────────────────────────────────╮
│ ◆ marks                           7 of 9 passed [1 failed] │
│ # # # # # # # # #                                          │
│ ▌ passed      7  78%   ▌ running     1  11%                │
│ ▌ failed      1  11%                                       │
╰────────────────────────────────────────────────────────────╯
```

`text`: the header's words alone. With `form = "line"` on the bottom line it is just the
words, as `main` is for the branch.

```
╭────────────────────────────────────────────────────────────╮
│ ◆ text                                                main │
╰────────────────────────────────────────────────────────────╯
```

Any body in the `line` form is one row:

```
 ▸ ◆ stack  ━━━━━━━━━━━━  7 of 9 passed             [1 failed]
```

## The built-in sources

| `source` | The numbers |
|---|---|
| `context` | the main session's context, used and free |
| `plan_usage` | each CLI's plan windows |
| `spend` | today's spend, by CLI |
| `tokens` | today's tokens: from cache, new input and output |
| `runs` | this repository's workflows and how each ended |
| `waiting` | what waits for you |

## The JSON a command prints

A command prints one JSON object and exits with code 0.

```json
{"summary": "7 of 9 passed", "level": "danger",
 "parts": [{"label": "passed", "value": 7, "color": "green"},
           {"label": "running", "value": 1, "color": "amber"},
           {"label": "failed", "value": 1, "color": "red"}]}
```

That is the `stack` picture above: `7` in bold, `of 9 passed` beside it, and the badge
`1 failed`, which is the part in the level's colour.

| Key | What it is |
|---|---|
| `summary` | The header's words. The first word is drawn bold, as the headline number. |
| `level` | `ok`, `warn` or `danger`. It colours the badge. Left out, it is `ok`. |
| `badge` | The badge's words. Left out, a reading that isn't `ok` names the first part in its level's colour: red for `danger`, amber for `warn`. |
| `parts` | The parts, in order. At most 64. |
| `parts[].label` | Its name. |
| `parts[].value` | A number, zero or more. |
| `parts[].max` | What the value is measured against. A meter needs it. |
| `parts[].color` | One of the colours above. Left out, parts take colours in turn. |
| `parts[].text` | How the value reads when the plain number won't do: `$1.86`, `840k`. |
| `parts[].note` | Dim words after the number: `resets 4:40pm`. |

A command that prints plain words works as it is: its first line is the summary. So this is a
whole mod:

```toml
# ~/.argonauts/mods/branch.toml
body   = "text"
places = ["mode-line"]
form   = "line"

[data]
command = ["git", "branch", "--show-current"]
every   = "10s"
```

`/mods branch mode-line` then puts it on the bottom line.

`/setup` writes a `checks.sh` beside `checks.toml` for you to start from. It prints nothing
until you put your numbers in, so the mod stays off the screen until then, and `/mods` shows
sample numbers. A program of your own needs `chmod +x`.

```sh
#!/bin/sh
# The numbers the checks mod draws. Put your own in: ask your CI, or count what your last
# test run said. While all three are 0 this prints nothing, and the mod stays off the screen.
passed=0 running=0 failed=0

total=$((passed + running + failed))
if [ "$total" -eq 0 ]; then
  echo "put your numbers in ${0##*/}, beside checks.toml" >&2
  exit 0
fi
level=ok
[ "$running" -gt 0 ] && level=warn
[ "$failed" -gt 0 ] && level=danger
printf '{"summary": "%s of %s passed", "level": "%s", "parts": [' "$passed" "$total" "$level"
printf '{"label": "passed", "value": %s, "color": "green"},' "$passed"
printf '{"label": "running", "value": %s, "color": "amber"},' "$running"
printf '{"label": "failed", "value": %s, "color": "red"}]}\n' "$failed"
```

With `passed=7 running=1 failed=1` it prints the JSON above.

## Show, hide and move a mod

| You type | What happens |
|---|---|
| `/mods` | Every mod, with a live preview of the one under the pointer. Enter opens its page. |
| `/mods checks` | Its page: body, form, places, how often its command runs, its levels. |
| `/mods checks above` | Shows it above the input, or takes it away when it is there. Any place works. |
| `/mods checks mode-line` | Puts it on the bottom line, last on the right, or takes it off. |
| `/pin checks`, `/unpin checks` | The same for above the input. |
| `/checks` | Hides it everywhere, or shows it again where it was. Every mod of yours is a command by its name. |
| `/mods bottom-line` | The bottom line's page: its parts, their order and their side, and the colour each CLI has in every mod. |

## Check a mod

`/doctor` names every file that didn't load and says what is wrong with it, each problem in
one line. `/mods` lists the same files under the mods. A file with a problem is skipped whole,
and every other mod keeps working. Fix the file and it loads at once.

A mod with no numbers yet draws nothing where it shows. `/mods` says why: its command hasn't
run, it ended with an exit code, it took too long, or what it printed didn't read.

## What a command may do

A command is a program that runs as you, so Argonauts keeps it small:

- It runs inside the sandbox, as a read-only agent does. It can read the repository and its
  own folder, write temporary folders only, and reach the network only when `sandbox.network`
  is on.
- It sees `PATH`, `HOME`, your language settings and a temporary folder. It sees no key and
  no token.
- It is stopped after its `timeout`, and what it printed last stays on screen until it prints
  again.
- It starts in the repository's folder, so `git` works as you'd expect.
- A mod from a project's `.argonauts/mods/` runs its command only after you trust it once, on
  its page with `t`. A change to that file, or to the program beside it, asks again.
- `mods.commands = false` in your settings turns every mod's command off. Mods with a built-in
  source are untouched.

The fields here are those of Argonauts Code {version}.
