# jit_codegen latency — what landed

Companion to `11-turn-latency-contract.md`, which measured `jit_codegen` at
2704/4070/4361ms (debug) and ranked it second behind `extract_spawn`. That
document's attribution named two candidate costs. One was real and is fixed;
the other is real in the source and never executes on real input.

## The two candidates, resolved

**Quadratic DCE scan — real, fixed.** `wrap_with_datacon_env` prepended one
`LetNonRec` per constructor in the accumulated session table, and emission ran
`free_vars(extract_subtree(body))` at every level of that spine. At level `i`
the body still held the remaining `N-i` levels, so one turn cost ~N
clones-and-walks of O(N) — scaling with table size, not fragment size, and
paid on every turn.

Wrapper RHSs are closed terms (a saturated `Con`, or a lambda chain over its
own freshly-minted binders), so `fv(body_i) = fv(fragment)` minus the binders
below `i`, and binders are pairwise distinct. Therefore "binder `b_i` is free
in `body_i`" is exactly "`b_i ∈ fv(fragment)`": the per-level probe was
recomputing membership in a single set N times. One `free_vars` pass over the
fragment now decides the whole referenced set up front.

**Per-turn constructor re-JIT — real in the source, absent at runtime.**
`emit_lam` does unconditionally declare and define a fresh Cranelift function
for every `Lam` it sees, with no cross-fragment cache, so a constructor
referenced as a *function value* would be recompiled every turn. Nothing
references one. GHC Core carries a saturation invariant on data constructors:
an unsaturated use in source (`map Just xs`, `foldr (:) []`) is eta-expanded
into a lambda wrapping a saturated application, never a bare `Var` naming the
constructor. The wrapper bindings a session-lifetime closure cache would
populate are therefore never created.

`tidepool-codegen/tests/datacon_never_used_as_value.rs` pins that invariant. If
it ever fires, the wrapper/closure emission path is live on real input again
and the cache becomes worth building.

## Measurements

Instrumentation is permanent: `CodegenPipeline::functions_defined` (one per
successful `define_function`), `DceScanStats { calls, nodes_walked, elapsed }`
fed by the `LetNonRec` probe, and one `log::debug!` line per `add_function`
under `RUST_LOG=tidepool::codegen=debug`.

Real GHC-extracted session (`resident_session`, 164→166 constructor table):

| | turn 1 (3098 nodes) | turn 2 (131 nodes) |
|---|---|---|
| `wrapped_cons` | 0 | 0 |
| `dce_calls` before / after | 60 → 0 | 60 → 0 |
| `dce_nodes` before / after | 6203 → 0 | 6203 → 0 |

Fixture (`bind_error_then_allocate`), four successive `add_function` calls:

| | before | after |
|---|---|---|
| `wrapped_cons` | 3 / 3 / 3 / 3 | 0 / 0 / 0 / 0 |
| `dce_calls` | 5 / 3 / 3 / 3 | 2 / 0 / 0 / 0 |
| `dce_nodes` | 527 / 19 / 16 / 19 | 7 / 0 / 0 / 0 |
| tree `nodes` | 178 / 11 / 10 / 11 | 170 / 3 / 2 / 3 |

Mutation check: reverting `datacon_env.rs` regressed the counters to the
before-row exactly; restoring recovered the after-row.

`normalize` is bracketed separately inside the shape phase — it rebuilds the
whole tree, so its cost tracks fragment size while the rest of shaping tracks
table size, and one bucket cannot separate them.

The turn-latency bench was not re-run: it was made optional for this class of
change, and slots were contended throughout.

## Gates

Differential (`haskell_suite_differential`, `TIDEPOOL_EXPENSIVE_TESTS=1`,
`--run-ignored all`): `tested=349, compared=312, closure_skip=34, mismatch=0,
both_error=0, jit_only_error=0, eval_jit_diverge=3, skipped=1` — PASS, every
counter identical to the 2026-08-08 baseline, `compared` above
`COMPARED_FLOOR=300`, the three divergences the known documented names.

Quick tier: 1742 run, 1742 passed, 10 skipped. `cargo check --workspace
--all-targets`, `cargo fmt --all -- --check` clean. Clippy raises nothing in
any changed file (the `ResponsePlan` `large_enum_variant` in `jit_machine.rs`
predates this work).

Two invocation notes for anyone re-running the differential gate. It is
`#[ignore]`d, so `-E 'binary(haskell_suite_differential)'` alone selects the
binary and runs **nothing** while still exiting 0 — check for `1 test run`,
not the exit code, and pass `--run-ignored all`. It also reads pre-built CBOR
from `haskell/test/suite_cbor` and spawns no extract subprocess, so it needs no
GHC slot; taking one would block real GHC work for a CPU-bound run.

## Still quadratic elsewhere

The pattern fix B removed at the DCE probe survives at six other emission
sites, all uncounted by `dce_scan` (which increments at the `LetNonRec` probe
only) and visible solely inside the coarse `emit_ms` bucket. Line numbers as of
`cb1b131d`, in `tidepool-codegen/src/emit/expr.rs`:

| line | site |
|---|---|
| 1164 | inside `topo_sort_deferred_simple` |
| 1242 | `compute_captures_promised` — closure/thunk capture |
| 2455 | LetRec RHS |
| 2547, 2691 | `lam_body` extraction |
| 2858 | deferred constructor fields |
| 2965 | a further `rhs_idx` free-vars |

Line 1164 sits inside `topo_sort_deferred_simple` and runs per deferred
binding, so the bespoke topological sort and a `free_vars(extract_subtree)` are
one hot spot rather than two: a compilation-wide indexed free-vars cache
defuses the inner cost independently of what replaces the outer algorithm.
