#!/usr/bin/env bash
# The full workspace test battery, via cargo-nextest.
#
# nextest runs every test in its OWN process (never two tests sharing one),
# which structurally de-races the JIT's process-global-ish state (signal
# handlers, GC, fork-safety harnesses). See .config/nextest.toml for
# the hazard-audit note and repo-root CLAUDE.md's Build & Test section.
#
# WARNING: this runs the ENTIRE workspace in one process and is HOURS long
# here (every GHC-heavy crate's test forks a real GHC extract, capped at 2
# concurrent per run via .config/nextest.toml's ghc-heavy test group) — this
# environment hard-kills background processes at ~380s,
# well short of that. Do not invoke this bare and walk away expecting it to
# finish. Prefer:
#   - a single crate/test slice: `scripts/battery.sh -p <crate> -E 'test(<name>)'`
#   - a full crate as a survivable shard: `scripts/battery-shard.sh <crate>`
# The named expensive suites (corpus_report, haskell_suite_differential,
# tidepool-testing::haskell_verified) are additionally gated behind
# TIDEPOOL_EXPENSIVE_TESTS=1 and stay skipped even here unless you set it.
# Only the last of the three is actually multi-hundred-second (measured:
# corpus_report ~8s, haskell_suite_differential ~27s, haskell_verified's
# individual proptest cases alone run 100s+).
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
  exec /home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- "$PWD/scripts/battery.sh" "$@"
fi

if ! command -v cargo-nextest >/dev/null 2>&1 && ! cargo nextest --version >/dev/null 2>&1; then
  echo "error: cargo-nextest not found. Install with: cargo install cargo-nextest --locked" >&2
  exit 1
fi

source "$(dirname "${BASH_SOURCE[0]}")/lib-extract.sh"
resolve_tidepool_extract

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
# No hardcoded --workspace: the root manifest is VIRTUAL, so a bare
# invocation already defaults to every member (script cd's to repo root
# above) — while an explicit `--workspace` OVERRIDES any caller-passed
# `-p <crate>`, silently building and listing the whole workspace when
# the caller asked for one crate (found live: `-p tidepool-mcp` ran 3612
# tests, not 175). Passing "$@" bare lets `-p` actually scope.
cargo nextest run --ignore-default-filter --no-fail-fast "$@" 2> >(tee "$tmp_log" >&2)
run_status=$?
set -e

if grep -qE '\b0 tests run:' "$tmp_log"; then
  echo "error: battery selected/ran ZERO tests — a silent no-op, not a pass" >&2
  [ "$run_status" -eq 0 ] && run_status=1
fi

exit "$run_status"
