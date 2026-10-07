# Typed agent work

Create one idle hosted child with `spawnSubagent context workspace
(defaultSpawnOptions actualSpec)`. The child receives the actual typed
`AgentSpec`; a successful spawn returns an `AgentRef` after its tools, workspace,
captured context, and provider attachment are ready. It does not start inference.
The first typed request or a human message activates it.

Choose `ForkCtx checkpoint` to reuse a retained context snapshot or `FreshCtx
prompt` to start from an explicit prompt. Fresh context does not include ambient
lexical bindings. A closure may still carry dependencies explicitly. Choose
`SameDir` to share the actual writable files, index, and HEAD; use an opaque
run-issued handle to reuse another registered workspace, or `ForkWorktree seed`
for a selected committed source. These choices do not replace compiled source or
install tools. Workspace names and actor identity are independent of the optional
ordinary text label.

When an `AgentRef` is available, a typed request activates it with raw input and
request options. Here the `AgentSpec` installs the child's tools and effects;
`@Text` selects the reply type, and `input :: Text` is the raw request input:

```haskell
Right pending <- request @Text worker input defaultRequestOptions
Right reply <- await (result pending)
display reply
```

`requestWithProgress` additionally returns an independent progress handle.
Keep that handle when intermediate updates matter; it is not the final result.
Spawn defaults to parent actor ownership and a request defaults to caller actor
ownership. Returning a handle does not transfer ownership. Waiting cancellation
does not cancel the request, and request cancellation does not retire its agent.

`result pending` is an `Await` value and `await` is the single observation path.
Applicative composition waits for required branches; the first terminal choice
also includes failures. The watch owner retains decisions, so release of a losing
response cannot change a winner. Settlement projections expose failures as
values when a collector needs each outcome. A wait does not poll its branches
again after the watch owner has decided.

Use `withScope` when a set of resources needs one runtime-owned delimiter. The
callback receives an opaque scope, and each resource joins it only if its own
options explicitly use `InScope scope`. Defaults do not change. Scope outcomes
preserve the callback result and cleanup result separately, including partial
cleanup and cancellation. Return live values into parent custody before body
resources retire. Nested scope cleanup uses the retained finalization path.

For Git delivery, load `exomonad-project-work` when its review and integration
method fits the task. A useful request states the objective, source revision,
owned paths, dependencies, acceptance, and escalation conditions. General
exploration and actor programming need no project role, group name, or batch
framework. Load `exomonad-define-actors` for persistent typed state and event
routing.

skill: exomonad-agent-work
