# Composed-tree gate runbook (pre-redeploy, run by root)

One full-breadth verification of the assembled tree — breadth is bought
ONCE here, not per-fold (test-economy policy). Run detached under
`scripts/ghc-slots.sh run` per batch, `--no-fail-fast` on everything.

## Rules (each earned by a specific incident this campaign)

- **Gate on tests-RUN counts, never exit codes.** An `#[ignore]`d target
  matched by `-E 'binary(...)'` WITHOUT `--run-ignored all` runs zero tests
  and exits 0. Assert run-count ≥ expected.
- **`--no-fail-fast` always.** Fail-fast + any known red cancels the suite
  and returns a partial count that reads like a result (cost two dev slots).
- **State the known-red allowlist up front.** Currently:
  `selfharness_compaction` (open-intermittent, garbage con_tag signature),
  `resident_session::nested_child_runs_while_parent_suspended_then_resumes`
  (NurseryExhausted class reopened, load-correlated). Anything else red is
  NEW.
- **Contention produces watchdog timeouts, not wrong values** — a timeout
  under load re-run in isolation is diagnosis, not noise-tolerance. The
  converse: a SUB-100ms assertion failure is never contention — do not
  classify fast failures as flake-by-load.
- **Capture full output to a file; extract after.** Piping test output
  through `tail`/`head` at capture time destroys the panic text you will
  need — two independent diagnoses were delayed by exactly this in one day.
- **A flaky test never folds.** 1-in-N flakes get fixed (or their claim
  narrowed to a documented non-property) BEFORE the branch lands; repetition
  gate for the fix: 15+ consecutive passes, since one green proves nothing.
- For `tidepool-macro`: compiling tests IS running the extractor
  (proc-macro expansion). Safe forms only: `cargo check -p tidepool-macro`
  (no `--all-targets`) if checking; the full nextest run below is
  slot-gated anyway. ~5 un-slotted expansion spawns occur once per cold
  worktree — expected, bounded.

## The battery

```bash
# 1. Workspace + quick tier (slot-free)
cargo check --workspace --all-targets
cargo nextest run --no-fail-fast                 # expect ~1742+, zero new reds

# 2. stdlib-fidelity's deferred gate (ONE slot, both commands in the batch)
#    Expected: N=98 total across jit_surface, ZERO failures.
#    N=53 or 67 => FOLDS WENT MISSING (count is the fold-completeness check).
#    works_fork/works_fork_map: green requires roster fix + FORK_TAG=11
#    composed; if red, verify composition before assuming regression.
#    If red elsewhere: jit-pinning's 5 hand-derived probes are first suspect.
scripts/ghc-slots.sh run -- bash -c '
  cargo nextest run --ignore-default-filter -j1 -p tidepool-runtime -E "binary(jit_surface)" --no-fail-fast
  cargo nextest run --ignore-default-filter -p tidepool-macro --no-fail-fast'

# 3. Harness acceptance (per batch, via slots): the 9-binary finalize/fork
#    set + selfharness_spine + selfharness_compaction (open-intermittent:
#    one red here with the garbage-tag signature is the KNOWN bug — record,
#    don't chase; anything else is new).

# 4. Expensive differential gate (one deliberate run)
# NOTE (2026-08-08): the suite is a test BINARY in tidepool-codegen —
# the original "-p tidepool-testing -E 'test(...)'" spelling matches
# NOTHING (0 run, 135 skipped, exit 0: the exact zero-tests trap this
# runbook's own counts rule exists to catch, and it caught it).
TIDEPOOL_EXPENSIVE_TESTS=1 scripts/ghc-slots.sh run -- \
  cargo nextest run --ignore-default-filter -p tidepool-codegen \
  -E 'binary(haskell_suite_differential)' --run-ignored all --no-fail-fast
# Baseline counters: tested=349 compared=312 closure_skip=34 mismatch=0
# both_error=0 jit_only_error=0 eval_jit_diverge=3 skipped=1; floor=300.

# 5. Deferred-verifications ledger (accumulated during the no-test window):
#    - selfharness_spine re-verify on final HEAD (was handed to root after
#      the turn-latency rebase; never re-run)
#    - spot-confirm codegen+heap suites on HEAD (gc-soundness's gates ran
#      pre-merge on an identical tree, never on the composed one)
```

Per-binary counts + durations recorded against expectations; deviations
investigated before redeploy, not after.
