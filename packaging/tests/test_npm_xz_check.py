"""Tests for the xz check release.yml adds to the npm package, on a made-up package laid out as dist lays it out.
Run from the repository root:

    python3 -m unittest discover -s packaging/tests
"""

import gzip
import hashlib
import importlib.util
import io
import json
import pathlib
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

NPM = pathlib.Path(__file__).resolve().parent.parent / "npm"
SPEC = importlib.util.spec_from_file_location("add_xz_check", NPM / "add_xz_check.py")
add_xz_check = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(add_xz_check)

DIST_FILES = {
    "package/package.json": '{"name": "argonauts-code", "version": "0.2.0"}\n',
    "package/binary.js": "// dist's own, left as it is\n",
    "package/install.js": add_xz_check.ENTRIES["package/install.js"][0],
    "package/run-argonauts.js": add_xz_check.ENTRIES["package/run-argonauts.js"][0],
    "package/README.md": "# argonauts-code\n",
}


def package(files: dict) -> bytes:
    out = io.BytesIO()
    with tarfile.open(fileobj=out, mode="w") as tar:
        for name, text in files.items():
            data = text.encode()
            info = tarfile.TarInfo(name)
            info.size, info.mode, info.uid, info.gid = len(data), 0o644, 1001, 1001
            tar.addfile(info, io.BytesIO(data))
    return gzip.compress(out.getvalue())


def files_in(package_bytes: bytes) -> dict:
    with tarfile.open(fileobj=io.BytesIO(package_bytes), mode="r:gz") as tar:
        return {member.name: tar.extractfile(member).read().decode() for member in tar.getmembers() if member.isfile()}


class AddXzCheck(unittest.TestCase):
    def test_the_check_goes_in_and_dists_other_files_stay_as_they_are(self):
        patched = files_in(add_xz_check.add_check(package(DIST_FILES)))
        self.assertEqual(patched["package/xz-check.js"], (NPM / "xz-check.js").read_text())
        self.assertIn('require("./xz-check").needXz();', patched["package/install.js"])
        self.assertIn("if (!getPackage().exists()) {", patched["package/run-argonauts.js"])
        for name in ["package/package.json", "package/binary.js", "package/README.md"]:
            self.assertEqual(patched[name], DIST_FILES[name])
        self.assertEqual(
            list(patched)[:5],
            [
                "package/package.json",
                "package/binary.js",
                "package/install.js",
                "package/xz-check.js",
                "package/run-argonauts.js",
            ],
        )

    def test_an_entry_dist_wrote_otherwise_is_refused(self):
        changed = dict(DIST_FILES, **{"package/install.js": "#!/usr/bin/env node\nrequire('./binary').install();\n"})
        with self.assertRaisesRegex(add_xz_check.Refused, "package/install.js isn't the file dist writes"):
            add_xz_check.add_check(package(changed))
        without = {name: text for name, text in DIST_FILES.items() if name != "package/run-argonauts.js"}
        with self.assertRaisesRegex(add_xz_check.Refused, "has no package/run-argonauts.js"):
            add_xz_check.add_check(package(without))

    def test_a_package_is_checked_once(self):
        patched = add_xz_check.add_check(package(DIST_FILES))
        with self.assertRaisesRegex(add_xz_check.Refused, "already has the xz check"):
            add_xz_check.add_check(patched)

    def test_only_a_package_with_the_check_is_one_to_publish(self):
        dists = package(DIST_FILES)
        self.assertFalse(add_xz_check.has_check(dists))
        self.assertTrue(add_xz_check.has_check(add_xz_check.add_check(dists)))

    def test_the_command_is_stored_executable_and_the_rest_as_dist_stored_them(self):
        patched = add_xz_check.add_check(package(DIST_FILES))
        with tarfile.open(fileobj=io.BytesIO(patched), mode="r:gz") as tar:
            modes = {member.name: member.mode for member in tar.getmembers()}
        self.assertEqual(modes.pop("package/run-argonauts.js"), 0o755)
        self.assertEqual(set(modes.values()), {0o644})

    def test_a_package_whose_command_isnt_executable_is_not_one_to_publish(self):
        out = io.BytesIO()
        with tarfile.open(fileobj=io.BytesIO(add_xz_check.add_check(package(DIST_FILES))), mode="r:gz") as old:
            with tarfile.open(fileobj=out, mode="w") as new:
                for member in old.getmembers():
                    data = old.extractfile(member).read()
                    if member.name == "package/run-argonauts.js":
                        member.mode = 0o644
                    new.addfile(member, io.BytesIO(data))
        self.assertFalse(add_xz_check.has_check(gzip.compress(out.getvalue())))

    def test_each_checksum_dist_wrote_reads_the_package_the_release_uploads(self):
        name = "argonauts-code-npm-package.tar.gz"
        dists = package(DIST_FILES)
        old = hashlib.sha256(dists).hexdigest()
        other = "0123456789abcdef" * 4
        with tempfile.TemporaryDirectory() as folder:
            distrib = pathlib.Path(folder)
            (distrib / name).write_bytes(dists)
            written = {
                f"{name}.sha256": f"{old} *{name}\n",
                "sha256.sum": f"{other}  argonauts-code-x86_64-unknown-linux-gnu.tar.xz\n{old}  {name}\n",
                "global-dist-manifest.json": json.dumps({"artifacts": {name: {"checksums": {"sha256": old}}}}),
                "local-dist-manifest.json": json.dumps({"artifacts": {"other": {"checksums": {"sha256": other}}}}),
            }
            for file, text in written.items():
                (distrib / file).write_text(text)
            script = NPM / "add_xz_check.py"
            files = [str(distrib / file) for file in written] + [str(distrib / "no-such.sha256")]
            said = subprocess.run(
                [sys.executable, "-I", str(script), str(distrib / name), "--checksums", *files],
                capture_output=True,
                text=True,
                check=True,
            )
            uploaded = (distrib / name).read_bytes()
            self.assertTrue(add_xz_check.has_check(uploaded))
            new = hashlib.sha256(uploaded).hexdigest()
            for file in ["argonauts-code-npm-package.tar.gz.sha256", "sha256.sum", "global-dist-manifest.json"]:
                text = (distrib / file).read_text()
                self.assertIn(new, text, file)
                self.assertNotIn(old, text, file)
            self.assertIn(other, (distrib / "sha256.sum").read_text())
            self.assertEqual((distrib / "local-dist-manifest.json").read_text(), written["local-dist-manifest.json"])
            self.assertIn("sha256.sum has the new package's SHA-256.", said.stdout)
            checked = subprocess.run(
                [sys.executable, "-I", str(script), "--has-check", str(distrib / name)], capture_output=True, text=True
            )
            self.assertEqual(checked.returncode, 0, checked.stderr)


@unittest.skipUnless(shutil.which("node"), "needs node")
class XzAdvice(unittest.TestCase):
    def advice(self, **system):
        system = {"platform": "linux", "xz": False, "tar": "tar (GNU tar) 1.35", "osRelease": "", "root": False, **system}
        script = f'console.log(JSON.stringify(require({json.dumps(str(NPM / "xz-check.js"))}).advice({json.dumps(system)})))'
        said = subprocess.run(["node", "-e", script], capture_output=True, text=True, check=True)
        return json.loads(said.stdout)

    def test_nothing_stops_an_install_that_can_unpack(self):
        self.assertIsNone(self.advice(platform="darwin"))
        self.assertIsNone(self.advice(xz=True))
        self.assertIsNone(self.advice(tar="BusyBox v1.37.0 (2026-01-12 10:00:00 UTC) multi-call binary."))
        self.assertIsNone(self.advice(tar="bsdtar 3.7.4 - libarchive 3.7.4"))

    def test_without_xz_it_says_what_to_install_for_the_system(self):
        debian = 'PRETTY_NAME="Debian GNU/Linux 12 (bookworm)"\nID=debian\n'
        self.assertEqual(
            self.advice(osRelease=debian, root=True),
            "argonauts-code needs xz to unpack its program, and this system has none.\n"
            "Install it with `apt-get update && apt-get install xz-utils`, then try again.",
        )
        ubuntu = "ID=ubuntu\nID_LIKE=debian\n"
        self.assertIn("`sudo apt-get update && sudo apt-get install xz-utils`", self.advice(osRelease=ubuntu))
        amazon = 'ID="amzn"\nID_LIKE="centos rhel fedora"\n'
        self.assertIn("`sudo yum install xz`", self.advice(osRelease=amazon))
        rocky = 'ID="rocky"\nID_LIKE="rhel centos fedora"\n'
        self.assertIn("`sudo dnf install xz`", self.advice(osRelease=rocky))
        self.assertIn("`apk add xz`", self.advice(osRelease="ID=alpine\n", root=True))
        self.assertIn("`sudo zypper install xz`", self.advice(osRelease='ID="opensuse-leap"\nID_LIKE="suse opensuse"\n'))
        self.assertEqual(
            self.advice(osRelease="ID=madeup\n").splitlines()[1],
            "Install xz with your system's package manager, then try again.",
        )


if __name__ == "__main__":
    unittest.main()
