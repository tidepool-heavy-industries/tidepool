# Shoal Haskell API

Use ordinary GHCi-style Haskell. Bind handles and values in one cell, then use
them in later cells. The default scope is `Tidepool.Actors.Shoal`; workspace
modules such as `Project.Work` provide project policy.

```haskell
let task = "Remove the stale path and report the focused check." :: Text
worker <- unfold (batch "cleanup" "implementation") $
  child @Text $
    coding projectHead $
      assignment "remove-stale-path" task

ready <- watch "implementation-ready" (awaitSettled worker)
pollWatch ready
```

An `unfold` is applicative. It constructs every child before submitting any
assignment, and children start after the cell commits. Combine independent
children with `<$>` and `<*>`; use a later cell for dependent work.

`child` returns a `Response result`. `responseActor` addresses its target.
`responseLaunch` is `Just` only on the request created with that launch; later
requests to the retained actor carry `Nothing`.

Every assignment has a validated `Label`, typed `input`, optional `guidance`
and `deadline`, and settlement reporting policy. Literal labels validate when
forced. Use `labelFromText` for external text. Requests notify their requesting
actor when they settle unless a watch or route registers for that response first.
Use `report = Silent` for a record actor settlement source.

`request @Report (responseActor worker) (assignment "revision" revisedTask)`
assigns more work to the retained actor. `withModel "executor"` selects a
frozen workspace alias. Use
`withModel (Literal "provider-model")` for an explicit provider name. Omitted
model and effort inherit the parent's effective selection. `withInstructions`,
`withContext`, `withLifetime`, and `withForkBudget` configure launch behavior;
they do not apply to requests sent to an existing actor.

Use `awaitResponse response` when any unavailable dependency should fail the
watch. Use `awaitSettled response` when failure belongs in the value. After a
wake, inspect the retained handle:

```haskell
state <- pollWatch ready
inspectFull (fmap settledValue state)
```

`pollResponse` distinguishes pending, cancellation pending, ready, and
unavailable responses. A wake is a reason to inspect retained handles; it does
not prove success. A typed reply is evidence of execution, not integration.
Use `responseWorktree` and repository observations to verify the
submitted commit before review or integration.

For progress, use `childWithProgress` or `requestWithProgress`, then
`pollProgress`. Progress publications are snapshots with independent cursors;
they are not terminal replies. Record actors (`R.*`) can collect progress and
settlements without model inference.

`me` is the current actor's exact address. A closure captures the `me` in scope
where it is defined; newly authored code in a child sees the child's address.
Use `sendMessage me text` only when steering the current actor is intended.

Cancellation is acknowledged by the target through its activation binding.
Stopping actors and releasing groups remain explicit supervision decisions.
Inspect failure values instead of reconstructing work blindly; earlier effects
in a rejected or interrupted cell may already have completed.
