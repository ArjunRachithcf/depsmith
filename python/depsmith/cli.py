"""Console entry point delegating argument handling and execution to Rust."""
import sys
from . import _native

def main() -> int:
    return _native.cli(['depsmith', *sys.argv[1:]])
