# Engine completion: approved implementation wave

Approved 2026-09-29 after the design review. This updates conflicting portions
of `engine-completion.md` and `harness-integration-runtime.md`. Root baseline is
`8bbfb9489`; preserve all existing worker commits and dirty changes. Foundation
is complete, but compiler reuse, concurrent publication, and final acceptance
are not. Pushes remain deferred; live G5 requires its separate readiness approval.

## Shared compiler contract

The user selected full declaration isolation, exact recoverable declaration
graphs, and durable publication success, not a restricted source-wrapper join.

One compiler transaction owns fresh EPS/HPT/finder state before hydration or
recompilation checks. Immutable artifacts may be reused; mutable typechecking
state must not leak between lexical environments. Verify exact toolchain,
module, interface/product hashes, package selection, and complete dependencies.
Reject differing artifacts for the same original (unit,module) in the required
closure before loading Names. Distinct Lib.G identities permit normal same-name
replacement. Do not rename or replay already-compiled private declarations.

Hydrate implementation SCCs into HPT with proper recursive knot tying. Keep
implementation metadata outside the public lexical graph. A persisted synthetic
Join interface and explicit virtual graph node supply original-name exports
and selected exact dfun/axiom identities. An ordinary downstream consumer must
use this same injection path, never rerender an unsafe source wrapper. Home
interface fallback into EPS is an invariant failure, not a permitted cache hit.

Separate selected family reduction visibility from the full retained family
axiom consistency closure. Check associated type/data families and injectivity
on joins AND every subsequent declaration compilation. Hidden incompatible
axioms reject. Ordinary hidden class dictionaries retain original behavior;
GHC overlap/fundep checks apply to selected and newly authored instances.

The join receipt binds request digest, reserved identity, expected paired public
snapshot, exports, selected instances, implementation closure, family evidence,
and persisted interface/product hashes. Runtime owns desired merge policy;
compiler certifies actual meaning. Whole-EPS equality is not a valid inventory.

## Products and native instances

Finish candidate lookup -> downsweep -> scoped recompilation checks -> SCC-closed
admission/hydration -> accepted receipt -> final input revalidation in one
transaction. Leaf hits are intermediate; support dependency-bearing/recursive
graphs. Unknown compile-time inputs remain misses. Seal product/interface pairs
through the existing toolchain cache, checking hashes, original ordinals, owner
alignment, representations and signatures. Preserve explicit cached-home refs.
Carry certified artifacts through CompiledArtifacts/CompiledTurn to resident
consumers instead of discarding them.

Demand and compile outside checkout. Install the sole target entry plus all new
reachable groups in one atomic batch. No provisional source-handle registry.
Code identity is separate from runtime GroupInstanceId (existing ProgramId).
Only exact instance leases inherited from lexical scope/capture permit mutable
CAF reuse. Otherwise identical code gets fresh private instances, including in
the same machine. Scope/tip custody owns materialized leases; unused metadata
does not root code. Publication and capture share original instances.

Extend existing CodeExport ownership with exact package/interface provenance
and callable signatures; do not introduce a second registry or spelling fallback.
Acquire requested handles/evidence within the install transaction; late failure
leaves machine, scope and evidence unchanged. Keep final-owner reclamation exact.

## Publication, execution and recovery

Use durable monotonic generation allocation per recovery lineage and sparse
typed declaration nodes. Reserve before staging; burn failed/cancelled IDs.
Authored and Join are distinct. Never derive public visibility from allocation.

Admit exact published declaration/binding/instance state into a private execution.
Stage final writes, retractions, leases, compiler proof, artifacts and fsynced
manifest temporary outside checkout. Revalidate both Accepted and Rejected
receipts; stale outcomes restage without replaying effects.

One decision state machine orders cancellation/publication: Running,
CancellationRequested, CommitClaimed, Published, terminal without publication.
Acquire machine checkout before the short decision lock. Claim commit, release
the decision lock for filesystem work while retaining publication serialization.
Cancellation during a claim remains pending until outcome is known.

Extend the existing atomic-write owner with staged publication and typed
before-rename versus published/durability-unconfirmed outcomes. The persistent
manifest is commit authority. After rename, perform the preflighted infallible
declaration/binding/instance visibility swap. Failure after rename is Published
with recovery durability unconfirmed, never ordinary failure/cancellation or
permission to replay. Normal success requires confirmed durability. Retry only
durability confirmation for that publication. Ephemeral modes are explicit.

Execution records own cursor, reply/control, private scope, reservations,
continuations, receipts and publication state. Actor lifecycle/coordination stay
actor-owned. Exact execution+step generations fence completion and cancellation.
Keep external waits outside actor handlers and machine mutation under checkout.
Do not remove dispatch serialization until paired publication and multi-cell
progress tests pass. Structured actor turns remain non-reentrant.

Successful captures keep completed private lexical/source state independently
of parent failure. Token revocation forbids new admissions; existing children
retain their shares. Exact cleanup stays with session owner until acknowledged.

Recovery v2 records a checksummed Authored/Join DAG, artifact closure, public root,
high-water, exports/retractions, instances and live-value dependencies. Durable
recovery roots pin artifacts through the existing cache owner. Rehydrate exact
artifacts, never replay private source/effects. Determine final visible heads
before losses; missing winners keep tombstones rather than resurrect older heads.
Certify a recovery projection of surviving declarations when possible. No live
heap, continuation or token recovery. Migrate safe v1 source manifests explicitly;
ambiguous legacy surfaces are reported lost rather than guessed.

## Integration and acceptance

Finish real M1 host/lifecycle fencing and pending raw/typed call plain-text
compaction, late output, cancellation and cleanup failure. Then attach embedded
children through existing lifecycle/capture ownership, one Store/scheduler per
run, exact origin operations and actual browser tree including Haskell actors.
Compile updated Haskell examples and reconcile prompts with final semantics.

Required gates: hidden orphan/nonorphan/transitive instances; old dictionary vs
new lookup; family conflicts; forced metadata lookup; source-hidden fresh-worker
consumers; recursive cache/demand closure; unused-code omission; distinct same-
machine captures; inherited CAFs; cross-group heap-top cycles; late rollback;
last-owner reclamation; stale success/rejection; both cancel/commit orders;
pre/post-rename faults/crash recovery; two parked cells plus progressing third
and controls; two children before parent failure; release/reconnect/reload/host
loss and pending compaction. Retain Git/helper evidence and disclose unchanged
process-scope/terminal pre_exec paths, without unrelated launcher migration.

At final joined boundary run every just verify constituent, producer-based
artifact regeneration, and matched worker/extractor/binary/harness/assets/client
packaging. Retain source hashes, commands, actual counts, logs, verified bundles,
and matched measurements; historical evidence is not exact before/after proof.
Publish dependencies before root only after the push hold is lifted. Prepare
binary/assets/workspace/task packet before separate live G5 approval.

Up to eight workers: Sol compiler/join/runtime/native owners; Luna bounded
scheduling/recovery/M1/review leaves. Exclusive file ownership and reviewed joins.
Builds use admitted 104 GiB slice; expensive Nix realizations serialize with
--cores 2 because the daemon separately has an 8 GiB cap. The matched shell
realization passed with that setting; final matched package remains required.
