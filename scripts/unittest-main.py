"""Adapt standard unittest discovery to the native nonzero test contract."""
import os
from pathlib import Path
import sys
import unittest


def main(names):
    if not names:
        raise ValueError("declare at least one unittest module")
    resource_root = os.environ.get("TIDEPOOL_SCRIPT_TEST_ROOT")
    if not resource_root:
        raise ValueError("declare TIDEPOOL_SCRIPT_TEST_ROOT for unittest imports")
    resource_root = Path(resource_root).resolve(strict=True)
    if not (resource_root / "scripts").is_dir():
        raise ValueError("unittest resource root has no declared scripts package")
    # Executing scripts/unittest-main.py otherwise puts only scripts/ on the
    # import path. The declared snapshot root owns package/module imports.
    sys.path.insert(0, str(resource_root))
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
