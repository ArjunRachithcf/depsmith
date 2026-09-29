"""Check release archive contents without extracting untrusted paths."""

import sys
import tarfile
import zipfile
from pathlib import Path, PurePosixPath


def check(path):
    if path.suffix == ".whl":
        with zipfile.ZipFile(path) as archive:
            names = archive.namelist()
        required = ["depsmith/py.typed", "depsmith/_native.pyi"]
    else:
        with tarfile.open(path) as archive:
            names = archive.getnames()
        roots = {name.split("/")[0] for name in names}
        if len(roots) != 1:
            raise ValueError(f"{path}: expected one sdist root")
        prefix = next(iter(roots)) + "/"
        names = [name.removeprefix(prefix) for name in names]
        required = [
            "Cargo.lock",
            "pyproject.toml",
            "python/depsmith/py.typed",
            "python/depsmith/_native.pyi",
            "tests/python/test_api.py",
        ]
    missing = [name for name in required if name not in names]
    if not any(PurePosixPath(name).name == "LICENSE" for name in names):
        missing.append("LICENSE")
    if missing:
        raise ValueError(f"{path}: missing {missing}")
    for name in names:
        parts = PurePosixPath(name).parts
        if (
            "HANDOFF.md" in parts
            or name.endswith("docs/progress.md")
            or ".git" in parts
        ):
            raise ValueError(f"{path}: contains local-only file {name}")
    print(f"{path.name}: license, typing files and archive contents verified")


if __name__ == "__main__":
    paths = [
        path
        for directory in sys.argv[1:]
        for path in Path(directory).iterdir()
        if path.name.endswith((".whl", ".tar.gz"))
    ]
    if not paths:
        raise SystemExit("No distributions found")
    for path in paths:
        check(path)
