"""GitHub's REST API through gh, which takes its token from GH_TOKEN. The bot's scripts share it; their tests hand
them a fake with the same four methods instead."""

import json
import subprocess


class GithubError(Exception):
    """A request GitHub refused, with gh's account of why."""


class Github:
    def get(self, path):
        return self._call("GET", path)

    def pages(self, path, key=None):
        """Every item of a listing, page after page. `key` names the list inside each page for listings that wrap
        it in an object, as workflow runs do."""
        pages = self._call("GET", path, paginate=True) or []
        items = []
        for page in pages:
            items += page.get(key, []) if key else page
        return items

    def post(self, path, body=None):
        return self._call("POST", path, body=body)

    def delete(self, path):
        return self._call("DELETE", path)

    def _call(self, method, path, body=None, paginate=False):
        command = ["gh", "api", "--method", method, path]
        if paginate:
            command += ["--paginate", "--slurp"]
        if body is not None:
            command += ["--input", "-"]
        done = subprocess.run(
            command,
            input=json.dumps(body) if body is not None else None,
            capture_output=True,
            text=True,
        )
        if done.returncode != 0:
            raise GithubError(done.stderr.strip() or f"gh api {method} {path} failed")
        return json.loads(done.stdout) if done.stdout.strip() else None
