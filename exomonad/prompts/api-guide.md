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

Every hosted tool field in an installed `AgentSpec` ends with `presentWith`,
which returns an abstract `Presented handler`. This required finishing
constructor selects model-facing text while preserving the handler's semantic
result. Use `presentWith id` for `Text`, `presentWith presentJson` for JSON, or
`presentWith presentDisplay` for a `Display` value. For example,
`lookup = presentWith id $ tool description handler`. A bare hosted handler is
rejected during spec compilation. Programmatic actor handlers do not need a
renderer. The after-tool hook receives `toolResultValue` as semantic JSON and
`toolResultOutput` as the selected text; it does not render or replace the
tool's text.

## Agent work

Create one idle child with `spawnSubagent context workspace
(defaultSpawnOptions actualSpec)`. The context is a captured `ForkCtx checkpoint`
or an explicit `FreshCtx prompt`. The workspace is `SameDir`, an opaque
`ExistingWorkspace` handle, or a `ForkWorktree` selected from a committed seed.
The options carry the actual typed `AgentSpec`, model and effort choices,
instructions, an optional ordinary text label, lifetime, and limits. Labels are
descriptive only; they do not identify actors, workspaces, or groups. `spawnLimits`
is an optional `SpawnLimits` record whose `maximumDescendantDepth` and
`maximumActiveDescendants` fields use opaque `DescendantDepth` and
`ActiveDescendants` quantities. Construct those quantities with
`descendantDepth` and `activeDescendants`; each accepts values from zero through
65,535, and zero forbids descendants. Active descendants include pending
admissions throughout the sponsored subtree. `Nothing` inherits the caller's
attenuated caps, including unbounded width; an explicit cap can only narrow them.

A successful spawn returns an idle `AgentRef`. It has not run inference. Its
first typed request or a human message activates it. A typed request supplies raw
input and request options. `request @Text agent rawInput defaultRequestOptions`
returns an `Either RequestError (Request Text)`; `requestWithProgress` also
returns an independent typed progress handle.

```haskell
Right worker <- spawnSubagent (FreshCtx prompt) SameDir (defaultSpawnOptions spec)
Right pending <- request @Text worker input defaultRequestOptions
Right answer <- await (result pending)
display answer
```

Here `spec` is the actual typed `AgentSpec` that installs the child's tools and
effects. `@Text` selects the request's reply type, while `input :: Text` is its
raw input; `prompt` is also `Text`. Spawn and request default to actor
ownership. Returning a handle does not transfer ownership. A runtime scope is
an explicit delimiter; a resource joins it only when its options use
`InScope scope`.

`result request` projects a typed `Await` value. Use the single `await` operation
to observe it. `Await` supports ordinary functor, applicative, and traversal
composition; `eitherOf` chooses the first terminal branch, including failure,
while an all-branches wait requires each branch to succeed. Settlement projections
let collectors retain failures as values. Progress is an independent typed
observation. Scope results keep callback and cleanup outcomes separate.

`spawnSubagent context workspace (defaultSpawnOptions actualSpec)` returns
`Either SpawnError AgentRef`. `request @Answer agent rawInput defaultRequestOptions`
returns `Either RequestError (Request Answer)`. `requestWithProgress @Progress
@Answer agent rawInput defaultRequestOptions` returns an `Either RequestError`
containing the request and an independent `Progress Progress` handle.
`result :: Request a -> Await a`; `await` observes an `Await` as either
`AwaitError` or its typed result.

Use `response :: Request a -> Await (ResponseResult a)` when the full result
matters: it retains the typed value together with its execution receipt and
worktree evidence. `settledResponse` returns the same result inside
`Either ResponseFailure`; `result` projects only its typed value, and
`settlement` projects the value while preserving `ResponseFailure`. An
`AwaitError` remains a separate failure of observation. In record actors,
`R.settlement request` is an event source for
`Either ResponseFailure (ResponseResult a)`.

The short request fence above shows only successful admission and reply. In the
example workspace's optional Project package,
[`Project.WorkflowExamples`](../examples/workspace/.exomonad/Project/WorkflowExamples.hs)
shows how `admitCandidates` retains admissions in `candidateAdmissions` and the
checkpoint-release outcome in `candidateCheckpointRelease`.
`awaitCandidates` returns either `CandidateObservationFailed AwaitError` or
`CandidateObservationReady` with every `CandidateSettlement`, including
`CandidateSpawnNotAdmitted`, `CandidateRequestNotAdmitted`,
`CandidateResponseUnavailable`, and `CandidateResponseReceived`. The last
contains a full `ResponseResult`; an authored `Blocked` outcome stays inside
its `responseValue`. `scopedCandidates` returns `ScopeOutcome CandidateRunReport`
with `runAdmissions` and `runObservation`, preserving body and cleanup outcomes
separately. `nextCandidateEvent` chooses between a terminal result and a
progress update. `AwaitError`, `ResponseFailure`, and an authored blocked result
remain distinct layers.

The workspace's `Project.WorkspaceEffects` is a local alias of generated
`ActorEffects`. A task, prompt, or label does not select or narrow that installed
profile; the actual supplied `AgentSpec` and runtime grants determine the child's
tools and effects.

For Git project implementation and delivery, `exomonad-project-work` describes
an optional authored workflow for scaffolding, assigning ready work, reviewing
exact candidates, repairing, and integrating with checks. General exploration
and actor programming do not require project roles or group names. Check any declaration details not shown here with targeted lookup before using
them in a workspace.

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
