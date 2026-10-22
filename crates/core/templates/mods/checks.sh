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
