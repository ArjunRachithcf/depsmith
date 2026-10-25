#!/usr/bin/env python3
"""Run depsmith end to end over a corpus of real repositories.

  run.py pin CANDIDATES.jsonl... --out repos.toml   pick 30 fresh repos per manager
  run.py run --pass N [--only TEXT]                 one pass over repos.toml

Each repository is cloned once at its pinned commit. Every step runs in a
fresh copy: discover, check, update --apply (then the native tool's lock
check, a re-check that must find nothing pending, and the impact driver), and
fix --apply with a Grype scan. One row per repository and step is appended to
results.tsv; logs stay under the work directory. Standard library only.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import itertools
import json
import os
import re
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

import tomllib

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
DEPSMITH = REPO / "target" / "release" / "depsmith"
PER_MANAGER = 30
STEP_TIMEOUT = 1800
EXPECTED = {"check": {0, 1}, "update": {0}, "fix": {0}, "recheck": {0}}
LOCKS = {
    "pixi": "pixi.lock",
    "conda": "conda-lock.yml",
    "uv": "uv.lock",
    "npm": "package-lock.json",
    "cargo": "Cargo.lock",
}
_lock = threading.Lock()
# Per-thread environment: each repository's package caches and temp files live
# under its work directory and are deleted with it, so disk use stays bounded.
_ctx = threading.local()
_clone_locks: dict[str, threading.RLock] = {}


def work_dir() -> Path:
    return Path(os.environ.get("CORPUS_WORK", REPO / "target" / "corpus"))


def tools() -> dict[str, str]:
    """`--tool` values from CORPUS_TOOLS (the `init --fetch-tools` cache)."""
    base = Path(os.environ.get("CORPUS_TOOLS", work_dir() / "tools"))
    found = {
        "pixi": next(base.glob("pixi/*/pixi"), None),
        "conda": next(base.glob("conda/*/micromamba"), None),
        "conda-lock": next(base.glob("conda-lock/*/venv/bin/conda-lock"), None),
        "grype": next(base.glob("grype/*/grype"), None),
        "npm": next(base.glob("npm/*/node-*/bin/npm"), None),
    }
    # Where Grype's database host is out of reach, a stand-in backed by the
    # GitHub advisory database (fake_grype.py, wrapped by fake-grype/grype).
    if os.environ.get("CORPUS_FAKE_GRYPE"):
        found["grype"] = base / "fake-grype" / "grype"
    return {name: str(path) for name, path in found.items() if path}


def run(
    cmd: list[str],
    cwd: Path,
    log: Path | None = None,
    timeout: int = STEP_TIMEOUT,
    env=None,
):
    """Run `cmd`; return (exit code, stdout, stderr, seconds)."""
    start = time.monotonic()
    try:
        proc = subprocess.run(
            cmd,
            check=False,
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=timeout,
            env={**os.environ, **getattr(_ctx, "env", {}), **(env or {})},
            stdin=subprocess.DEVNULL,
        )
        code, out, err = proc.returncode, proc.stdout, proc.stderr
    except subprocess.TimeoutExpired as e:
        code = -9
        out = (
            (e.stdout or b"").decode()
            if isinstance(e.stdout, bytes)
            else (e.stdout or "")
        )
        err = f"TIMEOUT after {timeout}s"
    seconds = time.monotonic() - start
    if log:
        log.parent.mkdir(parents=True, exist_ok=True)
        log.with_suffix(".out").write_text(out)
        log.with_suffix(".err").write_text(
            f"$ {' '.join(cmd)}\n# cwd {cwd}\n# exit {code}\n{err}"
        )
    return code, out, err, seconds


def clone(repo: str, ref: str | None) -> Path:
    """A cached shallow clone of `repo` at `ref` (default branch when None)."""
    with repo_lock(repo):
        return _clone(repo, ref)


def repo_lock(repo: str) -> threading.RLock:
    with _lock:
        return _clone_locks.setdefault(repo, threading.RLock())


def _clone(repo: str, ref: str | None) -> Path:
    dest = work_dir() / "cache" / repo.replace("/", "__")
    if (dest / ".git").exists():
        return dest
    tmp = dest.with_suffix(".tmp")
    shutil.rmtree(tmp, ignore_errors=True)
    tmp.mkdir(parents=True)
    url = f"https://github.com/{repo}"
    steps = [
        ["git", "init", "-q"],
        ["git", "fetch", "-q", "--depth", "1", url, ref or "HEAD"],
        ["git", "-c", "advice.detachedHead=false", "checkout", "-q", "FETCH_HEAD"],
    ]
    for cmd in steps:
        code, _, err, _ = run(cmd, tmp, timeout=600)
        if code:
            raise RuntimeError(f"clone {repo}: {err.strip()[-300:]}")
    tmp.rename(dest)
    return dest


def fresh_copy(src: Path, dest: Path) -> Path:
    shutil.rmtree(dest, ignore_errors=True)
    shutil.copytree(src, dest, symlinks=True)
    for cmd in (
        ["git", "config", "user.email", "corpus@example.org"],
        ["git", "config", "user.name", "corpus"],
    ):
        run(cmd, dest)
    return dest


def native_check(manager: str, root: Path, manifest: str, log: Path):
    """The native tool's own check that the target's lock matches its manifest."""
    cwd = root / Path(manifest).parent
    t = tools()
    cmd = {
        "npm": [
            "npm",
            "ci",
            "--dry-run",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
        ],
        "cargo": ["cargo", "metadata", "--locked", "--format-version", "1"],
        "uv": ["uv", "lock", "--check"],
        "pixi": [
            t.get("pixi", "pixi"),
            "lock",
            "--check",
            "--manifest-path",
            str(root / manifest),
        ],
    }.get(manager)
    if cmd is None:
        return None
    code, _, _, _ = run(cmd, cwd, log, timeout=1200)
    return code


def depsmith(
    root: Path, target: str, command: list[str], log: Path, scan: bool = False
):
    cmd = [
        str(DEPSMITH),
        "--root",
        str(root),
        "--json",
        "--non-interactive",
        "--timeout-seconds",
        "900",
        "--target",
        target,
    ]
    for name, path in tools().items():
        if name != "grype" or scan:
            cmd += ["--tool", f"{name}={path}"]
    return run(cmd + command, root, log)


def signature(text: str) -> str:
    """A failure message normalised for clustering."""
    line = next((l for l in text.splitlines() if l.strip()), "")[:400]
    line = re.sub(r"(/[\w.@+-]+)+", "<path>", line)
    line = re.sub(r"\b[0-9a-f]{12,}\b", "<hex>", line)
    line = re.sub(r"\d+(\.\d+)+([-.+\w]*)?", "<ver>", line)
    line = re.sub(r"\d+", "<n>", line)
    return line[:160]


def outcome(step: str, code: int, out: str, err: str):
    """(class, signature, detail) of one depsmith invocation."""
    if "panicked at" in err:
        msg = err[err.index("panicked at") :]
        return "bug", "panic: " + signature(msg.split("\n", 1)[-1] or msg), msg[:500]
    if code == -9:
        return "triage", f"{step}: timeout", err
    try:
        report = json.loads(out)
    except json.JSONDecodeError:
        return "bug", f"{step}: stdout is not JSON (exit {code})", (out or err)[-500:]
    proposal = report.get("proposal") or {}
    failures = proposal.get("failures") or []
    if failures:
        first = failures[0]
        msg = first.get("message") or json.dumps(first)
        return "triage", f"{step}: " + signature(msg), json.dumps(failures)[:2000]
    if report.get("error"):
        return "triage", f"{step}: " + signature(report["error"]), report["error"]
    if code not in EXPECTED[step]:
        tail = err.strip().splitlines()[-1] if err.strip() else ""
        return "triage", f"{step}: exit {code}: " + signature(tail), err[-1000:]
    return "ok", "", ""


class Recorder:
    def __init__(self, pass_id: str):
        self.pass_id = pass_id
        self.path = work_dir() / "results.tsv"
        if not self.path.exists():
            self.path.write_text(
                "pass\tmanager\trepo\tstep\tclass\texit\tseconds\tsignature\tdetail\n"
            )

    def add(
        self,
        entry: dict,
        step: str,
        cls: str,
        code,
        seconds: float,
        sig: str = "",
        detail: str = "",
    ):
        clean = lambda s: re.sub(r"[\t\n\r]+", " ", str(s))[:600]
        row = [
            self.pass_id,
            entry["manager"],
            entry["repo"],
            step,
            cls,
            str(code),
            f"{seconds:.1f}",
            clean(sig),
            clean(detail),
        ]
        with _lock, self.path.open("a") as f:
            f.write("\t".join(row) + "\n")
        print(
            f"[{entry['manager']:>14}] {entry['repo']:<45} {step:<8} {cls:<6} {sig[:80]}",
            flush=True,
        )
        return cls


def process(entry: dict, rec: Recorder):
    name = f"{entry['manager']}__" + entry["repo"].replace("/", "__")
    caches = work_dir() / f"pass-{rec.pass_id}" / "caches" / name
    tmp = caches / "tmp"
    tmp.mkdir(parents=True, exist_ok=True)
    cargo_home = caches / "cargo"
    cargo_home.mkdir(exist_ok=True)
    # Keep the toolchain's own cargo/rustup binaries reachable.
    for item in ("bin", "env"):
        src = Path.home() / ".cargo" / item
        if src.exists() and not (cargo_home / item).exists():
            (cargo_home / item).symlink_to(src)
    npm = tools().get("npm")
    _ctx.env = {
        # The pinned Node.js first, for npm and for its native lock check.
        "PATH": (str(Path(npm).parent) + os.pathsep if npm else "")
        + os.environ["PATH"],
        "TMPDIR": str(tmp),
        "RATTLER_CACHE_DIR": str(caches / "rattler"),
        "PIXI_CACHE_DIR": str(caches / "rattler"),
        "UV_CACHE_DIR": str(caches / "uv"),
        "npm_config_cache": str(caches / "npm"),
        "CARGO_HOME": str(cargo_home),
        "CONDA_PKGS_DIRS": str(caches / "conda-pkgs"),
        "MAMBA_ROOT_PREFIX": str(caches / "mamba"),
        "XDG_CACHE_HOME": str(caches / "xdg"),
    }
    try:
        _process(entry, rec)
    except Exception as e:  # noqa: BLE001 - one repository must not stop the pass
        rec.add(
            entry, "harness", "triage", -1, 0, "harness: " + signature(repr(e)), repr(e)
        )
    finally:
        _ctx.env = {}
        shutil.rmtree(caches, ignore_errors=True)


def _process(entry: dict, rec: Recorder):
    manager, target = entry["manager"], f"{entry['manager']}:{entry['target']}"
    name = f"{manager}__" + entry["repo"].replace("/", "__")
    base = work_dir() / f"pass-{rec.pass_id}"
    logs = base / "logs" / name
    try:
        src = clone(entry["repo"], entry.get("ref"))
    except Exception as e:  # noqa: BLE001
        rec.add(entry, "clone", "env", -1, 0, signature(str(e)), str(e))
        return
    copy = lambda step: fresh_copy(src, base / "trees" / name / step)

    root = copy("discover")
    code, out, err, secs = run(
        [str(DEPSMITH), "--root", str(root), "--json", "discover"],
        root,
        logs / "discover",
    )
    try:
        ids = [t["id"] for t in json.loads(out)["targets"]]
    except (json.JSONDecodeError, KeyError):
        ids = None
    if "panicked at" in err or ids is None:
        rec.add(
            entry,
            "discover",
            "bug",
            code,
            secs,
            "discover: " + signature(err),
            err[-500:],
        )
        return
    if target not in ids:
        rec.add(
            entry,
            "discover",
            "triage",
            code,
            secs,
            "discover: target not discovered",
            " ".join(ids)[:500],
        )
        return
    rec.add(entry, "discover", "ok", code, secs)
    shutil.rmtree(root, ignore_errors=True)

    if manager == "github-actions" and os.environ.get("CORPUS_ACTIONS_VIA_GIT"):
        root = copy("update")
        code, out, err, secs = run(
            [
                driver("corpus_actions"),
                "--ignored",
                "--nocapture",
                "--test-threads",
                "1",
            ],
            root,
            logs / "actions",
            env={"CORPUS_ROOT": str(root), "CORPUS_TARGET": target},
        )
        stats = next(
            (l for l in out.splitlines() if l.startswith("CORPUS_ACTIONS")), ""
        )
        if code == 0:
            rec.add(entry, "update", "ok", code, secs, "", stats)
            shutil.rmtree(base / "trees" / name, ignore_errors=True)
        else:
            text = out + err
            m = re.search(r"CORPUS_FAILURE ([^\n]*)", text) or re.search(
                r"panicked at [^\n]*\n([^\n]*)", text
            )
            cls = "triage" if m and "CORPUS_FAILURE" in m.group(0) else "bug"
            rec.add(
                entry,
                "update",
                cls,
                code,
                secs,
                "actions: " + signature(m.group(1) if m else text[-300:]),
                text[-1500:],
            )
        return

    root = copy("check")
    code, out, err, secs = depsmith(root, target, ["check"], logs / "check")
    cls, sig, detail = outcome("check", code, out, err)
    rec.add(entry, "check", cls, code, secs, sig, detail)
    pending = cls == "ok" and code == 1
    if cls == "ok":
        shutil.rmtree(root, ignore_errors=True)

    baseline = native_check(manager, src, entry["target"], logs / "native-baseline")
    root = copy("update")
    code, out, err, secs = depsmith(
        root, target, ["update", "--apply", "--yes"], logs / "update"
    )
    cls, sig, detail = outcome("update", code, out, err)
    rec.add(entry, "update", cls, code, secs, sig, detail)
    if cls == "ok" and not post_apply(
        entry, target, root, rec, logs, "update", baseline, pending
    ):
        shutil.rmtree(root, ignore_errors=True)

    root = copy("fix")
    code, out, err, secs = depsmith(
        root, target, ["fix", "--apply", "--yes"], logs / "fix", scan=True
    )
    cls, sig, detail = outcome("fix", code, out, err)
    rec.add(entry, "fix", cls, code, secs, sig, detail)
    if cls == "ok" and not post_apply(
        entry, target, root, rec, logs, "fix", baseline, None
    ):
        shutil.rmtree(root, ignore_errors=True)


def post_apply(entry, target, root, rec, logs, step, baseline, pending) -> bool:
    """Checks on an applied tree; True when a problem was found."""
    manager, manifest = entry["manager"], entry["target"]
    problem = False
    code, out, _, secs = run(
        ["git", "status", "--porcelain", "--untracked-files=all"], root
    )
    changed = [line[3:] for line in out.splitlines()]
    stray = [
        p
        for p in changed
        if not p.startswith(".depsmith/")
        and Path(p).parent != Path(manifest).parent
        and manager != "cargo"
    ]
    if stray:
        problem = True
        rec.add(
            entry,
            f"{step}-files",
            "triage",
            0,
            secs,
            f"{step}: changed files outside the target's directory",
            " ".join(stray),
        )
    if pending and not changed:
        problem = True
        rec.add(
            entry,
            f"{step}-files",
            "triage",
            0,
            secs,
            f"{step}: check found updates but apply wrote nothing",
            "",
        )
    if changed:
        code = native_check(manager, root, manifest, logs / f"native-{step}")
        if code is not None:
            if code == 0:
                rec.add(entry, f"{step}-native", "ok", code, 0)
            elif baseline == 0:
                problem = True
                rec.add(
                    entry,
                    f"{step}-native",
                    "bug",
                    code,
                    0,
                    f"{step}: native lock check rejects the applied files",
                    (logs / f"native-{step}.err").read_text()[-800:],
                )
            else:
                rec.add(
                    entry,
                    f"{step}-native",
                    "repo",
                    code,
                    0,
                    "native lock check failed before the update too",
                )
    if step != "update":
        return problem
    code, out, err, secs = depsmith(root, target, ["check"], logs / "recheck")
    cls, sig, detail = outcome("recheck", code, out, err)
    if cls == "ok" and code == 0:
        rec.add(entry, "recheck", "ok", code, secs)
    else:
        problem = True
        rec.add(
            entry,
            "recheck",
            "triage" if cls == "ok" else cls,
            code,
            secs,
            sig or "recheck: updates still pending after apply",
            detail or out[-1500:],
        )
    if manager in LOCKS and changed:
        driver = impact_driver()
        code, out, err, secs = run(
            [driver, "--ignored", "--nocapture", "--test-threads", "1"],
            root,
            logs / "impact",
            env={"CORPUS_ROOT": str(root), "CORPUS_TARGET": target},
        )
        stats = next((l for l in out.splitlines() if l.startswith("CORPUS_IMPACT")), "")
        if code == 0:
            rec.add(entry, "impact", "ok", code, secs, "", stats)
        else:
            problem = True
            panic = re.search(r"panicked at [^\n]*\n([^\n]*)", out + err)
            rec.add(
                entry,
                "impact",
                "bug",
                code,
                secs,
                "impact: "
                + signature(panic.group(1) if panic else (err or out)[-300:]),
                (out + err)[-1500:],
            )
    return problem


_drivers: dict[str, str] = {}


def driver(name: str) -> str:
    """The executable of the ignored integration test `name` (built once)."""
    with _lock:
        if name not in _drivers:
            proc = subprocess.run(
                [
                    "cargo",
                    "test",
                    "--release",
                    "-p",
                    "depsmith-core",
                    "--test",
                    name,
                    "--no-run",
                    "--message-format",
                    "json",
                ],
                cwd=REPO,
                capture_output=True,
                text=True,
                check=True,
            )
            for line in proc.stdout.splitlines():
                msg = json.loads(line)
                if (
                    msg.get("reason") == "compiler-artifact"
                    and msg.get("executable")
                    and msg["target"]["name"] == name
                ):
                    _drivers[name] = msg["executable"]
        return _drivers[name]


def impact_driver() -> str:
    return driver("corpus_impact")


def load_repos(path: Path) -> list[dict]:
    return tomllib.loads(path.read_text())["repo"]


def cmd_run(args):
    entries = load_repos(Path(args.repos))
    if args.only:
        entries = [
            e for e in entries if any(o in (e["manager"], e["repo"]) for o in args.only)
        ]
    if not DEPSMITH.exists():
        sys.exit(f"build first: cargo build --release -p depsmith ({DEPSMITH})")
    impact_driver()
    driver("corpus_actions")
    rec = Recorder(args.pass_id)
    # GitHub Actions lookups share one API rate limit; keep them to one at a time.
    actions = [e for e in entries if e["manager"] == "github-actions"]
    # Interleave the managers so slow solvers (conda) do not hold every worker.
    by_manager: dict[str, list] = {}
    for e in entries:
        if e["manager"] != "github-actions":
            by_manager.setdefault(e["manager"], []).append(e)
    others = [
        e for group in itertools.zip_longest(*by_manager.values()) for e in group if e
    ]
    with concurrent.futures.ThreadPoolExecutor(args.jobs) as pool:
        futures = [pool.submit(process, e, rec) for e in others]
        futures.append(pool.submit(lambda: [process(e, rec) for e in actions]))
        for f in concurrent.futures.as_completed(futures):
            if f.exception():
                print(f"harness error: {f.exception()!r}", flush=True)


def cmd_pin(args):
    """Resolve candidates to commits and keep the first PER_MANAGER whose
    target is discovered and whose lock passes the native check."""
    candidates = []
    for path in args.candidates:
        candidates += [
            json.loads(l) for l in Path(path).read_text().splitlines() if l.strip()
        ]
    previous = (
        load_repos(Path(args.out)) if Path(args.out).exists() and args.append else []
    )
    existing = {(e["manager"], e["repo"]) for e in previous}
    kept: dict[str, list] = {}
    room = {
        m: PER_MANAGER - sum(e["manager"] == m for e in previous)
        for m in {c["manager"] for c in candidates}
    }
    lock = threading.Lock()

    def check(c):
        # A rejected candidate's clone is deleted: the cache only keeps the corpus.
        with repo_lock(c["repo"]):
            if not keep(c) and not any(
                e["repo"] == c["repo"] for v in kept.values() for e in v
            ):
                shutil.rmtree(
                    work_dir() / "cache" / c["repo"].replace("/", "__"),
                    ignore_errors=True,
                )

    def keep(c) -> bool:
        manager = c["manager"]
        with lock:
            if (
                len(kept.get(manager, [])) >= room[manager]
                or (manager, c["repo"]) in existing
            ):
                return True
        code, out, _, _ = run(
            ["git", "ls-remote", f"https://github.com/{c['repo']}", "HEAD"],
            work_dir(),
            timeout=60,
        )
        if code or not out.strip():
            print(f"skip {c['repo']}: unreachable", flush=True)
            return False
        sha = out.split()[0]
        try:
            src = clone(c["repo"], sha)
        except Exception as e:  # noqa: BLE001
            print(f"skip {c['repo']}: {e}", flush=True)
            return False
        size = sum(
            f.stat().st_size
            for f in src.rglob("*")
            if f.is_file() and not f.is_symlink()
        )
        if size > 300 * 2**20:
            print(f"skip {c['repo']}: checkout over 300 MB", flush=True)
            return False
        code, out, _, _ = run(
            [str(DEPSMITH), "--root", str(src), "--json", "discover"], src
        )
        target = f"{manager}:{c['manifest']}"
        if target not in out:
            print(f"skip {c['repo']}: {target} not discovered", flush=True)
            return False
        if manager in LOCKS:
            lock_file = src / Path(c["manifest"]).parent / LOCKS[manager]
            if manager == "cargo":
                lock_file = c.get("lock") and src / c["lock"] or lock_file
            if (
                lock_file.exists()
                and sum(1 for _ in lock_file.open(errors="replace")) > 20000
            ):
                print(f"skip {c['repo']}: lock over 20k lines", flush=True)
                return False
        log = work_dir() / "pin-logs" / c["repo"].replace("/", "__")
        code = native_check(manager, src, c["manifest"], log)
        if code not in (None, 0):
            print(
                f"skip {c['repo']}: native lock check fails at {sha[:10]}", flush=True
            )
            return False
        with lock:
            if len(kept.setdefault(manager, [])) < room[manager]:
                kept[manager].append(
                    {
                        "manager": manager,
                        "repo": c["repo"],
                        "ref": sha,
                        "target": c["manifest"],
                        "notes": c.get("notes", ""),
                    }
                )
                print(f"keep {manager} {c['repo']} ({len(kept[manager])})", flush=True)
                return True
        return False

    with concurrent.futures.ThreadPoolExecutor(args.jobs) as pool:
        list(pool.map(check, candidates))
    mode = "a" if args.append else "w"
    with open(args.out, mode) as f:
        f.writelines(
            "\n[[repo]]\n"
            + "".join(
                f"{key} = {json.dumps(e[key])}\n"
                for key in ("manager", "repo", "ref", "target", "notes")
            )
            for manager in sorted(kept)
            for e in kept[manager]
        )
    print({m: len(v) for m, v in kept.items()})


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("run")
    p.add_argument("--pass", dest="pass_id", required=True)
    p.add_argument("--repos", default=str(HERE / "repos.toml"))
    p.add_argument("--only", nargs="*")
    p.add_argument("--jobs", type=int, default=4)
    p.set_defaults(func=cmd_run)
    p = sub.add_parser("pin")
    p.add_argument("candidates", nargs="+")
    p.add_argument("--out", default=str(HERE / "repos.toml"))
    p.add_argument("--append", action="store_true")
    p.add_argument("--jobs", type=int, default=4)
    p.set_defaults(func=cmd_pin)
    args = parser.parse_args()
    work_dir().mkdir(parents=True, exist_ok=True)
    args.func(args)


if __name__ == "__main__":
    main()
