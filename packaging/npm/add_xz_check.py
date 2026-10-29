#!/usr/bin/env python3
"""Adds the xz check to the npm package dist built, in place, before the release uploads it, so the release's copy is
the one npm publishes:

    python3 packaging/npm/add_xz_check.py target/distrib/argonauts-code-npm-package.tar.gz \
        --checksums target/distrib/*.sha256 target/distrib/*.sum target/distrib/*.json dist-manifest.json

xz-check.js goes in beside dist's files, install.js calls it before it installs, and run-argonauts.js calls it before
a first run installs the program. Everything else in the package stays as dist made it. A package whose two entry
files read otherwise than dist writes them is refused, so a newer dist can't ship a package without the check. Each
file named after --checksums that holds the package's SHA-256 as dist wrote it gets the new package's instead, and a
name that matches no file is passed over. With --has-check, it only says whether the package has the check, as the
npm publish asks before it publishes.
"""

import argparse
import gzip
import hashlib
import io
import pathlib
import sys
import tarfile

CHECK = pathlib.Path(__file__).resolve().parent / "xz-check.js"

# Each entry file as dist writes it, and as it reads with the check.
ENTRIES = {
    "package/install.js": (
        '#!/usr/bin/env node\n\nconst { install } = require("./binary");\ninstall(false);\n',
        '#!/usr/bin/env node\n\nrequire("./xz-check").needXz();\nconst { install } = require("./binary");\n'
        "install(false);\n",
    ),
    "package/run-argonauts.js": (
        '#!/usr/bin/env node\n\nconst { run } = require("./binary");\nrun("argonauts");\n',
        '#!/usr/bin/env node\n\nconst { run, getPackage } = require("./binary");\nif (!getPackage().exists()) {\n'
        '  require("./xz-check").needXz();\n}\nrun("argonauts");\n',
    ),
}


class Refused(Exception):
    pass


def add_check(package: bytes) -> bytes:
    """The package with the check in it."""
    out = io.BytesIO()
    seen = set()
    with tarfile.open(fileobj=io.BytesIO(package), mode="r:gz") as old, tarfile.open(
        fileobj=out, mode="w", format=tarfile.PAX_FORMAT
    ) as new:
        if "package/xz-check.js" in old.getnames():
            raise Refused("the package already has the xz check")
        for member in old.getmembers():
            data = old.extractfile(member).read() if member.isfile() else None
            if member.name in ENTRIES:
                was, becomes = ENTRIES[member.name]
                if data.decode() != was:
                    raise Refused(f"{member.name} isn't the file dist writes:\n{data.decode()}")
                data = becomes.encode()
                member.size = len(data)
                seen.add(member.name)
            new.addfile(member, io.BytesIO(data) if data is not None else None)
            if member.name == "package/install.js":
                check = CHECK.read_bytes()
                info = tarfile.TarInfo("package/xz-check.js")
                info.size, info.mode, info.mtime = len(check), 0o644, member.mtime
                info.uid, info.gid = member.uid, member.gid
                new.addfile(info, io.BytesIO(check))
    missing = sorted(set(ENTRIES) - seen)
    if missing:
        raise Refused(f"the package has no {', '.join(missing)}")
    return gzip.compress(out.getvalue(), mtime=0)


def has_check(package: bytes) -> bool:
    """Whether the package holds the check, with both entry files calling it."""
    with tarfile.open(fileobj=io.BytesIO(package), mode="r:gz") as tar:
        names = tar.getnames()
        if "package/xz-check.js" not in names:
            return False
        for name, (_, becomes) in ENTRIES.items():
            if name not in names or tar.extractfile(name).read().decode() != becomes:
                return False
    return True


def rewrite_checksums(was: bytes, now: bytes, files: list) -> list:
    """Each of `files` that held the SHA-256 of `was`, rewritten to hold that of `now`."""
    old, new = hashlib.sha256(was).hexdigest(), hashlib.sha256(now).hexdigest()
    rewritten = []
    for path in files:
        if not path.is_file():
            continue
        text = path.read_text()
        if old in text:
            path.write_text(text.replace(old, new))
            rewritten.append(path)
    return rewritten


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("package", type=pathlib.Path, help="the npm package dist built, a .tar.gz")
    parser.add_argument("--checksums", nargs="*", type=pathlib.Path, default=[], help="files that hold its SHA-256")
    parser.add_argument("--has-check", action="store_true", help="only say whether it has the check")
    args = parser.parse_args()
    package = args.package.read_bytes()
    if args.has_check:
        if has_check(package):
            print(f"{args.package} has the xz check.")
            return 0
        print(f"{args.package} has no xz check, so it isn't published.", file=sys.stderr)
        return 1
    try:
        patched = add_check(package)
    except Refused as why:
        print(f"Not adding the xz check to {args.package}: {why}", file=sys.stderr)
        return 1
    args.package.write_bytes(patched)
    print(f"Added the xz check to {args.package}.")
    for path in rewrite_checksums(package, patched, args.checksums):
        print(f"{path} has the new package's SHA-256.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
