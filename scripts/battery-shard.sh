#!/usr/bin/env bash
# Run ONE GHC-heavy crate's tests, as a survivable shard of the full battery.
#
# `scripts/battery.sh` runs the ENTIRE workspace (`--ignore-default-filter`)
# in one process, which is fine on a quiet dedicated machine but is
# hours-long here and gets hard-killed by this environment's ~380s
# background-process cap long before it finishes. Splitting by crate makes
# full coverage achievable as a sequence of shards, each of which fits under
# that cap (modulo the TIDEPOOL_EXPENSIVE_TESTS=1 suites — see below).
#
# Usage: scripts/battery-shard.sh <crate> [extra nextest args...]
#   scripts/battery-shard.sh tidepool-runtime
#   scripts/battery-shard.sh tidepool-codegen -E 'binary(proptest_ghc_idioms)'
#
# This does NOT set TIDEPOOL_EXPENSIVE_TESTS — the expensive suites
# (corpus_report, haskell_suite_differential, tidepool-testing::haskell_verified)
# stay skipped unless you export TIDEPOOL_EXPENSIVE_TESTS=1 yourself. Run
# those deliberately, one at a time, with their own budget — they are NOT
# what this script's ~380s-per-shard promise covers. Only haskell_verified is
# actually multi-hundred-second (measured: corpus_report ~8s,
# haskell_suite_differential ~27s, haskell_verified's individual proptest
# cases alone run 100s+).
# `corpus_report` and `haskell_suite_differential` are ALSO `#[ignore]`d, so
# reaching them additionally needs `--run-ignored all` scoped with `-E` (a
# bare `--run-ignored all` also un-ignores tidepool-codegen's deliberately-off
# known-bug repros and heavy fuzz lanes — see scripts/battery.sh), e.g.:
#   TIDEPOOL_EXPENSIVE_TESTS=1 scripts/battery-shard.sh tidepool-codegen \
#     --run-ignored all -E 'test(haskell_suite_differential) or test(corpus_report)'
#
# SUB-SHARDING the four crates named above: as of the 2026-08-18 trunk-battery
# sweep (plans/self-iterating-harness/trunk-battery-sweep-2026-08-18.md), a
# bare `-p <crate>` invocation ran 2.8x-4.0x over this script's ~380s budget
# for three of the four crates (only tidepool-handlers, 186 tests, still fit
# whole). The `-E 'binary(...) or binary(...)'` groups below (measured
# 2026-08-18 on a quiet box, TIDEPOOL_EXTRACT pre-built — a shared box under
# concurrent unrelated load inflates every number here 2-10x; re-measure
# before trusting a number that doesn't reproduce) replace the single
# `-p <crate>` shard for tidepool-harness/runtime/repl. Each group is its own
# `scripts/battery-shard.sh <crate> -E '...'` invocation — run them as a
# sequence, not concurrently (concurrent GHC-heavy shards on one box compete
# for the same `ghc-slots.sh` semaphore and CPU, which is what produced the
# 2-10x inflation during measurement). tidepool-handlers needs no split.
#
# tidepool-harness (5 shards; the crate's tests/*.rs binaries split
# acceptance_*/selfharness_*+companion_*/other, per the sweep's suggested
# harness split, plus one binary pulled out on its own):
#   -E 'binary(acceptance_askuser) or binary(acceptance_boot_compile_count) or binary(acceptance_consent_integrity) or binary(acceptance_cross_turn) or binary(acceptance_fanout) or binary(acceptance_finalize) or binary(acceptance_fork_combinators) or binary(acceptance_fork) or binary(acceptance_lazy_boot) or binary(acceptance_multi_target) or binary(acceptance_run_llm_turn) or binary(acceptance_selfharness) or binary(acceptance_value_bind)'      # 347s, 29/29 passed
#   -E 'binary(selfharness_budget) or binary(selfharness_compaction_fixes) or binary(selfharness_compaction) or binary(selfharness_context_window) or binary(selfharness_framing) or binary(selfharness_lifecycle) or binary(selfharness_spine) or binary(companion_context_ref) or binary(companion_mount_spike) or binary(companion_scope_trees) or binary(companion_snapshots)'   # 261s, 25/25 passed
#   -E 'binary(selfharness_persistence) or binary(selfharness_fn_finalize_spike)'   # 134s, 11/16 passed (5 known-red in fn_finalize_spike — a product bug, not a shard-sizing issue)
#   -E 'binary(companion_recursive_slice)'   # 154s, 0/6 passed at measurement time (product bug mid-fix in a sibling lane); isolated because the combined selfharness+companion group above hit 670s before this binary was pulled out
#   -E 'binary(agent_stack_scoping) or binary(decl_plane_run_scoping) or binary(dogfood_harness_typecheck) or binary(dogfood_observability) or binary(finalize_type_pinning) or binary(golden_path) or binary(outer_effects) or binary(outer_fanout) or binary(outer_subagent) or binary(timing_emission_pin) or binary(turn_lease) or binary(turn_splice) or binary(provider_behavior)'   # 138s, 51/54 passed (3 known-red, unrelated product bugs)
#
# tidepool-runtime (6 shards; the proptest_* family and the "a" bucket fit
# whole, the two heavier buckets split in half by binary test-count):
#   -E 'binary(proptest_cache_layer) or binary(proptest_gc_pressure) or binary(proptest_haskell_pipeline) or binary(proptest_jit_vs_eval) or binary(proptest_letrec) or binary(proptest_render_json)'   # 91s, 50/50 passed
#   -E 'binary(agent_mode_encoding) or binary(bignum_native) or binary(bridged_records_extract) or binary(cache_tests) or binary(captured_real_core) or binary(case_trap_graceful) or binary(constructors_of_type) or binary(cross_mode_existing) or binary(cross_mode_targeted) or binary(cross_mode_tests) or binary(eager_list_responses) or binary(extract_poison_diagnostic) or binary(extract_spawn_counted) or binary(flinch_katas) or binary(gc_and_errors) or binary(gc_stress_text_fold)'   # 129s, 70/70 passed
#   -E 'binary(prelude_coverage) or binary(generic_deriving_337) or binary(generic_form_diagnostics) or binary(generic_recursive_sums) or binary(generic_form_wire) or binary(generic_form_roundtrip)'   # 43s, 78/78 passed
#   -E 'binary(jit_surface) or binary(resident_session) or binary(nullary_sum_generic_deriving) or binary(patch_crosscheck_differential) or binary(multi_module_datacon) or binary(lsp_graph_walk) or binary(harness_profile_generic_surface) or binary(realm_varid_pinning) or binary(nested_mapm_tag255) or binary(lspnode_show_dot)'   # 103s, 63/64 passed (1 known-red, unrelated product bug)
#   -E 'binary(user_library) or binary(show_double_lens_sigill) or binary(run_llm_turn_sidecar) or binary(session_scope_retirement) or binary(sweep_repoint_smoke) or binary(session_decl_scope_tree) or binary(session_table_qualified_identity) or binary(session_decl_accum)'   # 43s, 85/87 passed (2 known-red, unrelated product bug)
#   -E 'binary(text_filter_gc) or binary(vendor_text_functions) or binary(stdlib_regressions_02_medium) or binary(stdlib_regressions_02) or binary(test_error_msg) or binary(validator_reject)'   # 44s, 66/66 passed
#
# tidepool-repl (7 shards; the crate's per-test weight is heavy enough that
# 48-test buckets alone exceed budget, so each of the three thematic buckets
# further splits by binary, plus the one binary whose single test ran 540s+
# under box contention is pulled out on its own):
#   -E 'binary(decl_plane)'   # 180s, 24/24 passed
#   -E 'binary(do_block_invariant) or binary(effects_smoke) or binary(cancel_lifecycle) or binary(block_value_semantics) or binary(bare_expr_retry_census) or binary(ask_resume) or binary(auto_verdict_dispatch)'   # 182s, 24/24 passed
#   -E 'binary(batch_turns_spawn_census)'   # 117s, 1/1 passed (its own shard: measured 540s+ mid-run under box contention during sizing, though a clean quiet-box rerun landed at 117s — kept isolated since it is a census/measurement suite, not because it is structurally slow)
#   -E 'binary(lifecycle_meta) or binary(it_binding) or binary(error_recovery)'   # 170s, 27/27 passed
#   -E 'binary(multi_binder) or binary(info_introspect) or binary(lifecycle_state) or binary(lost_session) or binary(gc_heap_verify_stress) or binary(gc_field_replay)'   # 307s, 21/21 passed (4 slow — GC stress suites)
#   -E 'binary(value_fidelity) or binary(shadow_rebind)'   # 255s, 26/26 passed
#   -E 'binary(text_bind) or binary(stub_fetch) or binary(session_acceptance) or binary(repro_decl_library_import) or binary(value_binding_acceptance) or binary(repro_t_multiline_sig) or binary(name_shadowing)'   # 147s, 14/14 passed
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [ $# -lt 1 ]; then
  echo "usage: $0 <crate> [extra nextest args...]" >&2
  exit 1
fi

# Take a host GHC slot for the whole shard — see scripts/battery.sh for why
# this is self-slotted rather than left to the caller. After the usage check,
# so a misinvocation fails immediately instead of after a slot wait.
if [ -z "${TIDEPOOL_GHC_SLOT:-}" ]; then
  exec /home/inanna/dev/tidepool/scripts/ghc-slots.sh run -- "$PWD/scripts/battery-shard.sh" "$@"
fi
crate="$1"
shift

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

# Same announced-binary sanity probe as battery.sh — see that script for why
# the checks are shaped this way (silent extract_env fallback, EPIPE hazard).
if [ ! -x "$TIDEPOOL_EXTRACT" ] || ! "$TIDEPOOL_EXTRACT" 2>&1 | grep -q '^Usage:'; then
  echo "error: TIDEPOOL_EXTRACT='$TIDEPOOL_EXTRACT' is not a runnable tidepool-extract (no 'Usage:' banner)" >&2
  exit 1
fi
echo "TIDEPOOL_EXTRACT=${TIDEPOOL_EXTRACT}"
echo "==> shard: -p ${crate} (--ignore-default-filter, TIDEPOOL_EXPENSIVE_TESTS=${TIDEPOOL_EXPENSIVE_TESTS:-unset})"

# `exec` here would replace this shell before any check could run — so a run
# that SELECTS ZERO TESTS (a typo'd -E filter, a crate/filter combination with
# nothing left after --ignore-default-filter) would inherit nextest's exit
# code and nothing else, and read as a pass whenever that code is 0. Capture
# the run instead: report a genuine test failure AND an empty selection if
# both occur (never let one mask the other), and propagate the real exit
# status. stderr is where nextest writes everything (PASS/FAIL/Summary lines
# included; stdout is otherwise unused) — tee it through unchanged via a
# process substitution so a caller piping/redirecting stderr still sees the
# identical live stream.
tmp_log="$(mktemp)"
trap 'rm -f "$tmp_log"' EXIT
set +e
cargo nextest run --ignore-default-filter --no-fail-fast -p "$crate" "$@" 2> >(tee "$tmp_log" >&2)
run_status=$?
set -e

if grep -qE '\b0 tests run:' "$tmp_log"; then
  echo "error: shard selected/ran ZERO tests for -p ${crate} — a silent no-op, not a pass" >&2
  [ "$run_status" -eq 0 ] && run_status=1
fi

exit "$run_status"
