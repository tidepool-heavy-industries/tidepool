#!/usr/bin/env bash
# The full workspace test battery, via cargo-nextest.
#
# nextest runs every test in its OWN process (never two tests sharing one),
# which structurally de-races the JIT's process-global-ish state (signal
# handlers, GC, fork-safety harnesses) that the old `-- --test-threads=1`
# discipline serialized against by brute force. See .config/nextest.toml for
# the hazard-audit note and repo-root CLAUDE.md's Build & Test section.
#
# WARNING: this runs the ENTIRE workspace in one process and is HOURS long
# here (every GHC-heavy crate's test forks a real GHC extract, capped at 4
# concurrent) — this environment hard-kills background processes at ~380s,
# well short of that. Do not invoke this bare and walk away expecting it to
# finish. Prefer:
#   - a single crate/test slice: `scripts/battery.sh -p <crate> -E 'test(<name>)'`
#   - a full crate as a survivable shard: `scripts/battery-shard.sh <crate>`
# The named multi-hundred-second suites (lazy_consumption_property_suite,
# effectful_lazy_ab_x8, corpus_report, haskell_suite_differential,
# tidepool-testing::haskell_verified) are additionally gated behind
# TIDEPOOL_EXPENSIVE_TESTS=1 and stay skipped even here unless you set it.
# `corpus_report` and `haskell_suite_differential` are ALSO `#[ignore]`d (a
# default nextest run must report them as ignored, not silently "passed" via
# early return) — reaching them needs BOTH TIDEPOOL_EXPENSIVE_TESTS=1 AND
# `--run-ignored all`. Do not pass `--run-ignored all` bare to this script:
# tidepool-codegen also carries `#[ignore]`d known-bug repros and heavy fuzz
# lanes (proptest_gc_recursion/host_arrays/ghc_idioms/jit_dispatch/
# boundary_roundtrip) that are deliberately off by default and will FAIL or
# run for ~68min if un-ignored. Scope with `-E`, e.g.:
#   TIDEPOOL_EXPENSIVE_TESTS=1 scripts/battery.sh -p tidepool-codegen \
#     --run-ignored all -E 'test(haskell_suite_differential) or test(corpus_report)'
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# Take a host GHC slot for the whole run, including the cabal build below.
# Everything past this point forks real `tidepool-extract` compiles, and the
# box is shared by every agent worktree — a run launched without a slot is how
# the 2026-08-09 load-159 incident started. Self-slotting means forgetting is
# not a failure mode; `scripts/ghc-slots.sh run` exports TIDEPOOL_GHC_SLOT, so
# an outer wrapper is respected rather than double-acquired.
if [ -z "${TIDEPOOL_GHC_SLOT:-}" ]; then
  exec "$PWD/scripts/ghc-slots.sh" run -- "$PWD/scripts/battery.sh" "$@"
fi

if ! command -v cargo-nextest >/dev/null 2>&1 && ! cargo nextest --version >/dev/null 2>&1; then
  echo "error: cargo-nextest not found. Install with: cargo install cargo-nextest --locked" >&2
  exit 1
fi

if [ -z "${TIDEPOOL_EXTRACT:-}" ]; then
  echo "==> TIDEPOOL_EXTRACT not set — building the dev tidepool-extract-bin"
  # The locally-built binary needs the with-packages GHC (supplying lens/
  # freer-simple) on PATH at runtime, or extraction fails with "Could not find
  # module Control.Lens". The deployed nix wrapper hard-codes that GHC's path;
  # reuse it so a bare `nix develop` run works without manual PATH surgery.
  _w="$HOME/.nix-profile/bin/tidepool-extract"
  if [ -x "$_w" ]; then
    _ghc="$(grep -oE '/nix/store/[^:"]*-with-packages/bin' "$_w" | head -1)"
    if [ -n "${_ghc:-}" ] && [ -d "$_ghc" ]; then
      export PATH="$_ghc:$PATH"
      echo "==> prepended with-packages GHC to PATH ($_ghc)"
    fi
  fi
  ( cd haskell && cabal build tidepool-extract-bin )
  # Split assignment from export: `export VAR="$(cmd)"` masks the command's
  # exit status (SC2155), so a failed list-bin would proceed with an empty var.
  TIDEPOOL_EXTRACT="$(cd haskell && cabal list-bin tidepool-extract-bin)"
  export TIDEPOOL_EXTRACT
fi

# The announced binary must actually run. eval_harness::extract_env silently
# falls back to a `cabal list-bin` binary when the announced one doesn't
# execute (so the banner below could name a binary the tests never used), and
# when nothing resolves the GHC-guarded suites "skip cleanly" — a green
# battery with zero GHC coverage. Same no-args `Usage:` probe extract_env uses
# — the banner is on stderr, written before stdout's diagnostics JSON, so a
# plain merged `2>&1` (not a stdout/stderr swap) sees it first either way.
# Do NOT truncate the read with `head -c N`: the extract binary ALWAYS writes
# a second thing after the banner (the diagnostics JSON, to stdout) — `head`
# closing the pipe the instant it has its N bytes races that second write,
# and an EPIPE there is an uncaught exception that fails the process (an
# intermittent nonzero exit with no other symptom). `grep` alone drains the
# pipe to EOF, so the writer never gets closed out from under it.
if [ ! -x "$TIDEPOOL_EXTRACT" ] || ! "$TIDEPOOL_EXTRACT" 2>&1 | grep -q '^Usage:'; then
  echo "error: TIDEPOOL_EXTRACT='$TIDEPOOL_EXTRACT' is not a runnable tidepool-extract (no 'Usage:' banner)" >&2
  exit 1
fi
echo "TIDEPOOL_EXTRACT=${TIDEPOOL_EXTRACT}"

# --ignore-default-filter: the full battery runs EVERY crate, including the
# GHC-extract-heavy ones that .config/nextest.toml's default-filter skips for
# quick inner-loop `cargo nextest run`. Same profile, so slow-timeout + the
# ghc-heavy thread cap still apply.
#
# `exec` here would replace this shell before any check could run — same
# zero-tests-as-a-pass trap scripts/battery-shard.sh closes; see that script's
# comment for the reasoning. Capture the run instead of masking it behind exec.
tmp_log="$(mktemp)"
trap 'rm -f "$tmp_log"' EXIT
set +e
cargo nextest run --workspace --ignore-default-filter "$@" 2> >(tee "$tmp_log" >&2)
run_status=$?
set -e

if grep -qE '\b0 tests run:' "$tmp_log"; then
  echo "error: battery selected/ran ZERO tests — a silent no-op, not a pass" >&2
  [ "$run_status" -eq 0 ] && run_status=1
fi

exit "$run_status"
