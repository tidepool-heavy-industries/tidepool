# Request and service lifecycle closeout — Option C

Status: authorized implementation. Base: 9cbdbf44d8a8b025d0e903fcdb26349734907f97.
Integration owner: /root, checkout tidepool-domain-runtime-join-20261023,
branch integration/g2-domain-runtime-20261023. Do not change the held warm
validation checkout until its owner explicitly releases it.

## Outcome and boundaries

Qualify the rebuilt request/service region, then rerun maintained G2 to locate
the real remaining frontier. Preserve the compiler graph, existing transaction
END/input-custody owner, worker pool and exact source-retention mechanism.
No accepted-request retries, jobs=1 workaround, authority from status JSON,
old finite-notebook route, generic failure framework or second root manager.
A migration includes deletion of replaced APIs, implementations and test bypasses.

Current evidence is validation/9cbd-checkpoint.{json,md} under
/srv/swarm/evidence/inanna/c-ownership-20261022: 15 executed, 12 passed,
3 failed, plus a warning startup with zero execution. Activation fails after
semantic refusal; the original cause was retained in Rust but discarded by
transport-first propagation. The promotion and activation-CLI failures are
incorrect test premises. The Nix dnix/store error remains unexplained.
Package acquisition's own durable record reports all 76 roots preboot-released;
the outer report lost that distinction. No current G2 qualification exists.

## Request contracts (W1/W2)

W1 replaces worker open/busy/borrow lifecycle fragments with private Ready,
SemanticallyRefused(first opaque reply), TransportBroken and Closed states.
Retain one existing wire gate and explicit accounting. Under that gate check
thread ownership, SOURCE envelope, current borrowed stage and stage monotonicity
before returning a retained refusal. Record first refusal before unlocking;
valid later calls return it locally without IO, sequence or budget changes.
Malformed resolved semantic payload remains distinct from actual wire refusal.
Interrupted/partial transport commits broken state before releasing the gate;
do not use rollback-on-exception state helpers that restore Ready.

Terminal admission must see the final rendered ExitCode, not merely
completionResult/BodySucceeded. Settle compiler tasks, revoke borrowing,
render with the remaining budget, validate lifecycle plus final code, then
write. Semantic refusal permits only failed output; swallowed refusal may not
emit status zero. Broken transport permits no terminal. Preserve RequestCompletion
cleanup/cancellation facts. No dependence on DependencyPlan semantic decoding
inside the framing owner.

W2 adds NativeDemandSession: NativeDemandResolver + Sized with associated
Outcome, owner(&self) -> NativeDemandOwner, and complete(self,
Result<ExtractRun, SpawnError>) -> Outcome. Keep the resolver callback object-safe.
Both public execute_with_native_demand entry points consume the session and
derive owner from it. Every normal early return belongs inside an inner execution
closure; complete runs once afterward, outside TRANSACTION_SCOPE RefMut. Endpoint
calls the private borrowed transaction executor, never the public consuming
wrapper. Private transport alone borrows &mut dyn NativeDemandResolver. No Send,
static, clone or boxing requirement. Semantic completion is not panic handling,
END acknowledgement, input release or publication authority; existing guards
and affine close sinks still own unwind. Delete old public borrowed routes and
the separate World finish API.

World completion returns a coupled ExtractRun + WorldDependencyObservations on
success. Semantic refusal retains original Box<CompileError> plus a closed sum
of received ExtractRun or failed SpawnError. Pure transport failure retains the
existing CompilerEndpoint error. Never stringify SpawnError or discard its private
submission classification. Preserve transaction close separately in its existing
owner. Update failclass exhaustively to expose both causes while preserving
semantic classification and independent transport restrictions. Record semantic
error before a post-resolution checkpoint/reply allocation/budget failure; trace
its typed cause, request owner, stage and sequence without payloads.

One private Rust protocol state in native_demand.rs owns correlation, sequence,
budget, refusal and terminal checks. Explicit Client constructor allows first
round deadline tightening and later equality; Relay requires its already-clamped
header exactly on every round. Keep each transport's process/retirement/quietness
responsibilities. Remove three local refused booleans and duplicate rules.

## Service contracts (W3/W4)

After pure admission and accepting the fresh root, one OwnedServiceRun exists
before identity probing/spawn. Publish initial evidence before launching resources.
All later success/failure/cancellation paths use its explicit finalizer; register
spawned children immediately. Reuse bounded process termination/reap primitives,
not the unbounded raw-child Drop loop. Attempt all independent safe settlement;
failed prerequisites prevent destructive dependent releases, not other observation
or reporting. Action, daemon reap, worker retirement, source settlement, artifact
retention and publication failure remain separate.

W3 owns the shared Rust service types/codec, schema-2 report writer and control
carrier. Agree exact callable signatures with W4 before implementation overlap.
Carrier messages are finite: Boot(epoch), acquisition outcome(reference/digest),
optional Ready(exact identity), terminal service observation. Validate operation,
epoch, order, duplicates, size and partial frames. Terminal-before-Ready is valid.
Use the existing authenticated inherited UnixStream, detach fd0 to /dev/null
after dup/authentication, retain CLOEXEC. Pump through startup, owned workload
observation and shutdown; no opaque blocking workload .status(). One absolute
startup deadline on both peers; independent bounded shutdown deadline. Keep
messages bounded with references, not inventories. No new asynchronous runtime.

Child reports its worker/socket obligations; parent supplies actual daemon reap.
Each slot keeps current and unresolved prior retirement; replacement cannot erase
uncertainty. No successful-rotation history registry. CompleteServiceRetirement
is privately issued only from actual matching process and slot evidence. EOF,
STOP acknowledgement, empty process scans and absent files issue no such proof.
The existing native-finalization CLI remains native-only.

W4 issues a durable PackageAcquisitionAttempt before provider spawn, bound to
canonical owner, attempt, epoch, launch/source/profile/record digests and immutable
exact root scope. Mutable acquisition.json remains progress, not authority.
Return exactly one bounded versioned provider frame: Acquired(receipt/profile
refs+digests), or Refused(primary failure plus NoRootsIssued/PrebootReleased/
RetainedUnknown settlement and separate cleanup/publication failures).
NoRootsIssued requires owner observation before mutation. Process exit/frame
mismatch, truncation or invalid evidence retains AttemptUnknown with custody.
Diagnostics stay off stdout. Preserve exact argv/status and bounded stderr or
retained log reference. Never replace primary failure with evidence-write failure.

Retain attempt/custody on provider failure and on success followed by receipt,
profile validation or HANDOFF-publication failure. Valid acquired receipt does
not become unpublished. Pre-HANDOFF settlement uses exact durable attempt plus
validated provider receipt/scope AND same-attempt CompleteServiceRetirement;
HANDOFF existence is not its prerequisite. No broad rediscovery or guessing from
missing files. Preserve acquired Python-guard/native-finalization ordering.
Keep recovery conservative when proof is incomplete; historical unknown evidence
need not become recoverable automatically.

One schema-2 owned-service report replaces lifecycle.json and
owned-compiler-outcome.json writers/readers. W3 owns Rust definition/writer;
W4 owns Python validation/qualification readers; W5 owns isolated-libtest consumer.
Retain historical files as evidence but reject old schema for new qualification.
Report serialization never grants cleanup authority.

## Parallel ownership

- W1: bridge/haskell/src/Tidepool/WorkerServer.hs and owning Haskell transport tests.
- W2: extract-cmd native_demand.rs, endpoint.rs, callback exports, native transport
  tests; ONLY execute_transaction_request_inner, read_direct_native_output,
  Worker::native_request adapter regions in daemon.rs; toolchain World resolver,
  consuming join, narrow failure types/classification and their tests.
- W3: extract-cmd frontend.rs, process helper extraction, NEW service types/codec,
  daemon startup/slot retirement/serve regions (not W2 adapters), Rust service tests.
- W4: package_source.rs, qualification.py provider/readers and their tests. Do not
  edit frontend.rs or daemon.rs. Consume W3's agreed service contract.
- W5: request.rs stale CLI fixture, certified_products.rs promotion/refusal test
  decomposition, associated tests; isolated-libtest.py/schema consumers after W3
  freezes schema. Do not introduce a second test authority or copied algorithm oracle.
- Root: integration, shared decisions, build-source registration, plan/status,
  critical cross-boundary review and validation coordination. Integration handles
  disjoint daemon changes deliberately, never blind conflict resolution.

Every worker uses an isolated source worktree, commits complete base-to-tip delta,
and reports pending compile/execution honestly. Current scratch is about104GiB:
no new cold build lanes until measured reservation keeps100GiB unreserved. Reuse
warm validator for joined checks; source review/formatting do not qualify parcels.

## Required evidence and sequencing

Compile changed owners AND direct cfg consumers. First deterministic tests:
concurrent first refusal and exactly one frame; stale/foreign/SOURCE refusals;
final ExitCode error/warning/swallowed-refusal controls; failed terminal/clean END/
next request; both deadline roles; finalization on every normal early error and
outside transaction borrow; semantic+transport+close evidence preserved.
Service: real owner startup refusal after registrations, preboot release,
post-provider receipt/profile/HANDOFF failure, partial carrier deadline/cancel,
STOP failure, terminal then daemon hang, daemon reap then truncated terminal,
multiple independent cleanup/publication errors and forged/crossed owners.
Property histories: uncertainty cannot strengthen retirement or authorize replay.

W5 uses typed Turn/activation builders, keeps CLI rejection independent. Separate
issued-history success from raw custody refusal; isolate one bad exact demand so
traversal order is irrelevant, with a separate both-ordinal assertion. Retain full
typed refusal and generated historical/equal-byte-different-issuer controls.

Then run actual activation, mixed-seal and startup histories; complete pending
regional matrix; independent scoped adversarial/purge audit, repair and repeat
until no unresolved in-scope findings; only then maintained G2 (including prior
published-declaration reuse). Freeze only after G2 acceptance. New structural
frontier requires review, not silently widening this wave. Investigate repeated
Nix root-query failure separately from these confirmed ownership gaps; no blind
retry, direct fallback, lowered concurrency or validation bypass.
