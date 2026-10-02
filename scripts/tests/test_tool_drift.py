"""The latest-tools report turns versions and results into per-tool issues."""

import importlib.util
import pathlib
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "tool-drift.py"
spec = importlib.util.spec_from_file_location("tool_drift", SCRIPT)
drift = importlib.util.module_from_spec(spec)
spec.loader.exec_module(drift)

DOCTOR = {
    "tools": [
        {
            "tool": "uv",
            "tested_versions": ["0.12.15"],
            "downloads": [
                {
                    "url": "https://github.com/astral-sh/uv/releases/download/0.12.15/uv-x86_64-pc-windows-msvc.zip",
                    "sha256": "aa",
                }
            ],
        },
        {"tool": "pixi", "tested_versions": ["0.80.0"]},
        {"tool": "grype", "tested_versions": ["0.119.0"], "downloads": []},
    ],
}


class DecideTests(unittest.TestCase):
    def test_each_tool_gets_at_most_one_issue_of_the_right_kind(self):
        issues = drift.decide(
            tested=drift.tested_versions(DOCTOR),
            latest={"uv": "0.12.21", "pixi": "0.81.0", "grype": "0.119.0"},
            results={"uv": "success", "pixi": "failure", "grype": "success"},
            mismatched={"uv": []},
        )
        by_tool = {i.tool: i for i in issues}
        self.assertEqual(sorted(by_tool), ["pixi", "uv"])
        self.assertEqual(by_tool["pixi"].kind, "broken")
        self.assertIn("pixi 0.81.0", by_tool["pixi"].title)
        self.assertEqual(by_tool["uv"].kind, "bump")
        self.assertEqual(by_tool["uv"].title, "Bump uv to 0.12.21")
        self.assertEqual(by_tool["uv"].version, "0.12.21")
        self.assertIn("0.12.15", by_tool["uv"].body)

    def test_checksum_drift_outranks_version_news(self):
        issues = drift.decide(
            tested={"uv": "0.12.15"},
            latest={"uv": "0.12.21"},
            results={"uv": "success"},
            mismatched={"uv": ["https://example/uv.zip"]},
        )
        self.assertEqual([(i.tool, i.kind) for i in issues], [("uv", "checksum")])
        self.assertIn("https://example/uv.zip", issues[0].body)

    def test_up_to_date_passing_tools_are_resolved(self):
        self.assertEqual(
            drift.decide(
                tested={"uv": "0.12.15"},
                latest={"uv": "0.12.15"},
                results={"uv": "success"},
                mismatched={},
            ),
            [],
        )

    def test_a_tool_without_a_run_is_not_judged(self):
        issues = drift.decide(
            tested={"cargo": "1.98.1"},
            latest={"cargo": "1.99.0"},
            results={},
            mismatched={},
        )
        self.assertEqual([(i.tool, i.kind) for i in issues], [("cargo", "behind")])
        self.assertIn("not run", issues[0].body)


SPEC = """            tools: vec![ToolSpec {
                name: "uv".into(),
                default: "uv".into(),
                tested_versions: vec!["0.12.15".into()],
"""
CATALOG = """const UV: &[Asset] = &[
    ("linux", "x86_64", "https://github.com/astral-sh/uv/releases/download/0.12.15/uv-x86_64-unknown-linux-musl.tar.gz", "11", Archive::TarGz, "uv-x86_64-unknown-linux-musl/uv"),
    ("windows", "x86_64", "https://github.com/astral-sh/uv/releases/download/0.12.15/uv-x86_64-pc-windows-msvc.zip", "22", Archive::Zip, "uv.exe"),
];

const MICROMAMBA: &[Asset] = &[
    ("linux", "x86_64", "https://github.com/mamba-org/micromamba-releases/releases/download/2.9.0-0/micromamba-linux-64", "33", Archive::Binary, "micromamba"),
];
"""
WORKFLOW = """env:
  UV_VERSION: ${{ inputs.latest-tool == 'uv' && 'latest' || '0.12.15' }}
  MICROMAMBA_PIN: ${{ inputs.latest-tool != 'conda' && '==2.9.0' || '' }}
"""


class BumpTests(unittest.TestCase):
    def repo(self):
        import tempfile

        directory = tempfile.TemporaryDirectory()
        root = pathlib.Path(directory.name)
        for path, text in [
            ("crates/core/src/uv.rs", SPEC),
            (
                "crates/core/src/conda.rs",
                SPEC.replace('"uv"', '"conda"').replace("0.12.15", "2.9.0"),
            ),
            ("crates/core/src/provision.rs", CATALOG),
            (".github/workflows/integration.yml", WORKFLOW),
        ]:
            (root / path).parent.mkdir(parents=True, exist_ok=True)
            (root / path).write_text(text)
        return directory, root

    def test_a_bump_moves_spec_assets_and_pin_together(self):
        directory, root = self.repo()
        with directory:
            digests = {
                "uv-x86_64-unknown-linux-musl.tar.gz": "aa",
                "uv-x86_64-pc-windows-msvc.zip": "bb",
            }
            drift.bump(
                root,
                "uv",
                "0.12.15",
                "0.12.21",
                "0.12.21",
                lambda repo, tag, name: digests.get(name),
            )
            self.assertIn(
                'vec!["0.12.21".into()]', (root / "crates/core/src/uv.rs").read_text()
            )
            catalog = (root / "crates/core/src/provision.rs").read_text()
            self.assertIn(
                'download/0.12.21/uv-x86_64-unknown-linux-musl.tar.gz", "aa"', catalog
            )
            self.assertIn(
                'download/0.12.21/uv-x86_64-pc-windows-msvc.zip", "bb"', catalog
            )
            self.assertNotIn("0.12.15", catalog)
            self.assertIn("2.9.0-0", catalog)
            self.assertIn(
                "|| '0.12.21' }}",
                (root / ".github/workflows/integration.yml").read_text(),
            )

    def test_micromamba_tags_carry_a_build_number(self):
        directory, root = self.repo()
        with directory:
            seen = []
            drift.bump(
                root,
                "conda",
                "2.9.0",
                "2.10.0",
                "2.10.0-1",
                lambda repo, tag, name: seen.append((repo, tag, name)) or "cc",
            )
            self.assertEqual(
                seen,
                [("mamba-org/micromamba-releases", "2.10.0-1", "micromamba-linux-64")],
            )
            self.assertIn(
                'download/2.10.0-1/micromamba-linux-64", "cc"',
                (root / "crates/core/src/provision.rs").read_text(),
            )
            self.assertIn(
                "'==2.10.0'", (root / ".github/workflows/integration.yml").read_text()
            )

    def test_a_missing_asset_stops_the_bump(self):
        directory, root = self.repo()
        with (
            directory,
            self.assertRaisesRegex(drift.BumpError, "uv-x86_64-pc-windows-msvc.zip"),
        ):
            drift.bump(
                root,
                "uv",
                "0.12.15",
                "0.12.21",
                "0.12.21",
                lambda repo, tag, name: "aa" if "linux" in name else None,
            )


class ResultsTests(unittest.TestCase):
    def results(self, files):
        import tempfile

        with tempfile.TemporaryDirectory() as directory:
            for name, status in files.items():
                pathlib.Path(directory, name).write_text(status + "\n")
            return drift.read_results(directory)

    def test_a_tool_fails_when_any_os_fails(self):
        results = self.results(
            {"uv-Linux": "success", "uv-macOS": "failure", "pixi-Linux": "success"}
        )
        self.assertEqual(results["uv"], "failure")
        self.assertEqual(results["pixi"], "success")

    def test_cargo_is_judged_only_when_every_run_agrees(self):
        self.assertNotIn(
            "cargo", self.results({"uv-Linux": "failure", "pixi-Linux": "success"})
        )
        self.assertEqual(
            self.results({"uv-Linux": "failure", "pixi-Linux": "failure"})["cargo"],
            "failure",
        )
        self.assertEqual(
            self.results({"uv-Linux": "success", "pixi-Linux": "success"})["cargo"],
            "success",
        )


class ParsingTests(unittest.TestCase):
    def test_release_tags_become_versions(self):
        self.assertEqual(drift.version_of("pixi", "v0.81.0"), "0.81.0")
        self.assertEqual(drift.version_of("conda", "2.10.0-1"), "2.10.0")
        self.assertEqual(drift.version_of("uv", "0.12.21"), "0.12.21")

    def test_pinned_assets_name_their_release(self):
        self.assertEqual(
            drift.pinned_assets(DOCTOR),
            {
                "uv": [
                    (
                        "astral-sh/uv",
                        "0.12.15",
                        "uv-x86_64-pc-windows-msvc.zip",
                        "aa",
                        "https://github.com/astral-sh/uv/releases/download/0.12.15/uv-x86_64-pc-windows-msvc.zip",
                    )
                ]
            },
        )

    def test_summary_lists_every_tool(self):
        text = drift.summary(
            tested={"uv": "0.12.15", "pixi": "0.80.0"},
            latest={"uv": "0.12.21", "pixi": "0.80.0"},
            results={"uv": "success", "pixi": "success"},
        )
        self.assertIn("| uv | 0.12.15 | 0.12.21 | success |", text)
        self.assertIn("| pixi | 0.80.0 | 0.80.0 | success |", text)


if __name__ == "__main__":
    unittest.main()
