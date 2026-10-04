#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "usage: $0 TEST_BINARY FIXTURE_TREE [TASTY_ARGUMENTS...]" >&2
  exit 2
fi

test_binary=$(realpath -- "$1")
fixture_tree=$(realpath -- "$2")
shift 2
# Read configured authority; the Rust issuer still independently admits the
# exact frontend/worker deployment before issuing any certificate.
if [[ -n ${TIDEPOOL_CANDIDATE_FIXTURE_ISSUER:-} ]]; then
  export TIDEPOOL_COMPILER_PRODUCER
  TIDEPOOL_COMPILER_PRODUCER=$("$TIDEPOOL_TEST_PYTHON" - "$TIDEPOOL_COMPILER_DEPLOYMENT" <<'PYDEPLOY'
import json
import sys
from pathlib import Path
path = Path(sys.argv[1])
if path.stat().st_size > 4 << 20:
    raise SystemExit("deployment manifest exceeds metadata bound")
manifest = json.loads(path.read_text())
identity = manifest["producer_identity"]
if manifest["schema"] != 1 or len(identity) != 32 or any(type(value) is not int or not 0 <= value <= 255 for value in identity):
    raise SystemExit("invalid declared compiler producer identity")
print(bytes(identity).hex())
PYDEPLOY
  )
fi
test_root=$(mktemp -d "${TMPDIR:-/tmp}/tidepool-haskell-fixtures.XXXXXX")
trap 'rm -rf -- "$test_root"' EXIT
# Copy declared inputs so no test can write into Buck's source symlink tree.
cp -RL -- "$fixture_tree/." "$test_root/"
cd "$test_root"
"$test_binary" "$@"
