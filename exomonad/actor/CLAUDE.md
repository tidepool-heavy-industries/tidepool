# Actor kernel

This crate owns exact-incarnation actor identity, lifecycle and ownership,
mailboxes, actor-local execution context, and provider-neutral actor sessions.
Machine sessions, JIT continuations, providers, concrete handlers, and
observability interfaces stay in their owning crates. Ractor is the scheduler;
`LocalActor` adds Tidepool identity, retained exits, live-value custody, and
call ancestry. Do not add a second scheduler or result/root registry.

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

Installation-issued effect rows carry their exact nominal imports through
templates, actor compile views, and inspection. Keep that source fragment coupled;
authored type text does not issue imports or additional effect authority.

Compiler work receives its stop relationship with the affine `CompilerWorkTicket`
issued by the execution or preparation owner. The ticket opens and settles the
entire native scope; callers must not create an unrelated cancellation token.
Shared preparation owns its producer independently: waiters retain close
observations without acquiring permission to interrupt that producer. Loading a
completed original also authenticates inside that scope and records a close;
it does not submit a physical compiler request. A stop
that loses the publication commit claim waits for settlement instead of revoking
completed output.

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
