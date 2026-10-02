"""The release install scripts verify the asset against SHA256SUMS first."""

import hashlib
import http.server
import io
import json
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


def sums_of(assets):
    return "".join(
        f"{hashlib.sha256(content).hexdigest()}  {name}\n"
        for name, content in assets.items()
    )


class Release:
    """Fake GitHub releases on 127.0.0.1: `<base>/download/<tag>/<asset>`,
    the same assets at `<base>/latest/download/<asset>`, and a releases API
    listing `tag` at `<base>/api`."""

    def __init__(self, assets, sums=None, tag=TAG):
        self.directory = tempfile.TemporaryDirectory()
        root = pathlib.Path(self.directory.name)
        sums = sums_of(assets) if sums is None else sums
        for folder in (root / "download" / tag, root / "latest" / "download"):
            folder.mkdir(parents=True)
            for name, content in assets.items():
                (folder / name).write_bytes(content)
            (folder / "SHA256SUMS").write_text(sums)
        (root / "api").write_text(json.dumps([{"tag_name": tag}]))
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

    def release(self, assets=None, **kwargs):
        if assets is None:
            assets = {f"depsmith-{TARGET}.tar.gz": tarball({"depsmith": PROGRAM})}
        release = Release(assets, **kwargs)
        self.addCleanup(release.close)
        return release

    def install(self, release, *args, env=None):
        return subprocess.run(
            ["sh", str(SCRIPTS / "install.sh"), "--prefix", self.prefix.name,
             "--base-url", release.base, *args],
            capture_output=True, text=True, check=False,
            env={**os.environ, "DEPSMITH_INSTALL_API": f"{release.base}/api", **(env or {})},
        )  # fmt: skip

    def installed(self):
        return pathlib.Path(self.prefix.name, "depsmith")

    def test_a_verified_release_is_installed(self):
        result = self.install(self.release(), "--version", TAG, "--target", TARGET)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.installed().read_bytes(), PROGRAM)
        self.assertTrue(os.access(self.installed(), os.X_OK))
        self.assertIn(f"Installed depsmith {TAG}", result.stdout)

    def test_the_latest_release_needs_no_api(self):
        result = self.install(
            self.release(),
            "--target",
            TARGET,
            env={"DEPSMITH_INSTALL_API": "http://127.0.0.1:9/none"},
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Installed depsmith latest", result.stdout)

    def test_pre_takes_the_newest_tag_from_the_api(self):
        release = self.release(tag="v9.9.9-rc1")
        result = self.install(release, "--pre", "--target", TARGET)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Installed depsmith v9.9.9-rc1", result.stdout)

    def test_the_platform_maps_to_its_target(self):
        fake = tempfile.TemporaryDirectory()
        self.addCleanup(fake.cleanup)
        uname = pathlib.Path(fake.name, "uname")
        uname.write_text(
            '#!/bin/sh\ncase "$1" in -s) echo Darwin;; -m) echo arm64;; esac\n'
        )
        uname.chmod(0o755)
        assets = {
            "depsmith-aarch64-apple-darwin.tar.gz": tarball({"depsmith": PROGRAM})
        }
        result = self.install(
            self.release(assets), "--version", TAG,
            env={"PATH": f"{fake.name}{os.pathsep}{os.environ['PATH']}"},
        )  # fmt: skip
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("(aarch64-apple-darwin)", result.stdout)

    def test_a_checksum_mismatch_installs_nothing(self):
        asset = {f"depsmith-{TARGET}.tar.gz": tarball({"depsmith": PROGRAM})}
        bad = f"{'0' * 64}  depsmith-{TARGET}.tar.gz\n"
        result = self.install(
            self.release(asset, sums=bad), "--version", TAG, "--target", TARGET
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("checksum", result.stderr.lower())
        self.assertFalse(self.installed().exists())

    def test_an_asset_missing_from_the_sums_is_refused(self):
        result = self.install(
            self.release(sums=""), "--version", TAG, "--target", TARGET
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("SHA256SUMS", result.stderr)
        self.assertFalse(self.installed().exists())

    def test_an_unsupported_platform_points_elsewhere(self):
        result = self.install(self.release({}), "--target", "riscv64-unknown-linux-gnu")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cargo install depsmith", result.stderr)

    def test_help_works_when_piped(self):
        script = (SCRIPTS / "install.sh").read_text()
        result = subprocess.run(
            ["sh", "-s", "--", "--help"], input=script, capture_output=True,
            text=True, check=False,
        )  # fmt: skip
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--prefix DIR", result.stdout)


class PowerShellScriptTests(unittest.TestCase):
    target = "x86_64-pc-windows-msvc" if os.name == "nt" else TARGET
    name = "depsmith.exe" if os.name == "nt" else "depsmith"

    def setUp(self):
        self.pwsh = shutil.which("pwsh")
        if not self.pwsh:
            self.skipTest("install.ps1 needs PowerShell (pwsh)")
        self.prefix = tempfile.TemporaryDirectory()
        self.addCleanup(self.prefix.cleanup)

    def release(self, sums=None):
        assets = {f"depsmith-{self.target}.tar.gz": tarball({self.name: PROGRAM})}
        release = Release(assets, sums=sums)
        self.addCleanup(release.close)
        return release

    def run_ps(self, command):
        return subprocess.run(
            [self.pwsh, "-NoProfile", "-NonInteractive", "-Command", command],
            capture_output=True, text=True, check=False,
        )  # fmt: skip

    def invocation(self, release):
        """Run the script text as `irm ... | iex` would, passing parameters
        through a scriptblock as the docs show."""
        script = str(SCRIPTS / "install.ps1").replace("'", "''")
        return (
            f"& ([scriptblock]::Create((Get-Content -Raw '{script}'))) "
            f"-Version {TAG} -Target {self.target} -Prefix '{self.prefix.name}' "
            f"-BaseUrl {release.base}"
        )

    def test_a_verified_release_is_installed(self):
        result = self.run_ps(self.invocation(self.release()))
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        installed = pathlib.Path(self.prefix.name, self.name)
        self.assertEqual(installed.read_bytes(), PROGRAM)

    def test_a_failure_throws_without_ending_the_session(self):
        bad = f"{'0' * 64}  depsmith-{self.target}.tar.gz\n"
        command = (
            f"try {{ {self.invocation(self.release(sums=bad))} }} "
            "catch { Write-Output \"caught: $_\" }; Write-Output 'session alive'"
        )
        result = self.run_ps(command)
        self.assertIn("caught: checksum mismatch", result.stdout, result.stderr)
        self.assertIn("session alive", result.stdout)
        self.assertFalse(pathlib.Path(self.prefix.name, self.name).exists())


if __name__ == "__main__":
    unittest.main()
