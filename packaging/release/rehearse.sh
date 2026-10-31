#!/bin/sh
# Rehearses a release on the exact commit the release pull request will hold, before the pull request opens: every
# check release-checks.yml runs (checks.sh), the crates dry run on a fresh target folder and cargo deny, as a release
# asks, that crates.io has had every crate the commit publishes, and the Publishers job's result from a Release dry
# run. A check that couldn't run is a failure, never a pass, so the pull request opens only after a rehearsal where
# every check ran and held.
#
#   sh packaging/release/rehearse.sh v0.2.1 [<commit>]
#
# The commit is HEAD unless named. The checks run in a temporary worktree of it, removed at the end, so the tree it
# runs from is left as it is. Whether the Release run is on main can't be checked before the merge, so the Release
# run checks that alone.
set -eu

if [ $# -lt 1 ] || [ $# -gt 2 ]; then
    sed -n '2,/^set -eu/p' "$0" | sed '$d; s/^# \{0,1\}//' >&2
    exit 2
fi
tag=$1
here=$(cd "$(dirname "$0")" && pwd)
commit=$(git rev-parse --verify "${2:-HEAD}^{commit}")
short=$(printf '%.9s' "$commit")

tree=$(mktemp -d "${TMPDIR:-/tmp}/release-rehearsal.XXXXXX")
output=$(mktemp)
cleanup() {
    git worktree remove --force "$tree" 2> /dev/null || rm -rf "$tree"
    rm -f "$output"
}
trap cleanup EXIT
trap 'exit 130' INT TERM
git worktree add --detach --quiet "$tree" "$commit"

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    green=$(printf '\033[32m') red=$(printf '\033[31m') yellow=$(printf '\033[33m') plain=$(printf '\033[0m')
else
    green='' red='' yellow='' plain=''
fi

failures=0
not_run=0

# rehearse <what it checks> <checks.sh arguments…>
rehearse() {
    what=$1
    shift
    status=0
    (cd "$tree" && sh "$here/checks.sh" "$@") > "$output" 2>&1 || status=$?
    case $status in
        0)
            found=$(tail -n 1 "$output" | cut -f 1)
            note=$(tail -n 1 "$output" | cut -s -f 2)
            if [ -n "$note" ]; then
                printf '%s✓%s  %-54s  %s\n' "$green" "$plain" "$found" "$note"
            else
                printf '%s✓%s  %s\n' "$green" "$plain" "$found"
            fi
            ;;
        2)
            not_run=$((not_run + 1))
            why=$(grep '^not run: ' "$output" | tail -n 1)
            printf '%s·%s  %-54s  %s%s%s\n' "$yellow" "$plain" "$what" "$yellow" "${why:-not run}" "$plain"
            ;;
        *)
            failures=$((failures + 1))
            why=$(grep '^error: ' "$output" | tail -n 1 | sed 's/^error: //')
            printf '%s✗%s  %-54s  %s%s%s\n' "$red" "$plain" "$what" "$red" "${why:-failed}" "$plain"
            grep -v '^error: ' "$output" | sed 's/^/     /' || true
            ;;
    esac
}

echo "Rehearsal for $tag on $short, the release pull request's head (every step of release-checks.yml)"
rehearse "CI passed" ci-passed "$commit"
rehearse "The CHANGELOG section" changelog "$tag" --ahead
rehearse "The release badge" badge "$tag"
rehearse "The update keys" update-keys
rehearse "The short install address" short-installer
rehearse "THIRD_PARTY_NOTICES.md" notices
rehearse "cargo publish --workspace --dry-run --locked" packages
rehearse "cargo deny check licenses advisories sources" licenses
rehearse "Every crate is already on crates.io" crates-exist
rehearse "Publishers" publishers "$commit"

if [ "$failures" -eq 0 ] && [ "$not_run" -eq 0 ]; then
    echo "${green}✓${plain} Every check ran on $short. Open the release pull request."
    exit 0
fi
total=$((failures + not_run))
if [ "$failures" -eq 0 ]; then
    how="not run"
elif [ "$not_run" -eq 0 ]; then
    how=failed
else
    how="failed or not run"
fi
noun=checks
[ "$total" -ne 1 ] || noun=check
echo "${red}✗${plain} $total $noun $how on $short, so no release pull request yet."
exit 1
