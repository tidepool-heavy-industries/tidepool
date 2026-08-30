# tidepool-actor — actor kernel

## Charter

This crate owns exact-incarnation actor identity, lifecycle and ownership,
mailbox scheduling, actor-local execution context, provider-neutral actor
agent sessions, and the neutral actor event vocabulary.

It does not own machine sessions or continuations (`tidepool-runtime` and
`tidepool-codegen`), provider backends (`tidepool-agent`), concrete capability
handlers, durable JSONL mechanics (`tidepool-repr`), or observability UI.

## Invariants

- An `ActorRef` names exactly one incarnation. Never silently retarget it.
- `ActorRef` is routing identity, not live-value ownership. Successful typed
  exits live in the shared managed Haskell cell carried by `AgentRef`; the
  registry retains only terminal metadata.
- Actor events record authoritative facts but are not the mutable actor
  registry and never serialize live heap values.
- Every event has both stream ordering and per-actor ordering. Adapters must
  preserve source ordering without inventing runtime facts.
- One actor admits at most one active turn of any kind. An admitted agent
  session spans all of its provider rounds, fenced-Haskell execution, retries,
  and corrective rounds; those are not separately admitted actor turns.
- Mailbox envelopes own the existing `RootCustody` token. Never serialize a
  live value or add a second root registry for actor messaging.
- `CallTicket`, `WaitTicket`, `CallDelivery`, and mailbox deliveries are
  cleanup guards: cancellation, abandonment, and actor exit must settle once
  and release every kernel-owned root.
- A synchronous caller remains non-reentrant until its ticket consumes the
  retained reply or failure. A dequeued handler holds the target's ordinary
  actor turn lease.
- Same-machine delivery is checked from the actor incarnation's `SessionId`
  before mailbox acceptance.
- The registry owns each incarnation's session, resource scope, and lexical
  scope. Mount callers supply an `ActorRef`; the admitted `TurnLease` carries
  the matching immutable context and principal.
- Actor-authored compilation binds a `SessionCompileView` through
  `ActorSessionContext::compile_view`. Do not pass an ambient session view or
  caller-authored import list around that exact-source membrane.
- The registry also retains exactly one accumulating `ActorAgentSession`
  state per incarnation. Reattachment shares that transcript; it never opens
  a parallel model context for the same actor.
- Mount fenced Haskell through the existing `AdmittedAgentSession` lease. Do
  not acquire a nested `Haskell` turn or release admission between a provider
  response and execution of its blocks.
- Provider inference never owns a resident-machine checkout. The actor
  workbench acquires and settles the machine around one fenced Haskell segment
  while the enclosing agent-session admission remains held.
- Typed model completion uses `Tidepool.Deliberation.complete`. GHC fixes its
  private `Complete result` row entry to the obligation type; Rust recognizes the
  private constructor nominally and rehomes the live payload into the actor's
  durable resource realm before closing the fragment realm.
- Exit events record whether the owner was already observing that exact exit
  through a call or wait at the terminal transition; advisory code consumes
  that fact instead of racing a later registry lookup.
- Runtime-authored context enters an accumulating agent transcript only at a
  legal provider boundary and uses its actual role.
- Model response parsing is shared from `tidepool-model-output`; actor policy
  must not grow a second fenced-block parser.
- Use the existing machine-session checkout and continuation registries; do
  not duplicate them here.
