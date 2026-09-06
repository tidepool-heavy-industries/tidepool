# Resident quiescence: cross-owner interface checkpoint

Inspected accepted service b6dcc03179f6688f4826f0f5b7009f2febe67a88.
This is source evidence, not executed authored cancellation proof. No actor,
runtime, custody or host lifecycle semantics were modified.

## Existing owner ordering and gaps

- resident_tools.rs::ResidentToolClient::dispatch_workbench/dispatch takes its
  shared dispatch_gate then sends Workbench/Tool and awaits a oneshot. Dropping
  that waiter releases this client gate but does not cancel actor-owned work.
  A pre-HTTP-fence admitted handler may reach this point after quiesce returns.
- local_actor.rs::handle serializes behavior invocations but has no hosted-work
  admission seal. resident_actor.rs::workbench checks standing/policy, not shutdown
  intent. RetainedActorExit::request_shutdown records intent consumed at bootstrap
  safe boundaries; it is not a general external-tool admission fence.
- LocalActorRef::shutdown queues Shutdown behind executing actor work. A completion
  callback is a separate ToolCompleted message; shutdown aborts unpublished fork
  groups, whereas successful tool_completed releases matching ready groups.
  These are different outcomes. An idle posture does not show prefix completion.
- local_actor.rs::finish_actor awaits behavior.shutdown, converts failure to Failed,
  publishes ActorTerminal, invokes stopped, then requests Ractor stop. Shutdown
  hook failure short-circuits resident realm closure. Failed is not cleanup success.
- shutdown_children times out/error-falls back to publishing requested Cancelled
  before kill(), without awaiting machine/hook/realm completion. Therefore even
  retained Cancelled is not a general cleanup receipt. abort_unpublished_groups
  also discards child shutdown results. These are source-observed failure paths,
  not a diagnosis of any historical incident.
- resident_workbench.rs::with_host_machine moves machine plus linear checkout
  into spawn_blocking; that closure settles independently of its async waiter.
  Dropping/aborting a waiter cannot prove effect termination. run_shutdown's30s
  limit is checkout ADMISSION, not total hook execution; suspended hooks fail.
- close_realm/retire_root_placement await exclusive machine checkout then clear
  realm/scope roots. runtime/session/resident.rs::close_realm reconciles parked
  roots; it does not join external processes/tasks. Success orders this cleanup
  after previous checkout, but seals neither future submissions nor all handlers.

## Proposed smallest owning API (not published or implemented)

Actor owner, not HTTP counters, should add an acknowledged mailbox barrier:

    LocalActorRef::seal_hosted_work()
      -> Result<HostedWorkSeal, KernelInvocationFailure>
    KernelMessage::SealHostedWork { reply }

HostedWorkSeal has private construction and exact ActorRef; it certifies that
this actor processed the barrier after preceding serialized Tool/Workbench work
and irreversibly denies later Tool/Workbench admission at LocalActor::handle.
Use a private admission enum in existing LocalActorState; no ID issuer, pending
call registry or per-client gate. All tool clients must encounter the owning
fence, including clones waiting on dispatch_gate. Return an explicit rejection,
not dispatch then cancel. Seal errors/timeouts retain uncertainty; awaiting through
retained handles must not silently cancel or start another retirement.

ResidentToolEndpoint needs a typed seal projection backed by that exact owner
(e.g. seal_hosted_work_boxed -> future Result<HostedWorkSeal,...>). Unsupported
implementations must return Unavailable, never default successful proof. The
existing default no-op complete callback is not a precedent for fake sealing.
Completion/reattach policy remains explicit: permit correlated completion while
sealed; reject new session/reattach that could change work ownership. A completion
may admit child work, so the eventual retirement owner must account for children.

Separately extend EXISTING retained lifecycle evidence, not ActorTerminal text,
with an exact typed ResidentCleanupOutcome (Confirmed vs Unconfirmed carrying
component failures). Produce Confirmed only after required hook/realm cleanup and
child cleanup outcomes are known. Force/timeout/panic cannot manufacture it.
LocalActorRef::shutdown_with_cleanup should return retained terminal AND this
outcome from the same owner; no second exit registry. Realm cleanup on failed
hook needs an explicit failure-preserving policy, not a short-circuit success.
This outcome covers actor/machine cleanup only, not arbitrary external handlers.

Intended host ordering: HTTP quiesce; await actor-owned hosted-work seal; reconcile
native durable completed boundaries or explicitly abort pending groups; await
owner shutdown with honest cleanup outcomes; HTTP drain/await; combine with exact
process/other resource receipts in parent's existing deployment owner. Never use
an immediate idle snapshot or HTTP completion as the actor seal. Native evidence
and effect-specific cleanup prerequisites remain independently gated.

## Acceptance before implementation can claim proof

Use actual authored Haskell endpoint + controlled owning handler, not the existing
HTTP mock: active work and queued late invocation; disconnect first waiter; seal
must await active ownership and reject late work; pending completed boundary must
have explicit release-vs-abort outcome; hook failure and child force must remain
unconfirmed; machine checkout settlement must survive waiter loss; stale/sibling
seals cannot authorize another actor. No such existing complete contract was found,
so this checkpoint adds no test that pretends to prove it. Runtime GC-root tests
and GHC-free kernel forcing tests do not substitute for authored cleanup evidence.

Independent read-only auditor retained as runtimeAudit; full runtimeAuditResult
contains exact source chain and candidate test names. HTTP implementer/reviewer
remain retained. Shared semantics require parent approval before implementation.
