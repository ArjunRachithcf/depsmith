#!/usr/bin/env python3
"""Summarise corpus results: per-manager step outcomes and failure clusters.

  report.py [--pass N] [--against M]

Rows whose signature matches a rule in triage.toml take that rule's class
(env, repo, design or bug); the rest keep the class the run gave them.
"""

from __future__ import annotations

import argparse
import collections
import csv
import os
import re
from pathlib import Path

import tomllib

HERE = Path(__file__).resolve().parent
WORK = Path(os.environ.get("CORPUS_WORK", HERE.parent.parent / "target" / "corpus"))


def rows(pass_id: str | None):
    with (WORK / "results.tsv").open() as f:
        data = list(csv.DictReader(f, delimiter="\t", quoting=csv.QUOTE_NONE))
    if pass_id is None:
        pass_id = data[-1]["pass"]
    # The last row for each repository and step wins, so a re-run replaces it.
    latest = {}
    for r in data:
        if r["pass"] == pass_id:
            latest[(r["repo"], r["step"])] = r
    rules = tomllib.loads((HERE / "triage.toml").read_text()).get("rule", [])
    for r in latest.values():
        for rule in rules:
            if re.search(rule["pattern"], r["signature"] + " " + r["detail"]):
                r["class"], r["note"] = rule["class"], rule.get("note", "")
                break
    return pass_id, list(latest.values())


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--pass", dest="pass_id")
    parser.add_argument("--against")
    parser.add_argument("--details", action="store_true")
    args = parser.parse_args()
    pass_id, data = rows(args.pass_id)
    print(f"# Pass {pass_id}: {len({r['repo'] for r in data})} repositories\n")
    table = collections.defaultdict(collections.Counter)
    for r in data:
        table[r["manager"]][r["class"]] += 1
    classes = sorted({c for t in table.values() for c in t})
    print("| manager | " + " | ".join(classes) + " |")
    print("|---|" + "---|" * len(classes))
    for manager in sorted(table):
        print(
            f"| {manager} | "
            + " | ".join(str(table[manager][c]) for c in classes)
            + " |"
        )
    clusters = collections.defaultdict(list)
    for r in data:
        if r["class"] not in ("ok", "skip"):
            clusters[(r["class"], r["signature"])].append(r)
    print("\n## Clusters\n")
    order = {"bug": 0, "triage": 1, "design": 2, "repo": 3, "env": 4}
    for (cls, sig), members in sorted(
        clusters.items(), key=lambda kv: (order.get(kv[0][0], 9), -len(kv[1]))
    ):
        managers = collections.Counter(m["manager"] for m in members)
        print(f"- **{cls}** ×{len(members)} {dict(managers)} `{sig}`")
        if args.details or cls in ("bug", "triage"):
            for m in members[:6]:
                print(f"    - {m['repo']} [{m['step']}] {m['detail'][:220]}")
    if args.against:
        _, old = rows(args.against)
        before = {(r["repo"], r["step"]): r["class"] for r in old}
        now = {(r["repo"], r["step"]): r["class"] for r in data}
        fixed = [
            k for k, c in now.items() if c == "ok" and before.get(k) not in (None, "ok")
        ]
        broke = [k for k, c in now.items() if c != "ok" and before.get(k) == "ok"]
        print(
            f"\n## Against pass {args.against}: {len(fixed)} fixed, {len(broke)} regressed"
        )
        for k in broke:
            print(f"- regressed: {k[0]} [{k[1]}] {now[k]}")


if __name__ == "__main__":
    main()
