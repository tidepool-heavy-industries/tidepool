# Exomonad API

The default notebook scope is `Tidepool.Actors.Exomonad`, plus configured workspace
modules. `Cmd`, `J`, `R`, `T`, `Map`, and `Set` name commands, Jev, record actors,
text, maps, and sets. `bash`, `withMemory`, `MiB`, `GiB`, `:=`, and `:&` are in scope.
Cells enable the usual extensions, including `TypeApplications`, `DataKinds`,
`OverloadedLabels`, and `OverloadedRecordDot`; standalone modules declare theirs.

Cell values retain their types without automatic rendering.
Use `display value` for bounded structured output; it returns a
`DisplayHandle value`. `expansions handle` gives opaque keys and field labels,
and `expand handle key` displays one field independently. Use
`display (show value)` when you want the textual `Show` form. Unsupported
fields stay opaque.

## Compose the program

Compose effectful functions with `>=>` or `do`; use the notebook's `&&&`, `***`
and `|||` for products and sums of Kleisli arrows. These operators sequence
effects; `Tidepool.Async` supplies concurrency. `Control.Lens` is in scope for
composable projections, updates and traversals through retained data.

Define a small language for the task and a machine that interprets it. Local
types name commands, observations and replies; a record actor holds state and
interprets inputs through its handlers. Jev supplies semantic choices whose
payloads can be values or continuations. Typed model-agent requests let those
handlers commission investigation and use its results in later transitions.
Use ordinary functions for a straight pipeline and actors for ongoing interaction;
both can reuse the same data, functions and judgments. See `exomonad-workbench`,
`exomonad-define-actors` and `exomonad-jev` for the composing vocabulary.

Where the actor admits `ModelCall`, `Tidepool.Model` provides
`invokeModel turn input` with `textTurn` or `typedTurn @Reply` and supplied
`AgentSpec` tools. Callbacks use your available effects; ambient tools and hooks
are not inherited. Calls in one cell share its model budget. Match `modelOutcome`,
retain `modelReceipt`, and handle typed failure before cleanup. An absent admitted
service returns a typed boundary failure.

Every function tool in an installed `AgentSpec` selects its model-facing text
explicitly: use `presentWith id` for `Text`, `presentWith presentJson` for JSON,
or `presentWith presentDisplay` for a `Display` value. For example,
`lookup = presentWith id $ tool description handler`. The hook receives
`toolResultValue` as semantic JSON and `toolResultOutput` as the selected text;
it does not render or replace the tool's text.

## Delegate and inspect

Typed requests and context unfolds compose agent work. For Git project
implementation, load `exomonad-project-work` for the default recursive delivery
workflow; `exomonad-fork` and `exomonad-coordinate` explain its Project helpers.

```haskell
let task = "Remove the stale path and report the focused check." :: Text
(worker, ready) <- spawnWatched "implementation-ready" (batch "cleanup" "implementation") $
  child @Text $ withLifetime ActorOwned $ withContext (selected id) $
    coding projectHead $
      assignment [label|remove-stale-path|] task
display ready
```

`spawnWatched` combines immediate `unfold` and a named settlement watch.
Explicit `ActorOwned` branches survive the cell. Default `InvocationOwned` branches
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
For another wave, `subgroup "wave-2"` nests under your path; pass only the
new segment.

Await immediate children with `waitFor`; use explicit `ActorOwned` branches for
work spanning model turns. `unfoldDeferred` starts children after the enclosing
call returns with its real result; never await them inside that invocation. See
`doc unfold` for settlement watches and child status.

```haskell
settled <- watch "wave-1-settled" (awaitAnySettled [parser, consumer, review])
```

Record actors (`R.*`) collect and route events without model inference;
`R.start` creates persistent services.

`currentCheckout` seeds the executing actor's checkout; `projectHead` selects
the project source and `atRef` an explicit commit. Live-source admission checkpoints
eligible edits without checks; inspect omission receipts before using that source.

`withModel "luna"` selects a workspace alias; `withModel (Literal "provider-model")`
selects an explicit model (`luna`: cheap tier; `executor`: `gpt-6.1-sol`). `withEffort Medium` sets effort. `previewBranch` shows resolved policy.
`withContext (selected render)` selects fresh context; `fromCheckpoint` uses an
exact retained capture. Immediate admission refuses unresolved `inherited`
context before allocation; use `unfoldDeferred` with explicit `ActorOwned` for
the enclosing call's completed context. Use `[label|orbit-motif|]`
for compile-checked static assignment labels; use `labelFromText` for dynamic
labels and handle its `Either`.

Default `haskell` and `haskell_sync` share effects. Only an explicit synchronous
profile declares `ContextReadWrite` (`import qualified Tidepool.Agent.Context as C`).
`editableTexts` traverses authored text and eligible visible message/result
bodies, not display previews; tool source/input and function
arguments stay pinned. `C.trimText reason retained` prefixes exact retained
text with an ordinary `[Trimmed: reason]` marker. Context, model and effort
commit together on whole-cell success. Same-model continuation forwards opaque
reasoning unchanged; incompatible cross-model history fails explicitly.
Restoring a saved context cannot remove current required native groups. For
curation then delegation, `unfoldDeferred` starts actor-owned children after
commit; they inherit context and Haskell bindings and cannot be awaited inside
their creating cell. See `doc workbench` and the compiled
`bridge/haskell/examples/model-turns/ContextWorkflow.hs`.

The activation supplies typed `sessionInput`, its reply declaration, and the
roster of siblings admitted with you; use `display sessionInput` to inspect it.
`respond`, `sessionReply` and `sessionInput` exist only while
a request is pending; discovery shows them then. A root has none of them.
`request @Report (responseActor worker) (assignment [label|revision|] input)`
assigns invocation-owned follow-up work. Await it before returning or explicitly
`detachRequest` for a request spanning turns. `pollRequestUpdate` inspects updates.
Requests notify their owner unless a watch/route takes over; record actor
settlement sources require `report = Silent`.

You may inspect another actor's response, command output, progress, or
worktree state, but reads never transfer control: ask the owner to cancel,
publish, release, write stdin, or mutate its worktree.

## Review and integrate

For Git project delivery, `exomonad-project-work` owns review and integration
policy. `responseWorktree` and Git expose the exact submission; `tryMerge`
integrates managed worktrees. Load `exomonad-review` for review/repair operations
and `exomonad-unfold` for submission observations. Reviewers running checks need
coding authority.

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

`Cmd.run` returns a retained result: `Cmd.stdout` is complete successful stdout
or an explicit issue; inspect failed outcomes and stderr. `J.ask` batches
judgments over supplied evidence; load `exomonad-jev` for composition. `me` is
lexically captured. `parentAgent` is your supervisor, receiving `sendMessage`
and settling requests, or `Nothing` for a root.

Run a shell string with `Cmd.run (Cmd.bashCommand "git status --short")`;
`[bash|...|]` is a literal Bash quotation that constructs the same `Command`.
Use `Cmd.withArguments` to pass dynamic values as positional arguments.

```haskell
result <- Cmd.run (Cmd.bashCommand "git status --short")
display (Cmd.stdout result)
```

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
The direct `bash` tool's `background: true` path requests source capture for its
completion notice, so it runs the extra probe; `Cmd.background` does so only when
the command is wrapped in `Cmd.withSource`.

For retained command-evidence composition see the compiled
`.exomonad/workspace/checks/background-command-example.hs`. Record-actor handlers
remain serialized while suspended: never await their own mailbox's next handler.

`R.attach` connects a later operation to an Event sink from `R.self`.
Handle refusal and retain cleanup; see `exomonad-define-actors`.

## Discover missing information

Start with this guide and assignment. `doc <topic>` is lookup query text,
never Haskell syntax. Use hosted `lookup` only if your active `AgentSpec`
supplies it. When the admitted notebook lists `Lookup`, use
`LookupApi.lookupRaw` with `LookupApi.lookupRequest`.
`Prelude.lookup` performs ordinary list lookup. `doc topics` lists guides and
skills; load an installed skill from `.agents/skills/<name>/SKILL.md` first.

```haskell
topics <- LookupApi.lookupRaw (LookupApi.lookupRequest ["doc topics"])
display (show topics)
```

Hosted lookup may add up to four Jev-selected related declarations or
alternatives; original failures remain. `polymorphic` needs call-site
constraints; `unknown` needs type information. `status` offers `summary`,
`detailed`, `watches`, `recovery`, `lineage`, `trace` and `bindings` without
compiling a cell.

Examples cite tested fixtures, not current-workspace proof. Follow an exact
locator for omitted code; never execute truncated examples.

For unfamiliar boundaries, load the relevant installed skill. Documentation
topics are the fallback.
