# Actor kernel

This crate owns exact-incarnation actor identity, lifecycle and ownership,
mailboxes, actor-local execution context, and provider-neutral actor sessions.
The resident workbench owns hosted request envelopes and authenticated checkpoint
coordinates; generic execution identities and receipts remain in `tidepool-runtime`.
Machine sessions, JIT continuations, providers, concrete handlers, and
observability interfaces stay in their owning crates. Ractor is the scheduler;
`LocalActor` adds Tidepool identity, retained exits, live-value custody, and
call ancestry. Do not add a second scheduler or result/root registry.

The actor's original behavior allocation travels intact through admission,
serial task transfer, and fenced rejoin. Serial tasks move its `Box<B>`;
mailbox handlers and completion finalizers borrow `B` through that box. Holding
an actor-state borrow across an await retains that actor's serialization.

Named native operations heap-own their future before composing polling adapters.
Future layout and native poll-stack use are separate costs: a future handle
bounds the enclosing state, while a native poll can still use its own stack.
Inspect each at its owning boundary.

Ractor serializes actor turns. Independent notebook executions retain their own
fenced task, cursor, reply and cancellation control while parked. Stateful actor
protocols remain exclusive until their RPC resolves. Delivery validates the exact
actor incarnation and machine session before mailbox acceptance. Mailbox values use the existing
managed Haskell custody machinery, and cancellation, abandonment, and exit
settle once and release kernel-owned roots. Compile actor-authored code through
its exact `SessionCompileView`; do not pass ambient scope ancestry or caller
imports through that boundary. Reattach to the accumulating interactive
session rather than opening parallel contexts for one actor. Worker lifecycle,
wake correlation, collection, and acknowledgement remain Rust interpreter
state.

Green frontiers remain owned by their admitted invocation. `InvocationExecution`
drives native advances alongside retained frontiers through its named execution
methods: an unpolled frontier can retain the FIFO position needed by the next
machine checkout. Keep nested effect and display advances inside that boundary.
Buffer ready completions until scheduler selection; cancellation drops both queued futures
and buffered completion custody under the same owner.

`ActorRequestSnapshot::select` issues an endpoint ordinal from one complete
actor generation or bootstrap installation. Argument admission travels with that
selection; dispatch enters the retained handler by ordinal, without declaration
equality or a second name lookup. Reload replaces the active generation while
issued selections retain their original source, context, public owner and
implementation. The native status control reserves its name at authored endpoint admission.
Reload uses the ordinary Haskell effect route from an exact admitted synchronous
endpoint carrying Reload, with separate source-owner authorization. Compatible
publication preserves the surface; explicit opt-in still validates the candidate
through the installed provider projection before publishing. The caller retains
its original generation and settled publication fact while continuing. After-tool and initial-context execution
retain separate optional admitted slots from the same installation; one slot's
presence cannot issue another slot. After-tool presentation awaits the
completed receipt before model delivery while the actor scheduler continues
servicing its ordinary effect continuations. It has no separate deadline,
cancellation control, or replay path. A failed hook preserves the original
committed output and semantic value, and records a diagnostic internally;
runtime shutdown retains the invocation's existing cleanup ownership.

Shared source preparation belongs to the run's compiler preparation owner.
`ActorWorkbenchSource` is configuration; native preparation requires a bound
source with a client issued by that owner. Clones and repeated bindings to one
client share pending work; another owner has independent pending work.
A waiter observes the producer's compiler receipts without its stop authority.
Run sealing closes admission and requests cancellation before waiting for physical
settlement; finalization checks that settlement at the owning boundary.
Standalone preparation uses `CompilerPreparationOwner::scope_with_client`, whose
guard closes work even when its future is dropped before polling.

Installation-issued effect rows carry their exact nominal imports through
templates, actor compile views, and inspection. Keep that source fragment coupled;
authored type text does not issue imports or additional effect authority.
Actor start and native spawn attenuate the selected keys and descendant limits
through `ActorCapabilities::preview_child`. Do not retain a named effect profile
as a second admission gate. Concrete workspace and provider grants remain
independent of effect availability.

Authored initial context uses the installed spec dispatcher and an actor-owned
workbench invocation, with the ordinary admitted effects. The actor journal
retains preparation, its exact semantic result and receipts, and publication
separately. Settlement retains the result before releasing its reply; a lost
host waiter cannot discard a completed seed. A recovered interrupted preparation
requires reconciliation, never implicit effect replay. Fresh initialization is
distinct from captured context, resume and spec reload. The host must preserve
caller seed independently and keep all activating input outside the conversation
until publication completes. Startup currently refuses forms, helper launch,
actor attachment and actor request/receive/await continuations at their typed
continuation boundaries. Filesystem operations use the actual granted handlers;
the current fresh-child composer shares its workspace-rooted filesystem handler
with root and does not establish workspace isolation. Jev uses the existing service
while the supervisor continues. Qualifying other startup effects needs execution
evidence through their actual services; effect membership alone is no support
claim. Root has no caller seed; only fresh child admission issues one, so legacy
absent-hook caller publication has no root successor to repeat it.
These startup events and host interruption exits use actor-journal version 10. Older runs remain on their
existing binaries; the new reader refuses them without mutation. This change
does not supply a migration or authorize restarting a live run.

Root startup pins the exact selected notebook public owner separately from
canonical actor admission. Carry that pin through the lifecycle claim, journal
certification and native transfer; retained earlier generations cannot substitute
for the selected surface. Absence of an unpublished canonical actor permits
following its recorded predecessor, while broken selected evidence refuses recovery.

Child capture resolves original nominal identities against local declarations
first, then acquired certified interfaces from the actor's source owner. Narrow
inherited interfaces through their issuer; copied source and rendered names do
not issue identity. Each child owns an independent capture release slot over
shared immutable projections. Allocator floors travel separately and install
no lexical roots. Selection controls default visibility while retaining the
certified implementation, instance, and family closure.

Compiler work receives its stop relationship with the affine `CompilerWorkTicket`
issued by the execution or preparation owner. The ticket opens and settles the
entire native scope; callers must not create an unrelated cancellation token.
The ticket returns the opaque settled compiler product for native publication;
close observations remain reporting data. The compiler owner aggregates nested
uncertainty before issuance, and prepared output retains its exact scope origin.
Shared preparation owns its producer independently: waiters retain close
observations without acquiring permission to interrupt that producer. Loading a
completed original also authenticates inside that scope and records a close;
it does not submit a physical compiler request. A stop
that loses the publication commit claim waits for settlement instead of revoking
completed output.

Root preparation issues one non-Clone mint before native allocation; allocating
entry points require that mint and consume it to publish the exact placement.
Adopting an already-prepared placement is a separate consuming transition. Consume
the publication record on actor handoff or confirmed native removal. Only the
consuming issuer constructs placement facts for control and supervisor messages;
sharing immutable facts does not duplicate cleanup authority. The supervisor
retains abandoned cleanup through producer death and a refused control notification.
Enqueueing a message or dropping a producer record does not confirm native cleanup.

Fork inheritance is a snapshot, not a live link to later parent turns. Keep
source seed, inherited scope, candidate commit, integration head, and recipient
acknowledgement distinct. `fork_workspace.rs` owns fork workspace preparation.
A writable project checkout does not imply an allocated bound worktree:
`currentCheckout` resolves the caller's root project or bound child checkout at
admission. Plans describe proposed work, not implemented contracts.

`request`/`typed_request` own request lifecycle. Admission, presentation, typed
reply, and recipient incorporation are separate facts; never turn failed
steering into a silent queued replacement assignment. Preserve unavailable
outcomes through `Await`/`Watch`: a wake is a reason to inspect the retained
handle, not proof of successful work or acceptance.

Retained requests preserve operation versus command completion as distinct kinds.
Only operations own mailbox activation, updates, reply claims and result admission;
queueing requires their admitted destination. Shared watches and owner observation
remain in the same registry and can retain either kind after its source releases.

Status and retained-output inspection contracts are summarized in
[inspection-contract.md](inspection-contract.md).
Wait and cancellation boundaries are summarized in
[waits-cancellation-contract.md](waits-cancellation-contract.md).

## Request update ownership

`request.rs` owns typed request lifecycle and retained observation metadata.
Its `request/updates.rs` child owns active-update presentation custody. The
Haskell facade exposes an opaque `RequestUpdate` obtained from one existing
`Response`; it does not mint request identity or carry runtime authority.

## Enforced invariants

- Update admission, presentation claim, reply settlement, and cancellation
  acknowledgement use the same request-state lock. A reply that wins before
  claim makes the update too late. A claim that wins prevents settlement.
- Only one unpresented update can be outstanding per request. Exact-incarnation
  ownership is checked at admission and observation, inside the registry.
- Claiming a delivery produces a non-cloneable `RequestUpdatePresentation`.
  Completion consumes the lease. Dropping it records `UpdateUnconfirmed`;
  caller cleanup cannot accidentally turn uncertainty into success.
- Private update states enumerate queued, presenting, presented, too late,
  not presented, and unconfirmed directly. There is no generic settled state
  capable of containing a queued observation.
- Reply and cancellation acknowledgement share the presentation fence. A
  deadline, abandonment, or cancellation request can release an owner's wait
  immediately, but cannot advance the target past input still in flight.
  Retirement ends that exact actor incarnation and remains available.
- A proven failure before submission releases the fence. An uncertain failure
  keeps it. Backend errors distinguish those cases in types; rendered text
  does not decide whether another assignment is safe.
- Update metadata shares the response's owner and forgetting boundary. There
  is no independent update registry, root store, or notification scheduler.

## Remaining structural opportunities

Presentation proof crosses a backend trust boundary: the kernel cannot establish
model-visible input insertion itself. The adapter must confirm the exact input
correlation event, never treat RPC acceptance as presentation. A provider-native
receipt type and capability negotiation could enforce more of that distinction
at the transport boundary. The current contract is process-local; restoring
updates after host loss requires provider reconciliation, not blind retry.

Progress, terminal delivery, observation, parent acceptance, and incorporation
are separate facts. Keep acceptance and incorporation in task-defined typed
values; a generic runtime status must not certify them.

Live values belong to the resident machine's custody graph, not the producing
actor's liveness. Actor retirement must release its roots without freeing code
reachable from another actor's closures, fork tips, or watch snapshots.
