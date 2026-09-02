# Actor kernel

This crate owns exact-incarnation actor identity, lifecycle and ownership,
mailboxes, actor-local execution context, and provider-neutral actor sessions.
Machine sessions, JIT continuations, providers, concrete
handlers, and observability UIs stay in their owning crates.

- An `ActorRef` always names one incarnation; never silently retarget it.
  Identity is not live-value ownership.
- Each actor admits at most one active turn of any kind. One admitted agent
  session includes all provider rounds, fenced-Haskell execution, retries, and
  corrective rounds.
- Mailbox envelopes and typed terminal values use the existing managed Haskell
  cell/root-custody machinery. Do not serialize live values or add another root
  registry.
- Ractor is the only actor scheduler. `LocalActor` adds Tidepool identity,
  retained exits, live-value custody, and call ancestry; do not recreate a
  runnable registry, parked-obligation table, or host task scheduler beside it.
- Messages own their live-value custody. Cancellation, abandonment, and exit
  must settle once and release every kernel-owned root.
- Delivery validates exact incarnation and machine session before mailbox
  acceptance. A synchronous caller remains non-reentrant until its RPC
  resolves or fails.
- Compile actor-authored code through the actor's exact `SessionCompileView`.
  Do not pass ambient scope ancestry or caller-authored import lists through
  this membrane.
- Reattach to the one accumulating `ActorAgentSession`; never open parallel
  model contexts for one actor. Provider inference must not hold a machine
  checkout.
- Parse model output through `tidepool-model-output`; do not grow an actor-local
  fenced-block parser.
- Worker lifecycle, wake correlation, collection, and acknowledgement are
  Rust interpreter state. Do not expose a copied worker registry as Haskell
  state or add a second result store.
- Treat `plans/actor-model/` as the current design authority for unlanded
  features. Do not implement speculative vocabulary ahead of a production
  consumer.
