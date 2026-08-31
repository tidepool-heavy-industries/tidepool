# Actor kernel

This crate owns exact-incarnation actor identity, lifecycle and ownership,
mailboxes, actor-local execution context, provider-neutral actor sessions, and
neutral actor events. Machine sessions, JIT continuations, providers, concrete
handlers, and observability UIs stay in their owning crates.

- An `ActorRef` always names one incarnation; never silently retarget it.
  Identity is not live-value ownership.
- Each actor admits at most one active turn of any kind. One admitted agent
  session includes all provider rounds, fenced-Haskell execution, retries, and
  corrective rounds.
- Mailbox envelopes and typed terminal values use the existing managed Haskell
  cell/root-custody machinery. Do not serialize live values or add another root
  registry.
- Tickets and deliveries are cleanup guards. Cancellation, abandonment, and
  exit settle once and release every kernel-owned root.
- Delivery must validate exact incarnation, machine session, and authority
  before mailbox acceptance. A synchronous caller remains non-reentrant until
  its ticket settles.
- Compile actor-authored code through the actor's exact `SessionCompileView`.
  Do not pass ambient scope ancestry or caller-authored import lists through
  this membrane.
- Reattach to the one accumulating `ActorAgentSession`; never open parallel
  model contexts for one actor. Provider inference must not hold a machine
  checkout.
- Parse model output through `tidepool-model-output`; do not grow an actor-local
  fenced-block parser.
- Treat `plans/actor-model/` as the current design authority until implemented
  contracts are promoted here. Do not implement speculative plan vocabulary
  ahead of its production consumer.
