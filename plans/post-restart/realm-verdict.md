# Realm-machine spike — VERDICT

Track 2 of `plans/post-restart/one-compile-bootstrap.md`. Question: can one JIT
machine hold multiple parked continuations with realm ownership and
cycle-scoped lifetime?

Prototype branch, preserved unmerged: **`root.realm-spike.proto`** (HEAD
`55a9df5c`). Findings: `spike-notes/realm-{checklist,lifetime,leak-comparison,prototype}.md`.
Verified at `root.realm-spike`, rebased onto `harness-interaction-surface`
(cd0f4002, jit-chain-2 folded).

---

## DECISION: GO, conditional on two enforced constraints

The design's foundation holds empirically, and the two costs the anchor doc
suspected are both real but bounded — one of them is a cost the design
*reduces* rather than introduces.

The conditions are not caveats to note and move past; each is a thing the
landing must actively enforce, and each is cheap:

1. **Realms sharing a machine must have position-compatible handled prefixes.**
   Every row the harness builds today satisfies this, but nothing checks it.
   The landing adds the check. (§5 — this is what keeps checklist Item 5 from
   being a wall.)
2. **The two suspension paths must not mix on one machine.** A slot-held
   continuation is unregistered and dies if a parked sibling's resume collects.
   The prototype closed the open side with an assert; the landing removes the
   slot path from any machine that parks. (§3)

Both are constraints on shape, not new mechanism. Nothing in the spike
uncovered a wall.

---

## 1. Scored checklist

Items 1–5 from `realm-checklist.md`, items 6–7 from `realm-lifetime.md`.
Ratings are that lane's; the STATUS column is this verdict's synthesis, which
in two cases differs from the lane's own reading.

| # | Item | Rating | Status after synthesis |
|---|------|--------|------------------------|
| 1 | `pending` / suspension bookkeeping | REAL | **Discharged empirically.** See below. |
| 2 | Binding / decl planes | REAL (display-name half) | Narrowed. VarId half near-FREE via D9; needs one pinning test. |
| 3 | Finalized + bound root slots | MECHANICAL | Field relocation, with a named latent bug if skipped. |
| 4 | Cancellation | MECHANICAL | Flag moves to the frame; install pattern already per-run. |
| 5 | Effect roster + suspend threshold | REAL | **Downgraded to an enforceable constraint.** See §5. |
| 6 | Persistent-root retirement | REAL | Real, bounded; cycle-scoping reclaims it fully. |
| 7 | Compiled-function lifetime | REAL | Real, unfixable by drop — but unification *reduces* it ~2.7×. |

**Item 1 is the one the spike existed to settle, and analysis alone could not.**
`realm-checklist.md` rated it REAL and said so honestly: the new invariant is
"N independently resumable parked continuations, resumed in caller-chosen
order, GC-safe throughout", which today's single-slot-plus-depth-counter cannot
express *at all* — not correctly, not incorrectly, simply not representably. It
flagged that only the prototype could settle whether the invariant holds. It
does (§2). Item 1 stays REAL as a measure of work, but it is no longer a risk.

**Item 3 carries a concrete bug the landing must not walk into.** If
`continuations` lands but `last_bound_root`/`suspended_finalized_root` stay as
machine-level `Option`s, two realms completing binds before either is drained
means the second overwrites the first, and `materialize_binder` silently binds
the WRONG value under the first realm's name — both are valid `RootSlot`s, so
nothing panics or type-errors. Today this is impossible (one computation per
machine, `&mut self` serialization). It becomes reachable the moment realms
land. The fix is moving both fields onto `ContinuationFrame`.

---

## 2. The falsifier: the core claim held

The claim under test: **permanent rooting replaces the temporal argument.**

Today a parked continuation is safe for two different reasons depending on
state — idle-suspended it is safe because no GC can run on a suspended machine
(the L7 `suspended_continuation.is_none()` asserts); mid-nested-child it is
safe because it is a registered GC root. The realm design drops the temporal
half entirely and roots every parked continuation for its whole parked
lifetime.

**HELD.** Eight continuations parked in one machine, a real collection forced
between every park, resumed in a shuffled order, every pre-suspension captured
value deep-verified, under `set_gc_poison(true)` + `set_heap_verify(true)`.

| case | forces | result |
|------|--------|--------|
| F1 | parent + a child that ALSO suspends; both parked; resume child-first | PASS |
| F2 | same two parks, resumed parent-first | PASS |
| F3 | GC **and heap doubling** between parks, then resume both | PASS |
| F4 | eight parks, GC between each, resumed `[3,0,7,5,1,6,2,4]` | PASS |
| A5-parked | bottom answer leaves the frame parked AND rooted; GC; retry succeeds | PASS |
| mixed-path guard | parked resume with the slot occupied panics | PASS |
| id hygiene | unknown / already-resumed ids error cleanly | PASS |

`cargo nextest run -p tidepool-codegen` → 694 passed at proto HEAD. Zero
existing tests edited, so the ~650 pre-existing codegen tests are an intact
control group.

**The PASSes mean something because the falsifier was shown to be able to
fail.** Deleting the single `register_stowed_root(slot)` call in
`park_continuation` kills F3, F4 and A5-parked deterministically on poisoned
heap tag 221 (0xDD). Re-run after the jit-chain-2 rebase: still kills. That
re-run mattered — a GC-path change that made parked continuations reachable by
some other route would have left the suite green and vacuous.

**Stated limitation, not buried:** F1 and F2 stay green under the negative
control. They force no collection between parks, so they test the registry's
ordering and bookkeeping, not memory safety. F3 and F4 carry the safety claim.

**The GC foundation the anchor doc said to verify rather than trust is
genuinely ready.** `perform_gc` folds `extend_stowed_roots` into its root
assembly unconditionally (`gc.rs:902`), on both the first Cheney pass and the
doubling re-evacuate. Zero collector changes were needed for N roots instead
of 1.

---

## 3. The boundary condition (constraint 2)

The prototype's shape — park into the map, leave `suspended_continuation` as
`None` — is what makes further computation possible on a machine holding parks:
the L7 asserts keep passing, so plain fragments, further parked turns, and
other realms' resumes all run freely. No nested-child mode is needed at all.

The corollary was not anticipated: **a continuation held in the SLOT is
unregistered**, protected only by the temporal argument. A machine holding both
a slot continuation and a parked one is unsound — resuming the parked one runs
collections while the slot-held one is unrooted. The run side was already
closed by the inherited L7 assert; the prototype added the sibling assert on
`resume_parked` plus a test that reaches the mixed state and confirms it panics
rather than corrupting silently.

**Consequence for the landing: lifting `ResidentSession`'s `ChildSuspended`
wall is a CONVERSION, not an addition.** You cannot park only the child — the
parent is in the slot, and resuming the child would unroot it. `pending:
Option<String>` becomes a map; `classify` mints a hole per suspension;
`resume`/`abort`'s validate-before-consume becomes a map lookup;
`run_child`/`run_child_pure` stop using nested-child mode entirely (with the
parent parked the machine is not suspended, so a child is an ordinary fragment
run); `finish_child_outcome`'s `ChildSuspended` arm is deleted.

Blast radius outside `resident.rs` is small and enumerated: `is_idle()` at
`tidepool-repl/src/server.rs:516` and `harness.rs:1732`; `run_child` at
`harness.rs:2246` and `:2474`; the `ChildSuspended` handler at `harness.rs:2505`;
`tidepool-runtime/tests/resident_session.rs` (3 sites). Note `resident.rs:208`
is also a Track-1 site — this conversion and the extract wave will collide
there.

---

## 4. Cycle-scoping and reclamation

Does dropping a machine reclaim? Three of four, and the fourth is bounded.

| resource | reclaimed on drop? | evidence |
|---|---|---|
| Session heap (`Vec<u64>`, nursery) | **Yes** | ordinary ownership; `Drop` → `free_session_heap` |
| Old-space arenas | **Yes** | `Drop` retires each arena's barrier bookkeeping, then the `Vec<u8>`s drop |
| Persistent / stowed root registries | **Yes** | `free_session_heap` clears all three registries |
| JITModule executable memory | **No** | measured, three lanes agree |

**The JITModule leak is by design and not tidepool's.**
`ArenaMemoryProvider::drop` (cranelift-jit 0.129.1, `memory/arena.rs:209`) frees
its reservation only if no segment was finalized — *"otherwise leak it since JIT
memory may still be in use."* Every machine finalizes on its first compile, and
`free_memory()` is called nowhere in the repository. So cycle-scoped drop
reclaims the heap side fully but leaves the code side behind.

**The leak is address space, not resident memory, and it is bounded.** The arena
is reserved `PROT_NONE` and demand-paged, so 256 MiB/machine is VSZ while only
finalized pages are resident. Two lanes measured the VSZ figure independently
and agree byte-exactly: 32 machines → 8,589,934,592 B = exactly 32 × 256 MiB; 1
machine → exactly 256 MiB. Ceiling is ~500k cycles (128 TiB ÷ 256 MiB), or ~32k
on a default `vm.max_map_count`. Both are far outside any realistic harness
session.

**Unification reduces this cost rather than introducing it — the finding that
flips Cost B's sign.** Today's architecture creates a machine per answerer
session, so it already leaks per machine. Measured on identical 512-fragment
workloads from one baseline in one process: 32 machines retain ~6.37 MB RSS, 1
machine retains ~2.36 MB — a stable 2.68–2.77× reduction across 4 runs. The fit
is ~125 KB fixed per machine + ~4.4 KB per function, so the leak is dominated by
per-machine setup, not per-function payload; unification pays the machine tax
once per cycle instead of once per answerer. On address space the improvement is
larger still — one 256 MiB reservation per cycle instead of one per session.

**Cross-lane disagreement, carried forward unresolved rather than reconciled.**
The three lanes disagree on the per-machine RSS constant: ~200 KB (`lifetime`),
~125 KB fixed + per-function (`leakcmp`), ~50 KB (`proto`). Two candidate causes
were named — fragment size (per-function cost is a shape property, not a
constant) and baseline placement (whether one-time process init is inside the
measured window). **Do not carry any of these numbers forward as a property of
the machine.** What all three agree on is the direction (monotonic, never
reclaimed) and the address-space figure (256 MiB/machine, exact).

An immortal unified machine remains firmly out, as the anchor doc said.
Cycle-scoped is what keeps the per-cycle payload bounded.

---

## 5. Why Item 5 is a constraint, not a wall

`realm-checklist.md` rated Item 5 REAL and carried that rating entirely on a
structural concern: `DispatchEffect` is positional over an `HList`
(`HCons::dispatch` peels tag 0 and recurses with `tag - 1`), and
`ResidentSession<H, O>` is monomorphized over ONE concrete `H` for its life. Two
realms with different handled-effect prefixes sharing one machine would
misroute — its worked example showed a `Memory` call wrongly suspending in one
direction and an `Ask` wrongly dispatching in the other.

That analysis is correct. It was scoped read-only to codegen/runtime, so it did
not check what rows the harness actually builds. Doing so resolves the question:

- `suspend_tag` is computed per decls-list as the position of the first
  interposed effect — `Ask | AskUser | RunLLMTurn | Fork | Finalize`
  (`engine.rs:722`).
- `standard_decls()` = `base_effects ++ [Ask, RunLLMTurn, Fork]`;
  `agent_decls()` appends `Finalize`. Handled prefix = `base`, threshold =
  `len(base)`.
- The outer driver compiles `vec![runllmturn_decl()]` — threshold **0**, handled
  prefix **empty**. This is the anchor doc's "interposed at threshold zero".
- `fork_child_decls` filters out `Fork`/`RunLLMTurn`, which are both *above* the
  threshold. Handled prefix is untouched: still `base`.

So every row the harness builds has a handled prefix that is either `base` or
empty. An empty prefix is position-compatible with anything, and every
agent-family row shares `base` identically. **The compatibility condition holds
across all of them** — including the exact pairing cycle-scoping proposes
(outer loop + that cycle's answerer tree).

Two things follow, and both are cheap:

1. **The threshold must be per-frame** (0 for the outer realm, `len(base)` for
   an answerer). This is the mechanical half the anchor's own frame sketch
   already anticipated — the frame carries its own `suspend_tag`.
2. **Interpreting WHICH interposed effect fired must be per-frame.** Removing
   `Fork` from the middle of a row shifts every interposed tag above it
   (`[AskUser, Fork, Finalize]` → `[AskUser, Finalize]` moves `Finalize` from 2
   to 1). Tags above the threshold never reach a handler — they suspend and go
   up to the caller — so this is not a dispatch hazard, but the caller must
   decode the tag against *that frame's* row. The frame carries its `DataConTable`
   and effect names.

**What the landing must add: an enforced check.** Nothing today verifies
prefix-compatibility, and the property holds by construction of row-building
code that was written for other reasons. Parking a realm whose handled prefix
disagrees position-by-position with the machine's installed handler stack, up to
the shorter threshold, must be refused loudly at park time. That converts a
silent-misroute class into a startup error.

A dynamic per-frame dispatch object (`Box<dyn DispatchEffect<U>>`) remains the
escape hatch if a future row genuinely needs a different handled prefix. It is
not needed for any row that exists.

---

## 6. Implication for `fork_snapshot`

**`fork_snapshot` does not exist.** Zero `.rs` hits anywhere in the workspace;
it appears only in planning documents. The anchor doc's "400-LOC clone shrinks,
maybe to nothing" describes a *future* cross-machine Cheney clone that the
full-fork decision (`harness-one-model-full-fork`, 2026-08-01) would need if
parent and child continuations lived on separate machines.

So the honest framing is **a cost avoided by not building something**, not a
cost this spike measured being paid. Stated as an implication, not a receipt:
the prototype confirms the runtime precondition full-fork wants — parent and
child continuations coexisting in one heap, with the child free to suspend. That
was the thing `ChildSuspended` forbade and the thing F1 demonstrates. Within a
cycle, a fork's inherited state is reachable with no cross-machine clone because
there is no second machine; `fork_snapshot` would shrink to the cross-cycle case
only. Whether it shrinks to nothing depends on whether cross-cycle fork
inheritance is needed at all, which is a harness question this spike does not
answer.

Today's segment-40 nested-child mechanism already runs parent + child on one
heap with zero cloning, so the "400 LOC" figure is itself sized for a
cross-machine case with no implementation to measure. Treat it as unquantified.

---

## 7. Recommended landing shape

Sequenced so each step is independently green:

1. **Registry at the machine layer** — `ContinuationId`, `RealmId`, `ParkKind`,
   `ContinuationFrame { cell, realm, suspend_tag, kind }`, `continuations:
   HashMap`. Invariant: a frame's `Box` cell is a registered `stowed_root` from
   park until resume, so `stowed_roots_count() == parked_count()` at every
   quiescent point. Land additively first, exactly as the prototype did — the
   existing tests stay a control group.
2. **Move the per-realm fields onto the frame** — `last_bound_root`,
   `suspended_finalized_root` (Item 3's named bug), `cancel_flag` (Item 4),
   `suspend_tag` + `DataConTable` (§5's mechanical half).
3. **Add the prefix-compatibility check** at park time (§5). Refuse loudly.
4. **Convert `ResidentSession`** — `pending` to a map, delete the
   `ChildSuspended` arm, drop nested-child mode. This is the conversion of §3,
   and it collides with Track 1 at `resident.rs:208` — sequence it after the
   extract wave's boot-site work, not concurrently.
5. **Add the Item 2 pinning test** — two scopes, colliding local names, assert
   neither's `ExternalEnv` ever contains the other's `SessionVarId`. D9 gives
   this property today but was motivated by compile cost, not realm isolation,
   and its own tests would keep passing if a future change reintroduced the
   leak.
6. **Realm-scope the display-name layer** — `current`/`resolve` is the surviving
   half of Item 2. A realm cannot corrupt another's binding (ids are minted
   fresh), but a `:bindings`-style view can report the wrong owner for a shared
   name. Lowest priority; it is a display bug, not a safety one.

Do NOT merge source-level capability rows at any step. The compile-time boundary
(answerer code cannot import `runLLMTurn`) is independent of machine
unification, and `fork_child_decls` is what enforces it.

---

## 8. What would flip this to NO-GO

Recorded so a later reader can check whether the verdict still holds:

- The negative control stops killing F3/F4 — the falsifier would be vacuous and
  the safety claim unsupported. (Re-checked once already, after jit-chain-2.)
- A harness row appears whose handled prefix is neither empty nor `base`, making
  §5's compatibility condition false in practice rather than merely unchecked.
- Cycle count per session approaches ~32k, where the address-space leak stops
  being theoretical.
- A realm needs to outlive its cycle, which reintroduces the immortal-machine
  growth the anchor doc ruled out.

---

## 9. Receipts

| claim | receipt |
|---|---|
| Falsifier F1–F4, A5-parked, 2 guards | `cargo nextest run -p tidepool-codegen -E 'binary(realm_multi_continuation)'`; per-case commands in `realm-prototype.md` |
| Negative control kills, post-fold | control patch (2 lines, uncommitted) → F3/F4/A5 die on tag 221 |
| No regressions | 694 passed at proto HEAD; 684 at `root.realm-spike`; 0 existing tests edited |
| Root growth 1:1 with binds | `realm_root_growth` — roots = N, old-space = N × 56 B at N ∈ {1,8,64} |
| JITModule not reclaimed | `realm_module_growth` — after-32-drops == peak, byte-exact in 3/4 runs |
| Unification ~2.7× less RSS | `realm_leak_comparison` — 6.37 MB vs 2.36 MB, 4 runs, ratio 2.68–2.77× |
| 256 MiB/machine VSZ | two lanes, byte-exact: 32 × 256 MiB and 1 × 256 MiB |
| Cycle-scoped drop | `realm_cycle_scoped_drop` — 256.00 MiB/machine VSZ, 0.05 MiB RSS |
| Row shapes (§5) | `engine.rs:722` threshold; `eval_prep.rs:78` `standard_decls`; `harness.rs:293` `fork_child_decls` |

All heap-touching runs under `TIDEPOOL_GC_POISON` + `TIDEPOOL_HEAP_VERIFY`.
Anchors verified against the post-jit-chain-2 tree.

---

## 10. Prototype branch

**`root.realm-spike.proto`**, HEAD `55a9df5c`, preserved unmerged. Contains the
continuation registry, the parked run/resume entries, `realm_multi_continuation.rs`,
and `realm_cycle_scoped_drop.rs`. Rebased onto the current tree and green there.
It is a prototype, not a landing: `suspended_continuation` and every existing
entry are untouched, which is what a landing would collapse.
