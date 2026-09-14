# Wave 6: prepared-STG effect suspension, retained imports, and resume

Wave 6 goal: a `PreparedMachine` session whose heap and continuations persist
across turns, effects suspend to Rust handlers and resume, later programs
link against retained values by generation, parked work interleaves, and
cancellation is scoped per turn while actor incarnations own their parked
continuations.

Wave 6A landed the substrate this design sits on: a persistent
`PreparedMachine` (`tidepool-codegen/src/prepared_program/machine.rs`), opaque
linear `PreparedValue`s with `inspect_outer`, managed entry arguments through
`run_entry_retained`, stable old-space root slots (`OldSpace::retain_prepared`
+ `RootHandleLedger`), and a real freer `send` artifact
(`haskell/test-prepared-stg/fixtures/freer-retention.cbor`) whose `E`
continuation survives collection as data. `plans/stg-wave6-handoff.md` named
executable imports as the next owner and forbade resident/workbench cutover
this wave; that handoff's content is absorbed here.

## Acceptance ladder

Each rung's criterion is a command/test that fails before the rung lands and
passes once it does. Ordered by minimal new surface first.

| Rung | Criterion | Status |
|---|---|---|
| 0 | Decode a real effect request with a real closure `Leaf` field (no suspension), classified without going through `observe()` (which rejects functions/PAPs) | **Done** (Wave 6A) |
| 1 | Smallest real end-to-end suspend/resume: print -> sleep -> print, single turn, single realm; heap object identity checked across resumes | Wave 6B (E1-E3) |
| 2 | Retained bindings across turns: turn N+1's program links against turn N's binding via `required_generation`, reads it off the same persistent heap (identity, not re-import by value) | Wave 6B (S1-S6) |
| 3 | Interleaved parked work: two suspended continuations share one heap, resumed out of order, survive an intervening nursery GC | Not started this wave |
| 4 | Cancellation of one parked turn among siblings, via a realm-scoped `CancelHandle`; sibling and `close_realm` counts unaffected | Not started this wave |
| 5 | Actor-turn authority: retiring an incarnation releases its parked frame; a different incarnation cannot resume it | Not started this wave |
| 6 | Composite: rungs 2-5 together in one resident session — the actual gate for calling Wave 6 done | Not started this wave |

Rung 3 has a dependency this design makes explicit rather than treats as an
implementation detail discovered mid-rung: entering a thunk that is
`DescriptorState::Evaluating` (blackholed) is today indistinguishable from
`<<loop>>` — that indistinguishability *is* the loop-detection mechanism.
Rung 1 and 2 are safe because at most one evaluator ever touches the heap at
a time; rung 3 puts two parked continuations' blackholed thunk chains on one
heap, so a resuming evaluator can re-enter a thunk blackholed by a *different*,
still-parked continuation and wrongly report `<<loop>>`. The blackhole-vs-loop
distinction (evaluator identity on the descriptor state, plus a wait/settle
contract for "not mine, park behind it") must land as its own deliverable
before rung 3 is attempted, not be discovered as a bug during it. No existing
design or ticket in `plans/` or `docs/` resolves this beyond the one paragraph
in `docs/stg-projection-inventory.md` ("`noDuplicate#` execution invariant")
and the mirrored bullet in `plans/stg-wave5-delivery.md`'s failure contracts.

## Decisions

**D1. The import seam is a multi-program machine, not value re-homing.**
Copying a retained value into a fresh per-program machine only works for
first-order data: closures and thunks carry code pointers into the producing
program and load their top table through `vmctx.prepared_tops`. A notebook's
retained bindings are routinely functions, and a parked continuation is a
closure. So one `PreparedMachine` must host several installed programs on one
heap, and generated code must find its own tops regardless of which program's
code is executing. Consequence: one machine-wide top table with a fixed
reserved capacity; each installed program compiles against a slot base and
claims a contiguous range; an import is one more top slot whose initial word
is the current pointer of a retained root slot, registered as a persistent
root like heap tops are today (`machine.rs` install loop). Exhaustion is a
typed error, not growth-by-reallocation (registered root addresses must not
move). Descriptor registries, static regions and stack-map registries become
per-machine unions of every installed program. Installed programs are never
uninstalled this wave (bounded by machine rotation later, per
`docs/continuation-parking-contract.md` "Bounded lifetime").

This directly resolves two blockers named in Wave 6A's handoff: `admission.rs`
rejecting any program with globals, and `ExecutionProjection.hs` only
declaring a global when the referenced id's module is not a home module (a
retained binding from an earlier turn of the *same* session is a home module
and would otherwise fail as `MissingPreparedTop`).

**D2. Resume is the compiled `qApp`, called through a Haskell wrapper entry.**
Freer suspension in the prepared engine is a plain call/return: `run_entry`
returns the `E` constructor at WHNF (`freer_boundary_tests.rs`). Given managed
arguments now exist, a wrapper top `resumeInt :: Arrs '[Req] Int Int -> Int#
-> Eff '[Req] Int; resumeInt k n = qApp k (I# n)` projected as a second entry
of the same artifact is a Rust-callable resume with no Rust-side walker, no
second decoder of the freer envelope, and no new codegen. It also settles the
open question of whether `qApp` survives as an admitted top: the probe
answers it by construction, and a failure to admit is itself the finding.
This wave does not touch `effect_machine.rs`/`jit_machine.rs`.

This is a narrower stand-in for the "apply closure/PAP to argument" primitive
the design investigations flagged as entirely missing from the prepared
engine (`apply.rs`'s dispatch machinery only runs for compile-time call
sites); D2 gets a Rust-callable apply without building that primitive by
routing through compiled Haskell instead of a new host routine.

**D3. The concurrency gate closes for rungs 1-4 without evaluator identity.**
Because suspension is by return, no native stack is captured and every thunk
on the path to `E` has settled; the only `Evaluating` headers that outlive a
call are single-entry thunks deliberately retained after consumption (pinned
in 6A). A parked continuation is inert heap data. `&mut self` on
`PreparedMachine` serializes turns. So the `noDuplicate#` no-op lowering
(`docs/stg-projection-inventory.md` "`noDuplicate#` execution invariant")
remains valid with interleaved parked continuations. This wave pins that with
tests and records it in the inventory. Evaluator identity is needed only if a
future design captures native stacks; that is out of scope and must be
re-decided then, not assumed closed.

On `BindingTable` reuse (per the Wave 6A handoff's instruction to reuse the
existing `PersistentSession`/`BindingTable` owner rather than add a pointer
cache): `BindingTable` (`tidepool-codegen/src/binding_table.rs`) keys
`SessionVarId -> BindingEntry { value: BoundValue(RootSlot), module:
SessionModule(generation), .. }` with ref-counted leases. `RootSlot` is the
same `old_space::RootSlot` type the prepared `RootHandleLedger` holds, so the
name/generation/lease ledger is reusable as-is; root registration stays with
`PreparedMachine` (`release` is the only deregistration path). The prepared
engine adds a `BoundValue` variant rather than a second table. `MachineLease`
in `persistent.rs` is unrelated (an affine borrow of the Core machine) and
must not be conflated with this.

## Non-goals (deferred to Wave 7)

- Real stack snapshots, IPE, libdw. There is no GHC stack-snapshot intrinsic
  in this prepared projection/native operation catalog, and the IPE decoder
  worker stays deferred by exact identity (`d90ca7551`).
- Atomic `MutVar#` variants (still deferred from Wave 5 delivery order item 4).
- Unused primitive-family completeness — Wave 6 does not expand the
  primitive catalog beyond what the acceptance-ladder scenarios call.
- Full corpus/producer cutover or deleting the old Core JIT
  (`plans/stg-production-cutover.md` phase 6). Wave 6 extends the
  continuation-parking *contract* to a second engine; it does not retire the
  first.
- GHC-faithful stack resumption on cancellation. Cancellation restores a
  thunk to `Live` and retries it from the beginning; it is not GHC's
  suspended-stack resumption. Wave 6's cancellation rung must not promise
  more than this.
- Managed host arguments and `Address` materialization for the general case
  (still-open Wave 5 execution limitations); Wave 6 only touches these where
  a specific ladder rung needs one.
- Resident-workbench cutover (routing real notebook turns through the
  prepared engine instead of Core). `session/prepared.rs`'s note that
  production `resident_workbench` still dispatches Core stands; Wave 6 builds
  and proves the substrate (`PreparedPersistentSession`) but wiring it in as
  the production turn path is a separate, later decision.

## Remaining rung owners (past this wave)

- **Rung 3 (registry generalization / blackhole-vs-loop):** owner
  `tidepool-codegen::jit_machine`'s parked-continuation registry
  (`ContinuationFrame`, `resource_ledger.rs`), generalized to root
  descriptor-backed STG objects instead of old-layout objects; the
  evaluator-identity decision on `DescriptorState::Evaluating` is prerequisite
  design work, not implementation, and must be settled before rung 3's test
  is written.
- **Rung 4 (realm cancellation):** owner the existing `CancelHandle`/
  `realm_cancel_handle` mechanism (`jit_machine.rs`), extended to replace
  `PreparedCancelHandle`'s bare invocation-scoped flag
  (`tidepool-runtime/src/session/prepared.rs:24-36`) with realm scoping,
  reset-before-retry, and `close_realm` teardown.
- **Rung 5 (actor authority):** owner `tidepool-actor` (unchanged authority
  contract: exact-incarnation ownership, single-outstanding-update-per-request
  lock, retirement ends the exact incarnation) paired with
  `tidepool-runtime::session::persistent::PreparedPersistentSession`'s move
  from thin wrapper to the STG analogue of `PersistentSession`'s stow-XOR-run
  discipline.
- **Rung 6 (workbench cutover, composite gate):** owner
  `tidepool-runtime::session::workbench` for the routing decision itself,
  gated on rungs 2-5 landing together in one resident-session test first.

## Status ledger (updated as tasks land)

Wave 6B tracks (see `plans/actually-since-you-found-jiggly-turing.md` for the
per-task cards): W1-W3 (independent prep), E1-E3 (effect resume,
same-program, no codegen change — rungs 0-1), S1-S6 (imports substrate,
codegen then runtime — rung 2), S5 (Haskell retained globals, parallel with
S1-S4), D1-D3 (this document and its follow-on updates).

This section is updated once E3/S6 land with what actually happened; commit
ledger and gate results are appended here per the plan's D3 documentation
task.
