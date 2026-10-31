#!/bin/sh
# The checks a release must pass, written once: release-checks.yml runs them step by step in the Release run, and
# rehearse.sh runs them on the release pull request's head before it opens. Run from the repository's root:
#
#   sh packaging/release/checks.sh <check> [arguments]
#
#   from-main                   the run is on main
#   ci-passed <commit>          CI passed on the commit
#   changelog <tag> [--ahead]   a stable release's CHANGELOG section is dated today or yesterday in UTC; with
#                               --ahead, as a rehearsal asks, any day from yesterday on
#   badge <tag>                 the README's release badge names a stable release's tag
#   update-keys                 the update keys are real minisign keys
#   short-installer             the short install address serves packaging/install.sh
#   notices                     THIRD_PARTY_NOTICES.md matches Cargo.lock
#   packages [<folder>]         `cargo publish --workspace --dry-run --locked` on a fresh target folder in <folder>
#   licenses                    `cargo deny --locked check licenses advisories sources`, with the cargo-deny CI pins
#   crates-exist                crates.io has had every crate this tree publishes (its index only, no token)
#   publishers <commit>         the Publishers job passed in a Release dry run on the commit, or on main today
#   cargo-about-version         prints the cargo-about the notices are made with
#
# A check that holds prints one line, what it found and then, after a tab, a note, and exits 0. One that doesn't
# exits 1 and says why on stderr. One that couldn't run (a tool or the network missing) exits 2, and a rehearsal
# counts that as a failure too.
set -eu

CARGO_ABOUT_VERSION=0.9.2
SHORT_INSTALLER=https://argonauts-code.github.io/install.sh
BADGE_COLOUR=f0a441
# Room for a fresh target folder, in GB.
PACKAGES_ROOM=40

held() {
    printf '%s\t%s\n' "$1" "${2:-}"
    exit 0
}

failed() {
    if [ "${GITHUB_ACTIONS:-}" = true ]; then
        echo "::error::$*" >&2
    else
        echo "error: $*" >&2
    fi
    exit 1
}

not_run() {
    if [ "${GITHUB_ACTIONS:-}" = true ]; then
        echo "::error::not run: $*" >&2
    else
        echo "not run: $*" >&2
    fi
    exit 2
}

warn() {
    if [ "${GITHUB_ACTIONS:-}" = true ]; then
        echo "::warning::$*" >&2
    else
        echo "warning: $*" >&2
    fi
}

short() {
    printf '%.9s' "$1"
}

# gh matches a run's commit by its full id only.
full_id() {
    git rev-parse --verify --quiet "$1^{commit}" 2> /dev/null || echo "$1"
}

# The UTC date $1 days from today.
utc_day() {
    at=$(($(date -u +%s) + $1 * 86400))
    date -u -d "@$at" +%Y-%m-%d 2> /dev/null || date -u -r "$at" +%Y-%m-%d
}

# Runs gh against this repository: the run's own in GitHub Actions, the origin remote's elsewhere.
repo_gh() {
    if [ -n "${GITHUB_REPOSITORY:-}" ]; then
        gh "$@" --repo "$GITHUB_REPOSITORY"
    else
        gh "$@"
    fi
}

need_gh() {
    command -v gh > /dev/null || not_run "needs gh, GitHub's command line"
}

from_main() {
    [ "${GITHUB_REF:-}" = refs/heads/main ] || failed "Releases are made from main, and this run is on ${GITHUB_REF:-no branch}"
    held "The release is made from main"
}

ci_passed() {
    sha=$(full_id "$1")
    need_gh
    passed=$(repo_gh run list --workflow ci.yml --commit "$sha" --status success --json databaseId --jq length) ||
        not_run "gh couldn't list CI's runs on $(short "$sha") (gh auth status says why)"
    [ "$passed" -gt 0 ] ||
        failed "CI hasn't passed on $(short "$sha"). A commit gets CI on main, or as a pull request's head (a draft is enough)"
    held "CI passed on $(short "$sha")"
}

changelog() {
    tag=$1
    ahead=${2:-}
    version=${tag#v}
    case $version in
        *-*) held "A beta needs no CHANGELOG section" ;;
    esac
    [ -f CHANGELOG.md ] || failed "there's no CHANGELOG.md here; run from the repository's root"
    heading="## [$version] - "
    dated=$(awk -v heading="$heading" 'index($0, heading) == 1 { print substr($0, length(heading) + 1); exit }' CHANGELOG.md)
    today=$(utc_day 0)
    yesterday=$(utc_day -1)
    if [ -z "$dated" ]; then
        failed "CHANGELOG.md needs a section headed \"## [$version] - $today\""
    fi
    case $dated in
        [0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]) ;;
        *) failed "CHANGELOG.md's section for $version is dated \"$dated\", which isn't a YYYY-MM-DD date" ;;
    esac
    found="CHANGELOG has \"$heading$dated\""
    if [ "$ahead" = --ahead ]; then
        if [ "$dated" = "$yesterday" ]; then
            held "$found" "Release must run today (UTC)"
        elif expr "$dated" \> "$yesterday" > /dev/null; then
            held "$found" "Release may run that day or the next (UTC)"
        fi
        failed "CHANGELOG.md's section for $version is dated $dated, and Release can run only on that day or the next (UTC): date it the day Release will run"
    fi
    if [ "$dated" = "$today" ]; then
        held "$found" "today in UTC"
    elif [ "$dated" = "$yesterday" ]; then
        held "$found" "yesterday in UTC, allowed"
    fi
    failed "CHANGELOG.md's section for $version is dated $dated; a release today needs $today or $yesterday (UTC)"
}

badge() {
    tag=$1
    case $tag in
        *-*) held "A beta leaves the release badge as it is" ;;
    esac
    [ -f README.md ] || failed "there's no README.md here; run from the repository's root"
    if grep -qF "https://img.shields.io/badge/release-$tag-$BADGE_COLOUR" README.md; then
        held "README's release badge names $tag"
    fi
    says=$(sed -n 's|.*https://img\.shields\.io/badge/release-\(v[0-9][0-9.]*\)-.*|\1|p' README.md | head -n 1)
    failed "README.md's release badge says ${says:-no version}: make it https://img.shields.io/badge/release-$tag-$BADGE_COLOUR?style=flat-square"
}

update_keys() {
    command -v python3 > /dev/null || not_run "needs python3"
    python3 packaging/updates/check_keys.py || failed "the update keys aren't real minisign keys (packaging/updates/check_keys.py says which)"
    held "The update keys are real minisign keys"
}

# The short install address serves a copy of packaging/install.sh from the Pages site, which a release doesn't
# update, so a copy left behind would install otherwise than the release says. Until the copy is there, the
# README names the release's own address.
short_installer() {
    served=$(mktemp)
    trap 'rm -f "$served"' EXIT
    status=$(curl --proto '=https' --tlsv1.2 -sSL -o "$served" -w '%{http_code}' "$SHORT_INSTALLER" 2> /dev/null) || status=000
    case $status in
        200) ;;
        404)
            warn "$SHORT_INSTALLER isn't served yet, so the README's install address stays the release's"
            held "The short install address isn't served yet" "the README names the release's own"
            ;;
        000) not_run "couldn't reach $SHORT_INSTALLER" ;;
        *) failed "$SHORT_INSTALLER answered $status" ;;
    esac
    if ! cmp -s "$served" packaging/install.sh; then
        diff -u "$served" packaging/install.sh >&2 || true
        failed "$SHORT_INSTALLER isn't the same as packaging/install.sh: copy it to the Pages site"
    fi
    held "The short install address serves packaging/install.sh"
}

notices() {
    have=$(cargo about --version 2> /dev/null | awk '{ print $2 }') || have=
    [ "$have" = "$CARGO_ABOUT_VERSION" ] ||
        not_run "needs cargo-about $CARGO_ABOUT_VERSION (cargo install --locked cargo-about@$CARGO_ABOUT_VERSION --features cli)${have:+; this machine has $have}"
    before=$(mktemp)
    log=$(mktemp)
    trap 'rm -f "$before" "$log"' EXIT
    cp THIRD_PARTY_NOTICES.md "$before"
    if ! sh packaging/notices/generate.sh > "$log" 2>&1; then
        cat "$log" >&2
        failed "packaging/notices/generate.sh failed"
    fi
    cmp -s "$before" THIRD_PARTY_NOTICES.md ||
        failed "THIRD_PARTY_NOTICES.md is out of date: run packaging/notices/generate.sh and commit it"
    held "THIRD_PARTY_NOTICES.md matches Cargo.lock" "cargo-about $CARGO_ABOUT_VERSION, as the workflow pins"
}

# cargo reuses what it unpacked before from <target>/package/tmp-registry while the version is unchanged, so only a
# fresh target folder checks this tree's crates.
packages() {
    parent=${1:-${TMPDIR:-/tmp}}
    command -v cargo > /dev/null || not_run "needs cargo"
    free=$(df -Pk "$parent" | awk 'NR == 2 { print int($4 / 1048576) }')
    [ "$free" -ge "$PACKAGES_ROOM" ] ||
        not_run "a fresh target folder needs about $PACKAGES_ROOM GB, and $parent has $free GB free"
    target=$(mktemp -d "$parent/release-packages.XXXXXX")
    log=$(mktemp)
    trap 'rm -rf "$target"; rm -f "$log"' EXIT
    if ! CARGO_TARGET_DIR="$target" CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_INCREMENTAL=0 \
        cargo publish --workspace --dry-run --locked > "$log" 2>&1; then
        tail -n 40 "$log" >&2
        failed "cargo publish --workspace --dry-run --locked failed"
    fi
    count=$(grep -c '^ *Packaging ' "$log" || true)
    held "cargo publish --workspace --dry-run --locked" "$count crates, fresh target folder"
}

licenses() {
    pinned=$(sed -n 's/.*tool: cargo-deny@\([0-9.]*\).*/\1/p' .github/workflows/ci.yml | head -n 1)
    have=$(cargo deny --version 2> /dev/null | awk '{ print $2 }') || have=
    [ -n "$have" ] || not_run "needs cargo-deny (cargo install --locked cargo-deny@${pinned:-<the version CI pins>})"
    [ -z "$pinned" ] || [ "$have" = "$pinned" ] ||
        not_run "needs cargo-deny $pinned, as CI pins it; this machine has $have"
    log=$(mktemp)
    trap 'rm -f "$log"' EXIT
    if ! cargo deny --locked check licenses advisories sources > "$log" 2>&1; then
        tail -n 40 "$log" >&2
        failed "cargo deny --locked check licenses advisories sources failed"
    fi
    held "cargo deny check licenses advisories sources"
}

# crates.io trusts a publisher only for a crate it already has, and the Publishers job checks main's crates. So a
# crate that is new in this tree is caught here, or the crates job stops on it after the GitHub release is out.
crates_exist() {
    command -v python3 > /dev/null || not_run "needs python3"
    command -v cargo > /dev/null || not_run "needs cargo"
    status=0
    found=$(python3 packaging/release/crates.py all-exist) || status=$?
    case $status in
        0) held "Every crate is already on crates.io" "$(printf '%s' "$found" | cut -f 1), none new" ;;
        2) not_run "couldn't read crates.io's index" ;;
        *) failed "crates.io hasn't had every crate this tree publishes (packaging/release/crates.py says which)" ;;
    esac
}

# The Publishers job runs in Release dry runs only, in the release environment, which may admit only main. So a
# dry run on main within the last day counts too: what it checks lives on npm, crates.io and GitHub, not in the tree.
publishers() {
    sha=$(full_id "$1")
    need_gh
    on_commit=$(repo_gh run list --workflow release.yml --commit "$sha" --event workflow_dispatch \
        --json databaseId --jq '.[].databaseId') || not_run "gh couldn't list Release's runs (gh auth status says why)"
    on_main=$(repo_gh run list --workflow release.yml --branch main --event workflow_dispatch --limit 20 \
        --json databaseId,createdAt --jq '.[] | select(now - (.createdAt | fromdateiso8601) < 86400) | .databaseId') ||
        not_run "gh couldn't list Release's runs (gh auth status says why)"
    # The run ids are numbers, one per line, so each splits into an argument of its own.
    # shellcheck disable=SC2086
    publishers_in "on $(short "$sha")" $on_commit
    # shellcheck disable=SC2086
    publishers_in "on main today" $on_main
    not_run "gh workflow run release.yml -f tag=dry-run"
}

# Settles the check on the newest of the runs that has a Publishers job, and returns when none has.
publishers_in() {
    where=$1
    shift
    for run in "$@"; do
        job=$(repo_gh run view "$run" --json jobs \
            --jq '[.jobs[] | select(.name | endswith("Publishers"))][0] | "\(.conclusion) \(.url)"') || continue
        case $job in
            "success "*) held "Publishers" "from the Release dry run $where" ;;
            "failure "*) failed "the Publishers job failed in the Release dry run $where: ${job#failure }" ;;
        esac
    done
}

check=${1:-}
[ $# -gt 0 ] && shift
case $check in
    from-main) from_main ;;
    ci-passed) [ $# -eq 1 ] && ci_passed "$1" ;;
    changelog) [ $# -ge 1 ] && changelog "$@" ;;
    badge) [ $# -eq 1 ] && badge "$1" ;;
    update-keys) update_keys ;;
    short-installer) short_installer ;;
    notices) notices ;;
    packages) packages "$@" ;;
    licenses) licenses ;;
    crates-exist) crates_exist ;;
    publishers) [ $# -eq 1 ] && publishers "$1" ;;
    cargo-about-version) echo "$CARGO_ABOUT_VERSION" && exit 0 ;;
esac
sed -n '2,/^set -eu/p' "$0" | sed '$d; s/^# \{0,1\}//' >&2
exit 2
