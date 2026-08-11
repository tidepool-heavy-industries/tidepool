#!/usr/bin/env bash
# Standing turn-latency instrument: ONE command producing ONE table of the
# per-turn latency numbers every perf lane needs as its receipt.
#
# Uses ONLY existing machinery — no new subsystem. The timing attribution
# vocabulary is `tidepool-runtime/src/timing.rs`'s: the
# `tidepool-timing phase=<name> ms=<n>` wire format `TIDEPOOL_TIMING=1` puts
# on `tidepool-extract`'s stderr, and the `tidepool_harness::timing::
# record_stage` tracing events the session/harness turn lanes already emit.
# The two small example bins this script drives
# (`tidepool-runtime/examples/bench_oneshot.rs`,
# `tidepool-repl/examples/bench_session.rs`) are thin callers of the PUBLIC
# compile/session API; row 5 reuses the pre-existing
# `tidepool-harness/examples/turn_latency_bench.rs` as-is (a real `Harness`
# turn driven via `ReplayProvider` — zero live model calls).
#
# Rows:
#   oneshot_cold — one compile_and_run_pure-shaped eval, a FRESH compile-memo
#                  dir every repeat (a genuine cache miss each time).
#   oneshot_warm — the SAME eval against an already-warm memo dir (a cache
#                  hit every repeat — no extract spawn at all).
#   session      — a fresh tidepool-repl `Session`: turn 0 (a decl) then 3
#                  subsequent single-item turns.
#   block5       — ONE `SessionCommand::Block` turn carrying 5 independent
#                  items — the BEFORE/AFTER INSTRUMENT for the in-flight
#                  batch-turns lane: what one call carrying 5 items costs
#                  TODAY, to contrast against `session`'s 4 separate calls.
#   harness      — one real answerer `Harness` turn via `ReplayProvider`
#                  (zero live model calls): `turn_latency_bench`'s
#                  `cold_vs_warm` scenario pinned to n=1 turn (its other two
#                  scenarios are disabled here via env — this script only
#                  wants the one row).
#
# Every row is MEDIAN-OF-3: the scenario binary is invoked three times (three
# separate processes — a fresh boot every time) and every `*_ms` key it
# reports is independently medianed across the three runs.
#
# Extract-side phase columns (`extract.*_ms` / `classify.*_ms`) need a
# TIDEPOOL_TIMING-capable `tidepool-extract` — this script builds one FRESH
# from this worktree exactly as scripts/battery.sh does when TIDEPOOL_EXTRACT
# is unset. A checked-in DEPLOYED nix-profile wrapper may predate
# TIDEPOOL_TIMING support (observed live on this box); if the caller sets
# TIDEPOOL_EXTRACT to one of those, extract-side phase columns silently go
# missing (wall_ms columns are unaffected) — not a bug, just an older binary.
#
# `jit_codegen_ms`/`run_exec_ms` are ABSENT from the session/block5 rows by
# construction, not a bug: `tidepool-repl/src/session.rs`'s bind/eval turns
# call `PersistentSession::add_fragment_session`/`bind_funcid` directly, not
# the `record_stage`-instrumented `tidepool-runtime/src/session/resident.rs`
# wrappers — so no JIT-side stage is emitted on the repl's actual turn path
# (extract-side phases and wall-clock are unaffected and accurate).
#
# Output: a stable, diff-able plain-text table to stdout AND to a timestamped
# file under the gitignored bench/ dir (results are never committed; this
# script and the example bins are).
#
# Usage: scripts/bench-turn.sh
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# --- self-slotting (copied from scripts/battery.sh — see that script's
# preamble for why the absolute parent-repo path is load-bearing: a
# worktree's own copy of ghc-slots.sh is inert / may be stale). ---
if [ -z "${TIDEPOOL_GHC_SLOT:-}" ]; then
  exec /home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- "$PWD/scripts/bench-turn.sh" "$@"
fi

# --- TIDEPOOL_EXTRACT resolution (copied from scripts/battery.sh) ----------
if [ -z "${TIDEPOOL_EXTRACT:-}" ]; then
  echo "==> TIDEPOOL_EXTRACT not set — building the dev tidepool-extract-bin"
  _w="$HOME/.nix-profile/bin/tidepool-extract"
  if [ -x "$_w" ]; then
    _ghc="$(grep -oE '/nix/store/[^:"]*-with-packages/bin' "$_w" | head -1)"
    if [ -n "${_ghc:-}" ] && [ -d "$_ghc" ]; then
      export PATH="$_ghc:$PATH"
      echo "==> prepended with-packages GHC to PATH ($_ghc)"
    fi
  fi
  ( cd haskell && cabal build tidepool-extract-bin )
  TIDEPOOL_EXTRACT="$(cd haskell && cabal list-bin tidepool-extract-bin)"
  export TIDEPOOL_EXTRACT
fi
if [ ! -x "$TIDEPOOL_EXTRACT" ] || ! "$TIDEPOOL_EXTRACT" 2>&1 | grep -q '^Usage:'; then
  echo "error: TIDEPOOL_EXTRACT='$TIDEPOOL_EXTRACT' is not a runnable tidepool-extract (no 'Usage:' banner)" >&2
  exit 1
fi
echo "TIDEPOOL_EXTRACT=${TIDEPOOL_EXTRACT}"
export TIDEPOOL_TIMING=1

# --- build the bench bins (release — this is a latency measurement) --------
echo "==> building bench bins"
cargo build --release --example bench_oneshot -p tidepool-runtime
cargo build --release --example bench_session -p tidepool-repl
cargo build --release --example turn_latency_bench -p tidepool-harness

ONESHOT_BIN="./target/release/examples/bench_oneshot"
SESSION_BIN="./target/release/examples/bench_session"
HARNESS_BIN="./target/release/examples/turn_latency_bench"

KV_DIR="$(mktemp -d)"
trap 'rm -rf "$KV_DIR"' EXIT

echo "==> oneshot_cold (3 fresh compile-memo dirs)"
for i in 1 2 3; do
  cold_dir="$(mktemp -d)"
  out="$KV_DIR/oneshot_cold.$i.out"
  err="$KV_DIR/oneshot_cold.$i.err"
  TIDEPOOL_COMPILE_CACHE_DIR="$cold_dir" "$ONESHOT_BIN" >"$out" 2>"$err"
  {
    grep -E '^wall_ms=[0-9]+$' "$out" | sed 's/^/oneshot_cold./'
    # No-match is legal (an older extract emits no timing lines) — don't let
    # pipefail turn an empty stderr into a silent whole-script abort.
    grep -oE 'tidepool-timing phase=[A-Za-z0-9_]+ ms=[0-9]+' "$err" \
      | sed -E 's/tidepool-timing phase=([A-Za-z0-9_]+) ms=([0-9]+)/oneshot_cold.extract.\1_ms=\2/' \
      || true
  } > "$KV_DIR/oneshot_cold.$i.kv"
  rm -rf "$cold_dir"
done

echo "==> oneshot_warm (one shared memo dir, warmed once, timed x3)"
warm_dir="$(mktemp -d)"
if ! TIDEPOOL_COMPILE_CACHE_DIR="$warm_dir" "$ONESHOT_BIN" \
    >"$KV_DIR/oneshot_warm.warmup.out" 2>"$KV_DIR/oneshot_warm.warmup.err"; then
  echo "error: oneshot_warm warm-up run failed:" >&2
  cat "$KV_DIR/oneshot_warm.warmup.err" >&2
  exit 1
fi
for i in 1 2 3; do
  out="$KV_DIR/oneshot_warm.$i.out"
  err="$KV_DIR/oneshot_warm.$i.err"
  TIDEPOOL_COMPILE_CACHE_DIR="$warm_dir" "$ONESHOT_BIN" >"$out" 2>"$err"
  {
    grep -E '^wall_ms=[0-9]+$' "$out" | sed 's/^/oneshot_warm./'
    # A warm run spawns NO extract, so an empty timing grep is the EXPECTED
    # case here, not an error — without || true, pipefail + set -e silently
    # kills the whole script at the first warm repeat.
    grep -oE 'tidepool-timing phase=[A-Za-z0-9_]+ ms=[0-9]+' "$err" \
      | sed -E 's/tidepool-timing phase=([A-Za-z0-9_]+) ms=([0-9]+)/oneshot_warm.extract.\1_ms=\2/' \
      || true
  } > "$KV_DIR/oneshot_warm.$i.kv"
done
rm -rf "$warm_dir"

echo "==> session + block5 (tidepool-repl Session, x3 fresh boots)"
for i in 1 2 3; do
  out="$KV_DIR/session.$i.out"
  "$SESSION_BIN" >"$out" 2>"$KV_DIR/session.$i.err"
  grep -E '_ms=[0-9]+$' "$out" > "$KV_DIR/session.$i.kv"
done

echo "==> harness (real Harness turn via ReplayProvider, x3 fresh boots)"
for i in 1 2 3; do
  json="$KV_DIR/harness.$i.json"
  TURN_LATENCY_BENCH_N=1 TURN_LATENCY_BENCH_SIZE_N=0 TURN_LATENCY_BENCH_RETRY_N=0 \
    TURN_LATENCY_BENCH_OUTPUT="$json" \
    "$HARNESS_BIN" > "$KV_DIR/harness.$i.out" 2>"$KV_DIR/harness.$i.err"
  python3 - "$json" > "$KV_DIR/harness.$i.kv" <<'PYEOF'
import json, sys
d = json.load(open(sys.argv[1]))
scenario = next(s for s in d["scenarios"] if s["scenario"] == "cold_vs_warm")
turn = scenario["turns"][0]
print(f"harness.wall_ms={turn['wall_ms']}")
for st in scenario["stages"]:
    print(f"harness.{st['stage']}_ms={int(round(st['median_ms']))}")
PYEOF
done

# --- aggregate: median-of-3 per key, across ALL rows at once ---------------
mkdir -p bench
stamp="$(TZ=UTC date +%Y%m%dT%H%M%SZ 2>/dev/null || date +%Y%m%dT%H%M%S)"
out_file="bench/bench-turn-${stamp}.txt"

{
  echo "tidepool bench-turn — ${stamp}"
  echo "TIDEPOOL_EXTRACT=${TIDEPOOL_EXTRACT}"
  echo "rows: oneshot_cold, oneshot_warm, session (turn0=decl, turn1-3=single-item),"
  echo "      block5 (N=5 batch-turns instrument), harness (Harness turn via ReplayProvider)"
  echo "median-of-3 (3 independent process runs per row); key = <n>_ms, all times ms"
  echo "jit_codegen_ms/run_exec_ms absent from session/block5: the repl's real bind/eval"
  echo "path bypasses the record_stage-instrumented resident.rs wrappers (see script header)"
  echo
  cat "$KV_DIR"/*.kv | python3 -c '
import sys, statistics
from collections import defaultdict
vals = defaultdict(list)
for line in sys.stdin:
    line = line.strip()
    if not line or "=" not in line:
        continue
    k, v = line.split("=", 1)
    try:
        vals[k].append(float(v))
    except ValueError:
        continue
for k in sorted(vals):
    vs = vals[k]
    med = statistics.median(vs)
    print(f"{k:<48} {med:>10.0f}   (n={len(vs)})")
'
} | tee "$out_file"

echo
echo "==> wrote $out_file"
