"""Report how the native tools depsmith drives drift from their latest releases.

Run by the Latest tools workflow after Integration ran once per tool with
only that tool at its latest release:

    python scripts/tool-drift.py DOCTOR_JSON RESULTS_DIR [--issues]

DOCTOR_JSON is `depsmith doctor --json` (tested versions and pinned
downloads); RESULTS_DIR holds one `<tool>-<os>` file per Integration job with
its status. Latest releases and asset digests come from the GitHub API via
`gh`. A Markdown summary goes to stdout; with --issues, each tool gets at most
one open issue labelled `latest-tools` (broken, behind, or checksum drift),
and resolved ones are closed.
"""

import json
import pathlib
import subprocess
import sys
from dataclasses import dataclass

LABEL = "latest-tools"

# Upstream release repository of each tool (by depsmith tool name).
REPOS = {
    "uv": "astral-sh/uv",
    "pixi": "prefix-dev/pixi",
    "grype": "anchore/grype",
    "conda-lock": "conda/conda-lock",
    "conda": "mamba-org/micromamba-releases",
    "cargo": "rust-lang/rust",
}


@dataclass(frozen=True)
class Issue:
    tool: str
    kind: str
    title: str
    body: str


def version_of(tool, tag):
    """The version a release tag names (`v0.81.0`, micromamba's `2.10.0-1`)."""
    version = tag.removeprefix("v")
    if tool == "conda":
        version = version.rsplit("-", 1)[0]
    return version


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
                Issue(
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
                Issue(
                    tool,
                    "broken",
                    f"{tool}: integration fails with {tool} {newest}",
                    f"Integration with {tool} {newest} (other tools pinned) "
                    f"fails; depsmith is tested with {version}.",
                )
            )
        elif newest and newest != version:
            ran = (
                f"Integration passes with {tool} {newest} (other tools pinned)."
                if result == "success"
                else f"Integration was not run against {tool} {newest}."
            )
            issues.append(
                Issue(
                    tool,
                    "behind",
                    f"{tool}: tested {version}, {newest} passes"
                    if result == "success"
                    else f"{tool}: tested {version}, {newest} released",
                    f"{ran} Bump `tested_versions` (and the pinned download in "
                    f"`provision.rs` and the Integration pin) from {version}.",
                )
            )
    return issues


def summary(tested, latest, results):
    """A Markdown table of every tool's tested and latest version and result."""
    lines = [
        "| Tool | Tested | Latest | Integration |",
        "|---|---|---|---|",
    ]
    for tool, version in sorted(tested.items()):
        lines.append(
            f"| {tool} | {version} | {latest.get(tool, '?')} | "
            f"{results.get(tool, 'not run')} |"
        )
    return "\n".join(lines)


def gh(*args):
    return subprocess.run(
        ["gh", *args], check=True, capture_output=True, text=True
    ).stdout


def latest_versions(tools):
    latest = {}
    for tool in tools:
        repo = REPOS.get(tool)
        if repo:
            tag = gh("api", f"repos/{repo}/releases/latest", "--jq", ".tag_name")
            latest[tool] = version_of(tool, tag.strip())
    return latest


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
    """Each tool's Integration result: failure if any OS failed."""
    results = {}
    for path in sorted(pathlib.Path(directory).glob("*")):
        tool = path.name.rsplit("-", 1)[0]
        status = path.read_text().strip()
        if results.get(tool) != "failure":
            results[tool] = "success" if status == "success" else "failure"
    # Every run uses stable Rust: cargo is judged only when all runs agree,
    # since one tool's failure says nothing about cargo.
    outcomes = set(results.values())
    if len(outcomes) == 1:
        results["cargo"] = outcomes.pop()
    return results


def sync_issues(issues, tools):
    gh(
        "label",
        "create",
        LABEL,
        "--color",
        "FBCA04",
        "--description",
        "Native tool releases ahead of or breaking depsmith",
        "--force",
    )
    open_issues = json.loads(
        gh("issue", "list", "--label", LABEL, "--state", "open", "--json", "number,title")
    )
    wanted = {issue.tool: issue for issue in issues}
    for tool in tools:
        existing = [i for i in open_issues if i["title"].startswith(f"{tool}: ")]
        issue = wanted.get(tool)
        for old in existing:
            if issue is None or old["title"] != issue.title:
                gh("issue", "close", str(old["number"]), "--comment", "Resolved or superseded by the latest run.")
        if issue and not any(old["title"] == issue.title for old in existing):
            gh("issue", "create", "--label", LABEL, "--title", issue.title, "--body", issue.body)


def main(argv):
    doctor = json.loads(pathlib.Path(argv[1]).read_text())
    results = read_results(argv[2])
    tested = tested_versions(doctor)
    latest = latest_versions(tested)
    issues = decide(tested, latest, results, mismatched_assets(pinned_assets(doctor)))
    print(summary(tested, latest, results))
    for issue in issues:
        print(f"\n- **{issue.kind}**: {issue.title}")
    if "--issues" in argv:
        sync_issues(issues, tested)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
