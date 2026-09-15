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
continuation survives collection as data. The Wave 6A handoff named
executable imports as the next owner and forbade resident/workbench cutover
this wave; its content is absorbed here and the handoff file is gone.

## Acceptance ladder

Each rung's criterion is a command/test that fails before the rung lands and
passes once it does. Ordered by minimal new surface first.

| Rung | Criterion | Status |
|---|---|---|
| 0 | Decode a real effect request with a real closure `Leaf` field (no suspension), classified without going through `observe()` (which rejects functions/PAPs) | **Done** (Wave 6A) |
| 1 | Smallest real end-to-end suspend/resume: print -> sleep -> print, single turn, single realm; heap object identity checked across resumes | **Done** (Wave 6B, E1-E3: `freer-resume.cbor`, `freer_resume_loop_drives_qapp_to_completion_via_managed_resume_arguments`, parking tests in `tidepool-runtime/tests/prepared_execution.rs`) |
| 2 | Retained bindings across turns: turn N+1's program links against turn N's binding via `required_generation`, reads it off the same persistent heap (identity, not re-import by value) | **Done with recorded limits** (Wave 6B, S1-S6: `retained_import_end_to_end_links_consumer_against_bound_producer_tops`; limits under "Rung 2 boundaries" below) |
| 3 | Interleaved parked work: two suspended continuations share one heap, resumed out of order, survive an intervening nursery GC | **Done** (C0, cross-program: `c0_two_installed_programs_park_and_resume_out_of_order_with_a_collection_between`, `c0_unrelated_entry_of_a_second_installed_program_runs_while_the_first_stays_parked`; scope note below) |
| 4 | Cancellation of one parked turn among siblings, via a realm-scoped `CancelHandle`; sibling and `close_realm` counts unaffected | **Done** (C1 `e398de760`, C1b `90cecaf49`) |
| 5 | Actor-turn authority: retiring an incarnation releases its parked frame; a different incarnation cannot resume it | **Substrate done, actor-crate proof still open** (A4 `1fb9fcda9`: `PreparedRuntime` is `Send`, registry-hostable, realm-scoped import leases, cross-realm argument refusal, `ActorRunTarget` impl; B1 `3d2b3909d` proves two independent realms behave exactly this way through `SessionRegistry` -- but `tidepool-runtime` cannot depend on `tidepool-actor`, so B1 stands in an actor incarnation with a bare `RealmId`, not a real `ActorRef`/`Incarnation`. No test yet drives `tidepool-actor`'s own exact-incarnation refusal through a real `PreparedRuntime`) |
| 6 | Composite: rungs 2-5 together in one resident session — the actual gate for calling Wave 6 done | **Session/registry substrate done; the actor-authority and workbench-routing halves are not** (B1 `3d2b3909d`: bind, import, cross-program call, park/resume out of order across a collection, realm cancellation, and independent realm retirement, all driven through one `SessionRegistry`-hosted `PreparedRuntime`, in one test; B2 `c8db43f3c`: a live session turn compiles through the real prepared-STG projection and a later turn imports an earlier one by retained generation -- the actual mechanism a notebook turn would use. Neither goes through `tidepool-actor`'s registry, and the workbench routing decision itself is not made -- see "Routing options" below) |

Rung 3's blackhole question is settled by D3 below for this engine's
suspension model: a freer request is returned as an `E` value at WHNF, no
native stack is captured, and every thunk on the path to `E` has settled, so
a parked continuation leaves no `DescriptorState::Evaluating` header behind
for another resume to trip over (`docs/stg-projection-inventory.md`,
"`noDuplicate#` execution invariant", and the E3 tests). Evaluator identity
on the descriptor state becomes necessary only if a future design captures
native stacks, and must be re-decided then. What rung 3 still lacks is the
cross-program pinning: two parked continuations from two installed programs
on one heap, resumed out of order across a collection.

**C0 landed** (`19890675e`): both tests pass with
`collect_before_observation: true` at every step, `retained_handle_count()
== 0` at the end. Scope, stated explicitly: these tests verify the
session/parking protocol across two installed programs, which is rung 3's
own stated criterion. They do not independently verify the deeper
native-frame stack-map-walk invariant (whether a collection triggered
*during* a second program's own live call correctly traces that program's
frames) -- mutation-tested and confirmed not to catch a stack-map-chain
truncated to the first registry, because both tests' collections happen
via `collect_before_observation`, a post-call trigger with no generated
frames live on the native stack. That invariant is S2b's separate target,
and S2b's own mutation test surfaced an unresolved finding there too (see
S2b below) -- it is not yet proven by any test in the suite.

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

## Rung 2 boundaries (what an import can and cannot do today)

An admitted global is a top-table slot published from a retained root at
install (`PreparedMachine::install_program` with `ImportBindings`), read by
identity on the shared heap. Generated code can load, hold, pass, return,
force, case on, and hold an imported value in a top-level constructor.
Calling one is narrower than that. What remains open:

- **admission has no case for a `Global` callee at all.**
  `tidepool-codegen/src/prepared_program/admission.rs`'s `ExprFrame::Call`
  arm matches `Atom::Ref(ValueRef::Local(id))` only; every other callee
  shape -- a `Global` reference (an import called directly by name, e.g.
  Haskell's `producerFn (length producerValue)`) included -- falls through
  to the wildcard rejection. Admission is whole-program
  (`CompileError::Unsupported` on any one reachable node rejects the whole
  artifact), so ANY reachable direct call to an import blocks the entire
  program from installing, not just that call. X2's runtime dispatch
  (`prepared_resolve_call`) never even runs for this shape: the call is
  rejected before compilation reaches emission. Confirmed against a real
  GHC-compiled fixture, not only a synthetic repro
  (`tidepool-runtime/tests/prepared_execution.rs`'s
  `s6_direct_global_call_is_not_yet_admitted`; `import-consumer-result.cbor`
  is kept as its own artifact for exactly this reason, so the gap does not
  also take down the working `consumerValueAt`/`consumerEntries` fixture).
  Not fixed this wave: extending admission's `Call` arm to recognise a
  `Global` callee (deciding admissibility against the declared
  `GlobalDecl`'s `entry_signature`) is codegen-invariant analysis work for
  a Fable-direct pass.
- calling an import held in a LOCAL value (a managed argument, a
  case-bound name) IS admitted and does resolve through X2's machine-wide
  tables for an exact application
  (`t2_closure_crosses_programs_and_collects_inside_the_producing_program`,
  `x2_b_forces_a_thunk_import_through_the_owning_programs_enter`); a
  foreign PAP, or partial/excess application of a foreign callee, is not
  yet served there either and fails as a typed `RuntimeError::UnresolvedCallee`,
  disposition `Reusable` (`c2992e66d`, `5102c0b08`, phase 2 pending);
- S5's contract for unfoldings of retained symbols: closed. A GHC Core
  plugin (`Tidepool.RetainedUnfoldings`, `42b2621b9`) withholds a retained
  symbol's unfolding from the simplifier before `load'` runs, so a
  notebook user never has to write `NOINLINE`; wired into the production
  one-shot compile path (`05276e0c8`) so a live `--retained-generation`
  request actually withholds, not just the test harness. The resident
  daemon still compiles with an empty retained set (documented limitation:
  its long-lived `HscEnv` would otherwise accumulate one withholding pass
  per request forever, since the plugin only ever prepends).

Host observation (`inspect_outer`, entry-result observation) resolves an
imported value, including another program's static cells, through the
machine-wide descriptor/static union. `required_evaluated` means weak head
normal form (a function or PAP counts, matching the projection's
`importedEntry`).

## Completion plan (what is left to call Wave 6 done)

Rung 6 is the gate: rungs 2-5 together in one resident session. Everything
below is ordered so each stage leaves the tree green and independently
useful, and so the design-gated stages come after the mechanical ones.
Effort labels are for one engineer driving directly; the GC/codegen stages
are not delegation candidates.

### Stage 1: imports become usable (rung 2 for real)

An import can be held and read today but not called, cased on, forced, or
placed in top-level data (see "Rung 2 boundaries"). A notebook that cannot
call a retained function has not really retained it.

- **X1 constructor descriptor interning.** One descriptor per constructor
  identity across every program on a machine (`DescriptorInterner`, owned
  by `PreparedMachine`; `CompiledProgram::compile_with`;
  `PreparedMachine::compile_for_install`; absorb at install with a typed
  `DescriptorShape` refusal). Closes `Case` on imported constructors,
  including `seq`, and evaluated-constructor enter. Acceptance: the former
  S3 finding test passes un-ignored (B's generated `Case` reads A's
  `Field(99)`); a conflicting declaration under a known identity is
  `CompileError::DescriptorShape`; codegen and runtime suites no worse.
  Status: **done**, `a0e41c70d` (codegen lib 470 passed; runtime session
  and integration suites 11 and 10 passed).
- **X2 function and thunk dispatch through the machine — done**
  (`c2992e66d`, `5102c0b08`, `af80ee6b5`). Machine-wide tables
  `MachineState::prepared_callables`/`prepared_enters`, filled at
  `PreparedMachine::install` from every installed program's function and
  thunk descriptors; `apply.rs::emit_dispatchers` and
  `entry.rs::emit_prepared_enter` fall through to host fns
  `prepared_resolve_call(vmctx, header, fingerprint)` /
  `prepared_resolve_enter(vmctx, header)`, guarded by a fixed-seed FNV-1a
  `resolve::signature_fingerprint` over argument representations and the
  result contract. Exact application of a foreign function and forcing a
  foreign thunk both work; `t2_closure_crosses_programs_and_collects_
  inside_the_producing_program` is un-ignored and passes. Foreign PAP,
  partial, and excess application are a typed reusable failure instead
  (`x2_foreign_callee_with_mismatching_signature_is_a_typed_reusable_
  failure`: `RuntimeError::UnresolvedCallee`, disposition `Reusable`) —
  phase 2, not yet built.
- **S3b import-holding tops — done** (`5102c0b08`). A top-level
  constructor referencing a `Global` is a heap top
  (`image.rs::heap_top_partition`); `install` publishes import slots
  before initialising heap tops and before the install-time collection,
  with rollback on every later failure arm; default-only algebraic `Case`
  skips descriptor matching. A consumer whose target is
  `(consumerResult, producerValue)` as static data compiles, installs and
  reads correctly across collections.
- **S5 unfoldings.** A retained symbol's unfolding must not be visible to a
  later turn's compilation (today `NOINLINE` in the probe stands in for
  it). Acceptance: `ImportProducer.hs` without `NOINLINE` still projects
  `producerValue`/`producerFn` as globals with no recovered
  `producerValue1..5`/`$wproducerFn` tops. Haskell, medium.
- **S2b GC residuals — landed with an open finding (`aa9001889`).** Two lib
  tests: `s2b_second_program_native_frame_is_walked_through_the_stack_map_
  chain` (a collection triggered from inside a second installed program's
  own live native call, tracing a bare Cranelift local never registered as
  a Rust root) and `s2b_a_static_object_is_retained_through_b_via_the_
  shared_static_region_set` (retention of one of A's genuinely static
  objects through B). Both pass on the unmodified tree. **Resolved by F2
  (`79f398a0c`):** the mutation-check gap was that the prepared engine's
  own collector (`host_fns/gc.rs::collect_prepared`) never poisoned its
  retired semispace, so a truncated-chain mutation left stale but
  still-readable bytes behind and neither test's allocation pattern
  exposed corruption. `collect_prepared` now poisons the retired
  semispace under `gc_poison_enabled()`, matching the legacy Cheney-copy
  path; the mutation test runs under a 256-byte nursery so the collection
  lands inside the second program's own live frame, and the truncated-chain
  mutation now fails deterministically. `walk_frames`'s degraded-chain
  behavior (skipping an untracked frame rather than erroring) is unchanged
  and not itself the bug; the fix was making corruption from that
  skip observable.

### Stage 2: parked work across programs and realms (rungs 3-4)

- **C0 rung 3 pinned across programs -- Done (`19890675e`).** Installs the
  freer-resume artifact twice on one machine (second compile via
  `compile_for_install`), parks one `k` from each, collects, resumes in the
  opposite order to completion against the pinned expectation
  (`c0_two_installed_programs_park_and_resume_out_of_order_with_a_
  collection_between`); then both parked while an unrelated entry of the
  other program runs
  (`c0_unrelated_entry_of_a_second_installed_program_runs_while_the_first_
  stays_parked`). See the ladder's rung 3 row for the scope note (session
  protocol, not the stack-map-walk invariant S2b targets).
- **C1 rung 4 realm-scoped cancellation.** `PreparedMachine` embeds the
  existing `ResourceLedger` (continuations empty this wave), `run_entry*`
  and `inspect_outer` take a `RealmId`, `realm_cancel_handle` returns the
  JIT's `CancelHandle` (`reset` is the retry path), `close_realm` settles
  exactly as `JitEffectMachine::close_realm` does; `PreparedRuntime` drops
  `PreparedCancelHandle` for `open_realm`/`cancel_handle`/`close_realm`.
  Acceptance: two realms each park a `k`; cancel R1 -> `Cancelled` inside
  generated code, `k` valid, R2 unaffected; reset, retry succeeds;
  `close_realm(R1)` returns exactly R1's handle count, R2 resumes, a second
  close is `(0, 0)`; machine `Reusable` and handle receipts match at every
  step. Medium. Depends on nothing in stage 1 but shares `machine.rs`, so
  after X2.

### Stage 3: sessions and actors (rungs 5-6)

- **Rung 5 actor-turn authority -- substrate done (A4 `1fb9fcda9`), actor-crate
  proof open.** The design question this section used to pose (does
  `PreparedRuntime` become the STG analogue of `PersistentSession`'s
  stow-XOR-run discipline, with `tidepool-actor`'s unchanged authority
  contract layered on top) is answered: yes, and it is built.
  `PreparedRuntime` is `unsafe impl Send` under the exact stowed-XOR-running
  argument `PersistentSession`/`JitEffectMachine` already use; it hosts
  cleanly in `SessionRegistry<PreparedRuntime, PreparedHole>` (proven by B1);
  `install_in`/`install_prepared_in` scope import leases to a `RealmId`,
  `close_realm_report` releases them and reports a `RealmRetirement`
  receipt; a managed argument minted under one realm is refused before any
  machine call if used under another (`PreparedRuntimeError::CrossRealmArgument`);
  `impl ActorRunTarget for PreparedRuntime` in `tidepool-actor::mount`
  mounts it exactly as `ResidentSession` already is. What remains: no test
  drives this through `tidepool-actor`'s OWN registry
  (`ActorMachineRegistry<H, O> = SessionRegistry<ResidentSession<H, O>, String>`)
  with a real `ActorRef`/`Incarnation` -- B1 could only stand in an
  incarnation with a bare `RealmId::fresh()`, since `tidepool-runtime`
  cannot depend on `tidepool-actor` (the dependency runs the other way).
  Closing this needs a `tidepool-actor`-side test (or a small
  `ActorMachineRegistry<H, O>` instantiated with `PreparedRuntime` in place
  of `ResidentSession<H, O>`) asserting the actual thing rung 5 promises:
  retiring one `ActorRef` incarnation's placement releases its parked
  frame via `retire_root_placement`, and a stale `Incarnation` is refused
  by the request layer exactly as `stale_incarnation_cannot_settle_a_request`
  already pins for the Core engine.
- **Rung 6 composite -- session/registry substrate done (B1 `3d2b3909d`),
  live-turn mechanism done (B2 `c8db43f3c`), actor-authority and routing
  halves open.** B1 is the first test to put a `PreparedRuntime` inside a
  `SessionRegistry` at all: six sequential checkouts prove bind, import,
  cross-program call, park/resume out of order across a collection,
  realm-scoped cancellation, and independent two-realm retirement all work
  TOGETHER through the real checkout/settle protocol, not just each in
  isolation the way the rung-by-rung tests pin them. B2 proves the actual
  cutover prerequisite: a turn shaped exactly like a notebook cell (one
  top-level declaration) compiles through the real prepared-STG projection
  (`tidepool-extract --target`, not the Core `--turn` path) and a LATER
  turn imports an EARLIER one's binding by retained generation, through a
  new `SessionTurns` mechanism (`tidepool-runtime/src/session/prepared_turn.rs`).
  B2 also confirms, independently of A2's finding, that a turn calling an
  imported binding directly hits the same admission gap (see "New findings"
  below) -- not a new problem, but now proven from two unrelated angles.
  What rung 6 as originally scoped still needs: rung 5's actor-crate proof
  above, and the workbench routing decision itself (see "Routing options").
- **The routing decision itself** -- `tidepool-runtime::session::workbench`
  choosing the prepared engine over Core for real notebook turns -- is
  covered in "Routing options" below, not taken by this wave. Wave 7
  non-goals stay non-goals regardless (no stack snapshots, no atomic
  `MutVar#`, no Core JIT deletion).

### Hygiene that rides along

- `tidepool-actor`'s 23 clippy diagnostics keep `just changed` from ever
  passing as a whole; either land that lineage or record them as known the
  way `tidepool-agent`'s were (`plans/stg-wave5-delivery.md`).
- `session::inspection::tests::one_inspection_compile_answers_type_info_and_browse_queries`
  fails on the `data Public` browse assertion at the pre-wave baseline;
  owner unknown, not this wave.
- Update "Rung 2 boundaries" and the inventory as X1/X2/S3b land; each
  removes a bullet there.

## Routing options (for the user, not decided by this wave)

The substrate rung 6 needs is built and tested (B1, B2), but routing real
notebook turns through the prepared engine instead of Core
(`tidepool-runtime::session::workbench`'s choice, `session/prepared.rs`'s
note that production `resident_workbench` still dispatches Core) is a
product decision, not a mechanical follow-on. Three shapes, roughly in
order of how much they commit to:

1. **Stay on Core; keep the prepared engine as a parallel, explicitly
   opt-in path.** Lowest risk. `SessionTurns`/`PreparedRuntime` remain
   usable directly (as B2's test already does) for whatever calls them,
   but `resident_workbench` keeps routing ordinary turns through
   `PersistentSession`. Defers the two remaining real gaps (rung 5's
   actor-crate proof; the direct-`Global`-call admission gap) indefinitely,
   since nothing forces closing them.
2. **Route new sessions through the prepared engine behind a flag,
   Core remains for existing/legacy sessions.** Forces closing rung 5's
   actor-crate proof and the admission gap first (a flagged session that
   cannot call an imported function directly, or whose actor authority is
   unverified, is not a real notebook backend). Gives real production
   signal before committing further, and keeps a fallback if the admission
   gap or something like it turns out to be deeper than expected.
3. **Prepared becomes the default engine for new sessions; Core is kept
   only for the cutover's own retirement schedule
   (`plans/stg-production-cutover.md` phase 6).** The actual destination
   the cutover plan describes. Requires everything option 2 requires, plus
   real turn classification (`TurnKind`/`classify_block`, not the
   caller-supplies-the-shape stand-in `SessionTurns::TurnForm` uses today),
   IO/bind-effect turn semantics (out of scope for `SessionTurns` as built),
   and session-root lifecycle policy (currently caller-owned, fine for a
   test, not for a real resident session's directory/process lifetime).

None of these is blocked on more codegen work beyond closing the admission
gap (stage G, see "New findings" below) -- the session/registry mechanism
itself (B1) and the turn-compile mechanism (B2) are both proven. The
decision is about product risk tolerance and how much of `SessionTurns`'s
deliberately-cut scope to build out first, not about remaining engine
capability.

## Status ledger

Wave 6B (2026-09-13/14), on `engine/stg-production-cutover` from
`3f43c7d4f`. Per-task cards were drafted in a session-local plan-mode file
(not part of this repository); their substance is captured in this
document's Decisions, Completion plan, and this ledger.

| Task | Outcome | Commits |
|---|---|---|
| W1 gate record | report only; clippy failure on `tidepool-actor` found pre-existing | — |
| W2 worktree hygiene | done; branch superseded by the corrected ledger, deleted | (`16c59a891` records it) |
| W3 closure ledger | corrected numbers written directly (402 fixture mismatches at the time, not 812; supersession hashes fixed) | `16c59a891` |
| E1 FreerResume probe + artifact | accepted | `5412e4606` |
| E2 resume loop | accepted | `34d6eca26` |
| E3 parking semantics | accepted; post-merge fixes for S1's API | `59973f08a`, `632e216f4`, `413f89ee1` |
| S1 machine-wide top table | accepted after one refutation (test coverage) | `7706fb2e6` (merge), `af007b864` |
| S5 retained-generation globals | accepted | `86c4dd52b` |
| D1 this plan | accepted | `617c1fb72` |
| S2 one heap per machine | T1/T3 accepted; T2/T4 blocked by the invocation gap, orchestrator-accepted with the residual recorded (S2b) | `84025f519`, merge `c971168b1` |
| S3 global lowering, admission, import bindings | accepted after direct review; `required_evaluated` widened to WHNF | `05e385cf3`, `8a9d9db5b`, merge `522c1028f` |
| S4 session custody | `BoundValue::Prepared`, `PreparedRuntime` bind/install/release, per-turn generations | `03f31d80f`, `c6942db51` |
| S6 end-to-end retained import | passes against the GHC oracle; probe reshaped around the rung-2 boundaries | `2659fce83` |
| D2 standing docs | inventory: `noDuplicate#` reasoning, admitted-import contract, remaining per-program tables | `9eb6efe99`, `6c9b00d64`, this commit |
| dev-ux | `just changed` no longer aborts at its first failing step | `5a5df1b72` |
| gate artifacts | fixtures-update fingerprint; `prepared_execution.rs` formatted | `3a7e6e999`, `25fe148d7` |
| X1 constructor descriptor interning | accepted; un-ignores S3's foreign-Case finding | `958e9faa5` |
| D1/D3 docs (rung-2 boundaries, completion plan, 6A handoff absorbed) | accepted | `754ce3347` |
| stg-wave5-delivery addendum (three W1 gate items resolved) | accepted | `dfa75b27f` |
| C0 rung 3 pinned across programs | accepted; scope note recorded (session protocol, not stack-map-walk) | `19890675e` |
| S2b GC residuals | tests pass; mutation test surfaced an open, unresolved finding in `walk_frames`'s degraded-chain semantics — recorded, not fixed | `aa9001889` |
| plans/README.md Wave 6 entry | accepted | `03029fc9d` |

### Gate run results (2026-09-14, `just changed 3f43c7d4f` through `aa9001889`)

Red in five steps. Reading (not rerunning) each, cross-referenced against
every commit's diff since the base:

| Step | Result | Cause |
|---|---|---|
| fmt, suite-check | pass | |
| workspace clippy | FAIL | 23 pre-existing `tidepool-actor` diagnostics (hygiene item above), unrelated to this wave |
| `cargo nextest run` (workspace) | FAIL, cancelled at 21/2950 | 7 `tidepool actor_host::command_jobs_tests` failures on the user's own in-progress cutover lineage; fail-fast left 2929 tests unrun, so this step proves nothing about the rest of the workspace |
| `cargo nextest run -p tidepool-codegen` | FAIL, cancelled at 502/833 (500 pass) | two legacy-engine tests, confirmed pre-existing at the wave base — see below |
| `scripts/fixtures.sh check` | FAIL | fixtures stale since S6 (`2659fce83`) touched `haskell/` after the fingerprint commit `3a7e6e999`; needs one more `fixtures-update` |
| runtime battery | FAIL before running | `TIDEPOOL_EXTRACT_WORKER` in the gate's environment is older than sources; environment, not tree |

**The two `tidepool-codegen::codegen` failures are confirmed pre-existing,
not a Wave 6B regression.** Both
`bind_error_then_allocate::error_bind_turn_then_allocating_bind_turn_stays_sane`
and `apply_acceptance::apply_gc_during_application_relocates_forced_callee`
were run twice: on HEAD (`TIDEPOOL_HEAP_VERIFY=1 TIDEPOOL_GC_POISON=1`, an
isolated target dir) and again in a worktree pinned at the wave base
`3f43c7d4f`, no env overrides. Both runs produced byte-identical panics.
No commit since `3f43c7d4f` touches `tidepool-codegen/src` or
`tidepool-heap/src` outside `prepared_program/` except `84025f519` (S2) and
`03f31d80f` (S4), and neither's diff reaches the code paths either failing
test exercises (S2's stack-map chain is a no-op for a single registered
pipeline, which is every legacy-engine caller; `DescriptorSpace`'s static
region set is prepared-only).

- `bind_error_then_allocate` is a contract collision: since `71f23ffda`
  (2026-09-12, already in the wave base), `RuntimeError::CaseTrap` maps to
  `MachineDisposition::Unavailable` (`host_fns/errors.rs`). This test's own
  turn 2 deliberately triggers a case-trap and then asserts the machine
  stays usable for turn 3 — a premise the disposition contract already
  contradicts, unrelated to any GC chain or heap-sharing change.
- `apply_acceptance`'s nursery-relocation failure (`nursery_size=400`,
  `left: 0, right: 3`) is real and still unclassified. Candidate sites are
  in the pre-base "wip" lineage (`b73298b99`, `e1a4b9145`), not this wave.

Follow-up cards (G2 rewrite the case-trap test to a `Reusable`-class error;
G3 Fable-direct GC debugging on the apply-relocation failure; an S2b poison
fix so the mutation test actually detects a truncated chain; G4 fixtures/
worker env; G5 workspace nextest fail-fast) are recorded in the session plan
file and will land as their own commits.

## Wave 6C-2 (2026-09-14, in-repo orchestrated wave, no worktrees)

Operating model: a Sonnet orchestrator (this session) spawned agents that
worked directly in this checkout (no `isolation: worktree`), each owning a
disjoint file set; the orchestrator alone ran builds/tests/commits. Full
card specs are in the session-local plan file (not part of this
repository); this ledger records what actually landed.

| Task | Outcome | Commit |
|---|---|---|
| G2/G4/G5a/G5b (Fable, step 0) | case-trap test rewrite, fixtures fingerprint, per-crate and workspace `--no-fail-fast` | `b43a34057`, `5c3601331`, `73bd8ec2e` |
| C1 realm-scoped cancellation (Fable) | `PreparedMachine` embeds `ResourceLedger`; `run_entry*`/`inspect_outer` take `RealmId` | `e398de760` |
| G3 heap growth structural fix (Fable) | root cause: legacy growth ignored the triggering allocation's size, not just utilization; `VMContext::gc_trigger` now `fn(vmctx, usize)`, one shared `heap_growth_target` policy for both collectors | `9e4f795d0` |
| A6 hygiene | `.gitignore` for stray target dirs, dead test types removed, `floating.rs` rustfmt fix | `a1642ddff` |
| A5 `session::inspection` browse failure | real bug: assertions checked pre-splice query indices, not a GHC format drift | `29abd7951` |
| A3 / C1b two-realm cancellation test | closes the multi-realm scenario C1's own commit deferred; surfaced that `MachineState::last_failure` is a machine-wide (not realm-scoped) latch cleared only by the next entry call | `90cecaf49` |
| A2 / S2b-fix | added poisoning to the mutation-check tests; confirmed the real gap: `collect_prepared` (prepared engine's collector) never calls `gc_poison_enabled()` at all — poisoning exists only in the legacy Cheney-copy path. Not fixed (GC-invariant, next Fable pass) | `a2e12265a` |
| A1 / X2a resolution substrate | `MachineState::prepared_callables`/`prepared_enters` tables, `prepared_resolve_call`/`prepared_resolve_enter` host fns, `signature_fingerprint` (hand-rolled fixed-seed FNV-1a, not `DefaultHasher`) | `c2992e66d` |
| gc_write_barrier consolidation | one recorder per old-to-young edge class: array payload slots (tenure-time + reach expansion) vs. old-space object fields (`write_barrier`) — two recorders covering the same ground had made the mutation-check tests green-either-way since `71f23ffda` | `2ffdfa5b9` |
| B1/B2/B3 / X2, S3b | cross-program call/enter dispatcher fallback wired to the X2a substrate; import-holding tops become heap tops, published before `initialize_heap_tops`/before `collect_on`, with rollback on every later failure arm | `5102c0b08` |
| A8 tidepool-actor clippy + `command_jobs_tests` triage | 23 diagnostics cleared (typed errors, `Box`ed large variants, documented `#[allow]`s only where the lint is genuinely wrong); `--all-targets -D warnings` still red only via pre-existing `tidepool-testing` debt (verified independently, out of scope); 7/23 `command_jobs_tests` classified (5 environmental, 2 real-bug candidates recorded, not fixed) | `067adcf18` |
| A4 legacy-engine test triage | all 9 pre-existing `tidepool-codegen::codegen` failures were test-authoring bugs (non-strict `let` used for sequencing instead of `Case`; one `f64`-vs-`f32` bit-width test bug) — zero engine-source changes | `075e44cb2` |
| fmt cleanup | `cargo fmt --all -- --check` clean workspace-wide | `16c38961c` |

**Not attempted this wave** (deferred, per the operating model's "the
tests are the review" and the user's request to hand back to Fable for
planning): C-1 (new X2/S3b acceptance tests, un-ignoring S2's T2), C-2 (S6
`consumerResult` end-to-end against the real apply path), C-3 (standing
docs update to `docs/stg-projection-inventory.md`/this file's "Rung 2
boundaries" section reflecting X2/S3b closing those bullets).

**Two real findings recorded for the next Fable pass -- both now resolved:**
1. The prepared engine's own collector (`collect_prepared`,
   `host_fns/gc.rs`) never wired in `gc_poison_enabled()` -- the S2b
   mutation-check tests could not detect a truncated stack-map chain this
   way (see A2 row above). Legacy engine had this; prepared engine did
   not. **Resolved by F2 (`79f398a0c`):** `collect_prepared` now poisons
   its retired semispace under `gc_poison_enabled()`; the mutation test
   runs under a 256-byte nursery so collection happens inside the second
   program's live frame, and the truncated-chain mutation now fails
   deterministically.
2. `MachineState::last_failure` was a machine-wide latch, not
   realm-scoped, cleared only by the next `begin_prepared_call` --
   `inspect_outer` between two realm-scoped calls could spuriously read
   back a stale `Cancelled` failure from an unrelated realm's prior call
   (see A3/C1b row above). **Resolved by F1 (`af80ee6b5`):**
   `MachineState` now separates the call outcome (`runtime_error`, first
   cause of one entry, settled at call end) from the machine latch
   (`last_failure`, first `Unavailable` cause, never cleared). Reusable
   causes (cancellation, language failure, `UnresolvedCallee`) never
   latch; an observation between calls sees only the latch.

Full verification after this wave (all green except the two documented,
pre-existing environmental classes -- `command_jobs_tests`'s 5
environmental + 2 real-bug-candidate failures, and the 3 pre-existing
`resident_interactive::lifecycle_tests` failures, all confirmed
unaffected by this wave's commits): `cargo check --workspace --tests`
clean; `cargo fmt --all -- --check` clean; `tidepool-codegen` full suite
green; `tidepool-runtime` lib + `prepared_execution` green;
`tidepool-testing` green.

## Wave 6C-3 (in-repo orchestrated wave, no worktrees)

Same operating model as 6C-2: a Sonnet orchestrator spawned agents working
directly in this checkout on disjoint file sets; the orchestrator alone ran
builds/tests/commits.

| Task | Outcome | Commit |
|---|---|---|
| F1 call-outcome/machine-latch separation | `MachineState` now tracks `runtime_error` (call outcome, settled at call end) separately from `last_failure` (machine latch, first `Unavailable` cause, never cleared); reusable causes never latch | `af80ee6b5` |
| F2 prepared collector poisoning | `collect_prepared` poisons its retired semispace under `gc_poison_enabled()`; S2b mutation test now fails deterministically under a 256-byte nursery | `79f398a0c` |
| A1 X2/S3b acceptance tests | T2 un-ignored (closed by X2); new `x2_b_forces_a_thunk_import_...`, `s3b_import_holding_top_...`, `s3b_default_only_case_...`; two fixture bugs found and fixed in review; admission-analysis finding (`apply::classify`'s Partial arm) root-caused, not fixed | `c60b8cf6f` |
| A3 S5 unfoldings (Haskell) | GHC Core plugin (`Tidepool.RetainedUnfoldings`) withholds a retained symbol's unfolding before `load'` runs; `ImportProducer.hs` no longer needs `NOINLINE` | `42b2621b9` |
| A6 wire retained set into production compile (found necessary while verifying A2/A3 together, not a pre-planned card) | `runPipelineSessionSelected`/`app/Main.hs`'s `PreparedStg` call now thread a live request's retained-generation set into `runCompile`; resident-daemon path documented as a deliberate, separate gap | `05276e0c8` |
| A2 S6 end-to-end consumerResult | Fixtures regenerated with the real withholding pass; found and pinned a real admission gap (a direct `Global`-callee call is never admitted, whole-program) rather than papering over it -- `consumerResultAt` projects to its own artifact so the gap doesn't regress the working `consumerValueAt`/`consumerEntries` fixture; new `s6_direct_global_call_is_not_yet_admitted` | `3b41d44d4` |
| A4 PreparedRuntime registry-hostable | `unsafe impl Send`, `PreparedHole`, realm-scoped import leases, `CrossRealmArgument` refusal, `ActorRunTarget` impl; two test fixtures fixed in review | `1fb9fcda9` |
| A5 standing docs | `docs/stg-projection-inventory.md`, `plans/stg-wave6.md`, `plans/README.md` rewritten to the current X2/S3b/F1/F2 contract; later corrected (see A2 row) once the direct-`Global`-call admission gap was found | `54a035aa8` |
| B1 rung 6 composite through `SessionRegistry` | First test to host `PreparedRuntime` in `SessionRegistry`; six checkouts prove bind/import/call, park/resume out of order across a collection, realm cancellation, and independent two-realm retirement all work together | `3d2b3909d` |
| B2 live prepared-STG session turns | New `SessionTurns` mechanism projects a turn through `--target` mode, not `--turn`; a later turn imports an earlier one by retained generation; independently reconfirms the direct-`Global`-call admission gap from an unrelated angle (a same-session call, not an S6 fixture) | `c8db43f3c` |
| B3 rung 5/6 docs + routing memo | this section, the ladder table, and "Routing options" above | (docs, see this commit) |
| G1 | not started (Fable) | TBD |

**Findings this wave, both real and both left for a Fable-direct pass:**

1. **Confirmed from two independent angles (A2's Haskell fixture, B2's
   live turn compile): `admission.rs`'s `ExprFrame::Call` arm has no case
   for a `ValueRef::Global` callee at all** -- broader than A1's
   originally-reported `apply::classify` Partial-arm gap (which only
   covers calls through an unknown-signature local). ANY direct call to an
   imported function, anywhere in a program's reachable closure, blocks
   that whole program from installing. This is real, GHC-Core-confirmed
   twice over (not just a synthetic repro), and belongs in the same
   Fable-direct codegen-invariant pass as the planned X2 phase 2 (foreign
   PAP/partial/excess) work -- likely before it, since it blocks the far
   more common case of an ordinary direct function call to an import. See
   "Rung 2 boundaries" above,
   `s6_direct_global_call_is_not_yet_admitted`
   (`tidepool-runtime/tests/prepared_execution.rs`), and B2's turn-3
   assertion (`tidepool-runtime/tests/prepared_turn.rs`) for the two pinned
   repros.
2. **Rung 5's actor-crate proof remains open**: B1 proves the
   `PreparedRuntime`/`SessionRegistry` substrate behaves correctly for two
   independent realms, but cannot exercise `tidepool-actor`'s own
   `ActorRef`/`Incarnation` refusal (`tidepool-runtime` cannot depend on
   `tidepool-actor`). See Stage 3's rung 5 entry above for exactly what a
   closing test needs to assert.

Full verification after Wave 6C-3 (`cargo fmt --all -- --check`;
`cargo check --workspace --tests`; both clean): `tidepool-codegen`
(`cargo nextest run -p tidepool-codegen --no-fail-fast`) 843 passed (2
slow), 2 skipped; `tidepool-runtime`, EVERY non-`#[ignore]`d lib and
integration test in one sweep (`cargo nextest run -p tidepool-runtime
--no-fail-fast --ignore-default-filter -E 'kind(lib) or
binary_id(=tidepool-runtime::prepared_execution) or
binary_id(=tidepool-runtime::prepared_resident_composite)'` --
`prepared_turn` is `#[ignore]`d, needs a resolved `$TIDEPOOL_EXTRACT`,
verified separately below) 186 passed, 0 skipped; `tidepool-testing`
(`cargo nextest run -p tidepool-testing --no-fail-fast`) 32 passed, 0
skipped. `prepared_turn`'s one `#[ignore]`d test, run explicitly twice
with a freshly resolved extractor
(`source scripts/lib-extract.sh && resolve_tidepool_extract && cargo
nextest run -p tidepool-runtime --test prepared_turn
--ignore-default-filter --run-ignored all`): 1 passed both times. `just
fixtures-update` run once this wave (Haskell source changed); only the
source fingerprint moved, the generated corpus bytes were unchanged.
Every number here was reproduced by the orchestrator directly, not taken
from an implementing agent's own report.
