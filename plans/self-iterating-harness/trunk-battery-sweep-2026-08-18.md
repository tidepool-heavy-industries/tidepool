# Trunk battery sweep — 2026-08-18

Scout lane sweep of the four GHC-heavy hot crates
(`tidepool-harness`, `tidepool-handlers`, `tidepool-runtime`, `tidepool-repl`)
via `scripts/battery-shard.sh`, one crate per invocation, on an unmodified
worktree at trunk (`root.battery-sweep`, branched from `main` at `01a8cacb`).
Goal: surface hidden reds in binaries no lane's verify set watches. No code
changes were made; this file is the only commit.

`TIDEPOOL_EXTRACT` was left unset in every invocation — each shard built its
own `tidepool-extract-bin` fresh via `cabal build`.

## Red list

12 new reds, plus 1 already-known/owned red re-confirmed present. Everything
else is green.

### tidepool-harness — 303 tests, 296 passed (8 slow), **7 failed**, 0 skipped [1071s]

**Known/owned — not re-reported as new** (per instructions: a sibling lane is
fixing it):

- `finalize_type_pinning::pinned_finalize_needs_the_type_in_scope`

**New reds — all 6 in one binary, one shared root cause:**

| test | failure line |
|---|---|
| `agent_stack_scoping::ask_is_a_compile_error_in_the_answerer_stack` | `tests/agent_stack_scoping.rs:276:5`: expected a GHC not-in-scope error naming `ask`, got: `Haskell compilation failed (2 diagnostic(s))` |
| `agent_stack_scoping::ask_is_a_compile_error_in_the_harness_stack` | `tests/agent_stack_scoping.rs:323:5`: expected a GHC not-in-scope error naming `ask`, got: `Haskell compilation failed (2 diagnostic(s))` |
| `agent_stack_scoping::base_effect_is_a_compile_error_in_the_answerer_stack` | `tests/agent_stack_scoping.rs:302:5`: expected a GHC not-in-scope error naming `httpGet`, got: `Haskell compilation failed (1 diagnostic(s))` |
| `agent_stack_scoping::finalize_is_a_compile_error_in_the_harness_stack` | `tests/agent_stack_scoping.rs:342:5`: expected a GHC not-in-scope error naming `finalize`, got: `Haskell compilation failed (1 diagnostic(s))` |
| `agent_stack_scoping::fork_child_leaf_row_cannot_fork` | `tests/agent_stack_scoping.rs:183:5`: expected a GHC not-in-scope error for `forkAll` on the leaf row, got: `Haskell compilation failed (1 diagnostic(s))` |
| `agent_stack_scoping::run_llm_turn_is_a_member_error_not_a_scope_error_in_the_answerer_stack` | `tests/agent_stack_scoping.rs:121:5`: expected a GHC Member error naming `RunLLMTurn` (nameable, but not in the answerer's row), got: `Haskell compilation failed (1 diagnostic(s))` |

All six assert on a *specific class* of GHC diagnostic (not-in-scope vs.
Member vs. named identifier) and instead get the collapsed generic message
`Haskell compilation failed (N diagnostic(s))`. The compile still fails as
expected — only the diagnostic-classification assertion the test makes is
broken. Reads as fallout from the compile-pipeline consolidation noted as
"prerequisite ... in flight" in `plans/README.md`'s Effect Protocol entry:
whatever used to thread GHC's per-diagnostic text through to the harness
error appears to now collapse multiple diagnostics into one summary string
before the test can pattern-match the specific one it wants.

**Lane verify-set reference check** (grepped `scripts/` and `.config/nextest.toml`
only — no plan-doc prose mentions count, since those are narrative not a
verify command): `agent_stack_scoping` and `finalize_type_pinning` are named
in **zero** scripts or nextest filters. Both are only ever reached via the
wholesale `-p tidepool-harness --ignore-default-filter` shard.

### tidepool-handlers — 186 tests, 186 passed, 0 skipped [101s] — all green

### tidepool-runtime — 851 tests, 847 passed (3 slow), **4 failed**, 9 skipped [1504s]

| test | failure |
|---|---|
| `agent_mode_encoding::compile_fail_positional_payload_call_input` | `tests/agent_mode_encoding.rs:407:18`: "a positional payload constructor must not compile" — panicked because the compile **succeeded** |
| `generic_deriving_337::positional_sum_tojson_rejected_at_compile_time` | `tests/generic_deriving_337.rs:175:18`: "positional payload sum deriving ToJSON must not compile" — panicked because the compile **succeeded** |
| `nested_mapm_tag255::nested_mapm_readfile_full_mcp_preamble` | `tests/nested_mapm_tag255.rs:167:13`: "REPRODUCED: nested mapM + readFile with full MCP preamble crashed: yield error: Haskell error: apply_cont_heap: failed to allocate E result during continuation composition — This is the tag=255 GC forwarding pointer bug." (JIT runtime_error kind=2 UserError) |
| `stdlib_regressions_02_medium::works_int_prism_floors_not_truncates` | `tests/stdlib_regressions_02_medium.rs:134:20`: assertion failed — `toJSON (-3.7 :: Double) ^? _Int` wants `-4` (floor), got `-3` (truncation toward zero) |

Two distinct failure shapes here, not one root cause:

- The two `compile_fail_*` tests are the **inverse** of the harness pattern
  above: they assert a positional-payload construct must be rejected at
  compile time, and it now compiles successfully instead. Possibly related to
  the same compile-pipeline work but in the opposite direction (something
  that used to reject now accepts), worth a shared look with the
  `agent_stack_scoping` cluster but not assumed to be the same bug.
- `nested_mapm_tag255` is a *stress-test regression*, not a compile-fail
  assertion — its own file header says it was "written to reproduce a tag=255
  crash... Fixed by the cache auto-invalidation (PR #259)... This test
  remains as a stress test." It is failing again now, with the same
  `apply_cont_heap` allocation-failure signature the original bug had. This
  looks like a genuine regression, not a stale expectation.
- `stdlib_regressions_02_medium` is a real numeric-correctness bug: the `_Int`
  prism on `Double` truncates toward zero instead of flooring, contrary to
  its own test name and the `-4` expectation.

**Lane verify-set reference check:** none of `agent_mode_encoding`,
`generic_deriving_337`, `nested_mapm_tag255`, `stdlib_regressions_02_medium`
are named in any script or nextest filter. All unwatched outside the full
shard.

### tidepool-repl — 198 tests, 196 passed (5 slow), **2 failed**, 0 skipped [1145s]

(First invocation of this shard was killed mid-`cargo build`, before any test
ran — 0 test data collected, not a red. Retried; the numbers above are from
the successful retry.)

| test | failure |
|---|---|
| `gc_field_replay::field_session_replay_split_turns_control` | `tests/common/mod.rs:246:9`: `bind corpus (bridged): unexpected error: {"error":"**failure-class:** \`user-haskell\`  **phase:** \`compile\`\nerror:\n    session bind 'corpus' captures the effect row in its type ([FileRead]); row-typed values cannot cross fragments — bind a pure value or inline the effectful part"}` |
| `gc_field_replay::field_session_replay_bridged_substrate_verified` | Same `tests/common/mod.rs:246:9` site, same "session bind 'corpus' captures the effect row" error class, 53.9s to fail (vs. 9.3s for the sibling) |

Both reds are in the same binary (`gc_field_replay`) and hit the same "bind
captures the effect row in its type" rejection on a session-bind named
`corpus`. One shared root cause, not two independent bugs.

**Lane verify-set reference check:** `gc_field_replay` is named in zero
scripts or nextest filters.

## Unwatched-binary inventory — tidepool-harness

`tidepool-harness/tests/` holds 33 test binaries (`.rs` files; `fixtures/`
and `support/` are support directories, not binaries). `.config/nextest.toml`
names exactly **one** of them individually: `provider_behavior`, carved out
as the sole quick-tier (pure-Rust, no-extract) exception in the
`default-filter` and the `ghc-heavy` group override. No script under
`scripts/` names any tidepool-harness binary individually either (`bench-turn.sh`
references the crate's `examples/turn_latency_bench.rs`, a benchmark, not a
test binary).

That leaves **32 of 33** binaries reachable *only* through the wholesale
`-p tidepool-harness --ignore-default-filter` invocation this sweep used —
i.e., only when someone runs the full crate shard, not through any narrower,
faster, more-often-run check:

```
acceptance_askuser            acceptance_boot_compile_count   acceptance_consent_integrity
acceptance_cross_turn         acceptance_fanout               acceptance_finalize
acceptance_fork_combinators   acceptance_fork                 acceptance_lazy_boot
acceptance_multi_target       acceptance_run_llm_turn         acceptance_selfharness
acceptance_value_bind         agent_stack_scoping             companion_context_ref
companion_mount_spike         companion_scope_trees           companion_snapshots
decl_plane_run_scoping        dogfood_harness_typecheck       dogfood_observability
finalize_type_pinning         golden_path                     outer_effects
outer_fanout                  outer_subagent                  selfharness_budget
selfharness_compaction_fixes  selfharness_compaction          selfharness_context_window
selfharness_fn_finalize_spike selfharness_framing             selfharness_lifecycle
selfharness_persistence       selfharness_spine               timing_emission_pin
turn_lease                    turn_splice
```

Both of this sweep's new-red binaries (`agent_stack_scoping`,
`finalize_type_pinning`) are in this unwatched set — consistent with the
premise that hidden reds accumulate exactly here.

(Plan documents under `plans/post-restart/` mention many of these binary
names in prose — e.g. as the acceptance test a landed feature added — but
prose mentions in a historical design doc are not a verify command anyone
re-runs; they were excluded from "watched" for that reason.)

## Shard-sizing observation

The `~380s` budget `scripts/battery-shard.sh` is sized for was badly missed
by three of the four crates on this run:

| crate | wall time | vs. ~380s budget |
|---|---|---|
| tidepool-harness | 1071s | 2.8x over |
| tidepool-handlers | 101s | under budget |
| tidepool-runtime | 1504s | 4.0x over |
| tidepool-repl | 1145s (after one killed retry) | 3.0x over |

Only `tidepool-handlers` (186 tests) still fits the original budget.
`tidepool-runtime` in particular has grown to 851 tests — nearly 5x
`tidepool-handlers`'s count — and its own comment block in
`.config/nextest.toml` already documents several genuinely-slow suites
(`selfharness_fn_finalize_spike`, `selfharness_persistence`,
`selfharness_budget`) each individually taking 60-110s. The first
`tidepool-repl` shard invocation was killed by this environment's background-
process cap mid-`cargo build`, before any test ran at all — on a cold
`target/`, the crate's own compile time alone can exceed the budget before a
single test starts, independent of test count.

This sweep did not attempt the `-E 'binary(...)'` half-split the task
instructions offered as a fallback, since each shard was allowed to run to
completion in the background rather than being hard-capped — but the
instructions' own premise (a shard "sized for the ~380s budget") no longer
holds for three of the four crates and should be treated as a real finding,
not just a process footnote.

## Recommendation

Adopt a **binary-scoped verify-set convention**: every lane whose spec adds
or substantively changes a `tests/*.rs` integration binary in a GHC-heavy
crate should register that binary's name in one canonical, greppable place
(e.g. a `# watched:` comment block per crate, or a small manifest file
alongside `.config/nextest.toml`) at landing time — not just in a plan
document's prose, which this sweep's grep treats as unwatched by design.
Pair that with a periodic full-crate sweep (this report's method) run on a
cadence independent of any single lane's landing, specifically to catch the
gap between "a binary exists" and "a binary is named somewhere someone
actually re-runs." Without both halves, new binaries default to unwatched
(as 32 of 33 in tidepool-harness currently are) and reds inside them — like
the 6 found here in `agent_stack_scoping` — can sit undetected indefinitely
between sweeps.
