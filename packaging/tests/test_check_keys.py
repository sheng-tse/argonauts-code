"""Tests for check_keys.py. Run from the repository root:
    python3 -m unittest discover -s packaging/tests
"""
import base64
import contextlib
import io
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "updates"))
import check_keys  # noqa: E402

REAL = base64.b64encode(b"Ed" + bytes(range(8)) + bytes(range(32))).decode()


def keys_file(*keys):
    folder = pathlib.Path(tempfile.mkdtemp())
    quoted = "".join(f'    "{key}",\n' for key in keys)
    path = folder / "mod.rs"
    path.write_text(f"pub const PUBLIC_KEYS: [&str; {len(keys)}] = [\n{quoted}];\n")
    return path


def checked(path):
    errors = io.StringIO()
    with contextlib.redirect_stderr(errors):
        code = check_keys.main(path)
    return code, errors.getvalue()


class CheckKeys(unittest.TestCase):
    def test_the_placeholders_stop_a_release(self):
        code, said = checked(keys_file("REPLACE-WITH-THE-SIGNING-PUBLIC-KEY", REAL))
        self.assertEqual(code, 1)
        self.assertIn("key 1 is still a placeholder", said)

    def test_real_keys_pass(self):
        self.assertEqual(checked(keys_file(REAL, REAL)), (0, ""))

    def test_a_key_that_isnt_minisigns_is_refused(self):
        code, said = checked(keys_file(base64.b64encode(b"not a key").decode(), "not base64!"))
        self.assertEqual(code, 1)
        self.assertIn("key 1 isn't a minisign public key", said)
        self.assertIn("key 2 isn't base64", said)

    def test_the_built_in_file_is_read(self):
        source = pathlib.Path(__file__).resolve().parents[2] / "crates/app/src/self_update/mod.rs"
        self.assertEqual(len(check_keys.built_in_keys(source.read_text())), 2)


if __name__ == "__main__":
    unittest.main()
