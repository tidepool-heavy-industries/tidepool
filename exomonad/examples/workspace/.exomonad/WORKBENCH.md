# Compose engineering work in Haskell

Use ordinary functions and `do` notation for a known sequence. Commands, typed
readiness, semantic judgments and follow-up actions compose without another
workflow interpreter. Keep original evidence handles and show a compact typed
projection. A model actor is useful for open investigation; a record actor is
useful for ongoing sources, queries or private serialized state.

## Commands and suspended continuations

`Cmd.run command` starts once and awaits terminal completion. `Cmd.await job`
continues the same Haskell computation when that job settles. Nonzero exits are
results; inspect outcome, stderr, output completeness and cleanup separately.
`Cmd.observe options job` returns bounded status normally. It never transfers
lifetime, cancels the command or reruns it.

Work starts invocation-owned. Await unfinished work before returning, or explicitly
transfer it: `Cmd.detach job` for an existing owned command, `Cmd.background command`
for an actor-owned start with completion notice. A returned or captured handle
alone does not extend lifetime. Scope exit cancels unfinished owned work while
retaining cleanup; borrowed observers cannot cancel another owner's resource.

`waitFor awaiting` suspends directly on `Await a` and returns
`Either WatchFailure a`. Applicative readiness preserves original typed handles.
Use a named `Watch` for inspectable subscriptions or model notification, and an
`EventSource` for ongoing delivery. Record-actor handlers remain serialized while
suspended. Never wait for an event that requires another handler on the same
mailbox to run; start independent work and consume its event instead.

## Children and context

`unfold` and `attemptUnfold` publish an applicative child group immediately.
Every branch selects `fromCheckpoint captured` or `selected render`. Capture a
focused scaffold with `checkpoint`; its exact Haskell environment and provider
prefix are independent of the child's checkout and authority. Releasing a
checkpoint prevents future admission; admitted children keep their own leases.
Unresolved inherited context is refused before allocation.

Immediate invocation-owned children can be awaited in that same invocation.
For independent work spanning model turns, decorate each branch with
`withLifetime ActorOwned` and retain original response/progress handles. Use
`unfoldDeferred` or `attemptUnfoldDeferred` only when children need the enclosing
call's real completed result and final bindings. Deferred branches require an
explicit persistent lifetime and the admission invocation must return before
children start. Never await deferred children before that return.

`request` submits a follow-up typed assignment to an existing actor;
`requestWithProgress` also returns its typed progress stream. Direct requests
are invocation-owned even on persistent actors: await them or inspect a successful
`detachRequest` receipt before returning. Project helpers intentionally handing
back unfinished work retain that transfer in `RequestHandoff`; refusal leaves
normal scoped cleanup. A busy actor queues
a new request. `updateRequest` targets an owned active request; admission and
presentation do not prove incorporation. Questions remain progress while the
original delivery is pending. Avoid circular waits on busy actors.

## Shared compositions and project policy

The pinned package's `Exomonad.Contrib` modules supply Types, Actors, Routing,
ReviewFlow, Merge, CheckResults, CheckPlan, PrepareContinue, RetainedEvidence and
Check.Cargo. They compose existing command, readiness, actor and worktree owners.
`Project.Work` owns task construction, instructions, model placement and direct
review requests; `Project.ReviewPolicy` owns review thresholds and semantic choices.
Configure their imports rather than adding another registry or universal plan DSL.

`unfoldWorkBatch` retains heterogeneous products of original typed handles and
maps results into an authored event sum. `unfoldWork` is the homogeneous-list
convenience. Its collector keeps progress, questions, terminal results and delivery
receipts. Optional observer failure cannot invalidate primary collection.
`acknowledgeWork` records inspected publications; acknowledgment is distinct from
incorporation and executed checks.

A `Candidate`'s `reportedChecks` are authored claims. Counted check evidence
retains original commands, source and terminal receipts. `ReviewedCheckpoint`
retains the original exact reviewed proof; a new integration head needs its own
executed `IntegrationCheck`. Reported delivery, review, integration and resource
release are separate facts. Unknown output, zero test matches and source mismatch
stop for the owner. ReviewFlow preserves those distinctions through bounded repair.

Use [RECURSIVE-WORK.md](RECURSIVE-WORK.md) for scaffold, ready batches and checked
integration. Load the boundary skill for commands, forks, routing, review or
cleanup; use targeted `lookup` for missing signatures. Registering an example
proves neither that the current workspace compiled nor that live acceptance passed.
