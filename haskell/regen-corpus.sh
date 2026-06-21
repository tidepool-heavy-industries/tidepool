#!/usr/bin/env bash
# Regenerate the real-Core corpus CBOR fixtures from haskell/test/corpus/Corpus.hs.
#
# REQUIRES the native-bignum extract binary: the gmp backend hits the __gmpn_* FFI
# wall on the Integer subset (no bug to surface), whereas the native backend
# compiles Integer arithmetic to pure Core — that is where #1 (roundingMode#) lives.
# Replay (the Rust corpus runner) is backend-agnostic once it's CBOR.
#
# A binding that can't be captured ("SKIPPED ... unresolved external") is a
# resolver coverage hole, NOT an accepted loss — this script FAILS on any skip.
#
# Env overrides (defaults target this worktree's native build):
#   TIDEPOOL_EXTRACT      native tidepool-extract-bin
#   TIDEPOOL_GHC_LIBDIR   that GHC's libdir
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

EXTRACT="${TIDEPOOL_EXTRACT:-$HERE/dist-newstyle/build/x86_64-linux/ghc-9.12.2/tidepool-harness-0.1.0.0/x/tidepool-extract-bin/build/tidepool-extract-bin/tidepool-extract-bin}"
export TIDEPOOL_GHC_LIBDIR="${TIDEPOOL_GHC_LIBDIR:-/nix/store/swcff7l71v3466rks25slabajwzrx51c-ghc-native-bignum-9.12.2/lib/ghc-9.12.2/lib}"

SRC="$HERE/test/corpus/Corpus.hs"
OUT="$HERE/test/corpus_cbor"

if [[ ! -x "$EXTRACT" ]]; then
  echo "FATAL: native extract binary not found/executable: $EXTRACT" >&2
  echo "Set TIDEPOOL_EXTRACT to the native-bignum tidepool-extract-bin." >&2
  exit 1
fi

rm -rf "$OUT"
mkdir -p "$OUT"

LOG="$(mktemp)"
"$EXTRACT" "$SRC" --all-closed --target-module-only --output-dir "$OUT" 2>&1 | tee "$LOG"

# Surface skips: an un-capturable corpus binding is a coverage hole.
if grep -qi "SKIPPED" "$LOG"; then
  echo "FATAL: corpus binding(s) SKIPPED (unresolved external) — coverage hole:" >&2
  grep -i "SKIPPED" "$LOG" >&2
  exit 1
fi

# Drop GHC-lifted local binders (`go_u6341068275337658369.cbor`): they are inlined
# into the bindings that use them, the runner filters them, and they are just
# repo noise. The kept program fixtures are self-contained (closed Core).
find "$OUT" -name '*_u[0-9]*.cbor' -delete

N="$(find "$OUT" -name '*.cbor' ! -name meta.cbor | wc -l | tr -d ' ')"
echo "Captured $N binding fixtures + meta.cbor into $OUT (0 skips, lifted-locals pruned)."

# Fixtures are gitignored (*.cbor) — force-add so the runner compiles on a fresh checkout.
git -C "$HERE/.." add -f "$OUT"/*.cbor
echo "git add -f'd corpus_cbor/*.cbor."
