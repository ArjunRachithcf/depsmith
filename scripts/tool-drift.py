"""Report how the native tools depsmith drives drift from their latest releases.

Run by the Latest tools workflow after Integration ran once per tool with
only that tool at its latest release:

    python scripts/tool-drift.py DOCTOR_JSON RESULTS_DIR [--issues]

DOCTOR_JSON is `depsmith doctor --json` (tested versions and pinned
downloads); RESULTS_DIR holds one `<tool>-<os>` file per Integration job with
its status. Latest releases and asset digests come from the GitHub API via
`gh`. A Markdown summary goes to stdout. With --act (run from a checkout of
main, GH_TOKEN a GitHub App token so pull requests trigger CI), a tool whose
latest release passes Integration gets a pull request that bumps its tested
version, pinned downloads and Integration pin together; a tool broken by its
latest release, a pinned download whose sha256 changed, or a bump that cannot
be made gets one open issue labelled `latest-tools`. Resolved issues close.
"""

import json
import os
import pathlib
import re
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
        "| Tool | Tested | Latest | Integration |",
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


def latest_tags(tools):
    """Each tool's latest release tag."""
    tags = {}
    for tool in tools:
        repo = REPOS.get(tool)
        if repo:
            tags[tool] = gh(
                "api", f"repos/{repo}/releases/latest", "--jq", ".tag_name"
            ).strip()
    return tags


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
    doctor = json.loads(pathlib.Path(argv[1]).read_text())
    results = read_results(argv[2])
    tested = tested_versions(doctor)
    tags = latest_tags(tested)
    latest = {tool: version_of(tool, tag) for tool, tag in tags.items()}
    actions = decide(tested, latest, results, mismatched_assets(pinned_assets(doctor)))
    print(summary(tested, latest, results))
    for action in actions:
        print(f"\n- **{action.kind}**: {action.title}")
    if "--act" in argv:
        act(actions, tested, tags)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
