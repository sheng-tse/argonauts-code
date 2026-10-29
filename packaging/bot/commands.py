#!/usr/bin/env python3
"""Answers a pull request comment that starts with /bot. .github/workflows/bot.yml runs it for each such comment:

    python3 -I packaging/bot/commands.py

`/bot rerun failed` reruns the failed jobs of the pull request's latest commit, and `/bot stop` stops the runs of
that commit still going. Maintainers, who may write to the repository, can do both. The pull request's author can
rerun its jobs, at most twice for each commit, and never a run that was stopped, so a maintainer's stop holds. Every
command gets a reply.

The comment comes from COMMENT_BODY and its writer from COMMENTER, the pull request from GH_REPO and PR_NUMBER.
BOT_LOGIN is the bot's own login: its earlier replies are where the reruns an author asked for are counted.
"""

import os
import pathlib
import sys
import urllib.parse

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from gh_api import Github, GithubError  # noqa: E402

AUTHOR_RERUNS = 2
# A run that failed or ran out of time. A maintainer's rerun takes a cancelled one too, finishing what was stopped;
# an author's leaves it, so it can't undo a maintainer's stop.
FAILED = {"failure", "timed_out"}
STOPPED = {"cancelled"}
HELP = "I know `/bot rerun failed` and `/bot stop`."


def command_of(body):
    """The words after /bot on the comment's first line, lowercased, or None when the comment isn't a command."""
    lines = (body or "").strip().splitlines()
    words = lines[0].lower().split() if lines else []
    if not words or words[0] != "/bot":
        return None
    return " ".join(words[1:])


def rerun_mark(sha):
    """The hidden line in a reply that records a rerun the author asked for."""
    return f"<!-- argonauts-bot: the author's rerun of {sha} -->"


def names(runs):
    """The runs' workflow names, sorted, as a sentence lists them: "CI", "CI and Packages", "CI, Labels and
    Packages"."""
    found = sorted({run["name"] for run in runs})
    return found[0] if len(found) == 1 else f"{', '.join(found[:-1])} and {found[-1]}"


def are(runs):
    return "is" if len({run["name"] for run in runs}) == 1 else "are"


def runs_of(api, repo, sha):
    """Every run the commit started as a pull request, newest first."""
    runs = api.pages(f"repos/{repo}/actions/runs?head_sha={sha}&event=pull_request&per_page=100", key="workflow_runs")
    return sorted(runs, key=lambda run: (run["created_at"], run["id"]), reverse=True)


def newest_of_each(runs):
    """The newest run of each workflow: an older run of one is behind a newer one and isn't worth rerunning."""
    seen, newest = set(), []
    for run in runs:
        if run["workflow_id"] not in seen:
            seen.add(run["workflow_id"])
            newest.append(run)
    return newest


def can_write(api, repo, login):
    try:
        found = api.get(f"repos/{repo}/collaborators/{urllib.parse.quote(login, safe='')}/permission")
    except GithubError:
        return False
    return (found or {}).get("permission") in ("admin", "write")


def reruns_asked_by_author(api, repo, number, sha, bot_login):
    mark = rerun_mark(sha)
    comments = api.pages(f"repos/{repo}/issues/{number}/comments?per_page=100")
    return sum(1 for comment in comments if comment["user"]["login"] == bot_login and mark in (comment["body"] or ""))


def rerun_failed(api, repo, sha, maintainer, earlier):
    """Reruns the failed jobs of the commit's newest runs. `earlier` counts the reruns its author asked for before,
    and is None when a maintainer asks."""
    short = sha[:7]
    if not maintainer and earlier >= AUTHOR_RERUNS:
        return (
            f"You've asked for {AUTHOR_RERUNS} reruns of `{short}`, the most its author can. A maintainer can rerun it "
            "again, and your next commit starts over."
        )
    runs = newest_of_each(runs_of(api, repo, sha))
    rerun = FAILED | STOPPED if maintainer else FAILED
    failed = [run for run in runs if run["status"] == "completed" and run["conclusion"] in rerun]
    if not failed:
        running = [run for run in runs if run["status"] != "completed"]
        if running:
            return f"Nothing has failed on `{short}`, and {names(running)} {are(running)} still running."
        return f"Nothing has failed on `{short}`."

    started, refused = [], []
    for run in failed:
        try:
            api.post(f"repos/{repo}/actions/runs/{run['id']}/rerun-failed-jobs")
            started.append(run)
        except GithubError as error:
            refused.append(f"Couldn't rerun {run['name']}: {error}")
    lines = []
    if started:
        said = f"Rerunning the failed jobs of {names(started)} on `{short}`."
        if not maintainer:
            left = AUTHOR_RERUNS - earlier - 1
            said += f" You can ask for {left} more on this commit." if left else " That was the last you can ask for."
        lines.append(said)
    lines += refused
    if started and not maintainer:
        lines.append(rerun_mark(sha))
    return "\n\n".join(lines)


def stop(api, repo, sha):
    """Stops every run of the commit that hasn't finished."""
    short = sha[:7]
    running = [run for run in runs_of(api, repo, sha) if run["status"] != "completed"]
    if not running:
        return f"Nothing is running on `{short}`."
    stopping, refused = [], []
    for run in running:
        try:
            api.post(f"repos/{repo}/actions/runs/{run['id']}/cancel")
            stopping.append(run)
        except GithubError as error:
            refused.append(f"Couldn't stop {run['name']}: {error}")
    lines = [f"Stopping {names(stopping)} on `{short}`."] if stopping else []
    return "\n\n".join(lines + refused)


def answer(env, api):
    """What one comment does. Returns the reply it posted, or None for a comment that isn't a command."""
    command = command_of(env.get("COMMENT_BODY"))
    if command is None:
        return None
    repo, number, who = env["GH_REPO"], int(env["PR_NUMBER"]), env["COMMENTER"]
    pull = api.get(f"repos/{repo}/pulls/{number}")
    sha = pull["head"]["sha"]
    maintainer = can_write(api, repo, who)

    if command == "rerun failed":
        if maintainer:
            reply = rerun_failed(api, repo, sha, maintainer=True, earlier=None)
        elif who == pull["user"]["login"]:
            earlier = reruns_asked_by_author(api, repo, number, sha, env["BOT_LOGIN"])
            reply = rerun_failed(api, repo, sha, maintainer=False, earlier=earlier)
        else:
            reply = "Only maintainers and this pull request's author can rerun its jobs."
    elif command == "stop":
        reply = stop(api, repo, sha) if maintainer else "Only maintainers can stop runs."
    else:
        reply = HELP

    api.post(f"repos/{repo}/issues/{number}/comments", {"body": reply})
    return reply


if __name__ == "__main__":
    print(answer(os.environ, Github()) or "Not a /bot command, so there's nothing to answer.")
