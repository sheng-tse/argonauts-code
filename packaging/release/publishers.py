#!/usr/bin/env python3
"""Checks that each place a release publishes to still trusts this repository, and publishes nothing:

    python3 packaging/release/publishers.py npm <package>
    python3 packaging/release/publishers.py crates-io
    python3 packaging/release/publishers.py homebrew <account>/<tap>

The Publishers job runs these in the Release dry run (publishers.yml), before the release pull request, because a
publisher found missing at publish time fails the release after the GitHub release is already out.

npm and crates.io trust a workflow file in an environment (release.yml in `release`), so the job trades the run's
identity for a publish token the way the publish jobs do. npm gives one only if the package trusts this run, and
the token is never used. crates.io gives one only if a crate trusts it; the token is revoked at once. crates.io
trusts a publisher only for a crate that exists, so the check also fails on a crate it has never had. Homebrew's tap
takes a stored token, HOMEBREW_TAP_TOKEN, which can expire unseen: the check asks GitHub whether it can still push.

Each check prints one line, and on a failure what to change, in words.
"""

import base64
import datetime
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import urllib.error
import urllib.parse
import urllib.request

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import crates  # noqa: E402

NPM = "https://registry.npmjs.org"
CRATES_IO = "https://crates.io"
GITHUB_API = "https://api.github.com"
GITHUB = "https://github.com"
USER_AGENT = crates.USER_AGENT
# A tap token that expires sooner than this is named in a warning, so it's replaced before a release needs it.
EXPIRY_WARNING_DAYS = 14


class Failed(Exception):
    """A publisher that doesn't trust this run. The first line says what's wrong; the rest, what to change."""


class Run:
    """The run's identity, as GitHub Actions hands it to a job with `id-token: write`."""

    def __init__(self, env=None, opener=None):
        env = os.environ if env is None else env
        self.url = env.get("ACTIONS_ID_TOKEN_REQUEST_URL", "")
        self.token = env.get("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "")
        self.open = opener or urllib.request.urlopen

    def id_token(self, audience):
        if not self.url or not self.token:
            raise Failed(
                "this job can't ask for the run's identity\n"
                "Give the Publishers job `id-token: write`, in publishers.yml and where release.yml calls it."
            )
        url = f"{self.url}&audience={urllib.parse.quote(audience, safe='')}"
        request = urllib.request.Request(url, headers={"Authorization": f"bearer {self.token}"})
        with self.open(request, timeout=30) as response:
            return json.loads(response.read())["value"]


def claims(jwt):
    """The token's claims, read without checking its signature: they only name things in messages."""
    try:
        payload = jwt.split(".")[1]
        return json.loads(base64.urlsafe_b64decode(payload + "=" * (-len(payload) % 4)))
    except (IndexError, ValueError):
        return {}


def trusted_as(jwt):
    """`release.yml in "release"`, from the token's own workflow and environment."""
    said = claims(jwt)
    workflow = said.get("workflow_ref", "").split("@")[0].rsplit("/", 1)[-1] or "this workflow"
    environment = said.get("environment")
    return f'{workflow} in "{environment}"' if environment else f"{workflow} outside any environment"


def call(opener, method, url, headers=None, body=None):
    """(status, body, headers) for a request; an HTTP error is an answer too."""
    data = json.dumps(body).encode() if body is not None else None
    headers = {"User-Agent": USER_AGENT, "Accept": "application/json", **(headers or {})}
    if data is not None:
        headers["Content-Type"] = "application/json"
    request = urllib.request.Request(url, data=data, method=method, headers=headers)
    try:
        with opener(request, timeout=30) as response:
            return response.status, response.read().decode(), response.headers
    except urllib.error.HTTPError as error:
        with error:
            return error.code, error.read().decode(errors="replace"), error.headers


def said_by(body):
    """The reason a registry gave, from the JSON shapes npm, crates.io and GitHub answer with."""
    try:
        answer = json.loads(body)
    except ValueError:
        return body.strip()[:200]
    if isinstance(answer, dict):
        if answer.get("errors"):
            return "; ".join(str(error.get("detail", error)) for error in answer["errors"])
        return str(answer.get("message") or answer.get("error") or answer)
    return str(answer)


def line(ok, place, subject, words, note=""):
    mark = "✓" if ok else "✗"
    return f"{mark}  {place:<10} {subject:<18} {words}" + (f"   {note}" if note else "")


def npm(package, run, opener=None, registry=NPM):
    """npm's token exchange, which answers only if `package` trusts this run's workflow and environment."""
    opener = opener or urllib.request.urlopen
    jwt = run.id_token(f"npm:{urllib.parse.urlparse(registry).hostname}")
    name = urllib.parse.quote(package, safe="@")
    status, body, _ = call(
        opener,
        "POST",
        f"{registry}/-/npm/v1/oidc/token/exchange/package/{name}",
        {"Authorization": f"Bearer {jwt}"},
    )
    if 200 <= status < 300 and json.loads(body).get("token"):
        return [line(True, "npm", package, f"trusts {trusted_as(jwt)}", "token issued, unused")]
    raise Failed(
        f"no trusted publisher matches {trusted_as(jwt)} (npm answered {status}: {said_by(body)})\n"
        f"npmjs.com › {package} › Settings › Trusted publishing: add this repository's release.yml, environment "
        "release. npm drops a trusted publisher that goes unused for two days after it's added."
    )


def crates_io(run, crate_list, opener=None, registry=CRATES_IO, index=crates.INDEX, get=crates.fetch):
    """crates.io's token exchange, then each crate's trusted publishers where crates.io lists them to that token,
    then the token revoked; and every crate already on crates.io."""
    opener = opener or urllib.request.urlopen
    jwt = run.id_token("crates.io")
    status, body, _ = call(opener, "POST", f"{registry}/api/v1/trusted_publishing/tokens", body={"jwt": jwt})
    token = json.loads(body).get("token") if 200 <= status < 300 else None
    if not token:
        raise Failed(
            f"no crate trusts {trusted_as(jwt)} (crates.io answered {status}: {said_by(body)})\n"
            "crates.io › each argonauts crate › Settings › Trusted Publishing: add this repository, workflow "
            "release.yml, environment release."
        )
    warnings = []
    try:
        untrusted, unlisted = trusted_publishers(opener, registry, token, jwt, crate_list)
    finally:
        revoked, _, _ = call(
            opener, "DELETE", f"{registry}/api/v1/trusted_publishing/tokens", {"Authorization": f"Bearer {token}"}
        )
    if 200 <= revoked < 300:
        lines = [line(True, "crates.io", "this repository", "trusted", "token issued, revoked")]
    else:
        lines = [line(True, "crates.io", "this repository", "trusted", "token issued, not revoked")]
        warnings.append(f"crates.io didn't revoke the token (it answered {revoked}); it expires within 30 minutes")
    if unlisted:
        warnings.append(
            "crates.io doesn't list a crate's trusted publishers to this token, so each crate's own trust shows "
            "only when the crates job publishes it"
        )
    if untrusted:
        raise Failed(
            f"{', '.join(untrusted)} don't trust {trusted_as(jwt)}\n"
            "crates.io › each of those crates › Settings › Trusted Publishing: add this repository, workflow "
            "release.yml, environment release."
        )
    missing = crates.never_published(crate_list, index, get)
    if missing:
        raise Failed(
            f"{', '.join(missing)} {'has' if len(missing) == 1 else 'have'} never been on crates.io\n"
            "crates.io trusts a publisher only for a crate that exists: publish each one's first version by hand "
            "(publish-crates.yml says how), then add release.yml in the release environment as its trusted publisher."
        )
    lines.append(line(True, "crates.io", f"{len(crate_list)} crates", "all already published, none new"))
    return lines, warnings


def trusted_publishers(opener, registry, token, jwt, crate_list):
    """The crates whose listed publishers don't include this run, and whether crates.io listed none to the token."""
    said = claims(jwt)
    repository = said.get("repository", "").partition("/")[2]
    workflow = said.get("workflow_ref", "").split("@")[0].rsplit("/", 1)[-1]
    untrusted = []
    for name, _ in crate_list:
        status, body, _ = call(
            opener,
            "GET",
            f"{registry}/api/v1/trusted_publishing/github_configs?crate={urllib.parse.quote(name)}",
            {"Authorization": f"Bearer {token}"},
        )
        if not 200 <= status < 300:
            return [], True
        configs = json.loads(body).get("github_configs", [])
        # The repository's account isn't compared: only those who manage a crate can add its publishers.
        if not any(
            config.get("repository_name", "").lower() == repository.lower()
            and config.get("workflow_filename") == workflow
            and config.get("environment") in (None, "", said.get("environment"))
            for config in configs
        ):
            untrusted.append(name)
    return untrusted, False


def homebrew(tap, token, opener=None, api=GITHUB_API, git_url=GITHUB, can_push=None):
    """Whether HOMEBREW_TAP_TOKEN still opens the tap, and still may push to it."""
    opener = opener or urllib.request.urlopen
    can_push = can_push or git_can_push
    fix = f"Make a fine-grained token with Contents read and write on {tap}, and save it as HOMEBREW_TAP_TOKEN."
    if not token:
        raise Failed(f"HOMEBREW_TAP_TOKEN isn't set for the release environment\n{fix}")
    status, body, headers = call(
        opener,
        "GET",
        f"{api}/repos/{tap}",
        {"Authorization": f"Bearer {token}", "Accept": "application/vnd.github+json"},
    )
    if status == 401:
        raise Failed(f"GitHub refused the tap token: it has expired or was revoked\n{fix}")
    if status in (403, 404):
        raise Failed(f"the tap token can't open {tap} (GitHub answered {status})\n{fix}")
    if not 200 <= status < 300:
        raise Failed(f"GitHub answered {status} about {tap}: {said_by(body)}")
    branch = json.loads(body).get("default_branch", "main")
    if not can_push(f"{git_url}/{tap}.git", branch, token):
        raise Failed(f"the tap token can open {tap} but not push to it\n{fix}")
    warnings = []
    expires = (headers.get("GitHub-Authentication-Token-Expiration") or "").strip()
    if expires and days_until(expires) < EXPIRY_WARNING_DAYS:
        warnings.append(f"the tap token expires on {expires}: replace it before then. {fix}")
    note = f"expires {expires.split()[0]}" if expires else ""
    return [line(True, "Homebrew", tap, "the tap token can still push", note)], warnings


def days_until(expires, now=None):
    """Days from now to GitHub's `2026-11-30 12:00:00 UTC` (or with an offset, `+0000`)."""
    now = now or datetime.datetime.now(datetime.timezone.utc)
    stamp = expires.replace(" UTC", " +0000")
    try:
        when = datetime.datetime.strptime(stamp, "%Y-%m-%d %H:%M:%S %z")
    except ValueError:
        return EXPIRY_WARNING_DAYS
    return (when - now).days


def git_can_push(url, branch, token):
    """A push of the branch's own tip back to it, as a dry run: GitHub checks the right to push before it answers,
    and nothing changes. The token goes through git's environment, never its arguments."""
    basic = base64.b64encode(f"x-access-token:{token}".encode()).decode()
    env = {
        **os.environ,
        "GIT_TERMINAL_PROMPT": "0",
        "GIT_CONFIG_COUNT": "1",
        "GIT_CONFIG_KEY_0": f"http.{url}.extraheader",
        "GIT_CONFIG_VALUE_0": f"AUTHORIZATION: basic {basic}",
    }
    with tempfile.TemporaryDirectory() as tree:
        steps = [
            ["git", "init", "--quiet", tree],
            ["git", "-C", tree, "fetch", "--quiet", "--depth", "1", url, f"refs/heads/{branch}"],
            ["git", "-C", tree, "push", "--quiet", "--dry-run", url, f"FETCH_HEAD:refs/heads/{branch}"],
        ]
        for step in steps:
            if subprocess.run(step, env=env, capture_output=True).returncode != 0:
                return False
    return True


def report(lines, warnings, out=sys.stdout, err=sys.stderr, actions=False):
    for text in lines:
        print(text, file=out)
    for text in warnings:
        print(f"::warning::{text}" if actions else f"warning: {text}", file=err)


def main(argv=None, env=None):
    argv = sys.argv[1:] if argv is None else argv
    env = os.environ if env is None else env
    actions = env.get("GITHUB_ACTIONS") == "true"
    arguments = {"npm": 2, "crates-io": 1, "homebrew": 2}
    if not argv or arguments.get(argv[0]) != len(argv):
        print(__doc__, file=sys.stderr)
        return 2
    place = {"npm": "npm", "crates-io": "crates.io", "homebrew": "Homebrew"}[argv[0]]
    subject = argv[1] if len(argv) > 1 else "this repository"
    try:
        if argv[0] == "npm":
            report(npm(argv[1], Run(env)), [], actions=actions)
        elif argv[0] == "crates-io":
            report(*crates_io(Run(env), crates.workspace_crates(crates.read_metadata())), actions=actions)
        else:
            report(*homebrew(argv[1], env.get("HOMEBREW_TAP_TOKEN", "")), actions=actions)
    except Failed as failure:
        what, _, fix = str(failure).partition("\n")
        print(line(False, place, subject, what))
        if fix:
            print(f"   {fix}")
        if actions:
            print(f"::error title={place}::{what}. {fix}".rstrip(), file=sys.stderr)
        return 1
    except (urllib.error.URLError, TimeoutError, crates.Unreadable, OSError, ValueError) as error:
        print(line(False, place, subject, f"couldn't be asked: {error}"))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
