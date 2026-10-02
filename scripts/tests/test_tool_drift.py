"""The Latest tools report turns releases and results into bumps or issues."""

import importlib.util
import json
import pathlib
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "tool-drift.py"
spec = importlib.util.spec_from_file_location("tool_drift", SCRIPT)
drift = importlib.util.module_from_spec(spec)
# Dataclasses resolve postponed annotations through sys.modules.
sys.modules["tool_drift"] = drift
spec.loader.exec_module(drift)

NOW = drift.parse_time("2026-10-02T00:00:00Z")

RELEASES = (
    {"tag_name": "0.12.23", "published_at": "2026-10-01T00:00:00Z"},
    {
        "tag_name": "0.13.0rc1",
        "published_at": "2026-09-20T00:00:00Z",
        "prerelease": True,
    },
    {"tag_name": "0.12.22", "published_at": "2026-09-24T12:00:00Z"},
    # A backport published later than a newer release.
    {"tag_name": "0.11.9", "published_at": "2026-09-25T00:00:00Z"},
    {"tag_name": "0.12.21", "published_at": "2026-09-10T00:00:00Z"},
    {"tag_name": "0.12.24", "draft": True, "published_at": None},
)

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
PINS = {"uv": "0.12.15", "conda": "2.9.0", "conda-lock": "4.0.2"}
UV_URL = "https://github.com/astral-sh/uv/releases/download/0.12.15/uv.zip"


class CandidateTests(unittest.TestCase):
    def test_the_highest_settled_stable_release_is_eligible(self):
        self.assertEqual(drift.eligible("uv", RELEASES, NOW, days=7), "0.12.22")
        self.assertEqual(drift.eligible("uv", RELEASES, NOW, days=0), "0.12.23")
        self.assertIsNone(drift.eligible("uv", RELEASES, NOW, days=60))

    def test_versions_compare_numerically_and_never_move_back(self):
        self.assertTrue(drift.newer("0.12.10", "0.12.9"))
        self.assertFalse(drift.newer("0.11.9", "0.12.15"))
        self.assertEqual(
            drift.candidates(
                {"uv": "0.12.15", "pixi": "0.81.0", "conda": "2.9.0"},
                {"uv": "0.12.22", "pixi": "v0.80.0", "conda": "2.10.0-1"},
            ),
            {"uv": "0.12.22", "conda": "2.10.0-1"},
        )

    def test_pick_runs_runnable_installable_candidates_only(self):
        picked = drift.pick(
            {"uv": "0.12.15", "conda-lock": "4.0.2", "cargo": "1.98.1"},
            {"uv": "0.12.22", "conda-lock": "v4.1.0", "cargo": "1.99.0"},
            available=lambda tool, version: tool != "conda-lock",
        )
        self.assertEqual(
            picked, [{"tool": "uv", "version": "0.12.22", "tag": "0.12.22"}]
        )

    def test_release_tags_become_versions(self):
        self.assertEqual(drift.version_of("pixi", "v0.81.0"), "0.81.0")
        self.assertEqual(drift.version_of("conda", "2.10.0-1"), "2.10.0")


class ResultsTests(unittest.TestCase):
    def results(self, files):
        with tempfile.TemporaryDirectory() as directory:
            for name, content in files.items():
                pathlib.Path(directory, name).write_text(content + "\n")
            return drift.read_results(directory)

    def test_a_tool_fails_when_any_os_fails(self):
        results = self.results(
            {"uv-Linux": "success 0.12.22", "uv-macOS": "failure 0.12.22"}
        )
        self.assertEqual(results["uv"], ("failure", "0.12.22"))

    def test_only_runs_of_the_candidate_count(self):
        self.assertEqual(
            drift.judged(
                {"uv": ("success", "0.12.21"), "pixi": ("success", "0.81.0")},
                latest={"uv": "0.12.22", "pixi": "0.81.0", "cargo": "1.99.0"},
            ),
            {"pixi": "success"},
        )


class DecideTests(unittest.TestCase):
    def test_passing_candidates_bump_and_failing_ones_raise_issues(self):
        actions = drift.decide(
            tested={"uv": "0.12.15", "pixi": "0.80.0", "grype": "0.119.0"},
            latest={"uv": "0.12.22", "pixi": "0.81.0"},
            results={"uv": "success", "pixi": "failure"},
            mismatched={},
        )
        by_tool = {a.tool: a for a in actions}
        self.assertEqual(sorted(by_tool), ["pixi", "uv"])
        self.assertEqual(by_tool["uv"].kind, drift.BUMP)
        self.assertEqual(by_tool["uv"].title, "Bump uv to 0.12.22")
        self.assertEqual(by_tool["uv"].version, "0.12.22")
        self.assertEqual(by_tool["pixi"].kind, drift.BROKEN)
        self.assertIn("pixi 0.81.0", by_tool["pixi"].title)

    def test_checksum_drift_outranks_version_news(self):
        actions = drift.decide(
            tested={"uv": "0.12.15"},
            latest={"uv": "0.12.22"},
            results={"uv": "success"},
            mismatched={"uv": ["https://example/uv.zip"]},
        )
        self.assertEqual([(a.tool, a.kind) for a in actions], [("uv", drift.CHECKSUM)])
        self.assertIn("https://example/uv.zip", actions[0].body)

    def test_unrun_candidates_such_as_cargo_are_never_bumped(self):
        actions = drift.decide(
            tested={"cargo": "1.98.1"},
            latest={"cargo": "1.99.0"},
            results={},
            mismatched={},
        )
        self.assertEqual([(a.tool, a.kind) for a in actions], [("cargo", drift.BEHIND)])
        self.assertIn("not run", actions[0].body)

    def test_up_to_date_tools_need_nothing(self):
        self.assertEqual(drift.decide({"uv": "0.12.15"}, {}, {}, {}), [])

    def test_summary_lists_every_tool(self):
        text = drift.summary(
            tested={"uv": "0.12.15", "pixi": "0.80.0"},
            latest={"uv": "0.12.22"},
            results={"uv": "success"},
        )
        self.assertIn("| uv | 0.12.15 | 0.12.22 | success |", text)
        self.assertIn("| pixi | 0.80.0 | — | not run |", text)


class AssetTests(unittest.TestCase):
    def test_release_urls_name_repo_tag_and_asset(self):
        self.assertEqual(
            drift.release_asset(UV_URL), ("astral-sh/uv", "0.12.15", "uv.zip")
        )
        self.assertIsNone(drift.release_asset("https://example.com/uv.zip"))

    def test_pinned_assets_come_from_doctor(self):
        doctor = {
            "tools": [
                {
                    "tool": "uv",
                    "tested_versions": ["0.12.15"],
                    "downloads": [{"url": UV_URL, "sha256": "aa"}],
                },
                {"tool": "cargo", "tested_versions": ["1.98.1"]},
            ]
        }
        self.assertEqual(
            drift.tested_versions(doctor), {"uv": "0.12.15", "cargo": "1.98.1"}
        )
        self.assertEqual(drift.pinned_assets(doctor), {"uv": [(UV_URL, "aa")]})


class BumpTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.directory.name)
        conda = SPEC.replace('"uv"', '"conda"').replace("0.12.15", "2.9.0")
        for path, text in [
            ("crates/core/src/uv.rs", SPEC),
            ("crates/core/src/conda.rs", conda),
            ("crates/core/src/provision.rs", CATALOG),
            (".github/tool-versions.json", json.dumps(PINS)),
        ]:
            (self.root / path).parent.mkdir(parents=True, exist_ok=True)
            (self.root / path).write_text(text)

    def tearDown(self):
        self.directory.cleanup()

    def read(self, path):
        return (self.root / path).read_text()

    def test_a_bump_moves_spec_assets_and_pin_together(self):
        digests = {
            "uv-x86_64-unknown-linux-musl.tar.gz": "aa",
            "uv-x86_64-pc-windows-msvc.zip": "bb",
        }

        def digest(repo, tag, name):
            return digests.get(name)

        drift.bump(self.root, "uv", "0.12.15", "0.12.22", "0.12.22", digest)
        self.assertIn('vec!["0.12.22".into()]', self.read("crates/core/src/uv.rs"))
        catalog = self.read("crates/core/src/provision.rs")
        self.assertIn(
            'download/0.12.22/uv-x86_64-unknown-linux-musl.tar.gz", "aa"', catalog
        )
        self.assertIn('download/0.12.22/uv-x86_64-pc-windows-msvc.zip", "bb"', catalog)
        self.assertNotIn("0.12.15", catalog)
        self.assertIn("2.9.0-0", catalog)
        pins = json.loads(self.read(".github/tool-versions.json"))
        self.assertEqual(pins, {**PINS, "uv": "0.12.22"})

    def test_micromamba_tags_carry_a_build_number(self):
        seen = []

        def digest(repo, tag, name):
            seen.append((repo, tag, name))
            return "cc"

        drift.bump(self.root, "conda", "2.9.0", "2.10.0", "2.10.0-1", digest)
        self.assertEqual(
            seen,
            [("mamba-org/micromamba-releases", "2.10.0-1", "micromamba-linux-64")],
        )
        self.assertIn(
            'download/2.10.0-1/micromamba-linux-64", "cc"',
            self.read("crates/core/src/provision.rs"),
        )
        pins = json.loads(self.read(".github/tool-versions.json"))
        self.assertEqual(pins["conda"], "2.10.0")

    def test_a_missing_asset_or_digest_changes_nothing(self):
        paths = ("crates/core/src/uv.rs", "crates/core/src/provision.rs")
        before = {p: self.read(p) for p in paths}

        def digest(repo, tag, name):
            return "aa" if "linux" in name else None

        with self.assertRaisesRegex(drift.BumpError, "uv-x86_64-pc-windows-msvc.zip"):
            drift.bump(self.root, "uv", "0.12.15", "0.12.22", "0.12.22", digest)
        self.assertEqual(before, {p: self.read(p) for p in paths})

    def test_a_tool_without_downloads_bumps_its_spec_and_pin(self):
        (self.root / "crates/core/src/conda.rs").write_text(
            SPEC.replace('"uv"', '"conda-lock"').replace("0.12.15", "4.0.2")
        )
        drift.bump(self.root, "conda-lock", "4.0.2", "4.1.0", "v4.1.0", lambda *a: None)
        self.assertIn('vec!["4.1.0".into()]', self.read("crates/core/src/conda.rs"))
        pins = json.loads(self.read(".github/tool-versions.json"))
        self.assertEqual(pins["conda-lock"], "4.1.0")


class CommandLineTests(unittest.TestCase):
    def test_bad_arguments_print_usage(self):
        self.assertEqual(drift.main(["tool-drift.py"]), 2)
        self.assertEqual(drift.main(["tool-drift.py", "report", "doctor.json"]), 2)


if __name__ == "__main__":
    unittest.main()
