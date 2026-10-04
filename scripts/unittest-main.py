"""Adapt standard unittest discovery to the native nonzero test contract."""
import sys
import unittest


def main(names):
    if not names:
        raise ValueError("declare at least one unittest module")
    suite = unittest.defaultTestLoader.loadTestsFromNames(names)
    count = suite.countTestCases()
    if count == 0:
        raise ValueError("native unittest target selected zero tests")
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except ValueError as error:
        print(error, file=sys.stderr)
        sys.exit(2)
