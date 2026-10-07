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

Commands start invocation-owned. Await unfinished work before returning, or explicitly
transfer it: `Cmd.detach job` for an existing owned command, `Cmd.background command`
for an actor-owned start with completion notice. A returned or captured handle
alone does not extend lifetime. Scope exit cancels unfinished owned work while
retaining cleanup; borrowed observers cannot cancel another owner's resource.

Compose `result request` projections as `Await`; `await` observes one `Await a`
and returns `Either AwaitError a`. Await values compose applicatively, and
`traverse` handles a collection without a separate batch primitive. Use
`eitherOf` when the first terminal branch should decide, including failure.
`R.start` explicitly creates a persistent record service with actor lifetime;
returning from its creator's invocation does not cancel that service. Handlers
without a hosted invocation use actor ownership for their work and remain
serialized while suspended. Never wait for an event that requires another
handler on the same mailbox to run; start independent work and consume its event
instead.

## Children and context

`spawnSubagent context workspace (defaultSpawnOptions actualSpec)` creates one
idle agent and returns `Either SpawnError AgentRef`; spawning does not start
inference. A partial failure retains its cleanup handles. Capture the intended
declarations, values, and context with `checkpoint` before choosing
`ForkCtx captured`; `FreshCtx prompt` supplies an independent explicit prompt.
Choose `SameDir` to share the actual writable files, index, and HEAD; choose an
opaque `ExistingWorkspace` or `ForkWorktree seed` when the granted directory or
selected committed source should differ. Workspace choice does not install tools
or select compiled source.

Spawn and request ownership default to the parent actor's custody. A typed
`request @Answer agent rawInput defaultRequestOptions` activates the agent and
returns `Either RequestError (Request Answer)`; a refusal keeps the already
spawned agent available for retry or retirement. `requestWithProgress` also
returns an independent typed progress handle when updates matter. The request handle is its control identity.
Use `result request` as an `Await` and `await` to observe it. `withScope` creates
a runtime-owned delimiter; resources join only when their own options explicitly
use `InScope scope`. Request, waiting, actor, and scope cleanup remain separate
operations. Admission and presentation do not prove incorporation. Questions
remain progress while the original delivery is pending; avoid circular waits on
busy actors.

## Shared compositions and project policy

The pinned `Project` and `Exomonad.Contrib` modules are optional authored
compositions over the core agent, request, actor, command, readiness, and
worktree APIs. Create agents explicitly with their actual spec and workspace,
then submit typed requests. A project may add event-source routing when several
already-created requests need ongoing progress observation; that collector does
not create agents or define a universal plan language. Configure the package
imports for the composition you use.

Keep publication, acknowledgment, incorporation, and executed checks as distinct
facts. A `Candidate`'s `reportedChecks` are authored claims. Counted check
evidence retains original commands, source, and terminal receipts.
`ReviewedCheckpoint` retains the original exact reviewed proof; a new integration
head needs its own executed `IntegrationCheck`. Unknown output, zero test matches,
and source mismatch stop for the owner. ReviewFlow preserves those distinctions
through bounded repair.

Use [RECURSIVE-WORK.md](RECURSIVE-WORK.md) for optional project delivery,
independent implementation, exact-candidate review, and checked integration.
Load the boundary skill for commands, agent requests, routing, review, or cleanup;
use targeted `lookup` for missing signatures. Registering an example proves
neither that the current workspace compiled nor that live acceptance passed.
