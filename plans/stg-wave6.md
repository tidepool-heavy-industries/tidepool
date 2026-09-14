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
| 4 | Cancellation of one parked turn among siblings, via a realm-scoped `CancelHandle`; sibling and `close_realm` counts unaffected | Not started this wave |
| 5 | Actor-turn authority: retiring an incarnation releases its parked frame; a different incarnation cannot resume it | Not started this wave |
| 6 | Composite: rungs 2-5 together in one resident session — the actual gate for calling Wave 6 done | Not started this wave |

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
identity on the shared heap. Generated code can load, hold, pass and return
an imported value. It cannot yet:

- call an imported closure, or force an imported thunk: `apply.rs`'s
  dispatchers and `entry.rs`'s enter routine match a callee against the
  compiling program's own function/thunk tables (`BadThunkState`, machine
  `Unavailable`) -- X2 below;
- (closed by X1, `a0e41c70d`) `Case` on an imported constructor: a program
  compiled through `PreparedMachine::compile_for_install` shares one
  descriptor per constructor identity with every earlier program, so its
  `Case` and evaluated-constructor enter recognise their cells; a program
  compiled standalone still sees only its own;
- hold an import in a top-level constructor: static data cannot carry a
  pointer known only at install (`image.rs` rejects it at compile time);
- install a closure containing any of the above bodies, since admission is
  whole-program.

Host observation (`inspect_outer`, entry-result observation) resolves an
imported value, including another program's static cells, through the
machine-wide descriptor/static union. Two further caveats: retained
generation matching is external-name-only, so a producer must withhold the
unfolding of a retained symbol (GHC otherwise inlines small static data
into the consumer as a recovered copy; the S6 probe uses `NOINLINE`), and
`required_evaluated` means weak head normal form (a function or PAP counts,
matching the projection's `importedEntry`).

Follow-ups, in dependency order: S3b (import-holding tops become heap
tops published after import slots; default-only `Case` skips dispatch),
then cross-program call and case dispatch through the machine-wide
registry (the real "apply an imported closure" primitive D2 deferred), then
S5's contract for unfoldings of retained symbols. S2b (two GC tests for
collection during a second program's live native call and static-region
admission across programs) is a small independent card.

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
- **X2 function and thunk dispatch through the machine.** After each
  per-program fast chain (`apply.rs::emit_dispatchers`,
  `entry.rs::emit_prepared_enter`), replace the terminal bad-state with a
  host lookup `prepared_resolve_entry(vmctx, header, signature_hash) ->
  code` over a machine-wide map `install` fills from every program's
  `pipeline.get_function_ptr` for its function and thunk descriptors, then
  `call_indirect` with the ABI `EntryAbi::cranelift_signature` gives (thunk
  bodies: `(vmctx, reference) -> (status, value)`). Foreign PAPs are a
  second step (read pending arguments through the PAP's own descriptor
  layout, then dispatch the underlying function). Acceptance: S2's T2 and
  T4 un-ignore and pass (a collection inside the producer's code while the
  consumer's frame is live; static admission through a call); S6 runs
  `consumerResult` (`producerFn (length producerValue)`) against the
  oracle's 6, with the pinned closure regenerated to include it; a call
  with a mismatching signature hash is a typed failure, machine
  `Reusable`. Medium-large.
- **S3b import-holding tops.** A top-level constructor referencing a
  `Global` becomes a heap top (`image.rs::heap_top_partition`),
  `initialize_heap_tops` resolves the field from the import slot, and
  `install` publishes import slots before initializing heap tops. Default-
  only algebraic `Case` skips descriptor matching. Acceptance: a consumer
  whose target is `(consumerResult, producerValue)` as static data
  compiles, installs and reads correctly across collections. Small.
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
  objects through B). Both pass on the unmodified tree. **Not settled:**
  mutation-testing the first test by truncating
  `MachineState::stack_map_registries()` to the first registry -- the exact
  mutation this card asked it to catch -- did NOT make it fail.
  `gc/frame_walker.rs::walk_frames` silently skips a frame whose return
  address matches no registry in the chain (treats it as an untracked
  host-boundary frame) rather than erroring, and this specific test's
  allocation pattern doesn't expose the resulting corruption -- plausibly
  because the untraced object's fromspace memory isn't reused before the
  test reads it back, not because the chain is actually correct. This is a
  genuine open question about `walk_frames`'s own semantics under a
  degraded chain, needs direct judgment rather than another Sonnet
  test-writing pass, and is not yet closed by any test in the suite.

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

### Stage 3: sessions and actors (rungs 5-6), design-gated

- **Rung 5 actor-turn authority.** Owner named in "Remaining rung owners"
  is `PreparedPersistentSession`, which does not exist; `PreparedRuntime`
  is the closest thing and now carries bindings, generations and leases.
  Design question for the user before any card: does `PreparedRuntime`
  become the STG analogue of `PersistentSession`'s stow-XOR-run discipline
  (a `MachineLease`-shaped affine borrow around `PreparedMachine`), with
  `tidepool-actor`'s unchanged authority contract (exact-incarnation
  ownership, one outstanding update per request, retirement ends the
  incarnation) layered on top -- or does `PersistentSession` itself grow
  an engine enum? `tidepool-actor` is mid-cutover under the user's own
  commits and carries 23 clippy diagnostics; that lineage must be read
  first. Acceptance sketch: retiring an incarnation releases its parked
  frame (its realm closes); a different incarnation cannot resume it
  (typed refusal by realm); leases held by an incarnation's installed
  programs release on retirement.
- **Rung 6 workbench cutover, the composite gate.** One resident-session
  test that exercises rungs 2-5 together: a turn binds a value, a later
  turn imports and calls it, two turns park and resume out of order, one
  is cancelled by realm, an incarnation retires and its work is released.
  Then, and only then, the routing decision in
  `tidepool-runtime::session::workbench`: real notebook turns through the
  prepared engine instead of Core (`session/prepared.rs`'s note that
  production `resident_workbench` still dispatches Core stands until this
  lands). Wave 7 non-goals stay non-goals (no stack snapshots, no atomic
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

A fresh `just changed 3f43c7d4f` gate run on the tree through `aa9001889`
was launched in the background; its verbatim result will be appended here
once it completes.
