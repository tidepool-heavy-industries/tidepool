# Shoal field guide

Status: evolving. This is the practical guide for using and improving Shoal.
It keeps the runnable surface visibly distinct from active replacement
designs. Runtime semantics remain owned by the relevant crate documentation;
active designs remain in `plans/`.

## Working model

Shoal is a typed orchestration environment for persistent Codex nodes working
in managed Git worktrees. Use the Haskell workbench for orchestration and live
typed state. Use native repository tools to inspect, test, review, and
integrate exact commits.

The root owns architecture, decomposition, integration, and cross-boundary
coherence. A child is worthwhile when work needs an independent Codex context,
an isolated worktree, concurrent reasoning, or a lifecycle boundary. Ordinary
calculation and small orchestration helpers belong in the root's live Haskell
environment.

Conversation prose explains tasks and wake reasons. It is not authoritative
state. Exact agent references, reply handles, worktree handles, and activation
inputs stay as typed Haskell values; exact commits and repository state are
verified with Git.

## Workbench basics

Start by inspecting the actual environment:

```haskell
:browse
:bindings
:type sessionInput
```

Use `:type` and `:info` when a name or constructor is unclear. `:show imports`
reports the effective module environment. The supported meta-command set is
intentionally smaller than full GHCi.

The workbench executes input units in order. A rejected unit stops the suffix;
earlier successful declarations and bindings remain committed. Effects already
performed by the rejected unit are not rolled back. Put a declaration group or
one effect sequence inside `:{` / `:}` and persist several results with one
outer tuple or record binding.

Compiler diagnostics are part of the interaction interface. A rejected input
reports the exact failing unit and cause, including completed prefix receipts.
Host routing, invocation, encoding, and panic failures are distinct errors and
retain their underlying cause. Empty wrapper output is never evidence that a
Haskell action completed.

Define campaign-specific types and helpers freely in the live session. Promote
a helper into the repository only after repeated use shows that it improves
more than one task and has one clear owner.

## Persistent agents

The default `Tidepool.Actors.Shoal` facade exposes Codex-node operations rather
than low-level actor construction:

```haskell
data AgentSpec
data AgentRef
data Response result
data Reply result
data RequestLabel

codingAgent   :: WorktreeHandle -> AgentSpec
readonlyAgent :: Text -> AgentSpec
startAgent    :: Member AgentLaunch effs => AgentSpec -> Eff effs AgentRef

request
  :: forall result input effs
   . Member Replies effs
  => AgentRef -> RequestLabel -> input -> Eff effs (Response result)

stopAgent
  :: (Member AgentControl effs, Member AgentInspection effs)
  => AgentRef -> Eff effs StopOutcome

pollResponse
  :: Member Replies effs
  => Response result -> Eff effs (ResponseState result)
```

Create worktrees explicitly, start every independent agent before waiting, and
put the requested result type at the dispatch site when the reply crosses
workbench input units:

```haskell
let Right reviewLabel = requestLabel "review-a"
let Right testLabel = requestLabel "test-b"
let Right bothLabel = watchLabel "review-and-test"
reviewerA <- startAgent (codingAgent treeA)
reviewerB <- startAgent (codingAgent treeB)

responseA <- request @ReviewReport reviewerA reviewLabel candidateA
responseB <- request @ReviewReport reviewerB testLabel candidateB

both <- watch bothLabel $
  (,) <$> awaitResponse responseA <*> awaitResponse responseB
```

`AgentRef` identifies one persistent actor, Codex context, Haskell scope,
worktree authority, and place in the recursive ownership tree. A
`Response result` belongs to the requester and supports repeatable typed
observation. The dual `Reply result` belongs to the target and authorizes one
settlement. Same-machine inputs and results may contain closures and types
declared in the caller's live session; they are not serialized.

`request` admits the private request payload and returns immediately. The
`Replies` interpreter owns registration and mailbox admission, so there is no
second public cast for callers to coordinate. The target may have many requests
queued, and each request may use unrelated input and result types. Settling a
request returns the target's private actor program to mailbox readiness; it
does not terminate the target. `stopAgent` is explicit and is ordered behind
earlier messages from the same sender.

Agent applications start eagerly without a bootstrap prompt or throwaway model
turn. Codex makes the empty conversation rollout durable before publishing its
v2 hosted-session callback. That callback records durable binding version 4;
only reading the accepted binding creates the queue-ready handle used by native
delivery. Older bindings are rejected on resume with guidance to start a fresh
root because a thread ID alone cannot establish queue readiness.

The target activation carries the task as an ordinary User message. Its
request scope mounts visibly distinct input and output authority:

```haskell
:type sessionInput
:type sessionReply
:type respond
respond (ReviewReport findings)
```

Submitting a type error rejects only that workbench input. Correct it in the
same persistent agent session. Ending a model response without replying leaves
the request pending. After a valid settlement, the same Codex identity handles
the next request with its accumulated conversation and declarations intact.

The low-level `Tidepool.Actor` API remains available through an intentional
advanced import. It is not imported or re-exported by the default facade.
The older blocking sub-answerer combinators now live under
`Tidepool.Answerer.Fork`; they serve the noninteractive self-harness and are
not the persistent-actor fork surface. Shoal context forks are described with
the applicative `Tidepool.Actors.Unfold` API exported by the default facade.

## Cache-preserving context unfold

Use `unfold` when several independent branches materially benefit from the
caller's accumulated model context. It forks the active provider thread and
the immutable Haskell binding tip, creates one persistent actor and named
worktree per branch, and returns typed handles in the applicative plan's
original shape. Each child receives the whole unfold call plus only its own
selected `sessionInput`; the parent keeps the original reply authority.

```haskell
let Right campaign = campaignLabel "normalization"
let Right wave = forkGroupLabel "implementation"
let Right domain = branchLabel "domain"
let Right ui = branchLabel "ui"

forks <- unfold (batch campaign wave) $
  (,) <$> child (coding @DomainReport domain projectHead domainTask)
      <*> child (coding @UiReport ui projectHead uiTask)
```

`Unfold` is deliberately applicative, not monadic: the runtime can see and
reserve the complete sibling shape before it publishes any assignment. A
successful admission must be the final effect boundary in the final Haskell
input unit. Pure projection or reshaping of its returned handles in that same
unit is allowed; no later effect is. Pre-publication failure rolls the whole
group back. After publication, each persistent child has its own lifecycle and
may receive typed follow-up requests.

The standard role rows are `ResearchEffects`, `CodingEffects`,
`ScaffoldEffects`, and `IntegrationEffects`. `narrowed` may select any
compile-time subset of the caller's row. The requested row, semantic role,
native command policy, workspace access, and descendant budget are projected
into one effective runtime policy; Haskell membership expresses intent while
opaque handles and runtime grants remain authoritative. Inspection-only
children may read and search but are denied builds, tests, formatters,
generators, installers, and other artifact-producing commands before process
launch.

Branches use readable hierarchical paths such as
`shoal/normalization/implementation/domain`, with deterministic numeric
suffixes on collision. `BranchReceipt`, `actorContext`, `listAgents`, and
`:status` retain exact actor/worktree identities beneath those readable names.
Request settlement also records the starting head and committed, staged,
unstaged, and untracked submission evidence before response/watch readiness.

## Replies, watches, and model turns

The architecture and remaining verification work are recorded in
[the persistent applications, typed replies, and watches plan](plans/actor-model/persistent-applications-replies-and-watches.md).

An interactive application remains attached for its actor incarnation. A
model turn ends when the model stops producing output. The permanent root has
no Haskell `complete`, `yield`, or `park` operation, and only its supervisor
may intentionally terminate it.

Requests use distinct capabilities for the two roles:

- `Response result` belongs to the requester and supports repeatable typed
  observation;
- `Reply result` belongs to the target and authorizes exactly one settlement;
- both identify one request and exact target incarnation.

A request activation retains `sessionInput`, `sessionReply`, and a monomorphic
`respond` across model turns. If the target ends a response without settling,
the request remains pending. Response state remains durable and pollable;
registered watches, incoming requests, and supervisor transitions publish
typed, sequenced activation events through the existing durable actor inbox.
Prose is only their presentation.

`Watch` provides typed readiness composition without becoming lifecycle
control or an executable program returned from a turn:

```haskell
review <- request @ReviewReport reviewer reviewLabel candidate
tests  <- request @TestReport tester testLabel candidate

both <- watch bothLabel $
  (,) <$> awaitResponse review <*> awaitResponse tests
```

`watch` returns immediately. When its condition becomes terminal, a durable
event reactivates the application and `pollWatch both` observes the typed
result. Unwatched responses do not spend an inference turn merely because
their state changes. A watch is a readiness subscription over response
handles, not an actor, result store, scheduler, hidden continuation API, or
replacement for ordinary end-of-response behavior.

`Await` deliberately has `Functor` and `Applicative`, but not `Monad`. It
combines known response dependencies while keeping subscription ownership and
cleanup simple. Add data-dependent subscription expansion only for a proven
orchestration case.

The root application is permanent. Ordinary model-response termination ends a
turn; it is not represented by a Haskell function or effect. The root exposes
no `complete`, `yield`, or `park`, and only its supervisor terminates the actor.
Successful `respond` transfers result custody and closes that request's current
workbench activation without running an effectful suffix.

Use `:status` for runtime-owned application, response, and watch state. Typed
handles and `pollResponse`/`pollWatch` remain authoritative; activation prose
and tmux panes are diagnosis surfaces.

Integrate incrementally. Once a candidate is clean, independently inspect its
exact diff and verification evidence, then land it if it is coherent. Do not
hold unrelated finished work behind the slowest child. Start dependent work
only from the commit that integrated its prerequisite.

A worker report is an authored claim. Verify the named commit, clean worktree,
changed targets, and important failure paths yourself. Tmux panes and tracing
are operator telemetry, not transport or lifecycle truth.

Keep concurrent verification focused. Compile every changed target, execute
the smallest tests that prove the behavior, use the repository's matched
Nix/extractor setup for Haskell-backed checks, and reserve broad validation for
a meaningful integration boundary.

## Current sharp edges

- An abnormally terminated root is recreated as a new actor incarnation while
  the Shoal host remains alive. Conversation survives, but previous Haskell
  bindings, actor references, response handles, worktree authority bindings,
  pending exits, and mounted live values do not. Reconcile through current Git
  and runtime state rather than transcript references.
- Linked worktrees share Git objects and configuration but not working files,
  indexes, or `HEAD`. Use the repository's matched extractor/toolchain path;
  stale inherited endpoints can otherwise compile a different checkout.
- During the first live code canary, Cargo output directories below the
  worker's mounted workspace disappeared during compilation. An explicit
  target outside that workspace survived. This has not yet been reproduced in
  a small repository; use the `shoal-console` canary below to determine whether
  the owner is actor workspace mounting, Codex command sandboxing, or the
  disposable Tidepool mirror rather than adding another target-path policy.
- A running Shoal process does not hot-reload Haskell, prompts, or runtime
  code. `just shoal-console` builds the current checkout, including uncommitted
  source, but an already-launched root keeps the snapshot embedded in its
  running host. Restart at a reviewed clean boundary before judging a changed
  facade live.

## Current implementation boundary

The vertical now includes persistent roots and children, dual response/reply
capabilities, exactly-once settlement, labeled applicative watches, typed
request deadlines and cancellation, typed stop and lifecycle observation,
role-specific effect rows, atomic cache-preserving unfold, recursive scaffold
and fold, managed worktree queries, and runtime `:status`/`actorContext` facts.
The old blocking answerer API remains available under
`Tidepool.Answerer.Fork`; it is not Shoal actor unfold.

Provider lineage, Haskell snapshot identity, fork group, exact effect row, and
optional cached/uncached input counts are observable. Missing provider usage
is represented as `Nothing`, never a fabricated zero. Tidepool deliberately
keeps an idle actor's backend attached today: reply settlement is not proof of
provider turn-idleness, and eager teardown would weaken inexpensive follow-up
and multi-wave orchestration. Process hibernation is an optional future
backend optimization once a durable provider idle/usage event can make it
race-free; it is not part of actor semantics.

## Next live canary

Use the independent, small `/home/inanna/dev/shoal-console` repository for the
next run. It has fast Rust tests and a dependency-free `cargo run -- --smoke`
contract, so lifecycle evidence is not buried under a cold Tidepool/Cranelift
build. Start it from the Tidepool checkout with a unique tmux name:

```sh
just shoal-console -- --session shoal-console-canary --no-attach
tmux list-panes -t shoal-console-canary -F '#{pane_id} #{pane_title}'
```

Inspect both the Host and Root panes before dispatch, while workers run, and
after settlement. The next live boundary is queued reuse of one persistent
worker through replies and watches:

1. In the root workbench, inspect `:browse`, `:bindings`, and `:status`; define
   one small report ADT. A root has no `sessionInput` outside a request scope.
2. Create one managed worktree with `allowDirtySnapshot` if the console source
   is dirty, start one `codingAgent`, and submit two real code-and-test requests
   to that same agent before watching either response. Use different result
   types, for example `Text` and the session-defined report ADT.
3. Make the first task add one narrowly named unit test and the second add a
   different small test or smoke-contract assertion in the same worktree. The
   second task should naturally build on the first checkout state. Neither task
   may be a readiness probe or throwaway message.
4. Register one applicative watch over the two original response handles and
   end the root response normally. Confirm the worker receives two request
   activations, `pollWatch` returns the exact typed pair, and the worker retains
   its Codex context and declarations between requests.
5. Verify each focused test independently, inspect the exact Git diff/commits,
   call `stopAgent`, and require an orderly retirement with no retry or
   missing-rollout log entry.
6. Check whether the console worker's normal Cargo target survives across both
   requests. If it disappears, reproduce with the smallest pair of commands
   and trace the existing workspace-mount and Codex sandbox owners. If it does
   not reproduce, record that the earlier failure was specific to the
   disposable Tidepool mirror rather than generalizing a workaround.
7. Stop the disposable tmux session and discard its managed canary worktree;
   do not merge the test-only changes.

Submit both requests before waiting. A representative shape is:

```haskell
responseA <- request @Text agent firstLabel ()
responseB <- request @CanaryReport agent secondLabel ()
both <- watch bothLabel ((,) <$> awaitResponse responseA <*> awaitResponse responseB)
```

Handle `createWorktree`'s `Either` explicitly and keep the `AgentRef`, both
`Response` handles, and the `Watch` in one fenced outer binding. Do not infer
success from panes, transcript prose, or process intent: require typed
settlement, host lifecycle evidence, and independently verified Git/test facts.

## Longer-term direction

- Expose a complete reply lifecycle observation when a real policy needs to
  branch over target failure or cancellation.
- Correlate actor incarnation, request/activation identity, hosted-tool
  invocation, compile attempt, and settlement in the existing structured
  tracing path.
- Feed durable provider turn-idle and token-usage events into the existing
  context observation seam before considering process hibernation.
- Keep common fan-out, review, and fold patterns as ordinary Haskell rather
  than adding a worker registry, merge queue, or second scheduler.

## Updating this guide

Add a current technique after it succeeds and its boundary is understood. Add
a sharp edge when it is repeatable and materially affects agent efficacy;
remove it when the owning fix lands. Prefer deleting obsolete advice over
preserving historical variants. Git is the history of this guide.
