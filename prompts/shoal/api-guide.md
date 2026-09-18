# Shoal Haskell API

Use ordinary Haskell in notebook cells. One cell may contain
declarations, bindings, and expressions; declarations are mutually recursive
and visible throughout the cell. The default scope is `Tidepool.Actors.Shoal`; workspace
modules such as `Project.Work` provide project policy.

```haskell
let task = "Remove the stale path and report the focused check." :: Text
(worker, ready) <- spawnWatched "implementation-ready" (batch "cleanup" "implementation") $
  child @Text $
    coding projectHead $
      assignment "remove-stale-path" task
ready
```

`spawnWatched label path plan` is `unfold path plan` followed by
`watch label (awaitSettled response)`; use `unfold` directly to admit several
children at once. The final expression displays the watch handle. End the model
turn while waiting.
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
`polymorphic` fits your row with a constraint the call site decides, so it is
usable as written; `unknown` needs more type information. Full signatures retain
their constraints.
`doc` lists topics and the workspace skills beside them; `doc <topic>` returns
one guide. Load the skill where one exists and use the topic as the fallback:
each topic's last line names its skill. Resource grants are checked when an
operation executes. Load `shoal-jev` for the judgment-model effect available in
every cell.
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

`reflect n` returns your own last `n` completed conversation turns, oldest
first, with their messages, tool calls, and tool results. Bind it once and reuse
the value as the context argument for the questions that follow instead of
restating your history by hand. `doc reflect` has the worked example.

`me` is the current actor's exact address. A closure captures the `me` in scope
where it is defined; newly authored code in a child sees the child's address.
Use `sendMessage me text` only when steering the current actor is intended.

Cancellation is acknowledged by the target through its activation binding.
Stopping actors and releasing groups remain explicit supervision decisions.
Inspect failure values before retrying. Typecheck rejection runs no effects;
runtime failure or interruption keeps the completed prefix.

Delegation at a glance:

- start work: `spawnWatched` or `unfold` with `child`; follow-up work for a
  retained actor: `request`.
- steer: `sendMessage` delivers a note; `updateRequest` clarifies the active
  request; `cancelRequest` withdraws a request (a queued one never starts) and
  keeps the actor; `stopAgent` retires the actor and waits for its resources
  to release (`StoppedNow`), or says what stays retained.
- inspect: `listAgents`, `lookupAgent`, `findAgentsByLabel`, `observeForkGroup`,
  `pollResponse`.
- wait: `watch` with `awaitSettled` (combine with `<*>` for all-of) or
  `awaitAnySettled` (wake when any settles).
- launch options: `withContext (selected render)` starts a fresh conversation
  with only the rendered input; `withModel`, `withEffort`; `previewBranch`
  shows the resolved launch before admission.
- finish: `tryMerge` integrates a submission; `planCleanup` and
  `executeCleanup` retire a group.

For repository delegation use these operations rather than generic agent tools:
they give typed results, worktree custody, and wakes.

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
spawnWatched :: WatchLabel -> ForkGroupPath -> Unfold effects (Response result)
             -> Eff effects (Response result, Watch (Settlement result))
awaitSettled :: Response result -> Await (Settlement result)
awaitAnySettled :: [Response result] -> Await [Maybe (Settlement result)]
watch :: Member Watches effects => WatchLabel -> Await result -> Eff effects (Watch result)
pollWatch :: Member Watches effects => Watch result -> Eff effects (WatchState result)
pollResponse :: Member Replies effects => Response result -> Eff effects (ResponseState result)
```

`Map.`, `Set.`, and `T.` provide maps, sets, and text. `Cmd.` provides command
composition; `R.` provides record actors; `J.` provides Jev. `bash`, `withMemory`,
`MiB`, `GiB`, and the packet operators `:=`, `:&`, and `Nil` are in scope.
`traverse`, `for`, `forM`, `forM_`,
and `for_` are already in scope. Use a type application such as `request @Report`
when the result type is otherwise unconstrained; later statements in the same
cell can often determine it.

Reusable effectful helpers should declare their `Member` constraints, as in
the example below. The workbench may be unable to retain an inferred polymorphic
effect row: an ambiguous `effects0` or `parent0` is a reason to name that row in
a signature, not to duplicate the helper's body at every call site. For a fork
helper, start from the `unfold`/`child` signatures above and the effects it uses.

## Find a capability when you need it

| Intended operation | Starting surface | Load for the next step |
| --- | --- | --- |
| Feed command evidence into a program | `Cmd.run`, `Cmd.quiet`, `Cmd.stdout` | `shoal-command`: stderr, complete output, paging, completion events |
| Judge evidence and select an action | `J.ask1`, `J.choice`, `J.accept`, `J.handle` | `shoal-jev`: packets, pools, speculative questions |
| React without another model turn | Record actors through `R.*` | `shoal-define-actors`: state, installed event sources, handlers |
| Delegate and collect typed replies | `spawnWatched`, `unfold`, `watch` | `shoal-unfold`; `shoal-cleanup` when retiring the work |
| Reuse a project-authored function | `doc topics`, module/name lookup | The installed module's exports and worked example |
| Inspect a partial failure or old handle | The cell receipt, `status` recovery view | `doc recovery`, `shoal-workbench` |

Load more detail when the intended operation needs it. For example, moving from
a short foreground command to an unattended long check is the point to read
the command-completion pattern. A new effect such as conversation reflection
needs an installed signature; a plan or worker report does not make it callable.

## Commands, judgments, and prepared follow-ups

Use `Cmd.run` when command results should feed code. `Cmd.stdout result` returns
complete successful stdout or an explicit issue; it does not silently truncate
or turn a nonzero exit into success. Use `Cmd.quiet` to retain data without routine
command display. For failed commands, inspect the outcome and stderr through the
output API; load `shoal-command` for paging and completeness contracts.

This helper reads recent commit subjects, asks which might explain a task, and
fetches the selected commit's stat without another model turn. Selection is a
reading aid, not a conclusion about the code. The task is an argument so intent
travels with the evidence. Every command argument comes from code or Git output.

```haskell
{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
import Tidepool.Effects.Core (Jev, Commands)
inspectRecentChanges :: (Member Jev effects, Member Commands effects) => Text -> Eff effects Text
inspectRecentChanges task = do
      listed <- Cmd.quiet (Cmd.run (Cmd.argv ["git", "log", "-8", "--format=%H%x09%s"]))
      case Cmd.stdout listed of
        Left issue -> pure ("Cannot read history: " <> T.pack (show issue))
        Right history -> do
          let rows = map (T.breakOn "\t") (T.lines history)
              offers = J.alt #unresolved "No listed subject explains the task, or subjects lack the deciding detail" ()
                J..| J.many [(oid, String (T.drop 1 subject), Cmd.argv ["git", "show", "--stat", "--oneline", oid]) | (oid, subject) <- rows]
          answer <- J.ask1 (J.state (object ["task" .= (task :: Text), "recent_history" .= history]))
            (J.choice "Which listed commit subject identifies a change worth inspecting for `task`?" offers)
          case answer of
            Left err -> pure ("Jev unavailable: " <> T.pack (show err))
            Right a -> case J.accept J.routing a of
              Left _ -> pure ("Needs inspection: " <> J.explain J.routing a)
              Right selection -> J.handle selection
                (#unresolved (\() -> pure "The listed subjects do not resolve what to read; inspect broader history or source.")
                  J..| J.onMany (\_ command -> do
                    result <- Cmd.quiet (Cmd.run command)
                    pure (either (\issue -> "Cannot read selected commit: " <> T.pack (show issue)) id (Cmd.stdout result))))
```

Call `inspectRecentChanges "Which recent change could explain the command output regression?"`
with your actual question. The helper is defined by the cell, not a shipped API.
Keep the returned evidence if another judgment needs it. For several independent
questions over one state, use `J.ask` with a packet and read fields from `J.answers`.
`J.accept` checks the winner's distribution; handle its selected alternative,
doubt, and transport failure separately. A confident unresolved answer remains
unresolved. Use `shoal-jev` for pools, speculative questions, and other patterns.
