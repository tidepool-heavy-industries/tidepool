# Wave 5: laziness, settlement, and exact body recovery

Baseline: `3b657cd08`. Full-wave implementation, with production sessions,
effects, retained-generation linking and resident cutover reserved for Wave 6.
Broad Core deletion is Wave 7. No Core fallback is added.

## Latest verification checkpoint

Pushed source head: `18e4e1f70`. Primitive byte access passed six adapter
tests; the machine-state selection passed 16 tests and the byte-pool bounds
unit test passed. Real recovered `showDouble` walking and the recovery suites
pass after fixing our consumer of GHC's deliberately undefined constructor
annotation. This repair has not yet been followed by a full corpus replay.
Schema-7/ABI-5 nonreturning-result migration now passes the Rust focused fold:
242 repr unit tests, ten codec tests, five cross-language/schema contracts,
102 prepared-program tests, six entry-ABI tests and 16 testing-crate unit tests.
The local seeds `47153c3d6` and `392cbabc5` were deliberately partial; those
results apply to the integrated working tree, not to either seed by itself.
The strengthened Haskell `recovered-body-test` now passes: exact tuple, unary
and Void entry/call signatures are asserted. Its partial-call case uses a
test-local prepared-STG variant because CorePrep eta-expands the source PAP;
it is not evidence that the original source retained a PAP node. Same-invocation
raised-CAF retry passes after relocation, including Live header/capture checks.

The external-graph fold passes eight heap external tests, four promotion tests,
one production host-GC growth test and all 103 prepared-program tests.
`bash scripts/dev-shell.sh cargo test --workspace --no-run --quiet` completed
successfully with warnings. Fresh source review accepted the nonreturning path;
external-graph review requested an explicit stable/disjoint-span unsafe owner
contract, which was added before acceptance. The graph worker's new promotion
fixture needed a zero-field helper correction and Rc ownership before publishing
its payload address. Source-walk preallocation removes a late allocation-failure
path after forwarding; it is not a reachable-graph preflight.

Prepared array primitive emission remains unimplemented. Young/Retained ledger
policy and final-copy minor reclamation are now implemented locally; production
integration verification passed GC 2/2, promotion 1/1 and machine-state 27/27.
Fresh source review accepted the scoped lifetime integration. These results
cover the owner and GC parcel committed in `0e35c2654`, not the array emitter
seed interleaved in history. Seeds `533a97130` and `e7e24c772`
are not verified checkpoints. The existing Core major collector does not supply
these mechanisms to the prepared path.

The complete replay pinned to `18e4e1f70` is
`target/prepared-corpus/suite.aq7NWK/results.json`, SHA-256
`f623bedb62400776eb1144685f851b98262f4cf79882491b9d2f525f4215ce8d`.
All 812 rows remain: 708 project, 684 validate, 424 admit, 256 execute and
136 match. All previous 136 matches remain. The 287 missing-comparison rows
include 120 successful observations lacking expectations and 167 failed
executions lacking expectations; they are not 287 successful programs.
There are 104 unsupported foreign/prim-call projection failures, 24 validation
failures, 116 global and 144 expression admission failures. Execution failures
include 74 Address results, 79 managed host arguments, nine missing scalar
arguments, three function observations, two observation budgets and one
`thunk_blackhole` child watchdog termination. The parent completed all rows.
`just fixtures-check` passed at this checkpoint; this is not semantic green.

Source/artifact diagnosis attributes all 24 validation failures to
`raiseDivZero#` and `raiseUnderflow#`: their enclosing thunks correctly declare
NoSuccess, while the producer emits returning operation signatures. The fix
belongs in operation projection, not validator relaxation. Separately, exact
recovery accepts a `patError` interface body whose type differs from its binder.
The first investigation incorrectly attributed this to `realIdUnfolding`;
the live-Id test found no unfolding, so the fat-interface path must be checked
directly. Investigate typed candidate rejection and exact fallback, never invent
representation-polymorphic binders or revive Core error-sentinel lowering.

The arithmetic-raise producer repair passed `execution-schema-projection` and
`recovered-body-test`; native terminal emission now has a separate focused
test awaiting its run. Array allocation is seeded through the real adapter,
with checked access/store implementation delegated. The first array seed
incorrectly retained source State# positions in results: actual projection
uses physical result reps but retains zero-width argument slots. The worker
reported the validator contradiction and the lead corrected the contract;
no validator relaxation was authorized.

This parcel record: lifetime implementation needed one consolidated semantic
correction (revoked structural retention and fallible sweep staging), then
production tests and a fresh Accept; raising projection needed one worker
round with both focused tests passing; array seed needed a lead contract
correction before its implementation round could complete. Source review and
test results are separate evidence. The generic foreign-call diagnostic did
not contain enough information to choose intrinsics, so capability work waits
for a bounded probe with call kind, identity and instantiated signature.

The rebuilt bounded probe identifies `stg_cloneMyStackzh` for `showDouble`
and `qq_fmt_double`: `[Void] -> Returns [UnliftedRef]`. Fresh GHC-source
consultation confirms this requires a real RTS stack snapshot, tracing and
decoding contract; an empty substitute would be wrong. These two roots reach
it through the `OPAQUE` placeholder bodies of `Tidepool.Double.renderDouble`
and `renderDoublePrec`, whose intended prepared intrinsic lowering is missing.
The next bounded intrinsic work is therefore managed Text formatting via
the existing pure `tidepool_bignum::haskell_show_double`, not pretending that
Cranelift frames are GHC stack objects. The other 102 rejected roots have not
yet been classified by actual operation identity.

The recovered-body test now separately proves that the requested and fat
`patError` binders agree, while that binder and its RHS disagree. It passes.
GHC's fat format serializes external tops by Name and reconstructs this one
as a wired-in Id, losing the defining lifted type. Typed rejection remains;
changing the binder to the RHS type is not an exact-body lookup. Relaxing
caller/defining Core-type equality for other internally well-typed bottoming
bodies is a separate future boundary question, not this failure's repair.

Array progress: the first boxed-family fold and native arithmetic-raise check
passed seven focused tests. Freeze/shrink/CAS then passed nine boxed-family
tests after one review correction added real-adapter coverage (the initial
handback tested their host functions only). Initial byte Word8/Int roundtrips
and bounds passed three native tests; additional byte alias/kind/revocation
checks await the serialized build slot. A GHC-produced fixture confirms all
ten selected byte-array signatures, including Word8 rather than Word64.
Canonical fixture regeneration changed only `.source-fingerprint`; byte
comparison passed and the new full corpus replay is running. No new corpus
match total or workspace-green claim is made yet.

The formatting investigation also found that OPAQUE preserves bottoming demand
information: an error placeholder can inform caller optimization before STG
projection. A late intrinsic wrapper alone cannot restore deleted successful
continuations. The source baseline must express the real returning semantics
before optimization. Formatting precedence must preserve negative zero and
must not force precedence for positive/NaN values merely because the outgoing
Core helper did so. These are correctness requirements for the intrinsic seed.

Trial conclusion for this fold: bounded view migration and exact verification
delegated cleanly; fixture writing needed correction when GHC optimized away
the intended premise; unsafe graph implementation needed lead corrections for
preallocation and a pointer-free error boundary. Fresh review also tightened
the unsafe alias contract. Those global invariants remained lead-owned.
Haskell fixture repair escalated from Luna to Sol, not another assertion
relaxation. Worker/planner token totals are unavailable. Lead events included
seed writing, two graph-contract corrections, fixture-premise review and
several lease/reactivation messages; orchestration was not free.

Last complete corpus evidence (before those source repairs):

Fixture regeneration/check against pushed `26a2f4341` passed; only the source
fingerprint changed. The resulting `suite.lasjm6/results.json` has SHA-256
`e968a32900dc6f1eb254b0fd192676ce69b82a4d7dff5612be657633cccad07f`.
It records 136 matching results, 120 missing expectations, 140 projection
failures, 16 validation failures, 260 admission failures and 140 execution
failures. All 396 admitted programs compiled. The newly reached recovery paths
exposed `mkSeqs shouldn't use the type arg` panics, now traced to our facts
walker forcing an unused GHC annotation. No comparison mismatch is reported.
This is not semantic-green evidence. Original-cohort accounting follows.

Completed accounting: all 812 originals are present (665 structured identity
matches, three unique record-parent fallbacks, 144 exact unique canonical-name
matches where failure rows omitted structured identity). None is ambiguous.
All previous 109 matches remain matches; the original 516-global cohort now
has 23 matches, 53 missing expectations and 440 not reaching comparison.
The 16 validation failures independently confirm the NoSuccess gap: local
`GHC.Prim.Exception.raiseDivZero`/`raiseUnderflow` entries advertise a lifted
result while BigNat/Integer callers demand their actual unlifted or multi-value
bottoming result. Do not relax the validator to hide this distinction.

The next source checkpoint passes `execution-schema-projection`,
`prepared-recovery-test`, and `recovered-body-test` through the dev shell;
the scoped recovery regression retains same-owner dependency edges rather
than declaring their bodies resolved. Focused codegen `bytes_tests` passes
3/3 and `double_to_int_tests` passes 4/4. The bytes worker corrected one
test-builder multiple-owner defect before its second run. Changed Rust file
format checks and `git diff --check` pass. Workspace `cargo fmt --all -- --check`
still reports formatting drift across legacy migration files; it did not
modify them. Fixture regeneration and integrated corpus replay are pending
for these source edits. The corpus numbers below remain the prior checkpoint.

`50beeb099` is pushed, including the retention verification below. The earlier
`3ab9ec613` focused prepared-program library selection passed
73 tests, including six real-adapter PAP cases and five settlement cases.
This is not a current workspace-compile or full-fixture claim.

The subsequent retention integration passed 79 prepared-program and 64 heap
library tests through `bash scripts/dev-shell.sh cargo test -p CRATE --lib`
(the codegen run selected `prepared_program`). Five new `w5_a5` tests cover
promotion/observation, old thunk updates and later minor GC, terminal partial
promotion, stable no-ops, and invalid static/external references. Parent review
found a missing Updated-to-old admission, a needless trait-object transmute,
and an outside-nursery no-op lacking ownership authentication; all were fixed.
The initial 73-pass runtime report lacked retention-specific tests and required
one consolidated test/repair follow-up. A fresh Luna review accepted the pointer
lifetime, but parent/Astra review overturned that conclusion: Box address
stability does not permit a shared-derived pointer to survive later exclusive
borrows. Old-space admission is now scoped around native execution/observation,
with automatic cleanup before promotion. The machine has Rc ownership and
registered root words use a private fixed Vec of UnsafeCells, so moving the
invocation does not carry Box-derived pointers across moves. An unwind test
checks admission cleanup. This was a lead scaffold correction as well as a
review disagreement, not an unqualified first-attempt acceptance.

`bash scripts/dev-shell.sh cargo build --workspace --tests` passed after that
ownership correction, with warnings (including the private retention entry
whose session consumer belongs to Wave 6). `just fixtures-check` first stopped
on a stale fingerprint. After unsetting inherited TIDEPOOL_EXTRACT and
TIDEPOOL_EXTRACT_WORKER, `just fixtures-update` and `just fixtures-check` passed.
Only the generated fingerprint changed; 695 fixture files were byte-identical.
That command also runs the corpus but does not fail on semantic result rows:
the new Suite report has 109 matches, 67 missing expectations, 11 projection
failures, 501 admission failures and 124 execution failures. There are no
comparison mismatches. The optimized divergent thunk_blackhole row still hit
the 120-second watchdog. This is not a semantic-green workspace claim.

Latest corpus evidence: `target/prepared-corpus/suite.GqnnQi/results.json`,
SHA-256 `03babe1a68f9dff4c61917746d64bf118d8faa63b4898409638d1a9f128a6a24`.
All 812 original tops are accounted for, with only three uniquely matched
record-parent spelling changes. Original thunk cohort: 53/80 now match;
original closed-global cohort: 0/516 match (488 admission failures, 17 execution
failures, 11 projection failures). The body-recovery majority is therefore
still unfinished despite the separate successful recovered-fst milestone.

The real Haskell `rintDouble` regression passes with its logical State#
position retained as Void. Native recognition accepts the two exact observed
signatures, not arbitrary insertion/removal of Void. Recovered `error` bodies
still expose a local-entry representation-polymorphism gap; no result rep is
fabricated to hide it. A fresh semantic consultation owns that decision.

The actor corpus source imports the production-generated Effects.Core types.
Its previous missing-module result was harness configuration, not engine
admission. The runner now obtains that include directory from the existing
MCP generator; its focused command-parser test passes. Reprojection moved to
a separate filename/declared-module mismatch, repaired by a qualified input
alias. The next actor-only run reports that awaitSettled is re-exported, not
defined in Tidepool.Agent.Watch; it has not reached engine admission.
The prepared-STG test stub is deliberately not used for this production probe.

## Contracts

Track B has scheduling priority. Recover exact unfolding/fat-interface bodies
with typed absence versus loading failure; preserve complete recursive groups.
Prepare each group in its defining module context through the existing GHC
pipeline. Close references introduced by preparation through a worklist, then
project the prepared modules together into one closed artifact. No synthetic
compilation module, specialization-name guessing, dictionary repair, retry
count, or exception-to-empty-map recovery. Session artifact linking remains
separate. Evidence for foreign capabilities precedes intrinsic registration.

Track A emits one program-level Tail-ABI prepared_enter. The compiled owner
has one descriptor registry with callable and constructor-observation views;
code identities resolve to finalized entries, never Rust casts of Tail code.
Managed SSA roots must survive a collection inside a thunk body. Success
publishes the evaluated tagged target and barrier before Updated, without a
safepoint. Reusable failures restore Live with captures intact. Terminal
integrity failure performs no heap write. SingleEntry does not memoize;
cancellation permits retry. Evaluating reentry reports a typed loop failure.

Prepared safepoints record through VMContext.machine_state, not TLS. Check
allocation slow paths, function entry, backedges, thunk entry and the update
commit boundary. Native stack bounds come from the Linux thread, including
guard and unwind reserve; checks must precede large frame consumption.

CAFs and every top object transitively referencing them belong to a rooted
invocation heap group. Reserve and initialize that group before publishing its
top slots. The closed immutable remainder stays static; image construction
independently rejects static-to-heap edges.

PAP arity includes Void; storage excludes Void. Static PAP descriptors identify
the original callable and pending count. Dynamic apply uses generated
signature-specific Tail dispatchers for exact, partial and oversaturated calls.
This refines the earlier host-buffer/bridge sketch: keeping pending and supplied
arguments in declared managed SSA roots removes a second rooting protocol and
never calls Tail code through a Rust function pointer. Dispatchers share the
same arity classifier; PAPs flatten to the original function plus prefix.
Primitive families share representation, failure and
rooting contracts, without importing Core layouts. Typed intrinsic identities
and external record-parent identity require coordinated wire migration.

Forcing observation uses rooted locations and re-reads after collection; no
borrowed nursery slice survives a force. Budgets bound expansion, cancellation
bounds evaluation, and cleanup is stack-safe. OldSpace owns descriptor-backed
regions, exact starts, retention promotion, and remembered slots. All old
managed mutations, including bulk array operations, use the barrier owner.
Retained closures pin code/descriptors. Terminal teardown uses ownership data.

### Retention implementation boundary

The prepared invocation must own its machine, top table and instantiated static
region while borrowing the compiled program. Keep this boundary non-Send:
putting an Rc<CompiledProgram> inside the existing unsafe-Send OldSpace would
hide the pipeline's Rc debug-registry aliases from the type checker. The
invocation privately owns OldSpace; use this owner in run_entry rather than
building a second test-only invocation path. Cross-thread/multi-generation
prepared sessions remain Wave 6, not an inferred unsafe Send implementation.
Borrow its admission view only within execution/observation scopes. Clear the
raw pointer before promotion obtains an exclusive borrow, and reinstall from
a fresh borrow for the next scope; allocation stability is not alias permission.

Selective retention promotion validates the complete initialized nursery before
its first forwarding write and rejects preexisting Forwarded headers. Through
immediate sibling fixup there is no mutator reentry. A scoped fixup capability
authenticates the exact source generation and newly promoted region starts;
Updated compression may also end in already admitted static/old objects. The
source descriptor/extent remain authoritative, but the target descriptor need
not be identical after indirection compression. Do not make arbitrary Forwarded
headers acceptable in ordinary nursery collection.

After the first promotion write, complete-root sibling fixup is mandatory and
non-cancellable: calling prepared_gc_trigger would be wrong because its poll
can skip collection. Preallocate destination and scratch before mutation. Any
failure that prevents fixup is terminal, retaining both heaps, descriptor/code
owners and roots through unwind. No native call or observation may see the
intermediate partially forwarded nursery. The old-space mutation barrier stays
the existing owner; descriptor arenas stay out of Core-layout compaction.

## Sequence and delegation

### Recovered bottoming bodies and external arrays

Current implementation checkpoint: `ba13cf1d3` is pushed. Checked primitive
byte access and exception-root ownership passed their focused contracts; the
generated raising path is not yet connected. The recovered `showDouble`
regression and recovery suites pass after the facts walker stopped forcing
GHC's deliberately undefined boxed-constructor annotation. The 140-row cohort
has not been remeasured after that repair. Local seeds `47153c3d6` and
`392cbabc5` start the coordinated schema-7/ABI-5 migration and intentionally
do not constitute a compiling checkpoint.

The current delegation review has already found two contract corrections:
the producer initially applied dead-end evidence before saturation, and the
validator initially rejected empty cases with known returning scrutinee reps.
Both were sent back with the precise accepted rule. These are semantic review
disagreements, not failures to fix by changing expectations. The first native
wiring brief was too broad; ownership was split into ABI/apply/adapter/plan
and emitter/invocation parcels before implementation continued.

The recovered `GHC.Internal.Err.error` body is present and ends in GHC's
`raise#`; its representation-polymorphic result is not a missing-body failure.
Eight external failing targets reproduce that exact path; three internal sat
rows have not been independently confirmed. Extend the shared signature result
contract to `Returns(reps) | NoSuccess`, preserving the distinction from a
successful zero-result computation. GHC's `isDeadEndId` supplies entry evidence;
the actual RaiseOp supplies operation evidence. Remove the separate global
boolean when migrating the wire. Projection, validation, linking, entry/PAP
ABI and primitive lowering move together in one schema/ABI migration. Partial
application still returns a function; saturation of NoSuccess never applies
an excess suffix. Its native ABI returns status only, with unexpected Success
reported as integrity failure. A normal Return cannot establish NoSuccess.
Divergence remains cancellable, not a fabricated language exception. Raising
retains the exact exception reference in invocation-owned root storage until
settlement; any presentation uses bounded descriptor observation, not Core
error-string decoding.

The wire result encoding will be `[0, reps]` for Returns and `[1]` for
NoSuccess (schema 7 / execution ABI 5). A fresh source consultation confirmed
that Wave 5 needs a pointer-free `RaisedException` language cause, not forced
exception formatting: the current corpus has no message/class-specific oracle.
The exact operand lives in a stable machine-owned root slot included in the
complete snapshot independently of temporary observation-root marks. It must
not be appended inside the raise host call, where observation cleanup would
truncate it. First raise wins; a prior cause cannot acquire an unrelated
operand, and later integrity failure preserves the cause while upgrading to
Unavailable. Clear the slot only on settlement/heap teardown. Do not force a
diagnostic after first cause is latched. Corpus failure comparison must match
the exact typed cause and Reusable disposition, never arbitrary runtime errors,
timeouts or unsupported forms. Add raising expectations only for independently
verified source cases; the optimized divergent blackhole remains distinct.

Array handles use fixed descriptors and a typed external edge, not variable
per-allocation descriptors or Core Lit objects. The existing MachineState
external-storage owner authenticates payload identity/kind/length/capacity and
owns reclamation. Boxed payload slots must participate in nursery copying,
selective promotion/fixup, retained-graph liveness and observation; aliases and
external cycles need deduplicated traversal. All pointer mutations and clone
initialization use the existing barrier. Remembered edges are not strong roots
for a full sweep. No sweep follows incomplete tracing. Resize cannot free a
payload still named by an alias; settle the prepared handle/resize contract
before reusing the legacy helper. These are required array-enabling contracts,
not permission to admit arrays ahead of their collector integration.

The external-storage extension keeps generation and revocation in that same
ledger: fresh payloads are Young/Active, promotion marks shared payloads
Retained, and successful minors reclaim only unreachable Young payloads.
Retained payload sweeping requires a full strong-root trace, never the
remembered-slot set. This avoids making every nursery collection scan the
retained graph while still reclaiming ordinary array-loop garbage. Wave 5's
production invocation does not yet retire retained roots; the existing major
retirement policy must be connected at the Wave 6 session boundary rather
than inventing an unreviewed pressure trigger in an array primitive.

The descriptor owns an explicit external edge kind. Its owner-authenticated
view exposes a bounded slot span, not a newly allocated Vec on every visit.
Minor copy and both promotion phases deduplicate shared external expansions
and skip slots already rewritten as snapshot roots; otherwise a second
rewrite mistakes a to-space pointer for an invalid source reference. Each
copying phase gets fresh traversal scratch. Full major tracing follows
validated nursery, retained and external graph views; Core's object decoder
must never inspect descriptor arenas.

`resizeMutableByteArray#` publishes a fresh payload identity, even when its
requested size is smaller: initialize
the new allocation/handle before revoking the old identity. Revoked aliases
remain structurally traceable and keep that allocation's address reserved,
but operations and observation reject them before accessing their contents.
Only a successful applicable liveness trace may release the allocation.
This avoids use-after-free/address-reuse ambiguity without adding a second
handle generation registry. Any incomplete copy/fixup cancels sweeping and
retains all buffers/payloads through native unwind.

The distinct `shrinkMutableByteArray#` and `shrinkSmallMutableArray#` primops
return only State#, not a replacement handle. They preserve payload identity
and allocation capacity, update the authenticated logical length in place,
and make all aliases see that length. Boxed shrink removes remembered slots
outside the new logical span. It must not revoke the only handle the caller has.

Array allocation reserves its fixed-size managed wrapper before allocating
the external payload. The wrapper receives a valid descriptor header, and
the noncollecting ledger allocation initializes its external slot before
publication. Allocating an unrooted Young payload before a collecting wrapper
reserve would let that very collection reclaim it. Allocation failure must
unwind without publishing the incomplete wrapper. This is a separate payload
allocation operation, not a host call added to the ordinary Construct fast path.

Array parcels after the result-contract fold:

- External view: move the shared kind/error vocabulary below codegen and
  replace per-visit slot Vec allocation with a bounded iterator. Preserve all
  existing ledger authentication. This parcel is implemented and focused checks pass.
- Descriptor graph: explicit external-edge metadata, one authenticated owner
  view, shared-payload/remembered-slot deduplication in minor and promotion
  copies. Tests must include aliasing through both roots and copied handles,
  promotion followed by unselected-root fixup, and late failure without sweep.
- Payload lifetime: Young/Retained classification in the existing ledger,
  successful-minor Young sweep, strong-root major tracing for retained payloads,
  and revoked-resize aliases. No per-object descriptor registration.
- Small/boxed arrays: new/read/write/index/size/freeze first, then copy/clone,
  CAS and shrink; exact logical State# and unlifted signatures. Seed one real
  adapter allocation/store/collection/read test before family delegation.
- Byte arrays: bounds-checked sized reads/writes/indexing, allocation/copy,
  size/freeze and explicit resize versus shrink alias contracts. No raw-address
  inference from an arbitrary Address field.

Lead commits semantic types/signatures, hardest path and a red contract before
implementation delegation. Searchable wave5 task markers identify scaffolds;
all must be removed before closure. Scaffolds return typed Unsupported, never
panic or todo. Combine record-parent and intrinsic wire changes in one early
schema migration with one fixture regeneration and a cross-language assertion.
A shared repr wire-program test builder precedes engine/projection test parcels.
Luna High owns bounded implementation and
fresh review; Medium owns exact mechanical checks. Three useful worker slots,
one serialized build slot. Two failed attempts escalate to Terra; another
failure pauses that parcel for semantic review. Disjoint file ownership.

1. B1 typed exact lookup; A1 prepared safepoint/stack groundwork; corpus and
   foreign-call inventory concurrently. B1 gets the first build slot.
2. B2 defining-context preparation and one real recovered caller; A2 generated
   entry/settlement, CAF partition and rooted initialization.
3. B3 dependency closure and corpus integration; A3 PAP/dynamic application;
   forcing observation after entry is stable.
4. Primitive families in disjoint parcels after shared layout/ABI seeds;
   evidence-backed foreign intrinsics across the language boundary.
5. Descriptor old-space retention and remembered-set integration following
   real thunk traffic. Fold and full corpus report.

Workers recursively split independently owned implementation, tests and fresh
review when useful; no worker resolves an unsettled shared semantic contract.
Lead records seed, assignment, review and correction rounds contemporaneously,
including whether the first brief was sufficient and the exact correction.
Each parcel names tests, owned files, fixed signatures and its command. Fresh
Luna review of lazy entry, defining-context recovery and old space reports
Accept, Correction(file/line), or Escalate(question).

The early stop line is body recovery through closure, lazy entry with settlement,
and forcing observation, demonstrated together by a real base-call result.
That is a coherent partial checkpoint, not full Wave 5 completion. PAPs,
primitive families and old space remain required for full completion. A simple
recovered body must avoid still-unimplemented forms to prove the first path;
do not claim all lazy base programs run at this boundary. Keep one worker slot
assigned to B until its first recovered call matches.

## Acceptance

Prove relocation during thunk update, reusable-failure restoration, terminal
non-restoration, SingleEntry retry, stack-bound failure, CAF partition and
static-edge rejection. Exercise PAP Void/mixed reps, oversaturation, GC and
cancellation. Force deep/cyclic values safely. Prove old-to-young thunk/array
edges and retained cycles/sharing/code custody. Recover a real base call and
execute through its caller. Keep Wave 4 tests passing and zero allocation host
calls on generated fast paths.

Preserve 812 corpus tops and the separate 347-name legacy ledger. Original
first-blocker cohorts: globals 516, thunks 80, expressions 15; projection
foreign/prim failures 10. Track each cohort to matching results, not merely
the next rejection. Annotate 46 Address and 21 managed-argument harness limits
without changing their historical outcomes. Every residual is named.

Checkpoint first passing track contracts and integrated shared-type changes.
Red owned contracts block parcel integration, not independent ready work.
Fold: formatting, diff check, workspace test-target compilation, relevant
just changed and fixtures-check with corpus execution. Report actual commands
and counts separately from compilation; no benchmark or planted-defect gate.
Push inventory, corpus evidence, friction notes and trial results. Closure
requires the intended mechanisms and interaction tests, not a named remainder
alone. Review entry/update, remembered slots and recovered-body preparation.

## Trial record

### Recovery-majority follow-up

Source/artifact diagnosis partitions the 473 current closed-global rows:
314 include unsupported constructor-worker references and 381 include
defining-module preparation failures, with 222 in both groups. Exact lookup
has succeeded for the preparation failures. These are not evidence for a
second identity-resolution rewrite. The first bounded correction materializes
authoritative nullary constructor workers as ordinary field-free tops; workers
with logical zero-width arguments are not nullary. A separate diagnostic
parcel captures the actual GHC error behind defining-preparation ExitFailure 1.

Contemporaneous lead events: three diagnostic/accounting assignments, one
inventory follow-up, one projection seed, one test implementation assignment,
and one seed collision correction from independent review. The corpus
accounting worker returned all 812 mappings without ambiguity or disagreement.
The nullary-worker brief points at the owner implementation and focused tests;
its sufficiency and test outcome remain pending. No token usage is exposed.

The subset-preparation reproduction names `stimesMonoid1` as an out-of-scope
same-owner reference in `$fMonoidProduct1`. The seed adds only free external
Ids from that defining module to the recovered-subset preparation scope,
excluding supplied binders; source-module lint is unchanged. `patError`'s
binder/RHS type mismatch is a distinct residual, not repaired by this scope.
Lead events added: one subset-scope seed and assignment, one inventory wording
correction, one build-lease handoff, and two nullary-test review messages. The
first nullary test brief was insufficient: source `(:)` construction does not
prove a first-class worker import survives STG. The fixture must demonstrate
the intended GHC form before asserting the projected result.

Nullary projection and recovery suites subsequently passed (one test target
each). Final source review requested removal of a duplicated test-side STG
walker and a tautological non-nullary assertion; that cleanup is not included
in the preceding result. The scalar inventory also found a real compiled-code
lifetime defect: literal-only byte allocations were dropped with ProgramPlan.
The owner seed retains the complete pinning map. Its worker parcel threads
heap-top initialization through that map and adds GHC's implicit trailing NUL
to storage, leaving logical wire keys unchanged. Lead also seeded checked
double2Int# lowering: exact F64-to-I64 signature, truncation toward zero, and
typed Overflow for GHC's undefined NaN/out-of-range domain. Both Rust test
parcels are pending; no new corpus success is claimed from these edits.

### Integrated recovery/lazy checkpoint

Connected PAP/primitive checkpoint: Terra's final prepared-program run passed
73/73 tests (`bash scripts/dev-shell.sh cargo test -p tidepool-codegen --lib
prepared_program -- --nocapture`). Six real-adapter PAP tests cover partial,
exact, excess, a suffix signature absent from the wire, mixed lifted/scalar/Void
prefix flattening under moving GC, a thunk returned before excess application,
and allocation cancellation without result publication followed by retry.
Demanded dispatcher signatures now close through an ordered cursor worklist.
The deterministic join-backedge settlement test also passed in that run.
The pure float and checked quotient/remainder families are connected; this does
not establish every primitive family or old-space retention.

The foreign-call diagnosis is now concrete: GHC projects rintDouble with
logical arguments `[Float64, Void]` and result `[Float64]`. The original
intrinsic seed recognized only its physical C signature. Preserve the final
State# argument on the wire and erase it only in native ABI lowering; accept
that exact variant, not arbitrary Void insertion. Rust coverage exercises both
forms; producer correction and corpus rerun remain separate evidence.

Subsequent parcel log (recorded while running):

Current result-contract migration log (still open; these are local parcel
events, not reconstructed totals for earlier context):

- Repr: one lead type/callable/test seed, one assignment, two semantic
  corrections (unknown oversaturation and known-rep empty cases), four small
  interface/coordination follow-ups. First brief was insufficient; worker
  implementation is awaiting the shared fixture/build boundary.
- Haskell: one assignment and one semantic correction on saturation evidence;
  one build-lease follow-up. First implementation was not accepted on source
  review. Cross-language tests remain the acceptance, not matching type names.
- Native recognition: one assignment, one helper-interface follow-up, one
  handback review. Implementation reported source-complete; no build claim.
- Native ABI/apply: one root native seed, an initially oversized wiring brief,
  two scope-narrowing messages, and one handback review. An unrelated
  CallerArea extension was sent back for removal. Emission/invocation moved
  to a separate owner; the scaffolding brief needed that correction.
- Constructor-panic diagnosis: the reproduction was useful, but the proposed
  GHC-pass workaround was rejected. A separate pinned-source read plus lead
  consumer inspection identified the actual fault. The corrected regression
  passed two Haskell suites. This is a review disagreement, not a clean
  one-round diagnostic success.

No harness worker/planner token totals are available. The current experiment
shows routine signature migration delegating cleanly, but continuation
semantics still requiring lead review; build-lease routing remains lead
overhead rather than implementation work.

Latest parcel events: integrated Rust verification needed one assignment and
one consolidated authorization for mechanical fixture repair; four failures
were stale wire tags or flat-tree ownership/order, not four backend defects.
The partial-PAP test required a lead contract correction: the Case-bound
callee has unknown callable metadata, so its demand is ordinary LiftedRef;
the underlying bottoming entry remains NoSuccess and still raises at runtime.
Haskell assertion review required one tests-only follow-up because the initial
tests checked raise# presence without proving entry/call result contracts.
The raised-CAF retry required one seed-shape assignment: retrying a new
invocation was insufficient evidence of same-machine restoration. External
view work has one lead type/iterator seed and one implementation assignment;
its build is pending. The array inventory was accepted after one read-only
round; it corrected the lead's conflation of shrink and resize signatures.

| Parcel | Worker rounds | Lead events / brief sufficiency | Outcome |
|---|---:|---|---|
| Float pure family | 2 Luna | Seed + assignment + semantic correction: constructor labels are not primOpOcc names; conversion branch was unreachable | Three focused tests passed after correction; no host-libm claim |
| A4 deep observation | 2 follow-ups | Rejected ignored expensive fixture and !Send owner transfer; required large validated image behind tiny compiled module | 20,000-node test passes on 256 KiB stack |
| A2 dynamic apply/PAP | 2 Luna | Arity/layout seed + assignment + correction on suffix closure, PAP-of-PAP, rooting, Enter and absent adapter tests | Escalated to Terra; not accepted from compilation alone |
| Retention custody | Sol read + fresh Astra consultation | Two bounded ownership questions and one forwarding-authentication follow-up | Non-Send borrowing invocation accepted; no unsafe Send widening |
| Invocation owner | 1 Luna | Type/Drop seed + assignment; report corrected to identify concurrent A2 reds rather than inherited failures | Production run_entry uses owner; promotion not implemented yet |

Actual native recursive-entry stack-bound and reusable language/stack failure
settlement tests passed with the relocation and cancellation tests (four
settlement cases). The newly added deterministic join-backedge cancellation
case has not yet run at this entry. Token totals remain unavailable.

First recovery corpus replay (`target/prepared-corpus/suite.SyCuEW`) retained
812 tops: 791 projected/validated, 262 admitted/compiled, 173 executed, 106
matched, zero comparison mismatches and 67 missing expectations. Baseline was
802 projected, 191 admitted, 123 executed and 56 matched. Recovery exposed
11 runtime-polymorphic projection rejections; the ten foreign-call rows still
reject. These are not a green full-wave result. Results SHA256:
`b416596e72201e92f28bd15382e402d336251407fba08b02ffc4304750843e31`.

The named `thunk_blackhole` timeout needs a semantic distinction: its 135-byte
prepared artifact (`132.prepared.cbor` in that replay) contains recursive
LetJoins with two zero-argument Jump nodes, not self-entry of an Evaluating
heap thunk. GHC has made the body an infinite join loop. The historical oracle
expects "blackhole", but the corpus runner's watchdog aborts without requesting
engine cancellation. Preserve this original outcome; do not change lowering
to fabricate a blackhole from a valid divergent join. A deterministic backedge
cancellation contract is the engine-side acceptance for this shape.

The current focused fold passed `bash scripts/dev-shell.sh cargo test -p
tidepool-codegen --lib prepared_program`: 58 passed, none failed. The repr
cross-language boundary passed `bash scripts/dev-shell.sh cargo test -p
tidepool-repr --test execution_schema_contract -- --nocapture`: three passed.
These include generated-entry relocation/settlement, sibling-root survival
while observation forces a collecting child, and the exact rintDouble seed.
They are not a workspace or full-corpus result.

The recovered `main:RecoveredBody:value:caller` package-fst example reached
projection, validation, admission, compilation, execution and matching result
1 through prepared-corpus. Artifact SHA256:
`283752e749df7ea59f2332c7ae95ac3f640b96f054252febdd47be2957562d8d`.
Disposable evidence is in `target/w5-recovered-fst/results.json`. Broad recovery,
PAPs, remaining primitives and descriptor old space are not established by it.

Closing shapes for this checkpoint (not full-wave closure): exact verification
and callee-identity-based test repair were accepted in one worker round. Pure
integer lowering needed correction and Terra escalation because names and
signatures were inferred instead of checked against GHC. Forcing observation
needed a semantic review/correction for FFI aliasing, pre-force admission and
incremental indexing; it should not have been handed off without these owner
contracts. Recovery review found bounded diagnostic/async/identity corrections,
which passed its two Haskell suites and extractor build. Lead events since the
preceding entry: three follow-up assignments, forcing semantic correction,
recovery contract correction, IR-test correction, integration evidence review,
and the rintDouble owner seed. Worker token totals remain unavailable; no cost
estimate is inferred from these counts. The 20,000-node forcing-observation
test is still in progress, not covered by the shallow alias-chain test.

Initial lead rounds: one accepted plan; one source/ownership read; one B1 seed;
one corpus inventory assignment. Update as work proceeds; no token totals are
available unless the harness exposes them. Closing task-shape assessment is
required before the final push.

### Initial parcels

| Parcel | Worker rounds | Lead events / brief sufficiency | Outcome so far |
|---|---:|---|---|
| Corpus inventory | 1 | Assignment + evidence review; brief sufficient for enumeration, suggested first body unnecessarily depended on FFI | Accepted inventory; ten targets share rintDouble evidence |
| Shared wire builder | 2 | Assignment + correction: worker omitted explicitly owned contract-test migration; then scalar-thunk baseline changed to executable Function and validated prepare helper requested | Source ready; focused builder/codec checks reported passing |
| Exact lookup | 2 | Seed + assignment + correction requiring actual tests rather than a list of suggested tests; review caught fixed temporary-directory deletion and GHC diagnostic rendering error | Tests written; build repair in progress |
| Haskell schema | 2 | Shared seed + assignment + cross-language fixture correction; Haskell-only encode assertion was insufficient | Source ready; build exposed exact-lookup error |
| Rust schema | 2 | Assignment + integrated-check correction; worker substituted cargo check for build and misclassified its changed expression-ID fixture as decoder defect | Escalated to Terra with diff evidence; canonical corpus key mismatch also identified |

Lead semantic seeds added: common source/recovered preparation owner, exact
unfolding lookup, invocation-machine cancellation poll, generated nursery-thunk
settlement, and a typed CAF execution regression. The latter is intentionally
red until entry/CAF integration; no passing lazy-engine claim is made.

### Schema and scaffold checkpoint

Actual `bash scripts/dev-shell.sh cargo build --workspace --tests` passed
after source freeze (49.69 seconds). Repr tests passed: 238 unit, 55 integration,
one doctest. Prepared admission tests passed 6/6. Haskell schema encode,
projection and corpus-mapping suites passed, one suite each; the prepared-STG
pipeline suite also passed. Both schema6 cross-language seed and M3 artifact
were generated by Haskell. These results do not establish lazy execution.

At that checkpoint the exact-interface test reported NoExtraDeclarations.
Terra subsequently established that LoadAllTargets rebuilt the fixture without
fat-interface flags. The test now retains those flags during loading and
deliberately recompiles the thin-interface case before inspecting it.
`bash scripts/dev-shell.sh bash -lc 'cd haskell && cabal test fat-iface-exact-test'`
passed 1/1. The mutually recursive fixture still requires the full Rec group.
This is lookup evidence, not recovered-body execution.

B2 reached two Luna build attempts (GHC Module/FindResult diagnostic instances)
and moved to Terra for verification. Its source adds real fst body recovery
and defining-context preparation; no execution claim yet. A1's worker reports
37 prepared_program tests passing after entry/CAF integration, including the
initial lazy allocation regressions. Fresh semantic review and stronger
relocation/settlement evidence remain. Full fixture freshness/corpus replay
remains for the integration fold.

Additional lead events: B1 evidence/diff review; B2 correction on recoverable
data-constructor wrappers and exact package identity; B2 escalation assignment;
B3 worklist semantic seed; A1 observation-state review and forcing-adapter
assignment. The first B2 brief needed the wrapper/finder correction. No worker
token totals are exposed. These events are recorded now, not reconstructed at
the final handback.

### Recovery and lazy-entry follow-up

B2 Terra verification passed
`bash scripts/dev-shell.sh bash -lc 'cd haskell && cabal test recovered-body-test fat-iface-exact-test'`
(one case in each suite). Defining-module preparation uses the exact package
finder and raw interface declarations rather than the stripped PIT view.
This proves recovery/preparation/projection of package fst, not execution.
A fresh Luna lookup review accepted the exact lookup owner without building.

A1 follow-up reports 39 passes using
`nix develop --command cargo test -p tidepool-codegen --lib prepared_program --no-default-features -- --skip w5_a3_integer_add_runs_through_real_adapter`.
That run preceded the A4 forcing-observation seed. It does not cover the new
seed or establish deterministic cancellation settlement. Parent corrections
covered packed-field store widths, initial CAF reserve sizing, dead
SingleEntry heap states, and omitted local-thunk admission. A fresh entry
review is in progress. B3 closure, scalar operations and the forcing seam
remain in progress, not green.

ABI 4 names the VMContext layout with prepared stack bounds. Schema remains 6;
the fixture regeneration must carry both versions together at the fold.

Current trial data: B2 required two Luna build rounds then one Terra repair
round; first brief insufficient on exact package lookup/wrapper preservation.
A1 initial 37-pass handback needed one consolidated semantic correction batch
and adapter follow-up; reported result 39 passes. Lead subsequently found
updated-header lookup and recursive observation chasing still needed review,
so that portion of the handback is not accepted. The integer seed needed a
lead correction from little-endian to canonical big-endian literal bytes;
that is a seed defect, not a worker failure.

Source review also found prepared allocation cancellation still using the
legacy TLS error recorder. It now samples and records on VMContext's machine;
the new named-safepoint test seed covers cancellation without TLS. Typed
FunctionEntry/Backedge/ThunkEntry/ThunkCommit wiring and generated settlement
interleavings remain outstanding until their tests run.

Recovered modules explicitly carry `ExactBodySubset` coverage, distinct from
`CompleteSourceModule`. Missing source-home tops remain producer errors; a
package body absent from a partial recovered module remains an explicit
global with recovery diagnostics. Treating both as complete source modules
would falsely turn an honest missing implementation into MissingPreparedTop.

The first integer handback passed seven focused tests and six admission tests,
but parent semantic review rejected width-polymorphic recognition for fixed
GHC names and the Word-shift argument representation. One correction attempt
is in progress; those tests were insufficient evidence for the recognition
table. Fresh A1 review also found local Enter still routed to the status-only
host guard despite the earlier handback. A1 moved to Terra for repair; the
corrected function-entry/local-thunk check has passed, with broader tests
still running. Test counts are evidence about those exact tests, not a proxy
for contract completion.

### First connected recovery milestone

`main:RecoveredBody:value:caller` now passes projection, validation, admission,
native compilation, execution and comparison against integer 1 through the
ordinary prepared corpus runner. Evidence is generated at
`target/w5-recovered-fst/results.json` (one program, six passed stages). The
artifact is SHA-256
`283752e749df7ea59f2332c7ae95ac3f640b96f054252febdd47be2957562d8d`.
This proves one real base call, not the original 516-program global cohort.

The root-authored `prepared_program::settlement_tests` passed 2/2 through the
dev-shell toolchain: an update to the relocated thunk with its original source
header Forwarded, plus named-poll cancellation and retry for Memoize and
SingleEntry. Backedge/captured-payload and terminal-injection evidence still
needs expansion. The allocation poll records through VMContext, not TLS.

A4's first handback passed five focused tests but needed correction: it built
the full heap index on each node despite the stated incremental index contract,
forced before exact-start admission, and held a shared result-area slice across
GC writes. Its 96-thunk test did not prove deep small-stack observation. These
are integration/coverage findings, not evidence to label the wave complete.
Scalar recognition required Terra after a fresh GHC audit found the remaining
`and64#` spelling and fixed Word64 shift-count mistakes; 10 focused scalar
tests passed after repair.
