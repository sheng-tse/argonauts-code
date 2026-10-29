"""Tests for draft_manifest.py, on a release folder made the way dist makes one. Run from the repository root:

    python3 -m unittest discover -s packaging/tests
"""

import contextlib
import datetime
import hashlib
import io
import json
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "updates"))
import draft_manifest  # noqa: E402

NOW = datetime.datetime(2026, 10, 27, 15, 0, tzinfo=datetime.timezone.utc)
TARGETS = ["aarch64-apple-darwin", "x86_64-unknown-linux-musl"]


def make_release(folder, version="0.1.1"):
    """dist-manifest.json, two archives and their .sha256 files, as a release holds them."""
    artifacts = {}
    for target in TARGETS:
        name = f"argonauts-code-{target}.tar.xz"
        data = f"the {target} build of {version}".encode()
        (folder / name).write_bytes(data)
        (folder / f"{name}.sha256").write_text(f"{hashlib.sha256(data).hexdigest()} *{name}\n")
        artifacts[name] = {"name": name, "kind": "executable-zip", "target_triples": [target],
                           "checksum": f"{name}.sha256"}
        artifacts[f"{name}.sha256"] = {"name": f"{name}.sha256", "kind": "checksum", "target_triples": [target]}
    artifacts["argonauts-code-installer.sh"] = {"name": "argonauts-code-installer.sh", "kind": "installer",
                                                "target_triples": TARGETS}
    (folder / "dist-manifest.json").write_text(json.dumps({"artifacts": artifacts}))


class Draft(unittest.TestCase):
    def setUp(self):
        self.folder = pathlib.Path(tempfile.mkdtemp())
        self.release = self.folder / "release"
        self.release.mkdir()
        make_release(self.release)
        self.out = self.folder / "stable.json"

    def run_draft(self, *extra, tag="v0.1.1", channel="stable"):
        argv = ["--release", str(self.release), "--tag", tag, "--channel", channel, "--out", str(self.out), *extra]
        errors = io.StringIO()
        with contextlib.redirect_stderr(errors):
            code = draft_manifest.main(argv, now=NOW)
        manifest = json.loads(self.out.read_text()) if code == 0 and self.out.exists() else None
        return code, manifest, errors.getvalue()

    def write_previous(self, **fields):
        previous = {"channel": "stable", "serial": 7, "version": "0.1.0", "published": "2026-10-20T15:00:00Z",
                    "min_version": "0.1.0", "halted": False, "yanked": [], "rollout": [], "platforms": {}}
        previous.update(fields)
        path = self.folder / "previous.json"
        path.write_text(json.dumps(previous))
        return str(path)

    def test_a_first_manifest_lists_every_archive_with_its_checksum_and_size(self):
        code, manifest, errors = self.run_draft()
        self.assertEqual(code, 0, errors)
        self.assertEqual(manifest["serial"], 1)
        self.assertEqual(manifest["version"], "0.1.1")
        self.assertEqual(manifest["min_version"], "0.1.0")
        self.assertFalse(manifest["halted"])
        self.assertEqual(sorted(manifest["platforms"]), TARGETS)
        mac = manifest["platforms"]["aarch64-apple-darwin"]
        data = (self.release / "argonauts-code-aarch64-apple-darwin.tar.xz").read_bytes()
        self.assertEqual(mac["sha256"], hashlib.sha256(data).hexdigest())
        self.assertEqual(mac["size"], len(data))
        self.assertEqual(
            mac["url"],
            "https://github.com/sheng-tse/argonauts-code/releases/download/v0.1.1/"
            "argonauts-code-aarch64-apple-darwin.tar.xz",
        )
        self.assertIn("minisign -S", errors)

    def test_the_release_gets_a_manifest_of_its_own_for_self_update_by_version(self):
        code, manifest, said = self.run_draft()
        self.assertEqual(code, 0, said)
        named = self.folder / "releases" / "0.1.1.json"
        self.assertEqual(json.loads(named.read_text()), manifest)
        self.assertIn(str(named), said)

    def test_the_model_catalog_the_release_ships_goes_beside_its_manifest_to_sign(self):
        code, _, said = self.run_draft()
        self.assertEqual(code, 0, said)
        catalog = self.folder / "models.toml"
        self.assertEqual(catalog.read_bytes(), draft_manifest.CATALOG.read_bytes())
        self.assertIn(f'-m {catalog} -t "argonauts-code models 0.1.1"', said)

    def test_a_stable_release_goes_on_beta_only_when_beta_offers_older(self):
        older = self.write_previous(channel="beta", version="0.1.1-beta.2")
        code, manifest, said = self.run_draft("--previous", older, "--if-newer", channel="beta")
        self.assertEqual(code, 0, said)
        self.assertEqual((manifest["channel"], manifest["version"]), ("beta", "0.1.1"))
        self.out.unlink()
        newer = self.write_previous(channel="beta", version="0.2.0-beta.1")
        code, _, said = self.run_draft("--previous", newer, "--if-newer", channel="beta")
        self.assertEqual(code, 0, said)
        self.assertFalse(self.out.exists())
        self.assertIn("nothing was drafted", said)

    def test_the_rollout_widens_by_the_day(self):
        _, manifest, _ = self.run_draft()
        self.assertEqual(
            manifest["rollout"],
            [
                {"from": "2026-10-27T15:00:00Z", "percent": 10},
                {"from": "2026-10-28T15:00:00Z", "percent": 50},
                {"from": "2026-10-29T15:00:00Z", "percent": 100},
            ],
        )

    def test_a_beta_reaches_every_beta_install_at_once(self):
        make_release(self.release, "0.2.0-beta.1")
        code, manifest, errors = self.run_draft(tag="v0.2.0-beta.1", channel="beta")
        self.assertEqual(code, 0, errors)
        self.assertEqual(manifest["rollout"], [{"from": "2026-10-27T15:00:00Z", "percent": 100}])

    def test_a_beta_never_goes_on_the_stable_channel(self):
        code, _, errors = self.run_draft(tag="v0.2.0-beta.1")
        self.assertEqual(code, 1)
        self.assertIn("beta channel only", errors)

    def test_the_serial_and_the_yanked_list_carry_over(self):
        previous = self.write_previous(yanked=["0.0.9"], min_version="0.0.8")
        _, manifest, _ = self.run_draft("--previous", previous, "--yank", "0.1.0")
        self.assertEqual(manifest["serial"], 8)
        self.assertEqual(manifest["yanked"], ["0.0.9", "0.1.0"])
        self.assertEqual(manifest["min_version"], "0.0.8")

    def test_an_older_version_than_the_one_offered_is_refused(self):
        previous = self.write_previous(version="0.1.2")
        code, _, errors = self.run_draft("--previous", previous)
        self.assertEqual(code, 1)
        self.assertIn("older than 0.1.2", errors)

    def test_halting_keeps_the_schedule_it_was_signed_with(self):
        signed = [{"from": "2026-10-26T15:00:00Z", "percent": 10}, {"from": "2026-10-27T15:00:00Z", "percent": 100}]
        previous = self.write_previous(version="0.1.1", rollout=signed, published="2026-10-26T15:00:00Z")
        code, manifest, errors = self.run_draft("--previous", previous, "--halted")
        self.assertEqual(code, 0, errors)
        self.assertTrue(manifest["halted"])
        self.assertEqual(manifest["serial"], 8)
        self.assertEqual(manifest["rollout"], signed)
        self.assertEqual(manifest["published"], "2026-10-26T15:00:00Z")

    def test_a_yanked_version_is_never_offered(self):
        code, _, errors = self.run_draft("--yank", "0.1.1")
        self.assertEqual(code, 1)
        self.assertIn("0.1.1 is yanked", errors)

    def test_an_archive_that_doesnt_match_its_checksum_is_refused(self):
        (self.release / "argonauts-code-x86_64-unknown-linux-musl.tar.xz").write_bytes(b"changed after the build")
        code, _, errors = self.run_draft()
        self.assertEqual(code, 1)
        self.assertIn("doesn't match", errors)

    def test_a_rollout_that_never_reaches_everyone_is_refused(self):
        code, _, errors = self.run_draft("--rollout", "10,50")
        self.assertEqual(code, 1)
        self.assertIn("end at 100", errors)

    def test_versions_order_as_semver_does(self):
        ordered = ["0.1.0-beta.2", "0.1.0-beta.10", "0.1.0-rc.1", "0.1.0", "0.1.1", "0.10.0"]
        self.assertEqual(sorted(reversed(ordered), key=draft_manifest.version_key), ordered)


if __name__ == "__main__":
    unittest.main()
