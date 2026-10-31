"""Tests for the Publishers check, against an npm, crates.io and GitHub the tests make up, and a tap that is a local
bare repository. Run from the repository root:

    python3 -m unittest discover -s packaging/tests
"""

import base64
import datetime
import importlib.util
import io
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest
import urllib.error
import urllib.parse

RELEASE = pathlib.Path(__file__).resolve().parent.parent / "release"
sys.path.insert(0, str(RELEASE))
SPEC = importlib.util.spec_from_file_location("publishers", RELEASE / "publishers.py")
publishers = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(publishers)

CLAIMS = {
    "repository": "sheng-tse/argonauts-code",
    "workflow_ref": "sheng-tse/argonauts-code/.github/workflows/release.yml@refs/heads/main",
    "environment": "release",
}
CRATES = [("argonauts-core", "0.2.1"), ("argonauts-code", "0.2.1")]
TRUSTING = {
    "repository_name": "argonauts-code",
    "workflow_filename": "release.yml",
    "environment": "release",
}


def jwt(claims):
    payload = base64.urlsafe_b64encode(json.dumps(claims).encode()).decode().rstrip("=")
    return f"header.{payload}.signature"


class Answer:
    def __init__(self, status, body, headers=None):
        self.status, self.body, self.headers = status, body, headers or {}

    def read(self):
        return self.body.encode()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        return False


class Internet:
    """Answers each request from a table of (method, url start) -> (status, body, headers), and keeps them all."""

    def __init__(self, table, claims=CLAIMS):
        self.table = {("GET", "https://ids.example/token?for=run&audience="): (200, {"value": jwt(claims)}, {})}
        self.table.update(table)
        self.asked = []

    def __call__(self, request, timeout=None):
        method = request.get_method()
        self.asked.append((method, request.full_url, dict(request.header_items()), request.data))
        for (want_method, start), (status, body, headers) in self.table.items():
            if method == want_method and request.full_url.startswith(start):
                text = body if isinstance(body, str) else json.dumps(body)
                if status >= 400:
                    raise urllib.error.HTTPError(request.full_url, status, "no", headers, io.BytesIO(text.encode()))
                return Answer(status, text, headers)
        raise urllib.error.HTTPError(request.full_url, 404, "not here", {}, io.BytesIO(b"{}"))

    def methods(self):
        return [(method, url.split("?")[0]) for method, url, _, _ in self.asked]


def run(internet):
    env = {"ACTIONS_ID_TOKEN_REQUEST_URL": "https://ids.example/token?for=run", "ACTIONS_ID_TOKEN_REQUEST_TOKEN": "t"}
    return publishers.Run(env, internet)


def index(names):
    def get(url):
        return '{"vers": "0.2.0"}\n' if any(url.endswith("/" + name) for name in names) else None

    return get


EXCHANGE = "https://registry.npmjs.org/-/npm/v1/oidc/token/exchange/package/argonauts-code"
TOKENS = "https://crates.io/api/v1/trusted_publishing/tokens"
CONFIGS = "https://crates.io/api/v1/trusted_publishing/github_configs?crate="


class Npm(unittest.TestCase):
    def test_a_package_that_trusts_this_run_gives_a_token_that_goes_unused(self):
        internet = Internet({("POST", EXCHANGE): (201, {"token": "npm_secret"}, {})})
        lines = publishers.npm("argonauts-code", run(internet), internet)
        self.assertEqual(len(lines), 1)
        self.assertIn('trusts release.yml in "release"', lines[0])
        self.assertIn("token issued, unused", lines[0])
        self.assertNotIn("npm_secret", lines[0])
        audience = urllib.parse.parse_qs(urllib.parse.urlparse(internet.asked[0][1]).query)["audience"]
        self.assertEqual(audience, ["npm:registry.npmjs.org"])
        self.assertEqual(internet.asked[1][2]["Authorization"], f"Bearer {jwt(CLAIMS)}")
        self.assertEqual(internet.methods()[1:], [("POST", EXCHANGE)])

    def test_a_package_that_doesnt_says_where_to_add_the_publisher(self):
        internet = Internet({("POST", EXCHANGE): (404, {"message": "No trusted publisher configured"}, {})})
        with self.assertRaises(publishers.Failed) as failure:
            publishers.npm("argonauts-code", run(internet), internet)
        what, fix = str(failure.exception).split("\n")
        self.assertIn('no trusted publisher matches release.yml in "release"', what)
        self.assertIn("No trusted publisher configured", what)
        self.assertIn("npmjs.com › argonauts-code › Settings › Trusted publishing", fix)

    def test_a_job_without_its_identity_says_which_permission_it_needs(self):
        with self.assertRaisesRegex(publishers.Failed, "id-token: write"):
            publishers.npm("argonauts-code", publishers.Run({}, Internet({})), Internet({}))


class CratesIo(unittest.TestCase):
    def exchange(self):
        return {("POST", TOKENS): (200, {"token": "cio_secret"}, {}), ("DELETE", TOKENS): (204, "", {})}

    def test_every_crate_trusting_this_run_passes_and_the_token_is_revoked(self):
        internet = Internet({**self.exchange(), ("GET", CONFIGS): (200, {"github_configs": [TRUSTING]}, {})})
        lines, warnings = publishers.crates_io(run(internet), CRATES, internet, get=index(["argonauts-core", "argonauts-code"]))
        self.assertIn("token issued, revoked", lines[0])
        self.assertIn("2 crates", lines[1])
        self.assertIn("all already published, none new", lines[1])
        self.assertEqual(warnings, [])
        self.assertEqual(internet.methods()[-1], ("DELETE", TOKENS))
        self.assertEqual(internet.asked[-1][2]["Authorization"], "Bearer cio_secret")
        self.assertEqual(json.loads(internet.asked[1][3]), {"jwt": jwt(CLAIMS)})

    def test_a_token_that_cant_list_publishers_still_passes_and_says_so(self):
        internet = Internet({**self.exchange(), ("GET", CONFIGS): (403, {"errors": [{"detail": "forbidden"}]}, {})})
        lines, warnings = publishers.crates_io(run(internet), CRATES, internet, get=index(["argonauts-core", "argonauts-code"]))
        self.assertEqual(len(lines), 2)
        self.assertIn("doesn't list a crate's trusted publishers", warnings[0])
        self.assertEqual(internet.methods()[-1], ("DELETE", TOKENS))

    def test_a_crate_that_trusts_another_workflow_fails_by_name_and_the_token_is_still_revoked(self):
        elsewhere = dict(TRUSTING, workflow_filename="publish.yml")
        internet = Internet(
            {
                **self.exchange(),
                ("GET", CONFIGS + "argonauts-core"): (200, {"github_configs": [TRUSTING]}, {}),
                ("GET", CONFIGS + "argonauts-code"): (200, {"github_configs": [elsewhere]}, {}),
            }
        )
        with self.assertRaisesRegex(publishers.Failed, 'argonauts-code don\'t trust release.yml in "release"'):
            publishers.crates_io(run(internet), CRATES, internet, get=index(["argonauts-core", "argonauts-code"]))
        self.assertEqual(internet.methods()[-1], ("DELETE", TOKENS))

    def test_no_trusting_crate_means_no_token_and_nothing_to_revoke(self):
        internet = Internet({("POST", TOKENS): (400, {"errors": [{"detail": "No matching config"}]}, {})})
        with self.assertRaises(publishers.Failed) as failure:
            publishers.crates_io(run(internet), CRATES, internet, get=index([]))
        self.assertIn("No matching config", str(failure.exception))
        self.assertIn("Trusted Publishing: add this repository", str(failure.exception))
        self.assertNotIn("DELETE", [method for method, _ in internet.methods()])

    def test_a_crate_crates_io_never_had_fails(self):
        internet = Internet({**self.exchange(), ("GET", CONFIGS): (403, "{}", {})})
        with self.assertRaisesRegex(publishers.Failed, "argonauts-code has never been on crates.io"):
            publishers.crates_io(run(internet), CRATES, internet, get=index(["argonauts-core"]))

    def test_a_token_crates_io_wont_revoke_is_named(self):
        internet = Internet({**self.exchange(), ("DELETE", TOKENS): (500, "{}", {}), ("GET", CONFIGS): (403, "{}", {})})
        lines, warnings = publishers.crates_io(run(internet), CRATES, internet, get=index(["argonauts-core", "argonauts-code"]))
        self.assertIn("token issued, not revoked", lines[0])
        self.assertIn("didn't revoke the token", warnings[0])


class Homebrew(unittest.TestCase):
    API = "https://api.github.com/repos/sheng-tse/homebrew-tap"

    def test_a_token_that_can_push_passes(self):
        internet = Internet({("GET", self.API): (200, {"default_branch": "main"}, {})})
        pushed = []
        lines, warnings = publishers.homebrew(
            "sheng-tse/homebrew-tap", "tap", internet, can_push=lambda *a: pushed.append(a) or True
        )
        self.assertIn("the tap token can still push", lines[0])
        self.assertEqual(warnings, [])
        self.assertEqual(pushed, [("https://github.com/sheng-tse/homebrew-tap.git", "main", "tap")])

    def test_an_expired_token_says_how_to_replace_it(self):
        internet = Internet({("GET", self.API): (401, {"message": "Bad credentials"}, {})})
        with self.assertRaisesRegex(publishers.Failed, "expired or was revoked\nMake a fine-grained token"):
            publishers.homebrew("sheng-tse/homebrew-tap", "tap", internet, can_push=lambda *a: True)

    def test_a_token_without_the_tap_cant_open_it(self):
        internet = Internet({("GET", self.API): (404, {"message": "Not Found"}, {})})
        with self.assertRaisesRegex(publishers.Failed, "can't open sheng-tse/homebrew-tap"):
            publishers.homebrew("sheng-tse/homebrew-tap", "tap", internet, can_push=lambda *a: True)

    def test_a_read_only_token_cant_push(self):
        internet = Internet({("GET", self.API): (200, {"default_branch": "main"}, {})})
        with self.assertRaisesRegex(publishers.Failed, "can open sheng-tse/homebrew-tap but not push to it"):
            publishers.homebrew("sheng-tse/homebrew-tap", "tap", internet, can_push=lambda *a: False)

    def test_no_token_at_all(self):
        with self.assertRaisesRegex(publishers.Failed, "HOMEBREW_TAP_TOKEN isn't set"):
            publishers.homebrew("sheng-tse/homebrew-tap", "", Internet({}))

    def test_a_token_expiring_soon_is_named(self):
        soon = (datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(days=3)).strftime("%Y-%m-%d %H:%M:%S UTC")
        headers = {"GitHub-Authentication-Token-Expiration": soon}
        internet = Internet({("GET", self.API): (200, {"default_branch": "main"}, headers)})
        lines, warnings = publishers.homebrew("sheng-tse/homebrew-tap", "tap", internet, can_push=lambda *a: True)
        self.assertIn(f"expires {soon.split()[0]}", lines[0])
        self.assertIn("the tap token expires on", warnings[0])

    def test_the_push_is_a_dry_run_that_changes_nothing(self):
        with tempfile.TemporaryDirectory() as root:
            tap, work = pathlib.Path(root, "tap.git"), pathlib.Path(root, "work")
            subprocess.run(["git", "init", "--quiet", "--bare", "--initial-branch=main", tap], check=True)
            subprocess.run(["git", "init", "--quiet", "--initial-branch=main", work], check=True)
            identity = ["-c", "user.name=t", "-c", "user.email=t@example.com"]
            subprocess.run(["git", "-C", work, *identity, "commit", "--quiet", "--allow-empty", "-m", "f"], check=True)
            subprocess.run(["git", "-C", work, "push", "--quiet", tap, "main"], check=True)
            before = subprocess.run(["git", "-C", tap, "rev-parse", "main"], capture_output=True, text=True).stdout
            self.assertTrue(publishers.git_can_push(tap.as_uri(), "main", "tap"))
            after = subprocess.run(["git", "-C", tap, "rev-parse", "main"], capture_output=True, text=True).stdout
            self.assertEqual(before, after)
            self.assertFalse(publishers.git_can_push(pathlib.Path(root, "none.git").as_uri(), "main", "tap"))


class Command(unittest.TestCase):
    def test_a_wrong_command_shows_the_usage(self):
        err = io.StringIO()
        sys.stderr = err
        try:
            self.assertEqual(publishers.main(["npm"], env={}), 2)
            self.assertEqual(publishers.main(["crates-io", "extra"], env={}), 2)
        finally:
            sys.stderr = sys.__stderr__
        self.assertIn("publishers.py homebrew <account>/<tap>", err.getvalue())

    def test_a_failure_prints_its_line_and_what_to_change(self):
        out, err = io.StringIO(), io.StringIO()
        sys.stdout, sys.stderr = out, err
        try:
            status = publishers.main(["homebrew", "sheng-tse/homebrew-tap"], env={"GITHUB_ACTIONS": "true"})
        finally:
            sys.stdout, sys.stderr = sys.__stdout__, sys.__stderr__
        self.assertEqual(status, 1)
        self.assertTrue(out.getvalue().startswith("✗  Homebrew"))
        self.assertIn("HOMEBREW_TAP_TOKEN isn't set", out.getvalue())
        self.assertIn("::error title=Homebrew::", err.getvalue())


if __name__ == "__main__":
    unittest.main()
