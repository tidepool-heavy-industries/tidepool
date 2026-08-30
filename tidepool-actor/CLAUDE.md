# tidepool-actor — actor kernel

## Charter

This crate owns exact-incarnation actor identity, lifecycle and ownership,
mailbox scheduling, actor-local execution context, and the neutral actor event
vocabulary.

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
- Use the existing machine-session checkout and continuation registries; do
  not duplicate them here.
