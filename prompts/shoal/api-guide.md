# Shoal Haskell API

Use ordinary Haskell in notebook cells. One cell may contain
declarations, bindings, and expressions; declarations are mutually recursive
and visible throughout the cell. The default scope is `Tidepool.Actors.Shoal`; workspace
modules such as `Project.Work` provide project policy.

```haskell
let task = "Remove the stale path and report the focused check." :: Text
worker <- unfold (batch "cleanup" "implementation") $
  child @Text $
    coding projectHead $
      assignment "remove-stale-path" task

ready <- watch "implementation-ready" (awaitSettled worker)
ready
```

The final expression displays the watch handle. End the model turn while waiting.
Truncated displays offer `cellDisplay.more`, which reads retained output without
repeating the original effect.

An `unfold` is applicative. It constructs every child before submitting any
assignment, and children start after the cell commits. Combine independent
children with `<$>` and `<*>`; use a later cell for dependent work. Define a
shared plan once and use an ordinary local Haskell selector to derive short
child assignments. Commit an authored scaffold when useful; admitted live source
mechanically checkpoints eligible changes on its current branch and
seeds children from that commit. It skips hooks and checks. Intermediate red
or incomplete commits are legitimate; the parent's contract governs delivery.

`child` returns a `Response result`. `responseActor` addresses its target.
`responseAdmission` is `Just` only on the request created with that launch; later
requests to the retained actor carry `Nothing`.

Every assignment has a validated `Label`, typed `input`, optional `guidance`
and `deadline`, and settlement reporting policy. Literal labels validate when
forced. Use `labelFromText` for external text. Requests notify their requesting
actor when they settle unless a watch or route registers for that response first.
Use `report = Silent` for a record actor settlement source.

`request @Report (responseActor worker) (assignment "revision" revisedTask)`
assigns more work to the retained actor. `withModel "executor"` selects a
frozen workspace alias; pair it with an explicit `withEffort Medium` when that
is the execution policy. Use
`withModel (Literal "provider-model")` for an explicit provider name. Omitted
model and the native effort selector's effective default are used. Verify the
selected launch before assuming effort inheritance. `withInstructions`,
`withContext`, `withLifetime`, and `withForkBudget` configure launch behavior;
they do not apply to requests sent to an existing actor.

Use `awaitResponse response` when any unavailable dependency should fail the
watch. Use `awaitSettled response` when failure belongs in the value. After a
wake, inspect the retained handle:

```haskell
state <- pollWatch ready
inspectFull (fmap settledValue state)
```

`lookup` searches names and types (`::type`) and ranks callable results by their
availability in your effect row. Type search is Hoogle-like and needs a complete
type: wildcard unknown parts with `_` and qualify types as they are imported,
e.g. `:: Cmd.Command -> _` finds functions from `Cmd.Command` to anything.
`unknown` needs more type information; full signatures retain their constraints.
`doc` lists topics and `doc <topic>` returns one guide. Resource grants are
checked when an operation executes.
`status` defaults to `summary` and
also provides `detailed`, `recovery`, `lineage`, `trace`, and `bindings` views.

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
Inspect failure values before retrying. Typecheck rejection runs no effects;
runtime failure or interruption keeps the completed prefix.

Common signatures (reference, not a cell to execute):

```haskell signatures
assignment :: Label -> input -> Assignment input
coding :: WorktreeSeed -> Assignment input -> Branch CodingEffects input result
researching :: WorktreeSeed -> Assignment input -> Branch ResearchEffects input result
child :: (KnownEffects child, Subset child parent)
      => Branch child input result -> Unfold parent (Response result)
unfold :: (Member Forks effects, Member Replies effects, Member AgentInspection effects)
       => ForkGroupPath -> Unfold effects result -> Eff effects result
request :: Member Replies effects
        => AgentRef -> Assignment input -> Eff effects (Response result)
awaitSettled :: Response result -> Await (Settlement result)
watch :: Member Watches effects => WatchLabel -> Await result -> Eff effects (Watch result)
pollWatch :: Member Watches effects => Watch result -> Eff effects (WatchState result)
pollResponse :: Member Replies effects => Response result -> Eff effects (ResponseState result)
```

`Map.`, `Set.`, and `T.` provide maps, sets, and text. `Cmd.` provides command
composition; `R.` provides record actors. `traverse`, `for`, `forM`, `forM_`,
and `for_` are already in scope. Use a type application such as `request @Report`
when the result type is otherwise unconstrained; later statements in the same
cell can often determine it.
