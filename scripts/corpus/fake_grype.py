#!/usr/bin/env python3
"""A Grype stand-in backed by the GitHub advisory database, for hosts that
cannot download Grype's own database.

  fake_grype.py index ADVISORY_DATABASE_CHECKOUT INDEX.json
  fake_grype.py db update | db status -o json | sbom:PATH -o json   (as grype)

The scan subcommands read the index named by CORPUS_ADVISORY_INDEX, built by
`index` from a checkout of github.com/github/advisory-database. Reviewed
advisories for npm, PyPI and crates.io are matched against each SBOM
component's package URL and reported in Grype's JSON shape, with every
`fixed` event of the affected range as the fix versions. Standard library only.
"""

from __future__ import annotations

import json
import os
import re
import sys
from pathlib import Path

ECOSYSTEMS = {"npm": "npm", "PyPI": "pypi", "crates.io": "cargo"}
LANGUAGES = {"npm": "javascript", "pypi": "python", "cargo": "rust"}


def normalize(kind: str, name: str) -> str:
    return re.sub(r"[-_.]+", "-", name).lower() if kind == "pypi" else name


def index(checkout: str, out: str) -> None:
    table: dict[str, list] = {}
    built = ""
    for path in Path(checkout, "advisories", "github-reviewed").rglob("*.json"):
        advisory = json.loads(path.read_text())
        if advisory.get("withdrawn"):
            continue
        built = max(built, advisory.get("modified", ""))
        severity = (advisory.get("database_specific") or {}).get(
            "severity"
        ) or "UNKNOWN"
        for affected in advisory.get("affected", []):
            package = affected.get("package", {})
            kind = ECOSYSTEMS.get(package.get("ecosystem"))
            if not kind:
                continue
            ranges = [
                r["events"]
                for r in affected.get("ranges", [])
                if r.get("type") in ("ECOSYSTEM", "SEMVER")
            ]
            entry = {
                "id": advisory["id"],
                "severity": severity.title(),
                "ranges": ranges,
                "versions": affected.get("versions", []),
            }
            table.setdefault(f"{kind}:{normalize(kind, package['name'])}", []).append(
                entry
            )
    Path(out).write_text(json.dumps({"built": built, "packages": table}))
    print(f"{len(table)} packages, built {built}")


def key(kind: str, version: str):
    """A sort key: numeric release parts, then pre-releases before the release."""
    version = version.strip().lstrip("vV").split("+")[0]
    if kind == "pypi":
        m = re.match(
            r"^(?:\d+!)?([\d.]+)(?:[-_.]?(a|alpha|b|beta|c|rc|pre|preview)[-_.]?(\d*))?"
            r"(?:[-_.]?(post|rev|r)[-_.]?(\d*))?(?:[-_.]?dev[-_.]?(\d*))?",
            version.lower(),
        )
        if not m:
            return ((0,), (9,))
        release = tuple(int(p) for p in m.group(1).strip(".").split(".") if p)
        while len(release) > 1 and release[-1] == 0:
            release = release[:-1]
        pre = (
            {"a": 0, "alpha": 0, "b": 1, "beta": 1}.get(m.group(2), 2)
            if m.group(2)
            else 3
        )
        return (
            release,
            (
                pre,
                int(m.group(3) or 0),
                int(m.group(5) or 0) if m.group(4) else -1,
                int(m.group(6) or 0) if m.group(6) is not None else 1 << 30,
            ),
        )
    core, _, pre = version.partition("-")
    release = tuple(int(p) if p.isdigit() else 0 for p in core.split("."))
    release += (0,) * (3 - len(release))
    parts = (
        tuple((0, int(p), "") if p.isdigit() else (1, 0, p) for p in pre.split("."))
        if pre
        else ()
    )
    return (release, (0, parts) if pre else (1, ()))


def affected(kind: str, version: str, entry: dict):
    """The fix versions of the range `version` falls in, or None."""
    if version in entry["versions"]:
        return []
    v = key(kind, version)
    for events in entry["ranges"]:
        start, fixes, hit = None, [], False
        for event in events:
            if "introduced" in event:
                start = event["introduced"]
            elif "fixed" in event or "last_affected" in event:
                end = event.get("fixed") or event.get("last_affected")
                low = start in (None, "0") or key(kind, start) <= v
                high = v < key(kind, end) if "fixed" in event else v <= key(kind, end)
                if low and high:
                    return [event["fixed"]] if "fixed" in event else []
                start = None
        if start is not None and (start == "0" or key(kind, start) <= v):
            hit = True
        if hit:
            return fixes
    return None


def scan(sbom_path: str) -> dict:
    data = json.loads(Path(os.environ["CORPUS_ADVISORY_INDEX"]).read_text())["packages"]
    matches = []
    for component in json.loads(Path(sbom_path).read_text()).get("components", []):
        purl = component.get("purl", "")
        m = re.match(r"^pkg:(npm|pypi|cargo)/(.+)@([^@?#]+)$", purl)
        if not m:
            continue
        kind, name, version = m.group(1), m.group(2).replace("%40", "@"), m.group(3)
        for entry in data.get(f"{kind}:{normalize(kind, name)}", []):
            fixes = affected(kind, version, entry)
            if fixes is None:
                continue
            matches.append(
                {
                    "artifact": {"purl": purl, "name": name, "version": version},
                    "matchDetails": [
                        {
                            "type": "exact-direct-match",
                            "matcher": "fake-grype",
                            "found": {"advisory": entry["id"]},
                        }
                    ],
                    "vulnerability": {
                        "id": entry["id"],
                        "namespace": f"github:language:{LANGUAGES[kind]}",
                        "severity": entry["severity"],
                        "fix": {
                            "versions": fixes,
                            "state": "fixed" if fixes else "not-fixed",
                        },
                    },
                }
            )
    return {"matches": matches}


def main() -> int:
    args = sys.argv[1:]
    if args[:1] == ["index"]:
        index(args[1], args[2])
        return 0
    if args[:2] == ["db", "update"]:
        return 0
    if args[:2] == ["db", "status"]:
        built = json.loads(Path(os.environ["CORPUS_ADVISORY_INDEX"]).read_text())[
            "built"
        ]
        print(json.dumps({"built": built, "schemaVersion": "github-advisory-database"}))
        return 0
    if args and args[0].startswith("sbom:"):
        print(json.dumps(scan(args[0][len("sbom:") :])))
        return 0
    print(f"unsupported: {args}", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
