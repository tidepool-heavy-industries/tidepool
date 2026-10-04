#!/usr/bin/env bash
set -euo pipefail
cd "$TIDEPOOL_SCRIPT_TEST_ROOT"
exec "$TIDEPOOL_TEST_PYTHON" scripts/unittest-main.py "$@"
