"""The docs-only guard accepts documentation edits and rejects anything else."""

import importlib.util
import pathlib
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "check-docs-only.py"
spec = importlib.util.spec_from_file_location("check_docs_only", SCRIPT)
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)

RUST = """//! Module.
/// Adds.
pub fn add(a: u8) -> u8 {
    let s = "// not a comment"; /* block /* nested */ */
    let r = r#"has "quotes" // and slashes"#;
    let c = '/'; let l: &'static str = "x";
    a + 1
}
"""


class TokenTests(unittest.TestCase):
    def test_rust_doc_and_comment_edits_are_equivalent(self):
        edited = RUST.replace("/// Adds.", "/// Adds one.\n/// More.").replace(
            "/* block /* nested */ */", "// other"
        )
        self.assertEqual(guard.rust_tokens(RUST), guard.rust_tokens(edited))

    def test_rust_string_contents_and_code_are_significant(self):
        for old, new in [
            ('"// not a comment"', '"// changed"'),
            ("a + 1", "a + 2"),
            ('r#"has "quotes" // and slashes"#', 'r#"has "quotes" // and"#'),
        ]:
            with self.subTest(new=new):
                self.assertNotEqual(
                    guard.rust_tokens(RUST), guard.rust_tokens(RUST.replace(old, new))
                )

    def test_python_docstrings_are_ignored_but_code_is_not(self):
        source = 'def f(x):\n    """Old."""\n    return x\n'
        documented = 'def f(x):\n    """New.\n\n    Args:\n        x: X.\n    """\n    return x\n'
        self.assertEqual(guard.python_code(source), guard.python_code(documented))
        self.assertNotEqual(
            guard.python_code(source),
            guard.python_code(source.replace("x\n", "x + 1\n")),
        )


class CommandTests(unittest.TestCase):
    def run_guard(self, files: dict[str, str], edits: dict[str, str]) -> int:
        with tempfile.TemporaryDirectory() as repo:

            def git(*args):
                subprocess.run(
                    ["git", "-C", repo, *args], check=True, capture_output=True
                )

            git("init", "-q")
            git("config", "user.email", "t@example.invalid")
            git("config", "user.name", "t")
            git("config", "commit.gpgsign", "false")
            for name, text in files.items():
                path = pathlib.Path(repo, name)
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text)
            git("add", "-A")
            git("commit", "-qm", "base")
            for name, text in edits.items():
                pathlib.Path(repo, name).write_text(text)
            return subprocess.run(
                [sys.executable, str(SCRIPT), "HEAD"],
                cwd=repo,
                capture_output=True,
                check=False,
            ).returncode

    def test_documentation_only_worktree_passes(self):
        edits = {"src/lib.rs": RUST.replace("/// Adds.", "/// Adds one.")}
        self.assertEqual(self.run_guard({"src/lib.rs": RUST}, edits), 0)

    def test_code_change_fails(self):
        edits = {"src/lib.rs": RUST.replace("a + 1", "a + 2")}
        self.assertEqual(self.run_guard({"src/lib.rs": RUST}, edits), 1)

    def test_other_file_change_fails_but_generated_reference_is_allowed(self):
        files = {"README.md": "a\n", "docs/wiki/Python-API.md": "a\n"}
        self.assertEqual(self.run_guard(files, {"README.md": "b\n"}), 1)
        self.assertEqual(self.run_guard(files, {"docs/wiki/Python-API.md": "b\n"}), 0)


if __name__ == "__main__":
    unittest.main()
