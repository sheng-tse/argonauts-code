#!/usr/bin/env python3
"""Compares how often the newest version crashes with the version before it, from the usage reports, and opens an
issue when the newest one crashes more. .github/workflows/crash-watch.yml runs it once a day:

    python3 packaging/crashes/watch.py

It reads the last three full days of `daily_usage` events through PostHog's query API, with the project
POSTHOG_PROJECT_ID and POSTHOG_QUERY_KEY, a personal API key that may only run queries, at POSTHOG_API_HOST
(https://us.posthog.com unless set). Without the key or the project it says so and stops. The issue goes to
GH_REPO through gh, once a version: an open issue about the same version is left as it is.

Halting a rollout stays the maintainer's act, a newly signed manifest with halted: true, so the issue only says what
rose and where.
"""

import json
import os
import subprocess
import sys
import urllib.request

EVENT = "daily_usage"
DAYS = 3
# Fewer reports than this say more about who happened to report than about the version.
AT_LEAST = 20
# The newest version has risen when it crashes at least twice as often per report as the one before, and by at
# least one crash in twenty reports, so a rare crash going from one to two isn't news.
TIMES = 2.0
BY_AT_LEAST = 0.05
DEFAULT_HOST = "https://us.posthog.com"

QUERY = f"""
SELECT properties.version, toString(properties.health.crashes), count()
FROM events
WHERE event = '{EVENT}'
  AND timestamp >= toStartOfDay(now()) - toIntervalDay({DAYS})
  AND timestamp < toStartOfDay(now())
GROUP BY 1, 2
LIMIT 50000
"""


class Version:
    """One version's reports over the days compared: how many, and their crashes by place."""

    def __init__(self, name):
        self.name = name
        self.reports = 0
        self.places = {}

    @property
    def crashes(self):
        return sum(self.places.values())

    @property
    def per_report(self):
        return self.crashes / self.reports if self.reports else 0.0

    def add(self, crashes, reports):
        self.reports += reports
        for place, count in crashes.items():
            self.places[place] = self.places.get(place, 0) + count * reports


def version_key(version):
    """Orders versions as semver does: 0.1.0-beta.2 < 0.1.0-rc.1 < 0.1.0 < 0.1.1."""
    core, _, pre = version.partition("-")
    numbers = tuple(int(part) for part in core.split("."))
    if not pre:
        return numbers, (1,)
    parts = tuple((0, int(part), "") if part.isdigit() else (1, 0, part) for part in pre.split("."))
    return numbers, (0, parts)


def crash_map(value):
    """A report's crashes by place, which the query hands over as JSON text, or nothing for a report without any."""
    if isinstance(value, dict):
        found = value
    else:
        try:
            found = json.loads(value) if value else {}
        except ValueError:
            return {}
    if not isinstance(found, dict):
        return {}
    return {str(place): int(count) for place, count in found.items() if isinstance(count, int) and count > 0}


def versions_of(rows):
    """Each version's totals from the query's rows of version, crashes and how many reports said so. A version
    that isn't one, from a build of no release, is left out."""
    versions = {}
    for name, crashes, reports in rows:
        try:
            version_key(name or "")
        except ValueError:
            continue
        versions.setdefault(name, Version(name)).add(crash_map(crashes), int(reports))
    return versions


def newest_two(versions):
    """The newest version with enough reports, and the newest before it with enough, oldest last."""
    enough = sorted(
        (version for version in versions.values() if version.reports >= AT_LEAST),
        key=lambda version: version_key(version.name),
        reverse=True,
    )
    return enough[0] if enough else None, enough[1] if len(enough) > 1 else None


def rose(newest, before):
    """Whether the newest version crashes more than the one before it, by the margins above."""
    return newest.per_report >= before.per_report * TIMES and newest.per_report - before.per_report >= BY_AT_LEAST


def title_for(newest):
    return f"Crashes rose in {newest.name}"


def body_for(newest, before):
    def row(version):
        return f"| {version.name} | {version.reports} | {version.crashes} | {version.per_report:.3f} |"

    places = sorted(newest.places.items(), key=lambda item: (-item[1], item[0]))[:5]
    lines = [
        f"In the usage reports of the last {DAYS} full days, {newest.name} crashed {newest.per_report:.3f} times a "
        f"report, against {before.per_report:.3f} for {before.name}.",
        "",
        "| Version | Reports | Crashes | Per report |",
        "| --- | --- | --- | --- |",
        row(newest),
        row(before),
        "",
        "Where it crashed most:",
        "",
    ]
    lines += [f"- `{place}`: {count}" for place, count in places]
    lines += [
        "",
        "A halt is a newly signed manifest with `halted: true` (packaging/updates/draft_manifest.py --halted). This "
        "issue halts nothing by itself.",
    ]
    return "\n".join(lines)


def query_posthog(host, project, key):
    """The query's rows, as PostHog's query API returns them."""
    request = urllib.request.Request(
        f"{host.rstrip('/')}/api/projects/{project}/query/",
        data=json.dumps({"query": {"kind": "HogQLQuery", "query": QUERY}}).encode(),
        headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=60) as response:
        answer = json.load(response)
    rows = answer.get("results") or []
    if len(rows) >= 50000:
        print("::warning::the query hit its row limit, so some reports weren't counted")
    return rows


def open_issues_titled(title, repo):
    found = subprocess.run(
        ["gh", "issue", "list", "--repo", repo, "--state", "open", "--search", f'"{title}" in:title',
         "--json", "title"],
        check=True, capture_output=True, text=True,
    )
    return [issue["title"] for issue in json.loads(found.stdout or "[]")]


def open_issue(title, body, repo):
    subprocess.run(["gh", "issue", "create", "--repo", repo, "--title", title, "--body", body], check=True)


def watch(env, query=query_posthog, open_titled=open_issues_titled, create=open_issue):
    """What a day's run does. Returns what it decided, as one line."""
    key, project = env.get("POSTHOG_QUERY_KEY"), env.get("POSTHOG_PROJECT_ID")
    if not key or not project:
        return "No POSTHOG_QUERY_KEY or POSTHOG_PROJECT_ID, so there are no usage reports to compare."
    versions = versions_of(query(env.get("POSTHOG_API_HOST") or DEFAULT_HOST, project, key))
    newest, before = newest_two(versions)
    if newest is None or before is None:
        return f"Fewer than two versions have {AT_LEAST} reports in the last {DAYS} days: nothing to compare."
    said = (
        f"{newest.name}: {newest.per_report:.3f} crashes a report over {newest.reports} reports; "
        f"{before.name}: {before.per_report:.3f} over {before.reports}."
    )
    if not rose(newest, before):
        return said
    title = title_for(newest)
    repo = env["GH_REPO"]
    if title in open_titled(title, repo):
        return f"{said} An issue is already open."
    create(title, body_for(newest, before), repo)
    return f"{said} Opened an issue."


if __name__ == "__main__":
    print(watch(os.environ))
    sys.exit(0)
