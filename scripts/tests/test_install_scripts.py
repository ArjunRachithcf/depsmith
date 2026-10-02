"""The release install scripts verify the asset against SHA256SUMS first."""

import hashlib
import http.server
import io
import os
import pathlib
import shutil
import subprocess
import tarfile
import tempfile
import threading
import unittest

SCRIPTS = pathlib.Path(__file__).resolve().parents[1]
TAG = "v9.9.9"
TARGET = "x86_64-unknown-linux-gnu"
PROGRAM = b"#!/bin/sh\necho 'depsmith 9.9.9'\n"


def tarball(files):
    data = io.BytesIO()
    with tarfile.open(fileobj=data, mode="w:gz") as archive:
        for name, content in files.items():
            info = tarfile.TarInfo(f"./{name}")
            info.size = len(content)
            info.mode = 0o755
            archive.addfile(info, io.BytesIO(content))
    return data.getvalue()


class Release:
    """A fake GitHub release at <base>/<tag>/<asset> on 127.0.0.1."""

    def __init__(self, assets, sums=None):
        self.directory = tempfile.TemporaryDirectory()
        root = pathlib.Path(self.directory.name) / TAG
        root.mkdir()
        for name, content in assets.items():
            (root / name).write_bytes(content)
        if sums is None:
            sums = "".join(
                f"{hashlib.sha256(content).hexdigest()}  {name}\n"
                for name, content in assets.items()
            )
        (root / "SHA256SUMS").write_text(sums)
        directory = self.directory.name

        class Quiet(http.server.SimpleHTTPRequestHandler):
            def __init__(self, *args):
                super().__init__(*args, directory=directory)

            def log_message(self, *args):
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Quiet)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.base = f"http://127.0.0.1:{self.server.server_address[1]}"

    def close(self):
        self.server.shutdown()
        self.directory.cleanup()


class ShellScriptTests(unittest.TestCase):
    def setUp(self):
        if os.name == "nt" or not shutil.which("sh"):
            self.skipTest("install.sh needs a POSIX shell")
        self.prefix = tempfile.TemporaryDirectory()
        self.addCleanup(self.prefix.cleanup)

    def install(self, release, target=TARGET):
        return subprocess.run(
            ["sh", str(SCRIPTS / "install.sh"), "--version", TAG, "--target", target,
             "--prefix", self.prefix.name, "--base-url", release.base],
            capture_output=True, text=True, check=False,
        )  # fmt: skip

    def installed(self):
        return pathlib.Path(self.prefix.name, "depsmith")

    def test_a_verified_asset_is_installed(self):
        release = Release({f"depsmith-{TARGET}.tar.gz": tarball({"depsmith": PROGRAM})})
        self.addCleanup(release.close)
        result = self.install(release)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.installed().read_bytes(), PROGRAM)
        self.assertTrue(os.access(self.installed(), os.X_OK))
        self.assertIn("depsmith", result.stdout + result.stderr)

    def test_a_checksum_mismatch_installs_nothing(self):
        asset = tarball({"depsmith": PROGRAM})
        release = Release(
            {f"depsmith-{TARGET}.tar.gz": asset},
            sums=f"{'0' * 64}  depsmith-{TARGET}.tar.gz\n",
        )
        self.addCleanup(release.close)
        result = self.install(release)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("checksum", result.stderr.lower())
        self.assertFalse(self.installed().exists())

    def test_an_asset_missing_from_the_sums_is_refused(self):
        release = Release(
            {f"depsmith-{TARGET}.tar.gz": tarball({"depsmith": PROGRAM})}, sums=""
        )
        self.addCleanup(release.close)
        result = self.install(release)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("SHA256SUMS", result.stderr)
        self.assertFalse(self.installed().exists())

    def test_an_unsupported_platform_points_elsewhere(self):
        release = Release({})
        self.addCleanup(release.close)
        result = self.install(release, target="riscv64-unknown-linux-gnu")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cargo install depsmith", result.stderr)


class PowerShellScriptTests(unittest.TestCase):
    def setUp(self):
        self.pwsh = shutil.which("pwsh")
        if not self.pwsh:
            self.skipTest("install.ps1 needs PowerShell (pwsh)")
        self.prefix = tempfile.TemporaryDirectory()
        self.addCleanup(self.prefix.cleanup)

    target = "x86_64-pc-windows-msvc" if os.name == "nt" else TARGET

    def install(self, release):
        return subprocess.run(
            [self.pwsh, "-NoProfile", "-File", str(SCRIPTS / "install.ps1"),
             "-Version", TAG, "-Target", self.target, "-Prefix", self.prefix.name,
             "-BaseUrl", release.base],
            capture_output=True, text=True, check=False,
        )  # fmt: skip

    def test_a_verified_asset_is_installed_and_a_mismatch_is_refused(self):
        name = "depsmith.exe" if os.name == "nt" else "depsmith"
        asset = tarball({name: PROGRAM})
        release = Release({f"depsmith-{self.target}.tar.gz": asset})
        self.addCleanup(release.close)
        result = self.install(release)
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertEqual(pathlib.Path(self.prefix.name, name).read_bytes(), PROGRAM)

        pathlib.Path(self.prefix.name, name).unlink()
        bad = Release(
            {f"depsmith-{self.target}.tar.gz": asset},
            sums=f"{'0' * 64}  depsmith-{self.target}.tar.gz\n",
        )
        self.addCleanup(bad.close)
        result = self.install(bad)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(pathlib.Path(self.prefix.name, name).exists())


if __name__ == "__main__":
    unittest.main()
