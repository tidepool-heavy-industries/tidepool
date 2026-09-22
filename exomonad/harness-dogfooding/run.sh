#!/usr/bin/env bash
# The selfharness dogfood launcher is retained as historical source only.
# Its binary and operator web surface were retired from the supported build.
set -euo pipefail

echo "exomonad/harness-dogfooding/run.sh is retired: tidepool-selfharness is not a supported binary" >&2
echo "Use the supported Shoal entrypoints from docs/GETTING-STARTED.md." >&2
exit 2
