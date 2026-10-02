#!/usr/bin/env bash
set -euo pipefail

bundle="$1"
source="$2"
test -x "$bundle/bin/tidepool-extract"
test -x "$bundle/bin/tidepool-extract-bin"
test -s "$bundle/share/exomonad/compiler-deployment.json"
test -n "${TIDEPOOL_GHC_LIBDIR:-}"

# Force a direct request to this package's worker instead of adopting any
# ambient resident daemon from the test runner.
unset TIDEPOOL_EXTRACT_DAEMON_SOCKET
unset TIDEPOOL_EXTRACT_WORKER

scratch="$(mktemp -d "${TMPDIR:-/tmp}/tidepool-worker-compile.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
cp "$source" "$scratch/WorkerSmoke.hs"

if ! "$bundle/bin/tidepool-extract" \
    --target answer \
    --output-dir "$scratch/out" \
    "$scratch/WorkerSmoke.hs" >"$scratch/stdout" 2>"$scratch/stderr"; then
  cat "$scratch/stdout"
  cat "$scratch/stderr" >&2
  exit 1
fi

artifact="$scratch/out/answer.prepared.cbor"
if [[ ! -s "$artifact" ]]; then
  echo "packaged worker did not write $artifact" >&2
  cat "$scratch/stdout"
  cat "$scratch/stderr" >&2
  exit 1
fi
