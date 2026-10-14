#!/bin/sh
# A tiny CLI plugin for tests. It speaks Argonauts Code's JSONL protocol, one normalized
# AgentEvent per stdout line, and calls no model.
#
#   --version         print a version and exit
#   --model <id>      echo the model back in the init event
#   --resume <id>     continue that session instead of starting one
#   --write <path>    create <path> under the working folder, and end with a task report
#                     of the change, as a worker does
#   --events <file>   print that file's lines after the init event, in place of the reply
#   --noise           print a line that isn't JSON, which the adapter keeps as unknown
#   --fail            print an error to stderr and exit 3
#
# The prompt arrives on stdin, and the reply says how many bytes it had.
set -eu

model=default
session=plugin-session-1
write=
events=
noise=
fail=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --version) echo "jsonl-plugin 1.0.0"; exit 0 ;;
        --model) model=$2; shift ;;
        --resume) session=$2; shift ;;
        --write) write=$2; shift ;;
        --events) events=$2; shift ;;
        --noise) noise=1 ;;
        --fail) fail=1 ;;
        *) ;;
    esac
    shift
done

bytes=$(wc -c | tr -d ' ')

if [ -n "$fail" ]; then
    echo "error: credentials or agent init failed" >&2
    exit 3
fi

printf '{"type":"init","session_id":"%s","model":"%s","tools":["read","write"]}\n' "$session" "$model"
if [ -n "$noise" ]; then
    echo "plugin: warming up"
fi
if [ -n "$events" ]; then
    cat "$events"
    exit 0
fi
printf '{"type":"message","text":"Read %s bytes of prompt.","parent":null}\n' "$bytes"
report=null
if [ -n "$write" ]; then
    printf '{"type":"tool_started","id":"t1","name":"write","input":{"path":"%s"},"parent":null}\n' "$write"
    echo "written by the plugin" > "$write"
    printf '{"type":"file_changed","path":"%s","kind":"add"}\n' "$write"
    printf '{"type":"tool_finished","id":"t1","ok":true,"output":null,"parent":null}\n'
    report=$(printf '{"status":"done","summary":"Wrote %s.","files_changed":["%s"],"tests_run":[],"concerns":[]}' "$write" "$write")
fi
printf '{"type":"usage","input_tokens":%s,"output_tokens":7,"cached_tokens":0,"cost_usd":null,"cumulative":true}\n' "$bytes"
printf '{"type":"result","text":"done","structured":%s,"is_error":false}\n' "$report"
