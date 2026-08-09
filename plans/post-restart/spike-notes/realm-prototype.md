# realm prototype — falsifier + cycle-scoping results

Branch: **`root.realm-spike.proto`** — preserved UNMERGED as the spike's
prototype artifact. Additive throughout: `suspended_continuation`,
`run_child_fragment`, `enter_nested_child` and every L7 assert are byte-identical
to their pre-prototype form, and no existing test was edited.

All numbers below are from this branch at commit `HEAD`, on this worktree.

---

## VERDICT ON THE CORE CLAIM

**The claim: permanent rooting replaces the temporal argument. It HELD.**

Today a stowed continuation is safe for two different reasons depending on
state. Idle-suspended, it is safe by a TEMPORAL argument — no GC can run on a
suspended machine, enforced by the L7 `suspended_continuation.is_none()` asserts
on every plain run entry. While a nested child runs, that argument is gone and it
is instead safe by being a REGISTERED GC ROOT.

The prototype drops the temporal half entirely: every parked continuation is a
`stowed_roots` entry from park until resume, and nothing else protects it. Eight
continuations parked in one machine, a real collection (including the doubling
re-evacuate) forced between every park, resumed in a shuffled order, every
pre-suspension captured value deep-verified — under GC poison + heap verify, all
green.

The claim held **with one boundary condition the design must absorb**: the two
suspension paths cannot be MIXED on one machine. See "Where the recommended
shape was wrong" below.

The GC foundation the anchor doc called "ready — VERIFY, don't trust" is in fact
ready. `perform_gc` folds `extend_stowed_roots` into its root assembly
unconditionally (`host_fns/gc.rs:902`), on the first Cheney pass and on the
doubling re-evacuate, which reuses the same `root_slots` vector. Nothing in the
collector needed changing for N roots instead of 1.

---

## THE FALSIFIER IS LIVE, NOT VACUOUS

Before trusting any PASS: delete the `register_stowed_root(slot)` call in
`park_continuation` and re-run. The parked continuations lose their only
protection and the suite dies deterministically, which is exactly what the
poison knob buys:

| case | negative-control outcome |
|------|--------------------------|
| F3 | `force_ptr: unexpected heap tag 221` on `resume_parked(ContinuationId(1))` |
| F4 | `force_ptr: unexpected heap tag 208` on `resume_parked(ContinuationId(3))` |
| A5 parked | same class of failure |
| F1, F2 | **still green** |

F1/F2 staying green is a real limitation, stated rather than hidden: neither
forces a collection between its parks, so they test the registry's ordering and
bookkeeping, not memory safety. **F3 and F4 are the cases that carry the safety
claim.** That split is documented in the test file's header so a later reader
does not over-read a green F1.

The control patch is two lines and is NOT committed — production code carries no
test-only escape hatch. To reproduce: comment out
`self.machine_state.register_stowed_root(slot);` in
`JitEffectMachine::park_continuation` and make `assert_rooting_receipt` return
early.

---

## What was built — the API as it actually settled

In `tidepool-codegen/src/jit_machine.rs`, all additive:

```rust
pub struct ContinuationId(pub u64);
pub struct RealmId(pub u64);
pub enum ParkKind { Plain, Binding { forced: bool } }

pub struct ContinuationFrame {
    cell: Box<*mut u8>,   // heap-stable; the GC rewrites *cell in place
    realm: RealmId,
    suspend_tag: u64,
    kind: ParkKind,
}

// on JitEffectMachine, ALONGSIDE `suspended_continuation`:
continuations: HashMap<ContinuationId, ContinuationFrame>,
next_continuation_id: u64,
```

Run/resume entries:

```rust
pub fn run_suspendable_parked(&mut self, table, handlers, user, suspend_tag, realm)
    -> Result<ParkedOutcome, JitError>;
pub fn run_fragment_suspendable_parked(&mut self, func_id, table, handlers, user,
                                       suspend_tag, realm, kind)
    -> Result<ParkedOutcome, JitError>;
pub fn resume_parked(&mut self, id, table, handlers, user, input)
    -> Result<ParkedOutcome, JitError>;

pub fn parked_count(&self) -> usize;
pub fn parked_ids(&self) -> Vec<ContinuationId>;   // ascending
pub fn parked_realm(&self, id) -> Option<RealmId>;
```

`ParkedOutcome` mirrors `SuspendableOutcome` plus the `ContinuationId` a
suspension parked under.

**THE INVARIANT**, documented on the `continuations` field: a frame's `Box` cell
is registered in `stowed_roots` from the moment it is parked until the moment it
is resumed — not just while a child runs. Consequently
`stowed_roots_count() == parked_count()` at every quiescent point on the parked
path, and every falsifier case asserts that equality at every step. That equality
is the receipt that rooting is what protects them.

Design points worth carrying into a landing:

- **`suspended_continuation` stays `None` on the parked path.** This is the load-
  bearing choice. It means the L7 asserts on the plain entries keep passing and
  keep protecting the single-slot path, and it means a machine holding N parks is
  not "suspended" — plain fragments, further parked turns, and other realms'
  resumes all run against it freely. No nested-child mode is needed.
- **Factored, not duplicated.** `run_suspendable_shared` and `resume_applied` are
  the shared bodies, parametrized by an internal `ParkTarget` the pre-existing
  entries pass as `Slot`. `finish_suspendable`'s `Done` arm is shared verbatim;
  only the suspend arm branches.
- **A5 is preserved on the parked path.** The answer is NF-forced BEFORE the
  frame is removed from the map, so a bottom-bearing answer leaves the frame
  parked AND still rooted. Tested, including a forced collection between the
  rejection and the retry.
- **Ids are never reused.** A resumed id is a clean "no continuation parked"
  error rather than a silent alias of some later park. A re-suspension during a
  parked resume mints a FRESH id in the same realm.
- **`ParkKind`** mirrors the existing `bind_forced: Option<bool>` split
  (`Plain` ↔ `None`, `Binding { forced }` ↔ `Some(forced)`), so the value-plane
  bind path composes with the registry without a second mechanism.

---

## Falsifier results

`tidepool-codegen/tests/realm_multi_continuation.rs`. Every case sets
`set_gc_poison(true)` + `set_heap_verify(true)` and uses a 2 KiB nursery (16 KiB
only where the case does not depend on collecting), so collections are real. F3
additionally asserts `gc_doubling_run_count() > 0` and `heap_verify_run_count()
> 0` — the doubling branch and the verifier are receipts, not assumptions.

| case | what it forces | result | command |
|------|----------------|--------|---------|
| F1 | park parent (A) + a fragment that ALSO suspends (B); both in the map, `stowed_roots_count() == 2`; resume B then A; deep-verify each pre-suspension captured value | **PASS** | `cargo nextest run -p tidepool-codegen -E 'test(f1_two_parks_resumed_child_first)'` |
| F2 | same two parks, resumed in the other order (A then B) | **PASS** | `cargo nextest run -p tidepool-codegen -E 'test(f2_two_parks_resumed_parent_first)'` |
| F3 | park A, park B, then two GC-forcing fragments that collect AND trip heap doubling, then resume both and deep-verify | **PASS** | `cargo nextest run -p tidepool-codegen -E 'test(f3_gc_and_heap_doubling_between_parks)'` |
| F4 | eight parks with distinct captured values, a forced GC between each, resumed in the fixed order `[3,0,7,5,1,6,2,4]`, every captured value asserted | **PASS** | `cargo nextest run -p tidepool-codegen -E 'test(f4_eight_parks_gc_between_each_shuffled_resume)'` |
| A5 parked | a bottom-bearing answer leaves the frame parked and rooted; a GC is forced after the rejection; the retry then succeeds | **PASS** | `cargo nextest run -p tidepool-codegen -E 'test(parked_bottom_answer_leaves_the_frame_parked_and_rooted)'` |
| mixed-path guard | a parked resume with the single slot occupied panics | **PASS** | `cargo nextest run -p tidepool-codegen -E 'test(parked_resume_while_the_slot_is_occupied_panics)'` |
| id hygiene | unknown and already-resumed ids are clean errors; a plain fragment still runs | **PASS** | `cargo nextest run -p tidepool-codegen -E 'test(resuming_an_unknown_or_already_resumed_id_errors_cleanly)'` |

Whole file, and the control group alongside it:

```
cargo nextest run -p tidepool-codegen \
  -E 'binary(realm_multi_continuation) or binary(nested_child_gc_rooting) or binary(continuation_gc_root)'
→ 14 tests run: 14 passed, 0 skipped
```

Full crate at HEAD:

```
cargo nextest run -p tidepool-codegen
→ 661 tests run: 661 passed, 8 skipped
```

(651 before this branch; 10 added. No existing test was edited — the ~400+
existing codegen tests are the control group, so a falsifier failure would have
been unambiguously the new path's fault.)

---

## Cycle-scoping — does reclamation-by-drop actually reclaim?

`tidepool-codegen/tests/realm_cycle_scoped_drop.rs`. 32 create-and-drop cycles,
each machine holding 4 parked continuations + 8 `add_function` fragments, with
one park resumed before the drop on alternating cycles.

```
cargo nextest run -p tidepool-codegen -E 'binary(realm_cycle_scoped_drop)' --no-capture
```

```
=== cycle-scoped drop: 32 machines, 4 parked continuations + 8 fragments each ===
                VSZ (MiB)    RSS (MiB)
baseline            407.2          9.2
peak               8599.2         10.8
after              8599.2         10.8
retained           8192.0          1.6
per machine        256.00         0.05

JIT arena reservation per machine: 256 MiB
/proc/self/maps: 51 -> 115 (2.00 VMAs/machine); vm.max_map_count = 1048576
=> exhausts vm.max_map_count after ~524,000 cycles
```

Bare control — `compile_session` only, no parks, no fragments, no runs:

```
VSZ retained: 8192.0 MiB (256.00 MiB/machine)
RSS retained:    0.3 MiB (  0.01 MiB/machine)
```

**What this says.**

- **RSS is essentially reclaimed.** 0.05 MiB per machine survives a drop, and the
  bare control shows 0.01 MiB of that is fixed per-machine cost — the rest is the
  finalized code pages of the 12 JIT functions each cycle defined (~6 KiB per
  function). The root registries and the session heap `Vec` are genuinely gone;
  `jit_machine.rs`'s `Drop` retires every old-space arena and then calls
  `free_session_heap`, and the prototype adds a registry drain that deregisters
  every parked frame's stowed root before its `Box` cell is freed.
- **JITModule executable memory is NOT reclaimed.** This was the open question
  and the answer is a leak by design: `ArenaMemoryProvider::drop`
  (cranelift-jit 0.129.1, `src/memory/arena.rs:209`) frees its reservation only
  if no segment was finalized — *"otherwise leak it since JIT memory may still be
  in use"*. Every machine finalizes (`CodegenPipeline::finalize` in
  `compile_inner`) and nothing calls `free_memory()`, so every dropped machine
  leaks its whole 256 MiB reservation.
- **The leak is address space, not memory.** The arena is reserved `PROT_NONE`
  and demand-paged, so the 256 MiB/machine is VSZ; only the finalized pages are
  resident, which is the 0.05 MiB. `peak == after` in the table because at these
  machine sizes the leaked residue dominates a live machine's own footprint.
- **The ceiling is ~500k cycles**, from either direction: 128 TiB of user address
  space ÷ 256 MiB, or (on a default `vm.max_map_count` of 65530 rather than this
  box's 1048576) ~32k cycles at 2 VMAs per machine. Either is far outside any
  realistic harness session, so **cycle-scoped drop is viable** — but it is a
  bounded leak, not a clean reclaim, and it scales with CYCLES, not with parks or
  fragments. Parks and fragments cost nothing at the address-space level: the
  full cycle and the bare control leak the identical 256.00 MiB/machine.
- **The cheap mitigation, if the ceiling ever matters**, is the arena size itself
  (`pipeline.rs`: `ArenaMemoryProvider::new_with_size(256 * 1024 * 1024)`) —
  shrinking it raises the cycle ceiling proportionally. The real fix is calling
  `JITModule::free_memory()` on pipeline drop, which requires proving no function
  pointer outlives the machine; `CodegenPipeline` already exposes `pub module`, so
  the door is open. Neither was attempted here — out of scope for the falsifier.

The VSZ finding is pinned as an assertion in the test, deliberately phrased as a
finding gate: if someone later makes the pipeline free its arena, the assertion
fails and should be rewritten to its opposite rather than relaxed.

**A sibling lane ('lifetime') is measuring the JITModule question independently.
If its numbers disagree with these, that disagreement is itself a finding and
should be reported as one, not reconciled away.**

---

## Where the recommended shape was wrong, and what I did instead

**1. The two suspension paths cannot coexist on one machine.** The spec's shape —
park into the map, leave `suspended_continuation` as `None` — is right, and it is
what makes further computation possible. But the corollary was not stated: a
continuation held in the SLOT is unregistered, protected only by the temporal
argument. If a machine holds a slot continuation and a parked one at the same
time, then resuming the parked one runs collections while the slot-held one is
unrooted, and the slot-held one dies.

The run side was already closed — `run_suspendable_shared` inherits the existing
L7 assert, so you cannot park while the slot is occupied. The resume side was
open, so I added the sibling assert to `resume_parked` and a test
(`parked_resume_while_the_slot_is_occupied_panics`) that reaches the mixed state
and confirms it panics rather than silently corrupting. This is additive — no
existing assert changed.

This is not a wall; it is a constraint on the landing's SHAPE, and it is the
answer to step 5 below.

**2. `finish_suspendable` needed `suspend_tag`.** The frame stores the tag so
`resume_parked` can replay it, which means the epilogue needs it too. Added as a
parameter; both pre-existing callers already had it in scope.

**3. `ParkedRaw` — an internal third outcome type.** `SuspendableOutcome` has no
id and `ParkedOutcome` requires one, so the shared bodies return an internal
`ParkedRaw` with `id: Option<ContinuationId>` that each public entry projects.
The alternative — leaking an `Option` into the public parked API — was worse.

---

## Step 5 (the `ChildSuspended` wall one level up) — NOT DONE, and why

Explicitly optional, and skipped. But finding (1) changes what it costs, so the
answer is worth more than the half-implementation would have been.

`ResidentSession` holds `pending: Option<String>` — the same one-slot shape, one
level up — and `run_child` (`resident.rs:415`) drives the child through
`run_child_fragment`, i.e. nested-child mode against a SLOT-held parent. The wall
is `finish_child_outcome`'s `SuspendableOutcome::Suspended { .. } =>
Err(ResidentError::ChildSuspended)` at `resident.rs:509`.

**Because the paths cannot mix, lifting the wall is a CONVERSION, not an
addition.** You cannot park only the child: the parent is in the slot, and
resuming the child would unroot it. `ResidentSession` has to move its parent to
the registry too, which means:

- `pending: Option<String>` → a map from hole-id to `ContinuationId`;
- `classify` (`resident.rs:~774`) mints a hole per suspension → inserts into that
  map;
- `resume`/`abort`'s validate-before-consume (`resident.rs:~646`) → a map lookup;
- `run_child`/`run_child_pure` stop using nested-child mode entirely — with the
  parent parked, the machine is not suspended and a child is an ordinary
  fragment run;
- `finish_child_outcome`'s `ChildSuspended` arm is deleted.

Blast radius outside `resident.rs` is small and enumerable: `is_idle()` at
`tidepool-repl/src/server.rs:516` and `tidepool-harness/src/harness.rs:1732`;
`run_child` at `harness.rs:2246` and `harness.rs:2474`; the `ChildSuspended`
handler at `harness.rs:2505`; and `tidepool-runtime/tests/resident_session.rs`
(3 sites). Note `resident.rs:196` is also a Track-1 (one-compile bootstrap) site,
so this conversion and the extract wave will collide there — deliberately, per
this lane's charter.

---

## Implication for `fork_snapshot`

Not measured here, so stated as an implication rather than a receipt: the
prototype confirms the runtime precondition the full-fork decision
(`harness-one-model-full-fork`, 2026-08-01) wants — parent and child
continuations coexisting in ONE heap, with the child free to suspend. Within a
cycle, a fork's inherited state is reachable without a cross-machine Cheney
clone, because there is no second machine. `fork_snapshot`'s 400-LOC clone would
shrink to the cross-cycle case only. Whether it shrinks to nothing depends on
whether cross-cycle fork inheritance is needed at all, which is a harness
question this spike does not answer.

---

## Method notes

- Every heap-touching prototype run was executed under GC poison + heap verify
  (`set_gc_poison(true)` + `set_heap_verify(true)`), with a nursery small enough
  to force real collections; F3 asserts the doubling branch and the verifier
  actually ran rather than assuming a nursery size forces them.
- No GHC-heavy runs were needed — `tidepool-codegen` is a pure-Rust crate in the
  fast default tier, so no `ghc-slots.sh` acquisition was involved.
- `cargo nextest run -p tidepool-codegen` was green before every commit that
  touched `tidepool-codegen/src`, not only at the end.
