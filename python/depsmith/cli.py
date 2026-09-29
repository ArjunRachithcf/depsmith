"""Console entry point delegating argument handling and execution to Rust."""

import sys

from . import _native


def main() -> int:
    """Run the ``depsmith`` command with the process arguments and return its exit status."""
    return _native.cli(["depsmith", *sys.argv[1:]])
