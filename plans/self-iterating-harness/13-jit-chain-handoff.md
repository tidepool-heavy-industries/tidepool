# jit-chain — cluster ledger and successor handoff

Spec material for the successor TL (`jit-chain-2`). Read alongside the
authoritative item list (the D-numbered findings) and
`11-jit-codegen-latency-receipt.md`. Where this document and a spec disagree
about a file or a site, this document is later and was checked against the
source.

## Cluster ledger

| cluster | scope | status |
|---|---|---|
| A | case-trap intern arena (D13), `Int64ToWord64` result tag, blackhole gap | **landed** (blackhole open, see below) |
| B | indexed free-vars analysis + petgraph topo sort (C4+C5) | **landed**, unmeasured |
| C | `DataConTable` accumulation hygiene (D3) | **landed**, two items deferred |
| D | `runtime_apply`/`runtime_tail_apply` + `FunctionImports` (D4+D5) | **not started** |
| E | lambda registry, stack-map lookup, `seed_external_env` (D7/D8/D9) | **not started** |
| F | `declare_env` (D6) | **not started** |

D, E and F were never started. E's fence (a sibling holding uncommitted
`jit_machine.rs` work) was lifted before the quiesce, so E is unblocked on
entry. B must own `emit/expr.rs` alone; D follows B on the same file.

## A failure class this lane hit twice in one day

**A test that asserts in a domain wider than production's types can inhabit.**
Both instances passed review and both were caught only by adversarial checking,
never by the suite itself.

1. The second-fragment gate claimed an emission axis it could not constrain.
   Caught by mutation: inducing the named error left it green. Claim narrowed.
2. `multiturn_merge_matches_flattened_sequential_property` asserted equivalence
   over turns modelled as `Vec<DataCon>`. Production passes a `DataConTable`,
   whose `iter()` is `by_id.values()` — at most one entry per id. The property
   is false for a turn holding two entries for one id, which production cannot
   produce. Caught by repetition: it failed roughly 1 run in 8, because
   proptest draws 2000 fresh cases per run with no persisted seed.

The guard, both directions: when a property test takes a wider, more convenient
input shape than production's, either constrain the generator to the production
invariant or assert only what holds in the wider domain. Taking the freedom
*and* asserting the narrow property is the bug.

Two corollaries earned the hard way. A single green run cannot clear an
intermittent test — repetition is the gate, and the rate sets the count. And
diagnostics must be captured whole to a file and extracted afterward: two
separate runs here were piped through `tail` at capture time, destroying the
panic text and the shrunk counterexample on exactly the runs that needed
forensics.

## Cluster B — landed, but unmeasured and unmutated

Folded from `root.jit-chain.cluster-b`. It reached its bar — compiles clean
post-rebase, 5/5 topological-sort goldens green, 11/11 `emit::` unit tests
green — but a quiesce cut its verification short. **Not done, and worth doing
before trusting the perf claim:** the before/after `emit_ms` measurement,
mutation checks on either half, and the GHC-heavy `resident_session`
confirmation. The correctness evidence is strong; the performance claim is
currently unevidenced.

**Verified green.** `emit/free_vars_index.rs` —
`FreeVarsIndex::compute`, a single forward pass over the flat node vector
(children precede parents, so no explicit stack walk is needed), with
`Rc<FxHashSet<VarId>>` shared for pass-through nodes and a canonical empty set.
Its equivalence test (`tests/free_vars_index_equivalence.rs`) matched the
reference `free_vars(extract_subtree(idx))` at every index across 103,011
real-corpus nodes (138 fixtures) and 14,686 generated-tree nodes.
`EmitSession.free_vars_idx` is built at all four construction sites
(`compile_expr`, `emit_lam`, `emit_thunk_promised`, LetRec phase 3a). All seven
conversion sites were converted and green.

The petgraph topological sort replaces the bespoke one: Kahn's algorithm over a
`DiGraph`, tie-broken by a min-heap on `NodeIndex` so ordering follows original
binding order, with `Dfs` for reachability. Determinism is a requirement, not a
nicety — a nondeterministic emission order would make compilation
irreproducible.

Its golden tests (independent, chain, diamond, cycle, self-reference) were
written and made green against the **old** algorithm before the replacement
landed. That ordering is what makes them a behaviour-identity check rather than
a restatement of the new implementation, and it is the only reason replacing an
emission-ordering algorithm was safe to do at all.

### Seven sites, not eight

Earlier spec material said eight `free_vars(extract_subtree(idx))` sites in
`emit/expr.rs`. There are seven. The candidate near line 2691 (LetRec phase 3a,
`lam_body_tree`) builds a nested tree with no adjacent `free_vars` call, so
there is nothing to convert there beyond the `EmitSession` wiring already done.

The `LetNonRec` DCE probe (near line 2041) is deliberately excluded: it is
separately instrumented and its counters are read by `jit_machine.rs`.

### Two facts that cost hours if rediscovered

**The index-versus-rebuilt-tree risk class is structurally closed inside
`emit/expr.rs`.** `EmitSession.tree` is a `&'a CoreExpr` immutable for the
struct's lifetime, and `emit/*.rs` contains no `replace_subtree` call. An
analysis cached by node index therefore cannot be consumed against a different
tree than it was built from. This is the failure shape that the reverted
constructor-wrapper prune was suspected of (an analysis computed before a
rebuild, consumed after it), so the question recurs whenever this area is
touched; it is settled for `expr.rs` by immutability, not by convention.

**`FreeVarsIndex` is not `Copy`, and the old `tree: &CoreExpr` was.** A closure
capturing `&args.sess.free_vars_idx` and used both before and after a loop that
mutably reborrows `args.sess` will not borrow-check. The previous code dodged
this by accident because a shared reference is `Copy`. The working shape is a
plain function taking the index as a parameter rather than a capturing closure,
as applied at the deferred-constructor-fields site.

### petgraph dependency line

`petgraph` 0.6 is declared directly in `tidepool-codegen/Cargo.toml`. The
manifests work centralizes common external dependencies through a generator
(`gen-workspace-deps.py`); on the post-batch base this line likely wants to be
`petgraph.workspace = true`, and the generator's equivalence check may flag the
direct declaration. Conform it on resume.

`petgraph` is mandatory for graph algorithms in this codebase — hand-rolled
traversals are not an accepted alternative.

## Cluster A — two fixes landed; the blackhole item is still open

`int64ToWord64#` now tags its result `LIT_TAG_WORD`. It had shared a match arm
with `Word64ToInt64`/`Int64ToInt` that unconditionally tagged `LIT_TAG_INT`,
correct for those two and wrong for a `Word64#` result.
`jitbug_int64_to_word64_result_tag` is un-ignored and now pins the invariant,
and `Int64ToWord64` is back in `prop_int_unary`'s operator list.

Case-trap diagnostic names are interned in a pipeline-owned arena rather than
`Box::leak`ed. The arena hands out a raw pointer and length rather than a
`&'static str`, because the strings live exactly as long as the pipeline, not
forever — claiming `'static` would be a lifetime lie about a pointer that
compiled code embeds. A `Box<str>`'s heap bytes are stable across `HashSet`
rehash, so interning more names does not invalidate pointers already handed
out.

**The blackhole item remains open**, and what was found narrows it usefully.
On a *synthetic* self-referential `LetRec`, both paths trap — they merely
classify differently: `run_pure` gives a clean
`Err(Yield(Runtime(BlackHole)))`, while calling `compile_expr` directly returns
a poison-closure pointer whose `heap_to_value_forcing` fails as
`Err(UnexpectedHeapTag(0))`. So the original one-line framing ("compile_expr
does not raise `runtime_blackhole_trap`") does not hold in that form.

That is *not* the same shape as the repo's already-documented divergence —
`haskell_suite_differential.rs`'s `EXPECTED_EVAL_JIT_DIVERGE` entry
`thunk_blackhole`, which concerns real GHC-lifted top-level Core returning a
value with **no trap at all**. Anyone resuming this should start from that
documented entry and the real lifted Core shape, not from a synthetic `LetRec`.
The synthetic experiment was discarded; the expensive differential suite was
not run to confirm firsthand.

## The chain, measured

Metadata constructors versus constructors actually reachable from the
fragment's own Core, read pre-wrap on a real GHC-extracted session:

| turn | `table_cons` | reachable | ratio | Cranelift funcs | blocks |
|---|---|---|---|---|---|
| 1 | 164 | 24 | 6.8:1 | 232 | 13,348 |
| 2 | 166 | 15 | 11.1:1 | 25 | 494 |

Read it as *single-digit-to-low-teens percent reachable*, not as the earlier
"hundreds versus dozens" framing — the table is 164-166, not hundreds. The
proportional win is large and grows across turns (the table accumulates while
fragments stay small), but anyone sizing the absolute win should use these
figures.

The count must be taken **pre-wrap**. `wrap_with_datacon_env` mechanically adds
a reference to every table constructor, so any count downstream of it equals
`table_cons` by construction — a measurement-point artifact, not a finding.

Both diagnostic walks are gated on the `tidepool::codegen` target being
enabled, and the pre-wrap walk is timed separately and subtracted out of
`shape_ms`. It has to sit inside that window (it needs the pre-wrap tree, which
the wrap then consumes), and without the subtraction the instrument inflates
the very metric it reports — visible only when logging is on, which is the only
time anyone reads it.

## Cluster C — landed; two items deferred with reasons

`merge_table` filters out constructors already held with identical metadata,
then batches the remainder through `DataConTable::extend_checked`, which sorts
each affected `by_type_name` bucket once instead of once per insert.

Both step-4 items were investigated and deferred on evidence, not on time:

- **Global/session metadata split.** The bootstrap Prelude table (~124-164
  entries) is very nearly all of what a steady-state turn re-presents, so the
  win is marginal. Splitting storage means either rewriting every
  `DataConTable` accessor to consult two backing stores — CBOR serialization
  included — or re-merging per call, which reintroduces the O(N) cost steps 1-3
  just removed.
- **Structural sharing into suspension snapshots.** Three call sites in
  `resident.rs` (~505, ~592, ~782) deep-clone `session_table()` per
  child/suspension result. Making that cheap requires `EvalResult`'s table
  field to become `Arc`-backed, but `EvalResult` is a public facade type
  re-exported from the top-level `tidepool` crate and consumed across four-plus
  `tidepool-runtime` test suites plus `tidepool-mcp`. Not locally contained.

A permanent `log::debug!` on `merge_table` (target `tidepool::session`) reports
`turn_cons`/`skipped`/`applied`/`session_cons_before`. No live measurement was
captured.

## Constructor-wrapper prune — reverted, re-land held

`cb1b131d` (`wrap_with_datacon_env` binds only referenced constructors) is
reverted, on this branch and on the shared tip. Its companions `e5bd7e43`
(wrapper manifest) and `9be7e89e` (DCE probe subtree scoping) are untouched and
stay: neither moves a decision point.

The prune is **not** implicated by the evidence. It was reverted during a
release window because a populated-session second-fragment failure was
suspected on symptom class, and a wrong referenced-set is a miscompile while
the prune is perf-only. The failure was subsequently shown to be intermittent
and to reproduce on a tree without the prune.

**Re-land conditions.** The live hypothesis is that the prune's allocation
profile shift (roughly 166 wrapper allocations per fragment down to zero) moved
GC timing enough to change the failure frequency of a latent rooting bug —
i.e. the prune is an exposer, not a cause. Re-landing before that bug is fixed
would raise the failure rate of something unfixed. Sequence: resolve the
rooting question, fix it if confirmed, then re-land the prune — at which point
the prune becomes a useful repro-amplifier for the rooting fix's own test. If
the rooting hypothesis dies too, re-land with a receipt line recording that it
was suspected on symptom class, not reproduced, and reverted out of caution.

## Constructor-tag investigation — what is settled

The failing error is `YieldError::UnexpectedConTag` (`yield_type.rs:43`): a run
result that claims to be a `Con` whose `con_tag` is neither the session's `Val`
nor `E`. This is the freer-simple classification of a run's *result* at the
effect-machine boundary — not `emit_case_trap`, not a case-alternative miss,
not an emission-time unresolved variable. Any explanation must account for a
wrong-tag result rather than an unbound reference.

Settled by direct evidence:

- The failing turn compiles through `add_function`, which does not call
  `lower_jump_crosses_lam`. `compile_inner` is the only caller and runs at
  bootstrap only. Prune-decision and emission therefore see the same tree.
- Constructor ids are **stable across extract invocations**. A real two-turn
  extract gives `t1_cons=124 t2_cons=164 accumulated_cons=164
  qualified_names=164 colliding_names=0`: turn 1's constructors are a strict
  subset of turn 2's with identical `DataConId`s, so the accumulated union adds
  nothing. `ConTags::from_table(t1) == ConTags::from_table(accumulated)`.
- No emission path resolves a constructor via `VarId(dc.id.0)`; the wrapper
  manifest has one consumer outside `datacon_env.rs`, a debug field.

Latent defects found along the way are written up at implementation precision
in `12-contags-staleness-findings.md` and are **not** fixed: the bootstrap-frozen
`ConTags`, the `Result` frozen at `MissingConTags`, and `by_qualified_name`
last-writer-wins. The third is sequenced after cluster C's work on the same
file, since adding a guard changes semantics that cluster C is required to
preserve.

The `by_qualified_name` guard can be a **hard error**, not a warning or a
deterministic tie-break. The precondition is positively proven, not merely
unobserved: a real accumulated table carries 164 qualified names with zero
duplicates. A collision therefore indicates the extractor's minting changed,
which is exactly the condition that should stop the run rather than be
silently absorbed.

The `Result` frozen at `MissingConTags` (defect 1b) is deterministic and live
today — it needs no precondition and is not masked by id stability.

### The guard has a waiting follow-up in cluster C's tests

`merge_table`'s skip-identical filter can select a different
`by_qualified_name` winner than the sequential fold, in one shape: when a later
turn's identical re-insert would have *reclaimed* ownership from a distinct id
that claimed the same qualified name earlier in the same turn. The pre-filter
cannot see that same-turn reordering. It is pinned by
`merge_table_skip_filter_reclaims_qualified_name_ownership_across_turns` and
deliberately not fixed — real sessions cannot reach it, since the accumulated
table is proven collision-free.

Because production's winner on that axis is decided by randomized `HashMap`
iteration, there is no canonical outcome to compare against. Every equivalence
check therefore drives both paths from the same explicit ordered vector, never
from `.iter()`, and `arb_datacon` keys `qualified_name` to `id` so the fuzzer
cannot wander into the undefined region. The `name` axis is still drawn
independently, so identity collisions remain fuzzed — only this one axis is
constrained.

**When the hard-error guard lands, re-widen that generator.** A guard makes the
collision case well-defined (an error), so the axis becomes fuzzable and the
narrowing stops being justified. The pinned divergence test should then become
a collision-error assertion.

## Gates this work added

Permanent, and independent of how the tag investigation resolves:

- `tidepool-runtime/tests/session_table_qualified_identity.rs` — the two-turn
  accumulated-table identity check (GHC-heavy).
- `datacon_never_used_as_value::accumulated_corpora_keep_one_id_per_qualified_name`
  (quick tier).
- `populated_session_second_fragment::bootstrap_contags_still_classify_the_accumulated_table`
  (quick tier), with a positive control that re-mints `Val` and proves the
  assertion fires.
- `tidepool-codegen/tests/populated_session_second_fragment.rs` — compiles a
  second fragment against the accumulated table via `add_function` and runs it
  via `run_child_fragment` with the parent stowed.

`datacon_never_used_as_value.rs` was extended onto the two axes it could not
see: the accumulated superset table, and the post-normalize tree (the stage the
compile path actually reads).

These pin a property the extractor was relied upon to have but nothing stated:
constructor id stability across invocations. Work that changes how the
extractor mints ids will trip these rather than silently misclassifying a
session's yields.

**What `populated_session_second_fragment` does not prove**, established by
mutation rather than asserted. Two breaks were induced:

- *Run-and-classify axis — teeth proven.* Minting `Val` in the bootstrap table
  under a different id than the fragment emits turns all four tests red, the
  child run failing as `Yield(UnexpectedConTag(10))` — the incident's own error
  variant. This axis is genuinely constrained.
- *Emission axis — teeth absent.* Compiling the second fragment against the
  **bootstrap** table instead of the accumulated one leaves all four tests
  green. Emission bakes each `DataConId` straight out of the `Con`/`Case` frame
  and never consults the table to resolve a constructor reference.
  `add_function`'s `table` argument feeds `normalize`,
  `wrap_with_datacon_env`, `lit_wrappers` and the primop id bundles — none of
  which a synthetic `Con`/`Case` fragment reaches. "Compiled against the
  accumulated table" is therefore *setup* in that file, not an assertion, and a
  wrong table there is invisible to it.

Giving it emission-axis teeth requires a fragment that reaches one of those
four consumers. Real GHC-extracted Core would; hand-built trees do not. That is
the concrete upgrade path if someone wants this gate to cover both axes.

The file's doc comment previously claimed the emission axis outright; the
mutation disproved it and the claim was narrowed. Both results are stated
inline in the test file.

It also does not reproduce `selfharness_compaction` and applies no GC pressure,
so it is blind to the allocation-profile hypothesis. And it runs through the
nested-child path whose `NurseryExhausted` guard is known-incomplete, so an
intermittent failure there is ambient rather than a signal about the fragment
path.

The prior gate in this area, `datacon_never_used_as_value.rs`, was blind for a
specific and instructive reason: it read each fixture tree raw off disk against
that fixture's *own* table, using the same `free_vars` the prune used. Every
axis it checked was self-consistent by construction — never a superset table,
never the post-normalize tree, and never compiling or running anything. It was
extended along both missing axes rather than given an extra case, since a case
would have left the shape of the blindness intact.

## Where to pick this up

In wave order, with what each needs:

1. **Cluster D** (`runtime_apply`/`runtime_tail_apply` + per-function
   `FunctionImports`). Owns `emit/expr.rs` after B. This is the
   correctness-sensitive one: its acceptance list is thunk-in-function-position,
   normal and partial application, runtime-error poison, nested tail calls,
   null-without-pending-tail, and cancellation/GC during application. It absorbs
   a duplicated tail-position path, so it is a complexity *reduction* — but a
   duplicated subtle path is exactly where a rewrite hides a behaviour change.
   Run the full differential plus the fork/harness acceptance binaries for this
   cluster specifically.
2. **Cluster E** (D7/D8/D9). `jit_machine.rs` and `stack_map.rs` are clear.
   Fold in the `ConTags` refresh from `12-contags-staleness-findings.md` here —
   it lives in `add_function` alongside the three caches that already refresh.
3. **Cluster F** (D6 `declare_env`). GC-critical. Verify Cranelift's
   `declare_value_needs_stack_map` contract *first* — function-wide versus
   block-sensitive decides between mark-once-at-creation and incremental
   live-roots. Proof is targeted stack-map tests under `TIDEPOOL_GC_POISON` and
   `TIDEPOOL_HEAP_VERIFY`, never inference. If the contract reading is
   ambiguous, escalate rather than guess.
4. **Cluster B's missing evidence** — before/after `emit_ms`, mutation checks on
   both halves, GHC-heavy `resident_session`. Cheap, and it converts a
   correctness-verified change into an evidenced one.
5. **The three latent defects** in `12-contags-staleness-findings.md`, and the
   `by_qualified_name` guard, which can now be a hard error.

## Gates run on the composed branch

| gate | result |
|---|---|
| quick tier (`--no-fail-fast`) | 1775 run, **1775 passed**, 9 skipped |
| differential (`--run-ignored all`, `TIDEPOOL_EXPENSIVE_TESTS=1`) | 1 run, 1 passed |
| GHC accumulation (`session_table_qualified_identity` + `resident_session`) | 5 run, 4 passed, 1 known-open |
| `cargo check --workspace --all-targets`, `fmt --check` | clean |

Differential counters, **identical to the 2026-08-08 baseline in every field**:

```
tested=349, compared=312, closure_skip=34, mismatch=0,
both_error=0, jit_only_error=0, eval_jit_diverge=3, skipped=1
eval_jit_diverge_names: ["xs'_u8286623314361937461", "thunk_blackhole",
                         "xs_u8286623314361937397"]
```

`mismatch=0` against baseline is what licenses cluster B: an indexed free-vars
analysis and a replaced emission-ordering algorithm are both places where a
wrong answer is a miscompile rather than a slowdown.

The one GHC failure is `NurseryExhausted`, verified by its error
(`Run(Jit(HeapBridge(NurseryExhausted)))`) rather than by test name.
`session_table_qualified_identity` and `multi_turn_accumulates_across_suspend_resume`
both pass, which is the evidence that real multi-turn accumulation still works
through cluster C's rewritten ingestion path.

Not run: the turn-latency bench, and cluster B's before/after `emit_ms`. The
performance claims in this branch are therefore *unevidenced*; the correctness
claims are gated.

## Test invocation notes

Every one of these cost real time in this lane. They are here so they cost the
next person none.

- **The differential gate lives in `tidepool-codegen`**, not `tidepool-testing`,
  and needs all three of: `-p tidepool-codegen`,
  `-E 'binary(haskell_suite_differential)'`, `--run-ignored all`, and
  `TIDEPOOL_EXPENSIVE_TESTS=1`. Get any of them wrong and it runs **zero tests
  and exits 0**. Gate on tests-run, never the exit code — this is not a
  hypothetical, it happened here.
- That gate reads pre-built CBOR and spawns no extract subprocess, so it needs
  no GHC slot. Taking one blocks real GHC work for a CPU-bound run.
- **`TIDEPOOL_EXTRACT` alone is not enough for GHC-heavy tests.** The extract
  binary shells out to `ghc`, so the nix `with-packages` wrapper must be on
  `PATH` too. Without it every test fails in well under a second with
  `ghc: readCreateProcess: posix_spawnp: does not exist` — five simultaneous
  failures that look alarming and mean nothing. Derive the path the way
  `scripts/battery.sh` does, by grepping the deployed wrapper for
  `-with-packages/bin`, rather than hardcoding a store path.
- **Capture diagnostics whole to a file and extract afterward.** Two separate
  runs here were piped through `tail` at capture time, destroying the panic
  text and the shrunk proptest counterexample on exactly the runs that needed
  forensics.
- A failure's **mode** is the finding, never its count. Contention produces
  timeouts; it does not produce wrong values, sub-100ms assertion failures, or
  five identical instant environment errors.

## Known-open failures, with signatures

- `selfharness_compaction` — fast-abort with a garbage (large 64-bit)
  constructor tag. A *deterministic* garbage-tag failure is a different bug.
- `NurseryExhausted` — surfaces as `Run(Jit(HeapBridge(NurseryExhausted)))`,
  seen on `resident_session::nested_child_runs_while_parent_suspended_then_resumes`
  (`resident_session.rs:340`). A fix landed covering the eager-response
  `value_to_heap` site; it resurfaced on the nested-child/stowed-continuation
  path, so the class is landed-but-incomplete. That fix is the pattern to
  extend.
