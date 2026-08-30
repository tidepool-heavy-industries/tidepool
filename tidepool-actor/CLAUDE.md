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
- Actor events record authoritative facts but are not the mutable actor
  registry and never serialize live heap values.
- Every event has both stream ordering and per-actor ordering. Adapters must
  preserve source ordering without inventing runtime facts.
- One actor admits at most one active turn of any kind.
- Mailbox envelopes own the existing `RootCustody` token. Never serialize a
  live value or add a second root registry for actor messaging.
- `CallTicket`, `CallDelivery`, and mailbox deliveries are cleanup guards:
  cancellation, abandonment, and actor exit must settle once and release every
  kernel-owned root.
- A synchronous caller remains non-reentrant until its ticket consumes the
  retained reply or failure. A dequeued handler holds the target's ordinary
  actor turn lease.
- Same-machine delivery is checked from the actor incarnation's `SessionId`
  before mailbox acceptance.
- Runtime-authored context enters an accumulating agent transcript only at a
  legal provider boundary and uses its actual role.
- Model response parsing is shared from `tidepool-model-output`; actor policy
  must not grow a second fenced-block parser.
- Use the existing machine-session checkout and continuation registries; do
  not duplicate them here.
