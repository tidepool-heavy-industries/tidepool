# Wave 5: laziness, settlement, and exact body recovery

Baseline: `3b657cd08`. Full-wave implementation, with production sessions,
effects, retained-generation linking and resident cutover reserved for Wave 6.
Broad Core deletion is Wave 7. No Core fallback is added.

## Latest verification checkpoint

`3ab9ec613` is pushed. The focused prepared-program library selection passed
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
a separate filename/declared-module mismatch in the source pipeline; a scoped
corpus input alias repair is in progress, not an engine pass.
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
