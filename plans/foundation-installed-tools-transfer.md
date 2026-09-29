# Installed request and short transaction transfer review

Read-only review of `3ed5ff603` (actor short admission) and `591d3c04c`
(installed tool lease) on 2026-09-28. This note separates behavior already
implemented from the request-owner adapter still being joined. No build or
runtime test was run for this review.

The observations below describe that historical revision. The joined embedded
adapter now supplies production request-snapshot and Store-admission consumers;
see `engine-harness-m1-adapter-handoff.md`. The canonical Codex HTTP endpoint
limitation below still applies: the adapter does not establish model-request-time
handler pinning for Codex. Admission leases must remain short; retirement still
waits for them before computing its shutdown budget.

## Implemented lifetime boundaries

- `MailboxAdmission::transaction` and `close` use the same mutex. Cooperative
  retirement closes admission and waits for accepted leases before terminal
  cleanup; replacement enqueues its fence under that mutex, then waits before
  transferring actor state. `wait_transactions` registers its `Notify` waiter
  before reading the count, so the final drop cannot be missed. See
  `exomonad/actor/src/kernel.rs` and `exomonad/actor/src/local_actor.rs`.
- An `ActorWorkbenchInvocation` can own an `InstalledToolLease` in the actor
  mailbox. Once accepted, dropping the HTTP caller drops its reply waiter but
  does not remove the mailbox's lease or turn a pending actor result into a
  confirmed cancellation. The lease retains the compiled handler root until
  the accepted invocation is settled or discarded by actor cleanup.

## Integration dependencies

- `snapshot_for_request` currently has no production caller. The only calls
  in this revision are the before/after reload test in
  `bridge/facade/src/actor_host/agent_spec_tests.rs`. Direct policy dispatch
  forwards `issued_tools: None`, and `ResidentKernelBehavior::workbench` then
  reads the actor's current installation when its queued invocation executes.
  The model request owner must pin the installation when issuing the request,
  before any later reload; dispatch-time selection is too late.
- The canonical Codex HTTP service does not hold that installed state. In
  `bridge/facade/src/actor_host/hosted_retirement.rs`, `start_endpoint`
  rebuilds `ResidentInteractivePolicy::local_with_tools` from declarations.
  That constructor has an empty `InstalledToolsState`; the HTTP handler in
  `bridge/facade/src/host_dynamic_tools.rs` dispatches through this policy.
  Pinning `LocalResidentInstallation.policy` alone therefore does not pin
  callbacks through the canonical service. The request-owner adapter must
  carry the exact issued lease through this transport boundary. Sequential
  Codex callbacks are not evidence of model-request-time pinning.
- `LocalActorRef::admit_transaction` currently has no production Store or
  binding caller; its only use is the local actor regression. The actor fence
  is ready, but a short host transaction is not protected until its owning
  Store/binding call acquires and drops the lease. It must drop before awaiting
  actor work. The wait is unbounded and occurs before the shutdown budget is
  computed, so a leaked or long-held lease can delay cooperative retirement;
  this is a remaining contract limit, not an observed production hang.

## Source and handler snapshot semantics

`reload_agent_spec` publishes the newly compiled source revision before
preparing a replacement handler. Its own contract deliberately keeps the old
handler serving if spec compilation fails, while later cells can import the
new modules to repair the spec. A request issued during preparation can
therefore pin the valid intermediate pair of new source and old handler once
request snapshots are wired. This is intentional publication behavior, not
an atomic source-and-handler swap. The existing test covers snapshots before
and after reload; it does not cover issuance during preparation. The request
owner should preserve and test this intermediate state rather than silently
changing reload behavior.
