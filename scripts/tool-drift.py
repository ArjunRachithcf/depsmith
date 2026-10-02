"""Track the native tools depsmith drives against their releases.

A tool's candidate release is its highest-versioned stable release published
at least --cooldown-days (default 7) ago and newer than the version depsmith
is tested with, so brand-new releases settle first and nothing moves back.

    python scripts/tool-drift.py pick DOCTOR_JSON
    python scripts/tool-drift.py report DOCTOR_JSON RESULTS_DIR PICKED_JSON [--act]

`pick` prints, as JSON for the Latest tools matrix, each runnable tool's
candidate release ({tool, version, tag}); conda-forge packages must already
carry it, since Integration installs them from there. `report` reads
`depsmith doctor --json` (tested versions and pinned downloads), RESULTS_DIR
(one `<tool>-<os>` file per Integration job holding "<status> <version>") and
the picked JSON, and prints a Markdown summary. With --act (run from a
checkout of main; GH_TOKEN a GitHub App token, so pull requests trigger CI;
ISSUES_TOKEN for issues), a tool whose candidate passed Integration on every
OS gets a pull request moving its tested version, pinned downloads and
`.github/tool-versions.json` pin together. A candidate that breaks
Integration, a pinned download that is gone or whose sha256 changed, a bump
that cannot be made, or a newer release that was not run (cargo, which CI
runs as stable Rust) gets one open issue labelled `latest-tools`. Resolved
issues close.
"""

from __future__ import annotations

import datetime
import json
import os
import pathlib
import re
import subprocess
import sys
import urllib.request
from collections.abc import Callable, Iterable
from dataclasses import dataclass

LABEL = "latest-tools"
COOLDOWN_DAYS = 7
CATALOG = "crates/core/src/provision.rs"
PINS = ".github/tool-versions.json"

# Action kinds: a pull request, or an issue.
BUMP, BROKEN, BEHIND, CHECKSUM = "bump", "broken", "behind", "checksum"


@dataclass(frozen=True)
class Tool:
    """Where a native tool is released and where depsmith pins it."""

    repo: str
    """GitHub repository publishing its releases."""
    spec: str
    """Rust source holding its `tested_versions`."""
    catalog: str | None
    """Its constant of pinned downloads in `provision.rs`, if any."""
    runnable: bool
    """Whether Integration can run it at a chosen version (pinned in PINS)."""
    conda_forge: str | None = None
    """Its conda-forge package, when Integration installs it from there."""


TOOLS = {
    "uv": Tool("astral-sh/uv", "crates/core/src/uv.rs", "UV", True),
    "pixi": Tool("prefix-dev/pixi", "crates/core/src/pixi.rs", "PIXI", True),
    "grype": Tool("anchore/grype", "crates/core/src/scan.rs", "GRYPE", True),
    "conda-lock": Tool(
        "conda/conda-lock", "crates/core/src/conda.rs", None, True, "conda-lock"
    ),
    # The conda adapter's solver, `conda`, is micromamba.
    "conda": Tool(
        "mamba-org/micromamba-releases",
        "crates/core/src/conda.rs",
        "MICROMAMBA",
        True,
        "micromamba",
    ),
    "cargo": Tool("rust-lang/rust", "crates/core/src/cargo.rs", None, False),
}


@dataclass(frozen=True)
class Action:
    """A pull request (`BUMP`) or an issue (any other kind) for one tool."""

    tool: str
    kind: str
    title: str
    body: str
    version: str = ""


class BumpError(Exception):
    """A bump that cannot be made; nothing was changed."""


def version_of(tool: str, tag: str) -> str:
    """The version a release tag names (`v0.81.0`, micromamba's `2.10.0-1`)."""
    version = tag.removeprefix("v")
    if tool == "conda":
        version = version.rsplit("-", 1)[0]
    return version


def version_key(version: str) -> tuple[int, ...]:
    """A sort key for dotted numeric versions."""
    return tuple(int(part) for part in re.findall(r"\d+", version))


def newer(candidate: str, tested: str) -> bool:
    return version_key(candidate) > version_key(tested)


def parse_time(text: str) -> datetime.datetime:
    """An aware datetime from GitHub's `2026-10-01T00:00:00Z`."""
    return datetime.datetime.fromisoformat(text.replace("Z", "+00:00"))


def eligible(
    tool: str,
    releases: Iterable[dict],
    now: datetime.datetime,
    days: int = COOLDOWN_DAYS,
) -> str | None:
    """The tag of the highest-versioned stable release published at least
    `days` before `now`, or None."""
    cutoff = now - datetime.timedelta(days=days)
    settled = [
        r["tag_name"]
        for r in releases
        if not r.get("draft")
        and not r.get("prerelease")
        and r.get("published_at")
        and parse_time(r["published_at"]) <= cutoff
    ]
    return max(
        settled, key=lambda tag: version_key(version_of(tool, tag)), default=None
    )


def tested_versions(doctor: dict) -> dict[str, str]:
    """The pinned (first tested) version of each tool in a doctor report."""
    return {
        tool["tool"]: tool["tested_versions"][0]
        for tool in doctor["tools"]
        if tool.get("tested_versions")
    }


def candidates(tested: dict[str, str], tags: dict[str, str]) -> dict[str, str]:
    """Tags of the eligible releases newer than the tested versions."""
    return {
        tool: tag
        for tool, tag in tags.items()
        if tool in tested and newer(version_of(tool, tag), tested[tool])
    }


def pick(
    tested: dict[str, str],
    tags: dict[str, str],
    available: Callable[[str, str], bool] = lambda tool, version: True,
) -> list[dict[str, str]]:
    """The runnable tools' candidate releases that can be installed."""
    picked = []
    for tool, tag in candidates(tested, tags).items():
        version = version_of(tool, tag)
        if TOOLS[tool].runnable and available(tool, version):
            picked.append({"tool": tool, "version": version, "tag": tag})
    return sorted(picked, key=lambda p: p["tool"])


def release_asset(url: str) -> tuple[str, str, str] | None:
    """(repo, tag, asset) of a GitHub release download URL."""
    parts = url.split("/")
    # https://github.com/OWNER/REPO/releases/download/TAG/ASSET
    if (
        len(parts) != 9
        or parts[2] != "github.com"
        or parts[5:7] != ["releases", "download"]
    ):
        return None
    return f"{parts[3]}/{parts[4]}", parts[7], parts[8]


def pinned_assets(doctor: dict) -> dict[str, list[tuple[str, str]]]:
    """Pinned GitHub release downloads per tool as (url, sha256)."""
    assets: dict[str, list[tuple[str, str]]] = {}
    for tool in doctor["tools"]:
        for download in tool.get("downloads") or []:
            if release_asset(download["url"]):
                assets.setdefault(tool["tool"], []).append(
                    (download["url"], download["sha256"])
                )
    return assets


def decide(
    tested: dict[str, str],
    latest: dict[str, str],
    results: dict[str, str],
    mismatched: dict[str, list[str]],
) -> list[Action]:
    """At most one action per tool: checksum drift, else a broken candidate,
    else a bump (candidate passed), else a candidate that was not run.
    `latest` holds candidate versions only; `results` the outcome of runs at
    exactly those versions."""
    actions = []
    for tool, version in sorted(tested.items()):
        newest = latest.get(tool)
        result = results.get(tool)
        bad = mismatched.get(tool) or []
        if bad:
            actions.append(
                Action(
                    tool,
                    CHECKSUM,
                    f"{tool}: pinned download no longer matches its sha256",
                    "These pinned assets are gone or no longer match the sha256 "
                    "depsmith verifies, so `depsmith init` would refuse them:\n"
                    + "\n".join(f"- {url}" for url in bad),
                )
            )
        elif not newest:
            continue
        elif result == "failure":
            actions.append(
                Action(
                    tool,
                    BROKEN,
                    f"{tool}: integration fails with {tool} {newest}",
                    f"Integration with {tool} {newest} (other tools pinned) "
                    f"fails; depsmith is tested with {version}.",
                )
            )
        elif result == "success":
            actions.append(
                Action(
                    tool,
                    BUMP,
                    f"Bump {tool} to {newest}",
                    f"Integration passes with {tool} {newest} (other tools pinned) "
                    f"in the weekly Latest tools run, and the release is at least "
                    f"{COOLDOWN_DAYS} days old; depsmith was tested with {version}. "
                    f"This moves the adapter's `tested_versions`, the pinned "
                    f"downloads (with the release's sha256 digests) and the "
                    f"`{PINS}` pin together.",
                    newest,
                )
            )
        else:
            actions.append(
                Action(
                    tool,
                    BEHIND,
                    f"{tool}: tested {version}, {newest} released",
                    f"Integration was not run against {tool} {newest} on its own "
                    f"(CI runs cargo as stable Rust; conda-forge may not carry it "
                    f"yet), so it is not bumped automatically. Bump "
                    f"`tested_versions` (and any pinned download and `{PINS}` pin) "
                    f"from {version} after checking it.",
                )
            )
    return actions


def bump(
    root: str | pathlib.Path,
    tool: str,
    old: str,
    new: str,
    new_tag: str,
    digest_of: Callable[[str, str, str], str | None],
) -> None:
    """Move `tool` from `old` to `new` in its spec, its pinned downloads (from
    release `new_tag`, digests from `digest_of(repo, tag, asset)`) and the
    PINS file. Raises BumpError, changing nothing, when an asset or digest is
    missing or a file does not hold the expected version."""
    root = pathlib.Path(root)
    info = TOOLS[tool]
    edits: dict[str, str] = {}

    def text(path: str) -> str:
        return edits.get(path, (root / path).read_text())

    spec = text(info.spec)
    pattern = f'tested_versions: vec!["{old}".into()]'
    if spec.count(pattern) != 1:
        raise BumpError(f"{info.spec} does not test {tool} {old} exactly once")
    edits[info.spec] = spec.replace(pattern, pattern.replace(old, new))

    if info.catalog:
        catalog = text(CATALOG)
        start = catalog.index(f"const {info.catalog}: &[Asset] = &[")
        end = catalog.index("];", start)

        def replace(match: re.Match) -> str:
            repo, _, asset = release_asset(match.group(1))
            asset = asset.replace(old, new)
            digest = digest_of(repo, new_tag, asset)
            if not digest:
                raise BumpError(f"{repo} {new_tag} has no asset {asset} with a digest")
            url = f"https://github.com/{repo}/releases/download/{new_tag}/{asset}"
            return f'"{url}", "{digest}"'

        block = re.sub(
            r'"(https://github\.com/[^"]+)", "[0-9a-f]+"', replace, catalog[start:end]
        )
        edits[CATALOG] = catalog[:start] + block + catalog[end:]

    if info.runnable:
        pins = json.loads(text(PINS))
        if pins.get(tool) != old:
            raise BumpError(f"{PINS} does not pin {tool} {old}")
        pins[tool] = new
        edits[PINS] = json.dumps(pins, indent=2) + "\n"

    for path, content in edits.items():
        (root / path).write_text(content)


def summary(
    tested: dict[str, str], latest: dict[str, str], results: dict[str, str]
) -> str:
    """A Markdown table of every tool's tested and candidate version and result."""
    lines = [
        "| Tool | Tested | Candidate (past cooldown) | Integration |",
        "|---|---|---|---|",
    ]
    for tool, version in sorted(tested.items()):
        lines.append(
            f"| {tool} | {version} | {latest.get(tool, '—')} | "
            f"{results.get(tool, 'not run')} |"
        )
    return "\n".join(lines)


def read_results(directory: str | pathlib.Path) -> dict[str, tuple[str, str]]:
    """Each tool's Integration (status, version): failure if any OS failed."""
    results: dict[str, tuple[str, str]] = {}
    for path in sorted(pathlib.Path(directory).glob("*")):
        tool = path.name.rsplit("-", 1)[0]
        status, _, version = path.read_text().strip().partition(" ")
        status = "success" if status == "success" else "failure"
        if results.get(tool, ("",))[0] != "failure":
            results[tool] = (status, version)
    return results


def judged(
    results: dict[str, tuple[str, str]], latest: dict[str, str]
) -> dict[str, str]:
    """Statuses of the runs that tested each tool's candidate release."""
    return {
        tool: status
        for tool, (status, version) in results.items()
        if latest.get(tool) == version
    }


def gh(*args: str, token: str | None = None) -> str:
    """Run gh; `token` replaces GH_TOKEN (issues use the workflow's token)."""
    env = dict(os.environ, GH_TOKEN=token) if token else None
    return subprocess.run(
        ["gh", *args], check=True, capture_output=True, text=True, env=env
    ).stdout


def release_digests(repo: str, tag: str) -> dict[str, str | None]:
    """Each asset of a release with its sha256 (None when GitHub has none).
    Raises CalledProcessError when the release does not exist."""
    assets = json.loads(
        gh(
            "api",
            f"repos/{repo}/releases/tags/{tag}",
            "--jq",
            "[.assets[] | {name, digest}]",
        )
    )
    return {
        a["name"]: (a["digest"] or "").removeprefix("sha256:") or None for a in assets
    }


def asset_digest(repo: str, tag: str, name: str) -> str | None:
    try:
        return release_digests(repo, tag).get(name)
    except subprocess.CalledProcessError:
        return None


def mismatched_assets(
    assets: dict[str, list[tuple[str, str]]],
) -> dict[str, list[str]]:
    """Pinned downloads that are gone or whose recorded sha256 differs. An
    asset without a recorded digest cannot be checked and is skipped."""
    mismatched: dict[str, list[str]] = {}
    for tool, rows in assets.items():
        for url, sha256 in rows:
            repo, tag, name = release_asset(url)
            try:
                digests = release_digests(repo, tag)
            except subprocess.CalledProcessError:
                digests = {}
            if name not in digests or digests[name] not in (None, sha256):
                mismatched.setdefault(tool, []).append(url)
    return mismatched


def eligible_tags(tools: Iterable[str], days: int) -> dict[str, str]:
    """Each tool's eligible release tag (see `eligible`)."""
    now = datetime.datetime.now(datetime.timezone.utc)
    tags = {}
    for tool in tools:
        if tool in TOOLS:
            releases = json.loads(
                gh("api", f"repos/{TOOLS[tool].repo}/releases?per_page=50")
            )
            tag = eligible(tool, releases, now, days)
            if tag:
                tags[tool] = tag
    return tags


def installable(tool: str, version: str) -> bool:
    """Whether Integration can install `tool` `version`: conda-forge must
    carry it when it comes from there; GitHub releases always do."""
    package = TOOLS[tool].conda_forge
    if not package:
        return True
    url = f"https://api.anaconda.org/package/conda-forge/{package}"
    with urllib.request.urlopen(url, timeout=60) as response:
        return version in json.load(response).get("versions", [])


def set_branch(repository: str, branch: str, sha: str | None) -> None:
    """Point `branch` at `sha` (creating it), or delete it when `sha` is None."""
    ref = f"repos/{repository}/git/refs/heads/{branch}"
    if sha is None:
        gh("api", "-X", "DELETE", ref)
        return
    try:
        gh("api", "-X", "PATCH", ref, "-f", f"sha={sha}", "-F", "force=true")
    except subprocess.CalledProcessError:
        gh(
            "api",
            f"repos/{repository}/git/refs",
            "-f",
            f"ref=refs/heads/{branch}",
            "-f",
            f"sha={sha}",
        )


def open_bump(action: Action, old: str, tag: str) -> None:
    """Bump `action.tool` on branch latest-tools/<tool> from main and open or
    update its pull request. On failure the branch is restored and the
    checkout is left unchanged."""
    repository = gh(
        "repo", "view", "--json", "nameWithOwner", "--jq", ".nameWithOwner"
    ).strip()
    branch = f"latest-tools/{action.tool}"
    base = subprocess.run(
        ["git", "rev-parse", "HEAD"], check=True, capture_output=True, text=True
    ).stdout.strip()
    try:
        previous = gh(
            "api", f"repos/{repository}/git/ref/heads/{branch}", "--jq", ".object.sha"
        ).strip()
    except subprocess.CalledProcessError:
        previous = None
    try:
        bump(".", action.tool, old, action.version, tag, asset_digest)
        set_branch(repository, branch, base)
        try:
            subprocess.run(
                [
                    sys.executable,
                    "scripts/commit-via-api.py",
                    repository,
                    branch,
                    base,
                    action.title,
                ],
                check=True,
            )
        except subprocess.CalledProcessError:
            # Never leave a pull request pointing at main with no change.
            set_branch(repository, branch, previous)
            raise
    finally:
        subprocess.run(["git", "checkout", "--", "."], check=True)
    number = gh(
        "pr",
        "list",
        "--head",
        branch,
        "--state",
        "open",
        "--json",
        "number",
        "--jq",
        ".[0].number // empty",
    ).strip()
    if number:
        gh("pr", "edit", number, "--title", action.title, "--body", action.body)
    else:
        gh(
            "pr",
            "create",
            "--head",
            branch,
            "--base",
            "main",
            "--title",
            action.title,
            "--body",
            action.body,
        )


def act(actions: list[Action], tested: dict[str, str], tags: dict[str, str]) -> None:
    """Open bump pull requests, then keep one `latest-tools` issue per tool."""
    issues = []
    for action in actions:
        if action.kind != BUMP:
            issues.append(action)
            continue
        try:
            open_bump(action, tested[action.tool], tags[action.tool])
        except (BumpError, subprocess.CalledProcessError) as error:
            issues.append(
                Action(
                    action.tool,
                    BEHIND,
                    f"{action.tool}: {action.version} passes but cannot be "
                    "bumped automatically",
                    f"{action.body}\n\nThe automatic bump failed: {error}",
                )
            )
    sync_issues(issues, tested)


def sync_issues(issues: list[Action], tools: Iterable[str]) -> None:
    """Keep exactly the wanted `latest-tools` issue open for each tool."""
    token = os.environ.get("ISSUES_TOKEN")
    gh(
        "label",
        "create",
        LABEL,
        "--color",
        "FBCA04",
        "--description",
        "Native tool releases ahead of or breaking depsmith",
        "--force",
        token=token,
    )
    open_issues = json.loads(
        gh(
            "issue",
            "list",
            "--label",
            LABEL,
            "--state",
            "open",
            "--json",
            "number,title",
            token=token,
        )
    )
    wanted = {issue.tool: issue for issue in issues}
    for tool in tools:
        existing = [i for i in open_issues if i["title"].startswith(f"{tool}: ")]
        issue = wanted.get(tool)
        for old in existing:
            if issue is None or old["title"] != issue.title:
                gh(
                    "issue",
                    "close",
                    str(old["number"]),
                    "--comment",
                    "Resolved or superseded by the latest run.",
                    token=token,
                )
        if issue and not any(old["title"] == issue.title for old in existing):
            gh(
                "issue",
                "create",
                "--label",
                LABEL,
                "--title",
                issue.title,
                "--body",
                issue.body,
                token=token,
            )


def main(argv: list[str]) -> int:
    args = [a for a in argv[1:] if not a.startswith("--")]
    days = COOLDOWN_DAYS
    if "--cooldown-days" in argv:
        value = argv[argv.index("--cooldown-days") + 1]
        days = int(value)
        args.remove(value)
    if not args or (args[0], len(args)) not in (("pick", 2), ("report", 4)):
        print(__doc__, file=sys.stderr)
        return 2
    doctor = json.loads(pathlib.Path(args[1]).read_text())
    tested = tested_versions(doctor)
    if args[0] == "pick":
        tags = candidates(tested, eligible_tags(tested, days))
        print(json.dumps(pick(tested, tags, installable)))
        return 0
    picked = json.loads(pathlib.Path(args[3]).read_text())
    # Runnable tools keep the release `pick` chose; the others are looked up.
    others = [t for t in tested if t in TOOLS and not TOOLS[t].runnable]
    tags = {
        **candidates(tested, eligible_tags(others, days)),
        **{p["tool"]: p["tag"] for p in picked},
    }
    latest = {tool: version_of(tool, tag) for tool, tag in tags.items()}
    results = judged(read_results(args[2]), latest)
    actions = decide(tested, latest, results, mismatched_assets(pinned_assets(doctor)))
    print(f"Candidates are releases at least {days} days old.\n")
    print(summary(tested, latest, results))
    for action in actions:
        print(f"\n- **{action.kind}**: {action.title}")
    if "--act" in argv:
        act(actions, tested, tags)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
