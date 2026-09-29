"""Run the examples in the public API docstrings."""

import doctest
import unittest

import depsmith


def load_tests(loader, tests, ignore):
    tests.addTests(doctest.DocTestSuite(depsmith, optionflags=doctest.ELLIPSIS))
    return tests


if __name__ == "__main__":
    unittest.main()
