#!/bin/sh
# A tiny one-shot CLI for tests. It reads the prompt on stdin, calls no model, and prints one
# JSON document when it exits, the shape `output = "final-json"` reads through its pointers.
#
#   --version               print a version and exit
#   --model <id>            echo the model back
#   --cwd <path>            echo the working folder back
#   --json-schema <file>    add a structured answer, as a CLI that enforces a schema would
#   --write <path>          create <path> under the working folder
#   --fail                  print an error document and exit 2
set -eu

model=default
cwd=
schema=
write=
fail=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --version) echo "final-json-plugin 1.0.0"; exit 0 ;;
        --model) model=$2; shift ;;
        --cwd) cwd=$2; shift ;;
        --json-schema) schema=$2; shift ;;
        --write) write=$2; shift ;;
        --fail) fail=1 ;;
        *) ;;
    esac
    shift
done

bytes=$(wc -c | tr -d ' ')
echo "final-json-plugin: starting"

if [ -n "$fail" ]; then
    printf '{"status":"error","sessionId":"exec-2","model":"%s",' "$model"
    printf '"error":{"kind":"timeout","message":"the run took too long"}}\n'
    exit 2
fi

if [ -n "$write" ]; then
    echo "written by the plugin" > "$write"
fi
structured=null
if [ -n "$schema" ] && [ -f "$schema" ]; then
    structured='{"summary":"done"}'
fi
printf '{"status":"ok","sessionId":"exec-1","model":"%s","final":"Read %s bytes in %s.",' "$model" "$bytes" "$cwd"
printf '"usage":{"input":%s,"output":9,"cached":"4"},"costUsd":0.0125,' "$bytes"
printf '"structured_output":%s,"error":null}\n' "$structured"
