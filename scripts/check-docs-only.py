"""Fail unless a change touches only documentation.

Usage: python scripts/check-docs-only.py BASE [HEAD]

Compares BASE with HEAD (or with the working tree when HEAD is omitted). Rust
files may differ only in comments, Python files only in docstrings, and the
generated wiki reference pages may change freely. Any other difference,
including added, deleted or renamed files, fails with exit 1.
"""

import ast
import re
import subprocess
import sys
from pathlib import Path

GENERATED = {"docs/wiki/CLI-reference.md", "docs/wiki/Python-API.md"}
RAW_STRING = re.compile(r'b?r(#*)"')
STRING = re.compile(r'b?"(?:\\.|[^"\\])*"', re.DOTALL)
CHAR = re.compile(r"b?'(?:\\(?:u\{[0-9a-fA-F]+\}|x[0-9a-fA-F]{2}|.)|[^\\'\n])'")
WORDS = re.compile(r"\w+|\S")


def rust_tokens(source: str) -> list[str]:
    """Tokens of Rust source with comments removed and whitespace ignored.

    String, raw-string and character literals are kept whole, so comment
    markers inside them stay significant.
    """
    tokens: list[str] = []
    code: list[str] = []
    i = 0

    def flush() -> None:
        tokens.extend(WORDS.findall("".join(code)))
        code.clear()

    while i < len(source):
        previous = source[i - 1] if i else " "
        at_word_start = not (previous.isalnum() or previous == "_")
        if source.startswith("//", i):
            flush()
            end = source.find("\n", i)
            i = len(source) if end < 0 else end
        elif source.startswith("/*", i):
            flush()
            depth, i = 1, i + 2
            while i < len(source) and depth:
                if source.startswith("/*", i):
                    depth, i = depth + 1, i + 2
                elif source.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
        elif at_word_start and (raw := RAW_STRING.match(source, i)):
            flush()
            closing = '"' + raw.group(1)
            end = source.find(closing, raw.end())
            end = len(source) if end < 0 else end + len(closing)
            tokens.append(source[i:end])
            i = end
        elif at_word_start and (
            literal := STRING.match(source, i) or CHAR.match(source, i)
        ):
            flush()
            tokens.append(literal.group())
            i = literal.end()
        else:
            code.append(source[i])
            i += 1
    flush()
    return tokens


def python_code(source: str) -> str:
    """The AST of Python source with every docstring removed."""
    tree = ast.parse(source)
    for node in ast.walk(tree):
        body = getattr(node, "body", None)
        if isinstance(body, list):
            kept = [
                statement
                for statement in body
                if not (
                    isinstance(statement, ast.Expr)
                    and isinstance(statement.value, ast.Constant)
                    and isinstance(statement.value.value, str)
                )
            ]
            node.body = kept or [ast.Pass()]
    return ast.dump(tree)


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], capture_output=True, text=True, check=True
    ).stdout


def content(revision: str | None, path: str) -> str:
    if revision is None:
        return Path(path).read_text()
    return git("show", f"{revision}:{path}")


def problems(base: str, head: str | None) -> list[str]:
    revisions = [base] if head is None else [base, head]
    found = []
    for line in git("diff", "--name-status", *revisions).splitlines():
        status, path = line.split("\t", 1)
        if path in GENERATED and status == "M":
            continue
        if status != "M":
            found.append(f"{path}: file {status} (only modifications are allowed)")
            continue
        old, new = content(base, path), content(head, path)
        if path.endswith(".rs"):
            same = rust_tokens(old) == rust_tokens(new)
        elif path.endswith((".py", ".pyi")):
            same = python_code(old) == python_code(new)
        else:
            same = False
        if not same:
            found.append(f"{path}: changes outside documentation")
    if head is None:
        for path in git("ls-files", "--others", "--exclude-standard").splitlines():
            found.append(f"{path}: new file (only modifications are allowed)")
    return found


def main() -> int:
    if not 2 <= len(sys.argv) <= 3:
        print(__doc__, file=sys.stderr)
        return 2
    found = problems(sys.argv[1], sys.argv[2] if len(sys.argv) == 3 else None)
    for problem in found:
        print(problem, file=sys.stderr)
    return 1 if found else 0


if __name__ == "__main__":
    raise SystemExit(main())
