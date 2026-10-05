#!/usr/bin/env bash
set -euo pipefail
TIDEPOOL_SCRIPT_TEST_ROOT=$(realpath -e -- "$TIDEPOOL_SCRIPT_TEST_ROOT")
export TIDEPOOL_SCRIPT_TEST_ROOT
# Resolve explicitly declared resource variables before entering the source tree.
for resource_name in ${TIDEPOOL_SCRIPT_RESOURCE_ENV:-}; do
  resource_path=$(realpath -e -- "${!resource_name}")
  export "$resource_name=$resource_path"
done
cd "$TIDEPOOL_SCRIPT_TEST_ROOT"
exec "$TIDEPOOL_TEST_PYTHON" scripts/unittest-main.py "$@"
