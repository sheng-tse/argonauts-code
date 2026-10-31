#!/usr/bin/env python3
"""What crates.io's index holds of the workspace's crates, for the crates job and the Publishers check:

    python3 packaging/release/crates.py to-publish
    python3 packaging/release/crates.py all-exist

`to-publish` prints the arguments that pick what `cargo publish --locked` should publish, one per line: `--workspace`,
then `--exclude <crate>` for each crate crates.io already has at its version, since cargo refuses a version it
has. So a rerun after a failure picks up where the failed run stopped. It prints nothing when crates.io has them all.

`all-exist` fails naming each crate crates.io has never had: crates.io trusts a publisher only for a crate that
exists, so a new crate's first version goes by hand (publish-crates.yml says how).

Either exits 2 when the index can't be read, which the release checks count as a check that couldn't run.

The crates are the workspace members cargo publishes to crates.io, read with `cargo metadata`.
"""

import json
import pathlib
import subprocess
import sys
import time
import urllib.error
import urllib.request

INDEX = "https://index.crates.io"
USER_AGENT = "argonauts-release-checks"
ROOT = pathlib.Path(__file__).resolve().parents[2]


class Unreadable(Exception):
    """crates.io's index couldn't be read."""


def index_path(name):
    """Where the sparse index keeps a crate's entry: 1/a, 2/ab, 3/a/abc, then ab/cd/abcd…"""
    name = name.lower()
    if len(name) <= 2:
        return f"{len(name)}/{name}"
    if len(name) == 3:
        return f"3/{name[0]}/{name}"
    return f"{name[:2]}/{name[2:4]}/{name}"


def workspace_crates(metadata):
    """Each crate cargo publishes, as (name, version), in the workspace's order."""
    members = set(metadata["workspace_members"])
    return [
        (package["name"], package["version"])
        for package in metadata["packages"]
        if package["id"] in members and package.get("publish") in (None, ["crates-io"])
    ]


def read_metadata():
    out = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return json.loads(out)


def fetch(url, tries=3):
    """The body at `url`, or None when it's not there (404). Other failures are tried again, then raised."""
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    for try_number in range(1, tries + 1):
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                return response.read().decode()
        except urllib.error.HTTPError as error:
            error.close()
            if error.code == 404:
                return None
            failure = f"{url} answered {error.code}"
        except (urllib.error.URLError, TimeoutError) as error:
            failure = f"couldn't reach {url}: {error}"
        if try_number < tries:
            time.sleep(5 * try_number)
    raise Unreadable(failure)


def versions(name, index=INDEX, get=fetch):
    """The versions crates.io has of `name`, yanked ones too, or None when it has never had the crate."""
    body = get(f"{index}/{index_path(name)}")
    if body is None:
        return None
    return {json.loads(line)["vers"] for line in body.splitlines() if line.strip()}


def to_publish(crates, index=INDEX, get=fetch):
    """The arguments that leave out what crates.io has, and the crates it has, by name."""
    there = [name for name, version in crates if version in (versions(name, index, get) or set())]
    if len(there) == len(crates):
        return [], there
    args = ["--workspace"]
    for name in there:
        args += ["--exclude", name]
    return args, there


def never_published(crates, index=INDEX, get=fetch):
    return [name for name, _ in crates if versions(name, index, get) is None]


def main(argv=None, metadata=None, get=fetch):
    argv = sys.argv[1:] if argv is None else argv
    if argv not in (["to-publish"], ["all-exist"]):
        print(__doc__, file=sys.stderr)
        return 2
    crates = workspace_crates(metadata if metadata is not None else read_metadata())
    try:
        if argv == ["to-publish"]:
            args, there = to_publish(crates, get=get)
            version = crates[0][1] if crates else "?"
            print(f"Already on crates.io at {version}: {', '.join(there) or 'none'}", file=sys.stderr)
            if not args:
                print(f"crates.io has all {len(crates)} crates, so there's nothing to publish.", file=sys.stderr)
            for arg in args:
                print(arg)
            return 0
        missing = never_published(crates, get=get)
    except Unreadable as error:
        print(f"crates.io's index: {error}", file=sys.stderr)
        return 2
    if missing:
        for name in missing:
            print(
                f"{name} has never been on crates.io, and crates.io trusts a publisher only for a crate that exists: "
                "publish its first version by hand (publish-crates.yml says how), then add release.yml in the "
                "release environment as its trusted publisher.",
                file=sys.stderr,
            )
        return 1
    print(f"{len(crates)} crates\tall already published, none new")
    return 0


if __name__ == "__main__":
    sys.exit(main())
