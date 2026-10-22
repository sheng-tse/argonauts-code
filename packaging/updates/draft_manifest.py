#!/usr/bin/env python3
"""Drafts the update manifest `argonauts self-update` reads, from a release's files, for the maintainer to sign.

The manifest is served at https://argonauts-code.github.io/updates/v1/<channel>.json, with its minisign signature
beside it as <channel>.json.minisig. Only the maintainer's key signs it, on their own machine, so CI only drafts:

    python3 packaging/updates/draft_manifest.py --release DIR --tag v0.1.1 --channel stable \\
        --previous-url https://argonauts-code.github.io/updates/v1/stable.json --out stable.json
    minisign -S -s <your key> -m stable.json -t "argonauts-code stable 0.1.1 serial 8"

DIR holds the release's files as dist made them: dist-manifest.json, each archive and its .sha256. The draft keeps
the previous manifest's yanked list and min_version, and its serial is one more, since the self-updater refuses a
manifest whose serial is lower than one it has seen. The rollout widens with the clock (10% for a day, then 50%,
then everyone, by default), so widening needs no new signature. Halting is a new draft of the same version with
--halted, signed like any other.
"""

import argparse
import datetime
import hashlib
import json
import pathlib
import sys
import urllib.error
import urllib.request

RELEASES = "https://github.com/sheng-tse/argonauts-code/releases"
CHANNELS = ("stable", "beta")
DEFAULT_ROLLOUT = {"stable": [10, 50, 100], "beta": [100]}
# The first public release: no install is older, so as a min_version it holds nobody back.
FIRST_RELEASE = "0.1.0"


class DraftError(Exception):
    """Something about the release or the previous manifest that a signed manifest must never carry."""


def version_key(version):
    """Orders versions as semver does: 0.1.0-beta.2 < 0.1.0-rc.1 < 0.1.0 < 0.1.1."""
    core, _, pre = version.partition("-")
    numbers = tuple(int(part) for part in core.split("."))
    if not pre:
        return numbers, (1,)
    parts = tuple((0, int(part), "") if part.isdigit() else (1, 0, part) for part in pre.split("."))
    return numbers, (0, parts)


def platforms(release_dir, tag):
    """Each archive dist built, by target, with its download, sha256 and size. The sha256 is checked against the
    .sha256 file dist wrote beside it."""
    manifest_path = release_dir / "dist-manifest.json"
    try:
        manifest = json.loads(manifest_path.read_text())
    except FileNotFoundError:
        raise DraftError(f"{manifest_path} is missing: DIR must hold the release's files as dist made them")
    found = {}
    for name, artifact in sorted(manifest["artifacts"].items()):
        if artifact.get("kind") != "executable-zip":
            continue
        archive = release_dir / name
        if not archive.is_file():
            raise DraftError(f"{name} is in dist-manifest.json but not in {release_dir}")
        data = archive.read_bytes()
        sha256 = hashlib.sha256(data).hexdigest()
        expected = (release_dir / artifact["checksum"]).read_text().split()[0]
        if sha256 != expected:
            raise DraftError(f"{name} doesn't match {artifact['checksum']}: {sha256} against {expected}")
        for target in artifact["target_triples"]:
            found[target] = {"url": f"{RELEASES}/download/{tag}/{name}", "sha256": sha256, "size": len(data)}
    if not found:
        raise DraftError(f"dist-manifest.json in {release_dir} lists no archives")
    return found


def rollout(percents, start, step_hours):
    """The share of installs offered the version from each moment on. Percents must rise and end at 100."""
    if not percents or percents[-1] != 100 or any(a >= b for a, b in zip(percents, percents[1:])):
        raise DraftError(f"the rollout {percents} must rise step by step and end at 100")
    if any(percent <= 0 for percent in percents):
        raise DraftError("every rollout step must offer the version to some installs")
    steps = []
    for index, percent in enumerate(percents):
        moment = start + datetime.timedelta(hours=step_hours * index)
        steps.append({"from": timestamp(moment), "percent": percent})
    return steps


def timestamp(moment):
    return moment.astimezone(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def read_previous(path, url):
    """The manifest the self-updater reads today, or None when this is the channel's first."""
    if path:
        return json.loads(pathlib.Path(path).read_text())
    if url:
        try:
            with urllib.request.urlopen(url, timeout=30) as response:
                return json.loads(response.read())
        except urllib.error.HTTPError as error:
            if error.code == 404:
                return None
            raise
    return None


def draft(args, now):
    version = args.tag.removeprefix("v")
    is_beta = "-" in version
    if is_beta and args.channel == "stable":
        raise DraftError(f"{args.tag} is a beta, so it goes on the beta channel only")

    previous = read_previous(args.previous, args.previous_url)
    if previous and previous.get("channel", args.channel) != args.channel:
        raise DraftError(f"the previous manifest is the {previous['channel']} channel's, not {args.channel}'s")
    if previous and version_key(version) < version_key(previous["version"]):
        raise DraftError(
            f"{version} is older than {previous['version']}, which the manifest offers today; "
            "move installs off a broken version with --yank instead"
        )
    redraft = previous is not None and previous["version"] == version

    yanked = sorted(set(previous.get("yanked", []) if previous else []) | set(args.yank), key=version_key)
    if version in yanked:
        raise DraftError(f"{version} is yanked, so it can't be offered")

    if args.rollout is None and redraft:
        # Halting or yanking keeps the schedule the version was signed with.
        steps = previous["rollout"]
        published = previous["published"]
    else:
        percents = args.rollout or DEFAULT_ROLLOUT[args.channel]
        start = args.start or now
        steps = rollout(percents, start, args.step_hours)
        published = timestamp(now)

    min_version = args.min_version or (previous or {}).get("min_version") or FIRST_RELEASE
    return {
        "channel": args.channel,
        "serial": (previous["serial"] + 1) if previous else 1,
        "version": version,
        "published": published,
        "notes": f"{RELEASES}/tag/{args.tag}",
        "min_version": min_version,
        "halted": args.halted,
        "yanked": yanked,
        "rollout": steps,
        "platforms": platforms(pathlib.Path(args.release), args.tag),
    }


def percents(text):
    try:
        return [int(part) for part in text.split(",")]
    except ValueError:
        raise argparse.ArgumentTypeError(f"{text!r} isn't a list of percents such as 10,50,100")


def moment(text):
    parsed = datetime.datetime.fromisoformat(text.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise argparse.ArgumentTypeError(f"{text!r} needs a time zone, such as 2026-10-27T15:00:00Z")
    return parsed


def parse(argv):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--release", required=True, help="the folder holding the release's files")
    parser.add_argument("--tag", required=True, help="the release's tag, such as v0.1.1")
    parser.add_argument("--channel", required=True, choices=CHANNELS)
    previous = parser.add_mutually_exclusive_group()
    previous.add_argument("--previous", help="the manifest served today, as a file")
    previous.add_argument("--previous-url", help="the manifest served today, by its address")
    parser.add_argument("--out", help="where to write the draft (default: <channel>.json)")
    parser.add_argument(
        "--if-newer",
        action="store_true",
        help="write nothing, and succeed, when the channel already offers this version or a newer one",
    )
    parser.add_argument("--min-version", help="min_version (default: the previous manifest's, else 0.1.0)")
    parser.add_argument("--yank", action="append", default=[], help="a version to move installs off, again for more")
    parser.add_argument("--halted", action="store_true", help="stop offering the version")
    parser.add_argument("--rollout", type=percents, help="percents of installs, one a step (default: 10,50,100)")
    parser.add_argument("--step-hours", type=int, default=24, help="hours between rollout steps (default: 24)")
    parser.add_argument("--start", type=moment, help="when the first step begins (default: now)")
    return parser.parse_args(argv)


def offered_already(args):
    """Whether the channel offers this version, or a newer one, today."""
    previous = read_previous(args.previous, args.previous_url)
    version = args.tag.removeprefix("v")
    return previous is not None and version_key(version) <= version_key(previous["version"])


def main(argv=None, now=None):
    args = parse(argv)
    now = now or datetime.datetime.now(datetime.timezone.utc)
    if args.if_newer and offered_already(args):
        print(f"draft_manifest: {args.channel} offers {args.tag} or newer already, so nothing was drafted",
              file=sys.stderr)
        return 0
    try:
        manifest = draft(args, now)
    except DraftError as error:
        print(f"draft_manifest: {error}", file=sys.stderr)
        return 1
    out = pathlib.Path(args.out or f"{args.channel}.json")
    body = json.dumps(manifest, indent=2) + "\n"
    out.write_text(body)
    # `argonauts self-update --version X` reads the release's own manifest.
    named = out.parent / "releases" / f"{manifest['version']}.json"
    named.parent.mkdir(parents=True, exist_ok=True)
    named.write_text(body)
    comment = f"argonauts-code {manifest['channel']} {manifest['version']} serial {manifest['serial']}"
    print(
        f"Drafted {out} and {named}: {manifest['version']} on {manifest['channel']}, serial "
        f"{manifest['serial']}. Sign each with\n"
        f"  minisign -S -s <your key> -m {out} -t \"{comment}\"\n"
        f"  minisign -S -s <your key> -m {named} -t \"{comment}\"\n"
        f"then publish them and their .minisig files under updates/v1/ on argonauts-code.github.io, "
        f"{named.name} in releases/.",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
