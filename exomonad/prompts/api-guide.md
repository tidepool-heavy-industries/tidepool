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

`spawnWatched` composes `unfold` and `watch (awaitSettled response)`. A wave is
one `unfold` with `<$>` and `<*>` over every independent child; dependent work
waits for a later cell:

```haskell
(parser, consumer, review) <- unfold (batch "feature" "wave-1") $ (,,)
  <$> child @Text (coding currentCheckout (assignment [label|parser|] parserTask))
  <*> child @Text (coding currentCheckout (assignment [label|consumer|] consumerTask))
  <*> child @Text (researching currentCheckout (assignment [label|contract-review|] reviewTask))
```
A later wave from the same actor uses `subgroup "wave-2"`: it nests under
your own path, so you pass only the new segment, never your full path.

End the turn. Each child's settlement notice wakes you with its reply; read
the full value with `pollResponse` only when the notice's preview is absent
(a `Text` reply shows no preview) or not
enough. A `watch` joins several responses into one wake:

```haskell
settled <- watch "wave-1-settled" (awaitAnySettled [parser, consumer, review])
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

`withModel "luna"` selects a workspace alias; `withModel (Literal "provider-model")`
selects an explicit model (`luna`: cheap tier; `executor`: Sol tier). `withEffort Medium` sets effort. `previewBranch` shows resolved policy.
`withContext (selected render)` selects fresh context. Use `[label|orbit-motif|]`
for compile-checked static assignment labels; use `labelFromText` for dynamic
labels and handle its `Either`.

The activation supplies typed `sessionInput`, its reply declaration, and the
roster of siblings admitted with you; use `inspectFull sessionInput` only for
omitted detail. `respond`, `sessionReply` and `sessionInput` exist only while
a request is pending; `lookup` shows them then. A root has none of them: use
the project's task and review constructors instead of recipes written for a
child. `request @Report (responseActor worker) (assignment [label|revision|] input)`
assigns follow-up work to a retained child. `pollRequestUpdate` inspects an accepted update handle.
Requests notify their owner unless a watch/route takes over; record actor
settlement sources require `report = Silent`.

You may inspect another actor's response, command output, progress, or
worktree state, but reads never transfer control: ask the owner to cancel,
publish, release, write stdin, or mutate its worktree.

## Review and integrate

A reply identifies a candidate, not an integrated result. Recover its exact
commit through `responseWorktree`; inspect it from your repository view with
`git show`/`git diff`, not the child's live working directory. Commission review
seeded at that revision (`atRef`); give the reviewer the contract, the owned
paths and the implementer reference for repairs. Refuse a candidate whose
diff touches paths outside its ownership before merging. Reviewers running
checks need coding authority. Integrate the accepted
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

In a cell, `Cmd.run` returns a retained result: `Cmd.stdout` is complete
successful stdout or an explicit issue; for failed commands inspect outcome and
stderr. `J.ask` batches semantic questions over supplied evidence; load
`exomonad-jev` for the worked composition. `me` is lexically captured;
`parentAgent` is the spawning actor or `Nothing` for a root.

## Discover missing information

Start from this guide and the assignment; no startup inventory ritual.
`lookup` accepts names, modules, and Hoogle-like types such as `:: Cmd.Command -> _`.
It may attach up to four Jev-selected related declarations or alternatives;
original failures remain failures. `polymorphic` is usable with call-site constraints; `unknown` needs more type
information. Use `doc topics` for guides and workspace modules; inspect their
exports/source where needed. `status` offers `summary`, `detailed`, `watches`,
`recovery`, `lineage`, `trace`, and `bindings` for runtime uncertainty without
compiling a cell.

Before hand-building a review, merge, or triage loop, `lookup`/`doc` installed
modules and skills: an existing actor is often four calls away, reimplementing
it by hand many more.

Load the relevant skill at an unfamiliar boundary: `exomonad-command` for retained
output/stdin/completion; `exomonad-workbench` for parser/type/display recovery;
`exomonad-jev` for typed judgment composition; `exomonad-unfold` for delegation/source;
`exomonad-coordinate`, `exomonad-fork`, `exomonad-orchestrate`, and `exomonad-review` for project
coordination; `exomonad-define-actors` for custom event handlers; `exomonad-cleanup` for
retirement; `exomonad-agent-spec` for typed tools and spec reload; `doc` is the
fallback.
