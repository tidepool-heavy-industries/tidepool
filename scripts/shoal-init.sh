#!/usr/bin/env bash
# One-command local Shoal bootstrap. Always use a matched worktree-built
# extractor frontend and Haskell compiler worker; an older installed wrapper
# must not leak into actor-policy compilation through PATH or inherited env.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

# `just` preserves its option separator as the first variadic recipe
# argument. It separates Just flags, not Shoal's clap input.
if [[ "${1:-}" == "--" ]]; then
  shift
fi

unset TIDEPOOL_EXTRACT
unset TIDEPOOL_EXTRACT_WORKER
unset TIDEPOOL_EXTRACT_DAEMON_SOCKET

source scripts/lib-extract.sh
resolve_tidepool_extract

echo "==> validating the local extractor/compiler endpoint"
probe_dir="$(mktemp -d -t shoal-endpoint-probe.XXXXXX)"
trap 'rm -rf "$probe_dir"' EXIT
# A bound endpoint writes its identity before reading the framed request. EOF
# after that identity is intentionally an incomplete request, so validate the
# fixed magic rather than treating the later EOF status as the probe result.
"$TIDEPOOL_EXTRACT" --compiler-endpoint-v1 \
  </dev/null >"$probe_dir/identity" 2>"$probe_dir/stderr" || true
endpoint_magic="$(od -An -tx1 -N8 "$probe_dir/identity" | tr -d '[:space:]')"
if [[ "$endpoint_magic" != "5450434944303031" ]]; then
  cat "$probe_dir/stderr" >&2
  echo "error: local tidepool-extract did not publish the TPCID001 compiler endpoint identity" >&2
  exit 1
fi
rm -rf "$probe_dir"
trap - EXIT

echo "==> building Shoal"
cargo build -p tidepool --bin shoal

exec "$PWD/target/debug/shoal" init "$@"
