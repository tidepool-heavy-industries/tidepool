# Exomonad API

The default notebook scope is `Tidepool.Actors.Exomonad`, plus configured workspace
modules. `Cmd`, `J`, `R`, `T`, `Map`, and `Set` name commands, Jev, record actors,
text, maps, and sets. `bash`, `withMemory`, `MiB`, `GiB`, `:=`, and `:&` are in scope.
Cells enable the usual extensions, including `TypeApplications`, `DataKinds`,
`OverloadedLabels`, and `OverloadedRecordDot`; standalone modules declare theirs.

## Choose the workflow

Use a pure function for a deterministic transform, an ordinary Haskell `do`
program for commands, suspended waits and dependent evidence reads, Jev for a bounded semantic judgment,
and a record actor for ongoing sources or stateful joins that do not need a
model round. Use a model actor when open-ended investigation or adaptation
needs model reasoning. These choices compose: code gathers authoritative facts;
Jev judges supplied evidence and never grants resource authority.

Try uncertain workflows in small cells; compose repeated sequences when a real
consumer needs them. Retain complete evidence, project compact typed views and
page `cellDisplay.more` without replaying effects.

## Delegate and inspect

Scaffold, admit ready parallel work and integrate checked results. Recursive
owners use `Project.Work` policy and `Exomonad.Contrib.Routing`; see
`RECURSIVE-WORK.md`. These primitives also support custom typed compositions.

```haskell
let task = "Remove the stale path and report the focused check." :: Text
(worker, ready) <- spawnWatched "implementation-ready" (batch "cleanup" "implementation") $
  child @Text $ withLifetime ActorOwned $ withContext (selected id) $
    coding projectHead $
      assignment [label|remove-stale-path|] task
ready
```

`spawnWatched` composes immediate `unfold` and a named settlement watch. The
explicit `ActorOwned` branch survives this cell. Default `InvocationOwned` branches
must settle within its creating invocation; returning handles does not extend it.
A wave composes independent children with `<$>` and `<*>`. Capture an exact
checkpoint to reuse your current reasoning:

```haskell
let parserTask = "Implement the parser." :: Text
let consumerTask = "Update its consumer." :: Text
let reviewTask = "Review the interface." :: Text
Right captured <- checkpoint "feature-scaffold"
(parser, consumer, review) <- unfold (batch "feature" "wave-1") $ (,,)
  <$> child @Text (withLifetime ActorOwned (withContext (fromCheckpoint captured) (coding currentCheckout (assignment [label|parser|] parserTask))))
  <*> child @Text (withLifetime ActorOwned (withContext (fromCheckpoint captured) (coding currentCheckout (assignment [label|consumer|] consumerTask))))
  <*> child @Text (withLifetime ActorOwned (withContext (fromCheckpoint captured) (researching currentCheckout (assignment [label|contract-review|] reviewTask))))
```
A later wave from the same actor uses `subgroup "wave-2"`: it nests under
your own path, so you pass only the new segment, never your full path.

Immediate children can be awaited in their admission cell with
`waitFor ((,) <$> awaitSettled parser <*> awaitSettled consumer)`, preserving the
continuation. For work spanning model turns, use explicit `ActorOwned` branches
as above and let settlement notices or a named watch wake you. `unfoldDeferred`
requires persistent lifetime and returns before children can start; never await
its children in that invocation. It records the real enclosing result, without
fabricating a completed transcript. Read
the full value with `pollResponse` only when the notice's preview is not
enough. A `watch` joins several responses into one wake:

```haskell
settled <- watch "wave-1-settled" (awaitAnySettled [parser, consumer, review])
```

`awaitSettled` preserves unavailable outcomes as values; `awaitResponse` fails
the watch on an unavailable dependency. `pollResponse` distinguishes pending,
cancellation pending, ready, and unavailable. Progress uses `childWithProgress`
and `pollProgress`; snapshots are not replies. Record actors (`R.*`) collect
and route events without model inference. `R.start` creates persistent services.

`currentCheckout` seeds the executing actor's checkout; `projectHead` selects
the project source and `atRef` an explicit commit. Live-source admission checkpoints
eligible edits without checks; inspect omission receipts before using that source.

`withModel "luna"` selects a workspace alias; `withModel (Literal "provider-model")`
selects an explicit model (`luna`: cheap tier; `executor`: Sol tier). `withEffort Medium` sets effort. `previewBranch` shows resolved policy.
`withContext (selected render)` selects fresh context; `fromCheckpoint` uses an
exact retained capture. Immediate admission refuses unresolved `inherited`
context before allocation; use `unfoldDeferred` with explicit `ActorOwned` for
the enclosing call's completed context. Use `[label|orbit-motif|]`
for compile-checked static assignment labels; use `labelFromText` for dynamic
labels and handle its `Either`.

The activation supplies typed `sessionInput`, its reply declaration, and the
roster of siblings admitted with you; use `inspectFull sessionInput` only for
omitted detail. `respond`, `sessionReply` and `sessionInput` exist only while
a request is pending; `lookup` shows them then. A root has none of them: use
the project's task and review constructors instead of recipes written for a
child. `request @Report (responseActor worker) (assignment [label|revision|] input)`
assigns invocation-owned follow-up work. Await it before returning or explicitly
`detachRequest` for a request spanning turns. `pollRequestUpdate` inspects updates.
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
waitFor :: Member Watches effects => Await result -> Eff effects (Either WatchFailure result)
watch :: Member Watches effects => WatchLabel -> Await result -> Eff effects (Watch result)
pollWatch :: Member Watches effects => Watch result -> Eff effects (WatchState result)
pollResponse :: Member Replies effects => Response result -> Eff effects (ResponseState result)
```

## Compose commands and judgment

In a cell, `Cmd.run` returns a retained result: `Cmd.stdout` is complete
successful stdout or an explicit issue; for failed commands inspect outcome and
stderr. `J.ask` batches semantic questions over supplied evidence; load
`exomonad-jev` for the worked composition. `me` is lexically captured;
`parentAgent` is your supervising actor, which receives `sendMessage` and
settles your request, or `Nothing` for a root.

`Cmd.run command = Cmd.start command >>= Cmd.await` preserves the continuation until terminal
completion, including nonzero exits. `Cmd.observe` returns bounded status normally;
observation never detaches. Default starts are invocation-owned. Use
`Cmd.background` for an actor-owned start with completion notice, or `Cmd.detach`
to transfer an existing owned job explicitly. Reads never rerun commands;
outcome, output completeness and cleanup remain separate facts.
Reports omit source provenance unless requested. Wrap a command in
`Cmd.withSource` when its report needs the starting directory, Git revision and
dirty state; this runs a separate admitted source probe before the command.
Ordinary `Cmd.run`, `Cmd.start` and `Cmd.background` avoid that extra process.

For retained command-evidence composition see the compiled
`.exomonad/workspace/checks/background-command-example.hs`. Record-actor handlers
remain serialized while suspended: never await their own mailbox's next handler.

`R.attach` connects a later operation to an Event sink from `R.self`.
Handle refusal and retain cleanup; see `exomonad-define-actors`.

## Discover missing information

Start from this guide and the assignment; no startup inventory ritual.
`lookup` accepts names, modules, and Hoogle-like types such as `:: Cmd.Command -> _`.
It may attach up to four Jev-selected related declarations or alternatives;
original failures remain failures. `polymorphic` is usable with call-site constraints; `unknown` needs more type
information. Use `doc topics` for guides and workspace modules; inspect their
exports/source where needed. `status` offers `summary`, `detailed`, `watches`,
`recovery`, `lineage`, `trace`, and `bindings` for runtime uncertainty without
compiling a cell.

Lookup examples name tested fixtures and prerequisites, not proof of the current
workspace's compilation. Ambiguous, unavailable and live bindings have no example.
Follow the exact locator for an omitted example; never execute truncated code.

Before hand-building a review, merge, or triage loop, `lookup`/`doc` installed
modules and skills: an existing actor is often four calls away, reimplementing
it by hand many more.

Load the relevant skill at an unfamiliar boundary: `exomonad-command` for retained
output/stdin/completion; `exomonad-workbench` for parser/type/display recovery;
`exomonad-jev` for typed judgment composition; `exomonad-unfold` for delegation/source;
`exomonad-coordinate`, `exomonad-fork`, and `exomonad-review` for project
coordination; `exomonad-define-actors` for custom event handlers; `exomonad-cleanup` for
retirement; `exomonad-agent-spec` for typed tools and spec reload; `doc` is the
fallback.
