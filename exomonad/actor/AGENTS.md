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
- Reattach to the one accumulating external interactive session; never open
  parallel coding-agent contexts for one actor. External inference must not
  hold a machine checkout.
- Worker lifecycle, wake correlation, collection, and acknowledgement are
  Rust interpreter state. Do not expose a copied worker registry as Haskell
  state or add a second result store.
- Plans describe proposed work, not implemented contracts. Verify current
  source and a production consumer before introducing orchestration vocabulary.

## Requests, forks, and observations

- `request`/`typed_request` own request lifecycle; `request/updates.rs` owns
  active-update disposition. Admission, presentation, typed reply, and recipient
  incorporation are separate facts. Never turn failed steering into a silent
  queued replacement assignment.
- Preserve unavailable outcomes through `Await`/`Watch`; a wake is a reason to
  inspect the retained handle, not proof of successful work or acceptance.
  Reuse the existing request/watch state rather than building another tracker.
- Fork-time inheritance is a snapshot, not a live link to subsequent parent
  turns. Keep source seed, inherited scope, candidate commit, integration head,
  and recipient acknowledgment distinguishable in receipts and observations.
- `fork_workspace.rs` owns fork workspace preparation. A writable project
  checkout does not imply an allocated bound worktree: root `projectHead` and
  child `boundHead` have different prerequisites.
- Inspect focused tests beside the owning request, workbench, or lifecycle
  module. Use `just test-lib tidepool-actor 'test(<name>)'`; compile changed
  consumers and test failure/cleanup paths, not only successful replies.
