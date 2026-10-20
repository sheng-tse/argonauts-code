#!/usr/bin/env bash
# Re-records the CLI output the adapter tests replay, for claude-code, codex, and
# antigravity-cli, on each agent's cheapest model. Every call is live and spends plan usage, three
# per agent. Each adapter's ignored `record` tests build the commands, so the fixtures show what
# the adapters pass today. Each lands in crates/adapters/<id>/fixtures/ under its scenario's
# name, in place of the one before it, with the version in the .meta.json beside it. As each is
# written, the recorder says whether its shape (the kinds of line and their fields) is the
# stored one's or what changed, so only a scenario whose shape changed needs keeping. The
# agents work in throwaway git repos under <scratch>/live/.
#
# Shims put first on PATH refuse, before it reaches the agent, any call that isn't one of
#   claude --model haiku --effort low
#   codex exec -m gpt-6-luna -c model_reasoning_effort="low"
#   agy --model gemini-3.8-flash-low --effort low
# with the long spellings of those flags allowed too.

set -euo pipefail

usage() {
    cat <<'USAGE'
Usage: rerecord-fixtures.sh --live --scratch <dir> [claude-code] [codex] [antigravity-cli]

Re-records the adapters' fixtures with live calls on the cheapest models: three calls per
agent. With no adapter named, it records all three. <dir> must be outside this repository;
the throwaway repos go in <dir>/live/. The S variable can take the place of --scratch.
USAGE
}

die() {
    echo "rerecord-fixtures: $*" >&2
    exit 2
}

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
repo=$(cd "$here/../../.." && pwd -P)

live=false
scratch="${S:-}"
adapters=()
while [ $# -gt 0 ]; do
    case "$1" in
        --live) live=true ;;
        --scratch)
            [ $# -ge 2 ] || die "--scratch needs a directory"
            scratch=$2
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        claude-code | codex | antigravity-cli) adapters+=("$1") ;;
        *)
            usage >&2
            die "unknown argument: $1"
            ;;
    esac
    shift
done

if ! $live; then
    usage >&2
    die "every call is live and spends plan usage, so pass --live to go ahead"
fi
[ -n "$scratch" ] || die "name a scratch directory with --scratch"
[ -d "$scratch" ] || die "$scratch is not a directory"
scratch=$(cd "$scratch" && pwd -P)
case "$scratch/" in
    "$repo/"*) die "the scratch directory must be outside $repo" ;;
esac
command -v cargo >/dev/null || die "cargo is not installed"
command -v git >/dev/null || die "git is not installed"
[ ${#adapters[@]} -gt 0 ] || adapters=(claude-code codex antigravity-cli)

run_dir="$scratch/live/rerecord-$(date -u +%Y%m%dT%H%M%SZ)"
shims="$run_dir/bin"
mkdir -p "$shims"

# write_shim <cli> <required first argument or ""> <model flags> <model> <effort flags> <effort>
write_shim() {
    local cli=$1 first=$2 model_flags=$3 model=$4 effort_flags=$5 effort=$6 real
    real=$(command -v "$cli") || die "$cli is not installed"
    {
        echo '#!/usr/bin/env bash'
        echo "# Written by rerecord-fixtures.sh: only the cheapest model and effort reach $cli."
        echo 'set -euo pipefail'
        printf 'real=%q\n' "$real"
        printf 'first=%q\n' "$first"
        printf 'model_flags=%q\n' " $model_flags "
        printf 'model=%q\n' "$model"
        printf 'effort_flags=%q\n' " $effort_flags "
        printf 'effort=%q\n' "$effort"
        cat <<'BODY'
refuse() {
    echo "rerecord-fixtures: refused $(basename "$0"): $1" >&2
    exit 97
}
has() { [[ "$1" == *" $2 "* ]]; }

case "${1:-}" in
    --version | -V | --help | -h) exec "$real" "$@" ;;
    plugin | login | logout) refuse "the $1 subcommand" ;;
esac
if [ -n "$first" ] && [ "${1:-}" != "$first" ]; then
    refuse "only \`$first\` runs"
fi

watched="$model_flags$effort_flags --fallback-model "
saw_model=false
saw_effort=false
pending=""
for arg in "$@"; do
    if [ -n "$pending" ]; then
        flag=$pending
        value=$arg
        pending=""
    elif [[ "$arg" == -*=* ]] && has "$watched" "${arg%%=*}"; then
        flag=${arg%%=*}
        value=${arg#*=}
    elif has "$watched" "$arg"; then
        pending=$arg
        continue
    else
        continue
    fi
    if [ "$flag" = --fallback-model ]; then
        refuse "--fallback-model $value"
    elif has "$model_flags" "$flag"; then
        [ "$value" = "$model" ] || refuse "$flag $value"
        saw_model=true
    elif [ "$value" = "$effort" ]; then
        saw_effort=true
    elif [ "$effort_flags" = " --effort " ] ||
        [[ "$value" == model=* || "$value" == model_reasoning_effort=* ]]; then
        refuse "$flag $value"
    fi
done
$saw_model || refuse "no model flag for $model"
$saw_effort || refuse "no effort flag for $effort"
exec "$real" "$@"
BODY
    } >"$shims/$cli"
    chmod +x "$shims/$cli"
}

write_shim claude "" "--model" haiku "--effort" low
write_shim codex exec "-m --model" gpt-6-luna "-c --config" 'model_reasoning_effort="low"'
write_shim agy "" "--model" gemini-3.8-flash-low "--effort" low

export PATH="$shims:$PATH"
# The Codex recorder falls back to this model when it's set; only the cheapest one may run.
unset ARGONAUTS_CODEX_MODEL

# The crate and its recording tests, in the order they must run: Codex resumes its build session.
recordings() {
    case "$1" in
        claude-code) echo "argonauts-adapter-claude-code record_read_only record_write record_shell" ;;
        codex) echo "argonauts-adapter-codex record_1_read_only_ok record_2_build_hello record_3_resume_ls" ;;
        antigravity-cli) echo "argonauts-adapter-antigravity-cli record_readonly_ok record_build_hello record_build_shell" ;;
    esac
}

for id in "${adapters[@]}"; do
    read -r crate tests <<<"$(recordings "$id")"
    live_dir="$run_dir/$id"
    mkdir -p "$live_dir"
    for test in $tests; do
        echo "rerecord-fixtures: $id $test"
        (
            cd "$repo"
            ARGONAUTS_RECORD=$test ARGONAUTS_LIVE_DIR=$live_dir \
                cargo test -p "$crate" --test it -- --ignored --exact "record::$test" --nocapture
        )
    done
    echo "rerecord-fixtures: $id fixtures are in crates/adapters/$id/fixtures/"
done

cat <<DONE
rerecord-fixtures: done. The throwaway repos are in $run_dir.
Each recording's .meta.json names its version, and the adapter's tested versions follow them.
Keep only the recordings reported above as "the shape changed". Put the others back with
git checkout and add the new version to "also_matches" in their .meta.json. A scenario that
resumes another goes with it: keep both or put both back. Then run the replay tests.
DONE
