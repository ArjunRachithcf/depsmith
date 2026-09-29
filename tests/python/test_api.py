import os
import pathlib
import tempfile
import unittest

import depsmith as updater


class ApiTests(unittest.TestCase):
    @unittest.skipUnless(
        os.environ.get("PIXI"), "set PIXI to run the native integration"
    )
    def test_native_pixi_prepare_apply_and_recheck(self):
        import sys

        platform = {"linux": "linux-64", "win32": "win-64", "darwin": "osx-64"}[
            sys.platform
        ]
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "pixi.toml").write_text(
                f'[workspace]\nname="smoke"\nchannels=[]\nplatforms=["{platform}"]\n'
            )
            options = updater.UpdateOptions(pixi=os.environ["PIXI"])
            proposal = updater.prepare(
                root, targets=["pixi:pixi.toml"], options=options
            )
            self.assertEqual(proposal.failures, ())
            self.assertFalse((root / "pixi.lock").exists())
            self.assertEqual(proposal.apply().applied, ("pixi.lock",))
            result = updater.prepare(root, targets=["pixi:pixi.toml"], options=options)
            self.assertEqual(result.failures, ())
            self.assertEqual(result.changes, ())

    def test_discovery_returns_typed_targets(self):
        with tempfile.TemporaryDirectory() as directory:
            pathlib.Path(directory, "pixi.toml").write_text(
                '[workspace]\nname="demo"\n'
            )
            targets = updater.discover(directory)
            self.assertIsInstance(targets[0], updater.Target)
            self.assertEqual(targets[0].id, "pixi:pixi.toml")

    def test_invalid_selection_raises_without_terminating_interpreter(self):
        with (
            tempfile.TemporaryDirectory() as directory,
            self.assertRaises(updater.ConfigurationError),
        ):
            updater.prepare(directory, targets=["missing"])

    def test_doctor_reports_adapter_capabilities(self):
        report = updater.doctor(
            options=updater.UpdateOptions(
                pixi="missing-pixi-binary", grype="missing-grype-binary"
            )
        )
        pixi = next(a for a in report["adapters"] if a["manager"] == "pixi")
        self.assertEqual(pixi["cooldown"]["status"], "unsupported")
        self.assertEqual({t["status"] for t in report["tools"]}, {"unavailable"})

    def test_undeclared_package_selection_raises_before_backend(self):
        with tempfile.TemporaryDirectory() as directory:
            pathlib.Path(directory, "pixi.toml").write_text(
                '[workspace]\nname="demo"\n[dependencies]\nruff="*"\n'
            )
            options = updater.UpdateOptions(
                pixi="missing-pixi-binary", packages=["ruf"]
            )
            with self.assertRaisesRegex(updater.ConfigurationError, "ruf"):
                updater.prepare(directory, targets=["pixi:pixi.toml"], options=options)

    def test_malformed_accept_raises_configuration_error(self):
        with tempfile.TemporaryDirectory() as directory:
            pathlib.Path(directory, "pixi.toml").write_text(
                '[workspace]\nname="demo"\n[dependencies]\nruff="==1"\n'
            )
            options = updater.UpdateOptions(pixi="missing-pixi-binary", accept=[">=1"])
            with self.assertRaisesRegex(updater.ConfigurationError, "NAME=REQUIREMENT"):
                updater.prepare(directory, targets=["pixi:pixi.toml"], options=options)

    def test_unscoped_suppression_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            pathlib.Path(directory, "pixi.toml").write_text(
                '[workspace]\nname="demo"\n'
            )
            unscoped = updater.Suppression(
                id="GHSA-v845-jxx5-vc9f", reason="accepted risk"
            )
            options = updater.UpdateOptions(
                pixi="missing-pixi-binary", suppressions=[unscoped]
            )
            with self.assertRaisesRegex(updater.ConfigurationError, "scope"):
                updater.prepare(directory, targets=["pixi:pixi.toml"], options=options)

    def test_backend_failure_is_a_typed_result(self):
        with tempfile.TemporaryDirectory() as directory:
            pathlib.Path(directory, "pixi.toml").write_text(
                '[workspace]\nname="demo"\n'
            )
            proposal = updater.prepare(
                directory,
                targets=["pixi:pixi.toml"],
                options=updater.UpdateOptions(pixi="missing-pixi-binary"),
            )
            self.assertEqual(len(proposal.failures), 1)
            self.assertEqual(proposal.changes, ())
            self.assertEqual(proposal.unresolved, ())
            with self.assertRaises(updater.OperationError):
                proposal.apply()


if __name__ == "__main__":
    unittest.main()
