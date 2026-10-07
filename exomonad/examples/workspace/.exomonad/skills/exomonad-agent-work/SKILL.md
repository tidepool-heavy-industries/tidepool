---
name: exomonad-agent-work
description: Compose hosted agents with typed requests, shared or forked workspaces, and applicative waits. Load when creating agents or coordinating concurrent agent work.
---

Hosted agents are ordinary typed resources. Create one with
`spawnSubagent context workspace (defaultSpawnOptions actualSpec)`. Context is
`ForkCtx checkpoint` for an explicit captured conversation or `FreshCtx prompt`
for an explicit prompt. The workspace is `SameDir`, an opaque run-granted
`ExistingWorkspace`, or `ForkWorktree seed` for a selected committed source.
Spawn options contain the actual typed `AgentSpec`, model and effort, instructions,
optional ordinary text label, lifetime, and limits. A label is descriptive; it
does not identify an actor, workspace, or group.

A successful spawn returns an idle `AgentRef`. Spawn does not start inference.
The first typed request or a human message activates it. Here `worker` is an
`AgentRef`, the `AgentSpec` installs the child's tools and effects, `@Text`
selects the reply type, and `input :: Text` is raw request input:

```haskell
Right pending <- request @Text worker input defaultRequestOptions
Right reply <- await (result pending)
display reply
```

`requestWithProgress` uses independent progress and reply types when intermediate
updates matter. A request carries raw typed input and request options; admission,
execution, and authored result errors remain distinguishable. The request handle
is its singular control identity. Spawn defaults to the parent actor's ownership;
a request defaults to its caller's ownership. Returning a handle does not transfer
either resource. Waiting cancellation does not cancel the request, and request
cancellation does not retire its target actor.

`response pending` retains the full `ResponseResult`, including its execution
receipt and worktree evidence; `settledResponse` also keeps `ResponseFailure` as
data. `result` and `settlement` project the typed reply, with only `settlement`
preserving `ResponseFailure`. `await` is the observation path, and `AwaitError`
remains a separate failure of observation. Compose independent waits
applicatively or traverse a collection; use `eitherOf` when the first terminal
branch should decide, including a failure. An all-branches wait requires each
branch to succeed. The watch owner retains immutable readiness decisions, so a
losing branch cannot later change a selected result. Progress is an independent
typed observation and does not replace the final reply.

Use `withScope` for a runtime-owned delimiter. The callback receives its opaque
scope, and each resource joins only when its options explicitly use
`InScope scope`. Default ownership is unchanged. Scope results preserve the
callback outcome and cleanup outcome separately; return live values into parent
custody before body resources retire. Nested scopes and incomplete cleanup use
the runtime's retained finalization path.

`SameDir` shares the actual writable files, index, and HEAD. It does not select
compiled source or install tools. A forked worktree selects one committed seed;
source selection, shared mutable files, and installed tools remain separate
choices. An attachment has its own actor lifetime, and retiring it does not
retire sibling actors or delete the directory.

For Git project delivery, load `exomonad-project-work` if its review and
integration method fits the task. For persistent state and event routing, load
`exomonad-define-actors`. General exploration needs no project workflow or actor
hierarchy.
