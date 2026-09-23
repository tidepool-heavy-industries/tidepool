# Exomonad API

The default notebook scope is `Tidepool.Actors.Exomonad`, plus configured workspace
modules. `Cmd`, `J`, `R`, `T`, `Map`, and `Set` name commands, Jev, record actors,
text, maps, and sets. `bash`, `withMemory`, `MiB`, `GiB`, `:=`, and `:&` are in scope.
Cells enable the usual extensions, including `TypeApplications`, `DataKinds`,
`OverloadedLabels`, and `OverloadedRecordDot`; standalone modules declare theirs.

## Delegate and inspect

```haskell
let task = "Remove the stale path and report the focused check." :: Text
(worker, ready) <- spawnWatched "implementation-ready" (batch "cleanup" "implementation") $
  child @Text $
    coding projectHead $
      assignment [label|remove-stale-path|] task
ready
```

`spawnWatched` composes `unfold` and `watch (awaitSettled response)`. Use `<$>` and
`<*>` for independent children in one `unfold`; use a later cell for dependent
work. After wake:

```haskell
state <- pollWatch ready
inspectFull (fmap settledValue state)
```

`awaitSettled` preserves unavailable outcomes as values; `awaitResponse` fails
the watch on an unavailable dependency. `pollResponse` distinguishes pending,
cancellation pending, ready, and unavailable. Progress uses `childWithProgress`
and `pollProgress`; snapshots are not replies. Record actors (`R.*`) collect
and route events without model inference.

Seed children from your executing checkout with `currentCheckout`: the root
project checkout or a child's bound checkout. Use `projectHead` to select the
project source explicitly, or `atRef` for an explicit commit. Live-source
admission checkpoints eligible edits on the source branch without hooks or
checks; inspect omission/fallback receipts.
Commit useful units without mistaking checkpoints for accepted delivery.

`withModel "executor"` selects a workspace alias; `withModel (Literal "provider-model")`
selects an explicit model. `withEffort Medium` sets effort. Omitted settings follow
the native launch selector's defaults; `previewBranch` shows resolved policy.
`withContext (selected render)` selects fresh context. Launch options do not
modify retained actors. Use `[label|orbit-motif|]` for compile-checked static
assignment labels. Use `labelFromText` for dynamic labels and handle its `Either`.

The activation supplies typed `sessionInput` and its reply declaration; use
`inspectFull sessionInput` only for omitted detail. Roots outside an assignment
have no reply binding. `request @Report (responseActor worker) (assignment [label|revision|] input)`
assigns follow-up work. `pollRequestUpdate` inspects an accepted update handle.
Requests notify their owner unless a watch/route takes over; record actor
settlement sources require `report = Silent`.

Inherited bindings keep their values and handles. You may inspect another
actor's response, command output, progress, or worktree state. Create your own
watch to receive a pending response; do not drain or unsubscribe another actor's
listener. Reads do not transfer control: ask the owner to cancel, publish,
release, write stdin, or mutate its worktree. A Ready notice means inspect the
handle; release may make it unavailable before your read. A value already
extracted from a successful read remains yours after release.

## Review and integrate

A reply identifies a candidate, not an integrated result. Recover its exact
commit through `responseWorktree`; inspect it from your repository view with
`git show`/`git diff`, not the child's live working directory. Commission review
at that revision; give the reviewer the contract and implementer reference for
repairs. Reviewers running checks need coding authority. Integrate the accepted
revision with `tryMerge` for a managed target or ordinary Git in your checkout;
verify that resulting revision before delivery. Load `exomonad-review` for the
compiled project review/repair recipe and `exomonad-unfold` for submission evidence.

## Core signatures

```haskell signatures
assignment :: Label -> input -> Assignment input
currentCheckout :: WorktreeSeed
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

## Compose commands and judgment

This task-local helper batches two semantic questions over successful complete
command output. Exit failure is handled in code; Jev unavailability stays explicit.

```haskell
import Tidepool.Effects.Core (Jev, Commands)
judgeChanges :: (Member Jev effects, Member Commands effects) => Text -> Eff effects Text
judgeChanges task = do
  result <- Cmd.run (Cmd.argv ["git", "diff", "--stat"])
  case Cmd.stdout result of
    Left issue -> pure ("Cannot read changes: " <> T.pack (show issue))
    Right changes -> do
      answer <- J.ask (J.rawState (object ["task" .= task, "changes" .= changes]))
        (#relevant := J.noul "Do these changed paths plausibly relate to the task?"
          :& #enough := J.noul "Does this diff stat suffice to establish task completion?")
      pure (either (\err -> "Jev unavailable: " <> T.pack (show err))
        (\a -> T.pack (show (a.relevant.yes, a.enough.yes))) answer)
```

`Cmd.stdout` returns complete successful stdout or an explicit issue. For failed
commands, inspect outcome and stderr. Bound command results show a job, exit
status and stream-byte summary; the full observation remains available through
the binding. `Cmd.quiet` suppresses routine display for unbound commands.
`reflect n` returns your latest `n` conversation turns including the active turn,
oldest first; reuse that evidence across questions. `me` is lexically captured; `parentAgent`
is the spawning actor or `Nothing` for a root.

## Discover missing information

Start from this guide and the assignment; no startup inventory ritual.
`lookup` accepts names, modules, and Hoogle-like types such as `:: Cmd.Command -> _`.
It may attach up to four Jev-selected related declarations or alternatives;
original failures remain failures. Look up qualified names explicitly for more detail.
`polymorphic` is usable with call-site constraints; `unknown` needs more type
information. Use `doc topics` for guides and workspace modules; inspect their
exports/source where needed. `status` offers `summary`, `detailed`, `recovery`,
`lineage`, `trace`, and `bindings` for runtime uncertainty.

Load the relevant skill at an unfamiliar boundary: `exomonad-command` for retained
output/stdin/completion; `exomonad-workbench` for parser/type/display recovery;
`exomonad-jev` for typed judgment composition; `exomonad-unfold` for delegation/source;
`exomonad-coordinate`, `exomonad-fork`, `exomonad-orchestrate`, and `exomonad-review` for project
coordination; `exomonad-define-actors` for custom event handlers; `exomonad-cleanup` for
retirement; `exomonad-agent-spec` for typed tools and spec reload. Use the corresponding
`doc` topic as fallback. Distinguish shipped APIs from project/example-only helpers.
