#!/usr/bin/env bash
# Run ONE GHC-heavy crate's tests, as a survivable shard of the full battery.
#
# `scripts/battery.sh` runs the ENTIRE workspace (`--ignore-default-filter`)
# in one process, which is hours-long here and gets hard-killed by this
# environment's ~380s background-process cap long before it finishes.
# Splitting by crate makes full coverage achievable as a sequence of shards,
# each of which fits under that cap (modulo the TIDEPOOL_EXPENSIVE_TESTS=1
# suites — see below).
#
# Usage: scripts/battery-shard.sh <crate> [extra nextest args...]
#   scripts/battery-shard.sh tidepool-runtime
#   scripts/battery-shard.sh tidepool-codegen -E 'binary(proptest_ghc_idioms)'
#
# This does NOT set TIDEPOOL_EXPENSIVE_TESTS — the expensive suites
# (corpus_report, haskell_suite_differential, tidepool-testing::haskell_verified)
# stay skipped unless you export TIDEPOOL_EXPENSIVE_TESTS=1 yourself. Run
# those deliberately, one at a time, with their own budget — they are NOT
# what this script's ~380s-per-shard promise covers; haskell_verified in
# particular runs for multiple hundreds of seconds.
# `corpus_report` and `haskell_suite_differential` are ALSO `#[ignore]`d, so
# reaching them additionally needs `--run-ignored all` scoped with `-E` (a
# bare `--run-ignored all` also un-ignores tidepool-codegen's deliberately-off
# known-bug repros and heavy fuzz lanes — see scripts/battery.sh), e.g.:
#   TIDEPOOL_EXPENSIVE_TESTS=1 scripts/battery-shard.sh tidepool-codegen \
#     --run-ignored all -E 'test(haskell_suite_differential) or test(corpus_report)'
#
# SUB-SHARDING: a bare `-p <crate>` invocation runs well over this script's
# ~380s budget for tidepool-harness/runtime/repl (only tidepool-handlers fits
# whole). The `-E 'binary(...) or binary(...)'` groups below replace the
# single `-p <crate>` shard for those three. Each group is its own
# `scripts/battery-shard.sh <crate> -E '...'` invocation — run them as a
# sequence, not concurrently (concurrent GHC-heavy shards on one box compete
# for the same `ghc-slots.sh` semaphore and CPU). Re-measure and re-bucket
# before trusting these groups against a renamed/added test binary.
#
# tidepool-harness (7 shards):
#   -E 'binary(acceptance_askuser) or binary(acceptance_boot_compile_count) or binary(acceptance_cross_turn) or binary(acceptance_finalize) or binary(acceptance_lazy_boot) or binary(acceptance_multi_target) or binary(acceptance_selfharness)'
#   -E 'binary(selfharness_budget) or binary(selfharness_compaction_fixes) or binary(selfharness_compaction) or binary(selfharness_context_window) or binary(selfharness_framing) or binary(selfharness_lifecycle) or binary(selfharness_spine) or binary(companion_mount_spike) or binary(companion_scope_trees)'
#   -E 'binary(selfharness_persistence) or binary(selfharness_fn_finalize_spike) or binary(persistence_migration_corpus)'
#   -E 'binary(companion_collapsed_slice) or binary(answerer_async_fork) or binary(fork_child_decl_plane_type) or binary(listen_channel)'
#   -E 'binary(agent_stack_scoping) or binary(decl_plane_run_scoping) or binary(dogfood_harness_typecheck) or binary(dogfood_observability) or binary(finalize_type_pinning) or binary(outer_effects) or binary(outer_fanout) or binary(outer_subagent) or binary(timing_emission_pin) or binary(turn_lease) or binary(provider_behavior)'
#   -E 'binary(compile_fail) or binary(delegate_positive_path) or binary(delegate_type_pinning)'
#   -E 'binary(minimal_watch_list) or binary(nested_async_repro) or binary(node_mailboxes) or binary(reinterpret_rowchange_repro) or binary(selfharness_decl_plane_replay) or binary(stable_effects_core_decl_plane) or binary(state_injection_memo_hit)'
#
# tidepool-runtime (7 shards):
#   -E 'binary(proptest_cache_layer) or binary(proptest_gc_pressure) or binary(proptest_haskell_pipeline) or binary(proptest_jit_vs_eval) or binary(proptest_letrec) or binary(proptest_render_json)'
#   -E 'binary(agent_mode_encoding) or binary(bignum_native) or binary(bridged_records_extract) or binary(cache_tests) or binary(captured_real_core) or binary(case_trap_graceful) or binary(constructors_of_type) or binary(cross_mode_existing) or binary(cross_mode_targeted) or binary(cross_mode_tests) or binary(eager_list_responses) or binary(extract_poison_diagnostic) or binary(extract_spawn_counted) or binary(flinch_katas) or binary(gc_and_errors) or binary(gc_stress_text_fold)'
#   -E 'binary(prelude_coverage) or binary(generic_deriving_337) or binary(generic_form_diagnostics) or binary(generic_recursive_sums) or binary(generic_form_wire) or binary(generic_form_roundtrip)'
#   -E 'binary(jit_surface) or binary(resident_session) or binary(nullary_sum_generic_deriving) or binary(patch_crosscheck_differential) or binary(multi_module_datacon) or binary(harness_profile_generic_surface) or binary(realm_varid_pinning) or binary(nested_mapm_tag255)'
#   -E 'binary(user_library) or binary(show_double_lens_sigill) or binary(run_llm_turn_sidecar) or binary(session_scope_retirement) or binary(sweep_repoint_smoke) or binary(session_decl_scope_tree) or binary(session_table_qualified_identity) or binary(session_decl_accum)'
#   -E 'binary(text_filter_gc) or binary(vendor_text_functions) or binary(stdlib_regressions_02_medium) or binary(stdlib_regressions_02) or binary(test_error_msg) or binary(validator_reject)'
#   -E 'binary(build_products_dir_differential) or binary(compile_fail) or binary(green_thread_representation) or binary(tenure_resume_gc_repro) or binary(word64_primops_random_probe)'
#
# tidepool-repl (7 shards):
#   -E 'binary(decl_plane)'
#   -E 'binary(do_block_invariant) or binary(effects_smoke) or binary(cancel_lifecycle) or binary(block_value_semantics) or binary(bare_expr_retry_census) or binary(ask_resume) or binary(auto_verdict_dispatch)'
#   -E 'binary(batch_turns_spawn_census)'
#   -E 'binary(lifecycle_meta) or binary(it_binding) or binary(error_recovery)'
#   -E 'binary(multi_binder) or binary(info_introspect) or binary(lifecycle_state) or binary(lost_session) or binary(gc_heap_verify_stress) or binary(gc_field_replay)'
#   -E 'binary(value_fidelity) or binary(shadow_rebind) or binary(bindings_dedup)'
#   -E 'binary(text_bind) or binary(stub_fetch) or binary(session_acceptance) or binary(repro_decl_library_import) or binary(value_binding_acceptance) or binary(repro_t_multiline_sig) or binary(name_shadowing)'
#
# Resident compile daemon (plans/compile-daemon-design.md, phase 1): same
# per-run daemon scripts/battery.sh can start — see its header for the full
# rationale. ON BY DEFAULT (kill switch: TIDEPOOL_EXTRACT_NO_DAEMON=1). This script
# starts one (or reuses an outer wrapper's, e.g. when chained across the
# sub-shard groups above) via the shared lib-extract.sh helpers, exports the
# socket for the nextest invocation below, and tears it down (by exact pid,
# escalating to SIGKILL after a 10s grace period) on exit, including
# SIGINT/SIGTERM. TIDEPOOL_EXTRACT_NO_DAEMON=1 is the kill switch.
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

source "$(dirname "${BASH_SOURCE[0]}")/lib-extract.sh"
resolve_tidepool_extract

# Per-run resident compile daemon (plans/compile-daemon-design.md §7 phase
# 1) — see scripts/battery.sh's matching comment for the full rationale;
# this mirrors it via the shared lib-extract.sh helpers rather than
# duplicating the logic. On by default (kill switch: TIDEPOOL_EXTRACT_NO_DAEMON=1).
# Outer-wrapper respect: when battery-shard.sh runs as one leg of a chain
# (scripts/battery-shard.sh's own header documents the multi-shard sequence
# for tidepool-harness/runtime/repl), a daemon already started by an earlier
# leg or an enclosing script is reused, not restarted or torn down here.
nextest_pid=""
tmp_log="$(mktemp)"
cleanup_exit() {
  rm -f "$tmp_log"
  teardown_battery_daemon
}
trap cleanup_exit EXIT
# See scripts/battery.sh's on_signal comment: a signal sent directly to this
# script's pid isn't delivered to a synchronous foreground command, so
# nextest runs backgrounded + waited (below) to make it interruptible.
on_signal() {
  echo "==> signal received — stopping nextest and tearing down the compile daemon" >&2
  # See scripts/battery.sh's on_signal comment: waits for nextest to
  # actually exit before returning, so the ghc-slots.sh semaphore slot is
  # never released while nextest or its child compiles might still be alive.
  [ -n "$nextest_pid" ] && _terminate_and_wait "$nextest_pid" "nextest"
  exit 130
}
trap on_signal INT TERM
start_battery_daemon

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
set +e
cargo nextest run --ignore-default-filter --no-fail-fast -p "$crate" "$@" 2> >(tee "$tmp_log" >&2) &
nextest_pid=$!
wait "$nextest_pid"
run_status=$?
set -e

if grep -qE '\b0 tests run:' "$tmp_log"; then
  echo "error: shard selected/ran ZERO tests for -p ${crate} — a silent no-op, not a pass" >&2
  [ "$run_status" -eq 0 ] && run_status=1
fi

exit "$run_status"
