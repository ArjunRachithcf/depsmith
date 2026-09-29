"""The CLI (`python -m depsmith ... --json`) and the Python API give equal results."""

import dataclasses
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest

import depsmith as updater

PIXI_MANIFEST = (
    '[workspace]\nname = "parity"\nchannels = ["conda-forge"]\n'
    'platforms = ["linux-64"]\n[dependencies]\npython = "3.12.*"\n'
)


def cli(*args: str) -> tuple[int, dict]:
    # A subprocess: the CLI installs process-wide interrupt handlers.
    result = subprocess.run(
        [sys.executable, "-m", "depsmith", *args, "--non-interactive", "--json"],
        capture_output=True,
        text=True,
        check=False,
    )
    return result.returncode, json.loads(result.stdout)


class ParityTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()  # enterContext needs 3.11
        self.addCleanup(directory.cleanup)
        self.root = pathlib.Path(directory.name)
        (self.root / "pixi.toml").write_text(PIXI_MANIFEST)

    def test_discovery(self):
        code, report = cli("discover", "--root", str(self.root))
        self.assertEqual(code, 0)
        api = [dataclasses.asdict(t) for t in updater.discover(self.root)]
        self.assertEqual(report["targets"], api)

    def test_doctor(self):
        code, report = cli("doctor", "--root", str(self.root))
        self.assertEqual(code, 0)
        self.assertEqual(report, updater.doctor(self.root))

    def test_backend_failure_proposal(self):
        args = ["--target", "pixi:pixi.toml", "--pixi", "missing-pixi-binary"]
        code, report = cli("check", "--root", str(self.root), *args)
        proposal = updater.prepare(
            self.root,
            targets=["pixi:pixi.toml"],
            options=updater.UpdateOptions(pixi="missing-pixi-binary"),
        )
        self.assertEqual(report["proposal"], proposal.to_dict())
        self.assertEqual(code, 3)
        self.assertEqual([f.code for f in proposal.failures], [3])

    def test_errors_map_to_exit_codes_and_exception_types(self):
        cases = [
            (
                ("check", "--root", str(self.root), "--target", "missing"),
                lambda: updater.prepare(self.root, targets=["missing"]),
            ),
            (("recover", "--root", str(self.root)), lambda: updater.recover(self.root)),
        ]
        for args, call in cases:
            with self.subTest(command=args[0]):
                code, report = cli(*args)
                self.assertEqual(code, 2)
                with self.assertRaises(updater.ConfigurationError) as raised:
                    call()
                self.assertEqual(report["error"], str(raised.exception))

    def test_tool_paths(self):
        args = ["--target", "pixi:pixi.toml", "--tool", "pixi=missing-pixi-binary"]
        code, report = cli("check", "--root", str(self.root), *args)
        proposal = updater.prepare(
            self.root,
            targets=["pixi:pixi.toml"],
            options=updater.UpdateOptions(tools={"pixi": "missing-pixi-binary"}),
        )
        self.assertEqual(report["proposal"], proposal.to_dict())
        self.assertIn("missing-pixi-binary", proposal.failures[0].message)
        code, report = cli("check", "--root", str(self.root), "--tool", "pixi")
        self.assertEqual(code, 2)
        self.assertIn("NAME=PATH", report["error"])

    @unittest.skipUnless(os.environ.get("PIXI"), "set PIXI to run the native parity")
    def test_native_preview(self):
        pixi = os.environ["PIXI"]
        args = ["--target", "pixi:pixi.toml", "--pixi", pixi]
        code, report = cli("check", "--root", str(self.root), *args)
        proposal = updater.prepare(
            self.root,
            targets=["pixi:pixi.toml"],
            options=updater.UpdateOptions(pixi=pixi),
        )
        self.assertEqual(code, 1, report)
        self.assertEqual(report["proposal"], proposal.to_dict())


if __name__ == "__main__":
    unittest.main()
