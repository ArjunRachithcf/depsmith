"""Track the native tools depsmith drives against their releases.

A tool's "latest" is its newest stable release published at least
--cooldown-days (default 7) ago, so brand-new releases settle first.

    python scripts/tool-drift.py pick DOCTOR_JSON
    python scripts/tool-drift.py report DOCTOR_JSON RESULTS_DIR [--act]

`pick` prints, as JSON for the Latest tools matrix, each runnable tool whose
eligible release differs from its tested version, with that version and tag.
`report` reads `depsmith doctor --json` (tested versions and pinned
downloads) and RESULTS_DIR (one `<tool>-<os>` file per Integration job holding
"<status> <version>"); only runs of the eligible release count. It prints a
Markdown summary. With --act (run from a checkout of main, GH_TOKEN a GitHub
App token so pull requests trigger CI, ISSUES_TOKEN for issues), a tool whose
eligible release passes Integration gets a pull request that bumps its tested
version, pinned downloads and Integration pin together; a tool broken by it,
a pinned download whose sha256 changed, or a bump that cannot be made gets
one open issue labelled `latest-tools`. Resolved issues close.
"""

import datetime
import json
import os
import pathlib
import re
import subprocess
import sys
from dataclasses import dataclass

LABEL = "latest-tools"
COOLDOWN_DAYS = 7
# Tools Integration can run at a chosen version (cargo is always stable Rust).
RUNNABLE = ("pixi", "uv", "grype", "conda-lock", "conda")

# Upstream release repository of each tool (by depsmith tool name).
REPOS = {
    "uv": "astral-sh/uv",
    "pixi": "prefix-dev/pixi",
    "grype": "anchore/grype",
    "conda-lock": "conda/conda-lock",
    "conda": "mamba-org/micromamba-releases",
    "cargo": "rust-lang/rust",
}


# Where each tool's tested version, pinned downloads and Integration pin live.
SPEC_FILES = {
    "uv": "crates/core/src/uv.rs",
    "pixi": "crates/core/src/pixi.rs",
    "grype": "crates/core/src/scan.rs",
    "conda-lock": "crates/core/src/conda.rs",
    "conda": "crates/core/src/conda.rs",
    "cargo": "crates/core/src/cargo.rs",
}
CATALOG = "crates/core/src/provision.rs"
CATALOG_CONSTS = {"uv": "UV", "pixi": "PIXI", "grype": "GRYPE", "conda": "MICROMAMBA"}
WORKFLOW = ".github/workflows/integration.yml"
PINS = {
    "uv": "UV_VERSION",
    "pixi": "PIXI_VERSION",
    "grype": "GRYPE_VERSION",
    "conda-lock": "CONDA_LOCK_PIN",
    "conda": "MICROMAMBA_PIN",
}


@dataclass(frozen=True)
class Action:
    """A pull request (`bump`) or an issue (any other kind) for one tool."""

    tool: str
    kind: str
    title: str
    body: str
    version: str = ""


class BumpError(Exception):
    pass


def version_of(tool, tag):
    """The version a release tag names (`v0.81.0`, micromamba's `2.10.0-1`)."""
    version = tag.removeprefix("v")
    if tool == "conda":
        version = version.rsplit("-", 1)[0]
    return version


def parse_time(text):
    """An aware datetime from GitHub's `2026-10-01T00:00:00Z`."""
    return datetime.datetime.fromisoformat(text.replace("Z", "+00:00"))


def eligible(releases, now, days=COOLDOWN_DAYS):
    """The tag of the newest stable release published at least `days` before
    `now`, or None."""
    cutoff = now - datetime.timedelta(days=days)
    candidates = [
        (parse_time(r["published_at"]), r["tag_name"])
        for r in releases
        if not r.get("draft") and not r.get("prerelease") and r.get("published_at")
    ]
    settled = [c for c in candidates if c[0] <= cutoff]
    return max(settled)[1] if settled else None


def pick(tested, tags):
    """Runnable tools whose eligible release (by tag) is not the tested one."""
    return [
        {"tool": tool, "version": version_of(tool, tags[tool]), "tag": tags[tool]}
        for tool in RUNNABLE
        if tool in tested
        and tags.get(tool)
        and version_of(tool, tags[tool]) != tested[tool]
    ]


def tested_versions(doctor):
    """The first tested version of each tool in a doctor report."""
    return {
        tool["tool"]: tool["tested_versions"][0]
        for tool in doctor["tools"]
        if tool.get("tested_versions")
    }


def pinned_assets(doctor):
    """Pinned GitHub release assets per tool as (repo, tag, asset, sha256, url)."""
    assets = {}
    for tool in doctor["tools"]:
        for download in tool.get("downloads") or []:
            url = download["url"]
            parts = url.split("/")
            # https://github.com/OWNER/REPO/releases/download/TAG/ASSET
            if parts[2] != "github.com" or parts[5:7] != ["releases", "download"]:
                continue
            repo = f"{parts[3]}/{parts[4]}"
            assets.setdefault(tool["tool"], []).append(
                (repo, parts[7], parts[8], download["sha256"], url)
            )
    return assets


def decide(tested, latest, results, mismatched):
    """At most one issue per tool: checksum drift, else broken, else behind."""
    issues = []
    for tool, version in sorted(tested.items()):
        newest = latest.get(tool)
        result = results.get(tool)
        bad = mismatched.get(tool) or []
        if bad:
            issues.append(
                Action(
                    tool,
                    "checksum",
                    f"{tool}: pinned download no longer matches its sha256",
                    "These pinned assets no longer match the sha256 depsmith "
                    "verifies, so `depsmith init` would refuse them:\n"
                    + "\n".join(f"- {url}" for url in bad),
                )
            )
        elif result == "failure":
            issues.append(
                Action(
                    tool,
                    "broken",
                    f"{tool}: integration fails with {tool} {newest}",
                    f"Integration with {tool} {newest} (other tools pinned) "
                    f"fails; depsmith is tested with {version}.",
                )
            )
        elif newest and newest != version and result == "success":
            issues.append(
                Action(
                    tool,
                    "bump",
                    f"Bump {tool} to {newest}",
                    f"Integration passes with {tool} {newest} (other tools pinned) "
                    f"in the weekly Latest tools run; depsmith was tested with "
                    f"{version}. This moves the adapter's `tested_versions`, the "
                    f"pinned downloads (with the release's sha256 digests) and the "
                    f"Integration pin together.",
                    newest,
                )
            )
        elif newest and newest != version:
            issues.append(
                Action(
                    tool,
                    "behind",
                    f"{tool}: tested {version}, {newest} released",
                    f"Integration was not run against {tool} {newest} on its own, "
                    f"so it is not bumped automatically. Bump `tested_versions` "
                    f"(and any pinned download and Integration pin) from {version} "
                    f"after checking it.",
                )
            )
    return issues


def bump(root, tool, old, new, new_tag, digest_of):
    """Move `tool` from `old` to `new` in the spec, the pinned downloads (from
    release `new_tag`, digests from `digest_of(repo, tag, asset)`) and the
    Integration pin. Raises BumpError, changing nothing, when an asset is
    missing or a file does not hold the expected version."""
    root = pathlib.Path(root)
    edits = {}

    def text(path):
        return edits.get(path, (root / path).read_text())

    spec = text(SPEC_FILES[tool])
    pattern = f'tested_versions: vec!["{old}".into()]'
    if spec.count(pattern) != 1:
        raise BumpError(f"{SPEC_FILES[tool]} does not test {tool} {old} exactly once")
    edits[SPEC_FILES[tool]] = spec.replace(pattern, pattern.replace(old, new))

    if tool in CATALOG_CONSTS:
        catalog = text(CATALOG)
        start = catalog.index(f"const {CATALOG_CONSTS[tool]}: &[Asset] = &[")
        end = catalog.index("];", start)
        block = catalog[start:end]

        def replace(match):
            parts = match.group(1).split("/")
            repo, asset = f"{parts[3]}/{parts[4]}", parts[8].replace(old, new)
            digest = digest_of(repo, new_tag, asset)
            if not digest:
                raise BumpError(f"{repo} {new_tag} has no asset {asset}")
            url = "/".join(parts[:7] + [new_tag, asset])
            return f'"{url}", "{digest}"'

        block = re.sub(r'"(https://github\.com/[^"]+)", "[0-9a-f]+"', replace, block)
        edits[CATALOG] = catalog[:start] + block + catalog[end:]

    if tool in PINS:
        lines = text(WORKFLOW).splitlines(keepends=True)
        found = False
        for index, line in enumerate(lines):
            if line.strip().startswith(f"{PINS[tool]}:") and old in line:
                lines[index] = line.replace(old, new)
                found = True
        if not found:
            raise BumpError(f"{WORKFLOW} does not pin {tool} {old}")
        edits[WORKFLOW] = "".join(lines)

    for path, content in edits.items():
        (root / path).write_text(content)


def summary(tested, latest, results):
    """A Markdown table of every tool's tested and latest version and result."""
    lines = [
        "| Tool | Tested | Latest (past cooldown) | Integration |",
        "|---|---|---|---|",
    ]
    for tool, version in sorted(tested.items()):
        lines.append(
            f"| {tool} | {version} | {latest.get(tool, '?')} | "
            f"{results.get(tool, 'not run')} |"
        )
    return "\n".join(lines)


def gh(*args, token=None):
    """Run gh; `token` replaces GH_TOKEN (issues use the workflow's token)."""
    env = dict(os.environ, GH_TOKEN=token) if token else None
    return subprocess.run(
        ["gh", *args], check=True, capture_output=True, text=True, env=env
    ).stdout


def eligible_tags(tools, days):
    """Each tool's eligible release tag (see `eligible`)."""
    now = datetime.datetime.now(datetime.timezone.utc)
    tags = {}
    for tool in tools:
        repo = REPOS.get(tool)
        if repo:
            releases = json.loads(gh("api", f"repos/{repo}/releases?per_page=50"))
            tags[tool] = eligible(releases, now, days)
    return {tool: tag for tool, tag in tags.items() if tag}


def asset_digest(repo, tag, name):
    """The sha256 GitHub records for a release asset, or None."""
    try:
        digest = gh(
            "api",
            f"repos/{repo}/releases/tags/{tag}",
            "--jq",
            f'.assets[] | select(.name == "{name}") | .digest',
        ).strip()
    except subprocess.CalledProcessError:
        return None
    return digest.removeprefix("sha256:") or None


def mismatched_assets(assets):
    mismatched = {}
    for tool, rows in assets.items():
        for repo, tag, name, sha256, url in rows:
            digest = gh(
                "api",
                f"repos/{repo}/releases/tags/{tag}",
                "--jq",
                f'.assets[] | select(.name == "{name}") | .digest',
            ).strip()
            if digest != f"sha256:{sha256}":
                mismatched.setdefault(tool, []).append(url)
    return mismatched


def read_results(directory):
    """Each tool's Integration (status, version): failure if any OS failed."""
    results = {}
    for path in sorted(pathlib.Path(directory).glob("*")):
        tool = path.name.rsplit("-", 1)[0]
        status, _, version = path.read_text().strip().partition(" ")
        status = "success" if status == "success" else "failure"
        if results.get(tool, ("",))[0] != "failure":
            results[tool] = (status, version)
    return results


def judged(results, latest):
    """Statuses of the runs that tested each tool's eligible release. Every
    run uses stable Rust: cargo is judged only when all runs agree, since one
    tool's failure says nothing about cargo."""
    statuses = {
        tool: status
        for tool, (status, version) in results.items()
        if latest.get(tool) == version
    }
    outcomes = {status for status, _ in results.values()}
    if len(outcomes) == 1:
        statuses["cargo"] = outcomes.pop()
    return statuses


def open_bump(action, old, tag):
    """Bump `action.tool` on branch latest-tools/<tool> from main and open or
    retitle its pull request. Raises BumpError when the bump cannot be made."""
    repository = gh(
        "repo", "view", "--json", "nameWithOwner", "--jq", ".nameWithOwner"
    ).strip()
    branch = f"latest-tools/{action.tool}"
    base = subprocess.run(
        ["git", "rev-parse", "HEAD"], check=True, capture_output=True, text=True
    ).stdout.strip()
    try:
        bump(".", action.tool, old, action.version, tag, asset_digest)
        try:
            gh(
                "api",
                "-X",
                "PATCH",
                f"repos/{repository}/git/refs/heads/{branch}",
                "-f",
                f"sha={base}",
                "-F",
                "force=true",
            )
        except subprocess.CalledProcessError:
            gh(
                "api",
                f"repos/{repository}/git/refs",
                "-f",
                f"ref=refs/heads/{branch}",
                "-f",
                f"sha={base}",
            )
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


def act(actions, tested, tags):
    """Open bump pull requests, then keep one `latest-tools` issue per tool."""
    issues = []
    for action in actions:
        if action.kind != "bump":
            issues.append(action)
            continue
        try:
            open_bump(action, tested[action.tool], tags[action.tool])
        except (BumpError, subprocess.CalledProcessError) as error:
            issues.append(
                Action(
                    action.tool,
                    "behind",
                    f"{action.tool}: {action.version} passes but cannot be bumped automatically",
                    f"{action.body}\n\nThe automatic bump failed: {error}",
                )
            )
    sync_issues(issues, tested)


def sync_issues(issues, tools):
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


def main(argv):
    days = COOLDOWN_DAYS
    if "--cooldown-days" in argv:
        days = int(argv[argv.index("--cooldown-days") + 1])
    command, doctor = argv[1], json.loads(pathlib.Path(argv[2]).read_text())
    tested = tested_versions(doctor)
    tags = eligible_tags(tested, days)
    if command == "pick":
        print(json.dumps(pick(tested, tags)))
        return 0
    latest = {tool: version_of(tool, tag) for tool, tag in tags.items()}
    results = judged(read_results(argv[3]), latest)
    actions = decide(tested, latest, results, mismatched_assets(pinned_assets(doctor)))
    print(f"Releases count once published at least {days} days ago.\n")
    print(summary(tested, latest, results))
    for action in actions:
        print(f"\n- **{action.kind}**: {action.title}")
    if "--act" in argv:
        act(actions, tested, tags)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
