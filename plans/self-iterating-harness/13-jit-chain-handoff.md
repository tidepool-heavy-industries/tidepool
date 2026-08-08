# jit-chain — cluster ledger and successor handoff

Spec material for the successor TL (`jit-chain-2`). Read alongside the
authoritative item list (the D-numbered findings) and
`11-jit-codegen-latency-receipt.md`. Where this document and a spec disagree
about a file or a site, this document is later and was checked against the
source.

## Cluster ledger

| cluster | scope | status |
|---|---|---|
| A | case-trap intern arena (D13), `Int64ToWord64` result tag, blackhole gap | see below |
| B | indexed free-vars analysis + petgraph topo sort (C4+C5) | **held, WIP branch** |
| C | `DataConTable` accumulation hygiene (D3) | **landed**, two items deferred |
| D | `runtime_apply`/`runtime_tail_apply` + `FunctionImports` (D4+D5) | **not started** |
| E | lambda registry, stack-map lookup, `seed_external_env` (D7/D8/D9) | **not started** |
| F | `declare_env` (D6) | **not started** |

D, E and F were never started. E's fence (a sibling holding uncommitted
`jit_machine.rs` work) was lifted before the quiesce, so E is unblocked on
entry. B must own `emit/expr.rs` alone; D follows B on the same file.

## Cluster B — resume from the WIP branch, do not restart

Branch `root.jit-chain.cluster-b`, commit `2bbc583f`. Not folded: it carries an
edit that was never compiled, and an unverified edit must not reach a submitted
branch. The branch is durable independently of its worktree.

**Verified green before the checkpoint.** `emit/free_vars_index.rs` —
`FreeVarsIndex::compute`, a single forward pass over the flat node vector
(children precede parents, so no explicit stack walk is needed), with
`Rc<FxHashSet<VarId>>` shared for pass-through nodes and a canonical empty set.
Its equivalence test (`tests/free_vars_index_equivalence.rs`) matched the
reference `free_vars(extract_subtree(idx))` at every index across 103,011
real-corpus nodes (138 fixtures) and 14,686 generated-tree nodes.
`EmitSession.free_vars_idx` is built at all four construction sites
(`compile_expr`, `emit_lam`, `emit_thunk_promised`, LetRec phase 3a). All seven
conversion sites were converted and green.

**Not verified.** The petgraph topological sort: Kahn's algorithm over a
`DiGraph`, tie-broken by a min-heap on `NodeIndex` so ordering follows original
binding order, with `Dfs` for reachability. Written in full and hand-traced
against all five golden shapes, but the final edit was never compiled. The
golden tests (independent, chain, diamond, cycle, self-reference) are written
and were green against the *old* algorithm, which is what makes them a
behaviour-identity check rather than a restatement of the new one. Budget ~10
minutes to compile-check and rerun them before anything else.

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

## Test invocation notes

- The differential gate is `#[ignore]`d. Selecting its binary without
  `--run-ignored all` runs nothing and still exits 0 — check tests-run counts,
  never the exit code.
- That gate reads pre-built CBOR and spawns no extract subprocess, so it needs
  no GHC slot.
- `selfharness_compaction` is open-intermittent. Signature: a fast-abort with a
  garbage (large 64-bit) constructor tag. A *deterministic* garbage-tag failure
  is a different bug.
