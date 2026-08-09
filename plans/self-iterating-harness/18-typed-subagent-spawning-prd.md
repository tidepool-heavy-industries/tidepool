# PRD — Typed headless subagents

**Status:** proposed (2026-08-08)  
**Owner:** self-iterating harness / resident-agent surface  
**Depends on:** the Generic structural substrate described in
[`14-generic-derived-askuser-prd.md`](14-generic-derived-askuser-prd.md) and
the effect-polymorphic authored surface described in
[`15-generic-surface-wave.md`](15-generic-surface-wave.md), plus the
cycle-scoped multi-continuation realm landing governed by
[`../post-restart/realm-verdict.md`](../post-restart/realm-verdict.md)
**First backend:** Codex app-server, authenticated through the operator's
existing ChatGPT login

> **Dependency status (root, 2026-08-08):** the Generic-elaboration GO/NO-GO
> gate (PRD 14 migration step 1) is DISCHARGED — the generic-surface spike
> proved Symbol-metadata reflection elaborates on the real extract/JIT.
> Consume the spike via `16-generic-spike-receipts.md` (corrections applied
> in place: `Occurs`-family visited check, real `Proxy` metadata), NOT the
> original spike report — the pre-correction version is wrong in two
> specifics. The realm landing is in flight (`../post-restart/realm-build.md`).

## Summary

Tidepool should let an authored Haskell resident create, steer, observe, and
compose headless coding agents through a small typed effect. Ordinary monadic
Haskell remains the orchestration language:

```haskell
loop :: State -> M Effs State
loop st = do
  observations <- observe st
  orientation  <- orient st observations
  decision     <- decide st orientation
  act st decision
```

`loop` performs one checkpointable resident cycle and returns its next
semantic `State` to the driver. The driver checkpoints that value and invokes
the next cycle. Between cycles Rust deliberately regains control to compose a
fresh system prompt and runtime context, apply compaction and budgets, select
the next deployed resident source, and restore the checkpointed value. Async
handles and parked continuations may coexist within a cycle, but v1 reaches an
agent-quiescent boundary before returning: raw handles and Haskell closures
never become cross-cycle orchestration state.

An agent's communication contract is described separately using a
Servant-inspired Generic record. The record declares the tools the child may
call into its parent; ordinary ADTs declare messages sent to the child and the
child's terminal result:

```haskell
data WorkerTools mode = WorkerTools
  { askParent
      :: mode :- Call Question Decision

  , requestReview
      :: mode :- Call ReviewRequest Review

  , reportProgress
      :: mode :- Notify Progress
  }
  deriving (Generic)
```

The value-level interpretation supplies descriptions and handlers. Tool
descriptions may be assembled dynamically with `fmt` when the agent is
created; they are then frozen for the lifetime of that agent thread:

```haskell
workerTools st :: WorkerTools (AsServerT (M Effs))
workerTools st = WorkerTools
  { askParent = tool
      [fmt|Ask the resident to resolve decisions about {projectName st}.|]
      resolveQuestion

  , requestReview = tool
      [fmt|Request an independent review under {reviewPolicy st}.|]
      runReviewer

  , reportProgress = tool
      "Report progress that may change the resident's plan."
      recordProgress
  }
```

`compileTools` generically combines record selectors, endpoint types, runtime
descriptions, and handlers into:

- Codex dynamic-tool declarations;
- structural input schemas and output codecs;
- a dispatcher for server-initiated tool calls; and
- compact documentation, mocks, and protocol tests.

The first runtime backend is `codex app-server`. One long-lived app-server
process owns many concurrent Codex threads. A Haskell `AgentHandle input
result` identifies one of those threads; it does not own a subprocess. Codex
provides the headless coding harness, ChatGPT authentication, native editing
and command tools, thread steering, interruption, persistence, and streamed
activity. Tidepool provides the typed agent contract, Haskell handlers,
structured orchestration, workspace assignment, receipts, budgets, and policy.

## Product thesis

Tidepool's differentiator is where the harness lives and how quickly it can
change. A frontier model guided by a human edits a small repo-local resident;
Tidepool compiles and deploys that source from the repository. The resident can
therefore change at conversation cadence rather than vendor-release cadence,
while the Haskell type checker acts as the regression suite for integration
logic that would otherwise be fragile stringly glue.

Each layer uses the language and tools its author was optimized for. Frontier
models author orchestration in terse typed Haskell. Coding workers retain the
edit, shell, search, and test loops on which their harnesses were trained.
Tidepool does not put Haskell between workers and the filesystem merely for
purity: that experiment added little value. Haskell instead integrates models,
workers, operator decisions, policy, and typed state at the layer where its
compositional leverage is real.

The near-term product is a working orchestrator for Tidepool's own software
development. The held-out recursive-improvement evaluation follows once that
orchestrator is useful and observable.

> **Endgame (Inanna, 2026-08-08):** the current Exomonad swarm is
> ultimately hosted on this substrate — dev-tree replaces it for
> Tidepool's own development. Exo's MCP sidecar and message routing go
> unused in that future: they exist to drive interactive sessions, and
> this PRD drives headless agents directly through the backend adapter,
> which is simpler and strictly better. No Exomonad machinery is ported
> (see PRD 19's endgame note); typed spawning + worktree events + the
> resident loop are the whole orchestration story.

Codex already has a maintained headless server with the primitives this design
requires:

- one process hosting multiple agent threads;
- ChatGPT subscription authentication;
- native repository inspection, editing, and command execution;
- per-thread dynamic tools with JSON-schema inputs;
- server-initiated tool calls that wait for a host response;
- active-turn steering and interruption;
- ephemeral, resumable, and forked threads;
- structured final-output constraints; and
- streamed command, diff, tool, usage, and lifecycle events.

Tidepool's distinctive value is the layer above those mechanics: a small
repo-local Haskell resident that can continuously turn typed state into
structured worker activity, let workers call back into the resident's effect
environment, and fold authoritative results and activity receipts into its
next state.

```text
typed resident state
        │
        ├── act ─────> typed agent handles ─> assigned workspaces
        │                                  │
        │                          generated tools call
        │                          back into parent Eff
        │                                  │
        └── next state <── typed results + runtime receipts
```

The resident is analogous to a heavily customized xmonad configuration:
small, ordinary Haskell, intensely repo-specific, and expected to change
frequently. Tidepool is the stable runtime beneath it. Headless coding agents
are interchangeable workers beneath that program.

There is one deeper runtime story across the interaction surface. `askUser
@T`, typed `runLLMTurn` answers, and typed agent results all suspend a Haskell
continuation on a typed hole to be filled by an external answerer: operator,
resident model, or headless worker. Their Generic interpreters and transport
details differ, but they are instances of one suspension family. Typed
subagents should extend the realm machinery rather than introduce an unrelated
parking mechanism.

## Locked design decisions

### Ordinary monadic Haskell owns control flow

There is no graph, node, edge, stage, OODA, or workflow DSL. A resident may
implement any of those structures using functions, recursion, `case`, normal
data types, and effects.

Agent use composes through `M effs`. Agent specifications describe individual
workers; they do not become a second orchestration language.

### Agent contracts use a Servant-style Generic record

The agent tool API uses Servant's record-mode pattern rather than `:<|>` chains.
The record selector is the canonical authored tool identity:

```haskell
askParent -> ask_parent
```

V1 uses one deterministic camel-to-snake conversion. An explicit
`Named "wire_name"` override is a possible compatibility feature, not part of
the initial surface.

### Documentation and handlers are values

Descriptions do not live in type-level `Symbol`s. A tool value pairs its
description with its handler:

```haskell
data Tool m input output = Tool
  { description :: Text
  , handler     :: input -> m output
  }
```

This permits descriptions assembled from typed resident state using `fmt`.
The tool record is compiled once when the agent thread is created because
Codex dynamic tools are thread-scoped rather than turn-scoped.

The tool's input and output shapes remain in its endpoint type. The Generic
compiler obtains structural schemas and codecs from those types; the model
does not restate them as JSON Schema.

### Async typed handles are the primary surface

`spawnAgent` returns immediately with a handle. Synchronous delegation is a
library combinator built from `spawnAgent` and `waitAgent`, not the primitive.
This permits the resident to continue reasoning, spawn reviewers, ask the
operator, integrate other results, or steer workers while work remains in
flight.

Handles strongly type authored messages and terminal results. Lifecycle state
is runtime data, not a phantom-type state machine.

### Realms and cycles define scheduling semantics

The realm spike proved that one JIT machine can hold multiple independently
parked, GC-safe continuations and resume them in driver-chosen order. Typed
agents depend on that landing and obey its cycle-scoped lifetime constraint.

Within one cycle:

- continuations interleave only at explicit suspension points;
- a handler runs until it returns or performs another suspending effect;
- a handler that spawns and waits for a reviewer becomes another parked
  continuation in the same realm;
- the driver chooses which ready continuation resumes next; and
- generated-tool dispatch shares the current resident realm rather than
  forking Haskell state. `Fork` remains a separate effect.

Semantic resident state is explicit immutable data. A resumed handler sees the
lexical values it captured plus the results of effects performed after resume;
shared runtime ledgers are consulted through effects, not hidden mutable
closure state. The cycle folds results and receipts into the returned `State`.

V1 requires quiescence before a cycle returns. Durable agent identities and
cross-cycle reattachment may later be represented as checkpointed data, but a
parked Haskell continuation is never the representation of pending work across
cycles.

Near-term resident revision occurs at a deployment boundary: a frontier model
and human edit the repository, Tidepool waits for a quiescent cycle boundary,
then Rust deploys the new source under the existing checkpoint-compatibility
policy. Live self-rewriting inside a cycle is not a v1 behavior.

### The server owns workers

Tidepool starts one app-server process per backend/authentication/configuration
domain, not one process per agent. That server owns multiple loaded Codex
threads. The Tidepool registry owns thread identities, subscriptions, pending
tool handlers, workspaces, receipts, and policies.

V1 does not need an elaborate LRU. Terminal and idle agents may accumulate
within one resident iteration or delegation wave. An intermediate wave boundary
may keep running and tool-parked agents live; the outer resident-cycle boundary
must first make them quiescent and then release them.

### The Haskell surface is provider-neutral

Authored code says `Agent`, `AgentSpec`, `spawnAgent`, and `AgentHandle`, never
`Claude`, `Codex`, MCP, JSON-RPC, or subprocess. Codex app-server is the first
backend for pragmatic reasons: its harness behavior is strong, its headless
server is unusually well integrated, and custom harnesses can reuse the
operator's existing ChatGPT subscription. The choice is not a thesis that
resident authors and workers must come from different providers.

Backend and exact model selection are runtime policy. Agent specifications may
express semantic requirements such as `Fast`, `Capable`, or `Deep`; a debug or
benchmark configuration may pin an exact backend/model without making it part
of the normal authored protocol.

Codex uses its deeper app-server integration, but the abstract child-to-parent
operation is the blocking host tool call standardized by MCP. An MCP-speaking
headless harness can therefore implement the same Agent backend later without
making MCP the authored Haskell interface or forcing Codex through its less
capable transport.

### Children do not need Haskell

The child sees its native coding tools, generated tools, instructions, task,
and structured completion requirement. Its generated tools call ordinary
Haskell handlers in the parent resident. The child neither writes Haskell
commands nor understands Tidepool's effect system.

### Runtime receipts are authoritative

The model may summarize what it did, but Tidepool records actual commands,
exit results, changed files, diffs, tests, token usage, timings,
interruptions, and tool-call outcomes from app-server events and direct
workspace observation.

## Goals

1. Let a resident launch many headless coding agents from ordinary Haskell.
2. Strongly type messages sent to an agent and the agent's terminal result.
3. Let the agent call a dynamically generated, task-specific tool interface
   whose handlers run in the parent's `M effs` environment.
4. Derive tool names, schemas, dispatch, and result decoding from ordinary
   Generic records and ADTs.
5. Preserve asynchronous orchestration, active steering, interruption,
   progress observation, follow-up turns, and recursive delegation.
6. Reuse ChatGPT-authenticated Codex and its native coding harness rather than
   reproduce edit, command, session, and context machinery.
7. Run each coding worker in a caller-selected workspace under an explicit
   sandbox policy.
8. Return typed results alongside authoritative execution/activity receipts.
9. Keep the public authored vocabulary small enough to teach in one compact
   paragraph and one example.
10. Make the resident program cheap for frontier models to rewrite on a
    per-repository and eventually per-problem-distribution basis.

## Non-goals

- A LangGraph-style graph or workflow representation.
- Encoding agent running/idle/finished states in phantom types or linear types.
- Type-level prompts, descriptions, model names, workspaces, or retention
  policies.
- A TUI/tmux integration or interactive subprocess driving.
- Teaching the child Haskell as its default control interface.
- Provider parity in v1.
- Exposing Codex app-server JSON-RPC directly to authored Haskell.
- Treating model claims as verification receipts.
- Per-turn replacement of an existing thread's generated tool set.
- A sophisticated thread LRU or durable distributed scheduler in v1.
- Making `codex-codes` types part of Tidepool's public Rust or Haskell API.
- Making recursive self-improvement safe or effective by assertion; that is an
  evaluation question after the substrate exists.
- Creating git worktrees, managing branches, merging changes, or promoting a
  canonical branch. The separate
  [`19-managed-worktrees-events-prd.md`](19-managed-worktrees-events-prd.md)
  composes worktree allocation and repository events around Agent; spawning
  itself accepts an assigned workspace.

## Primary authored experience

### Communication types

```haskell
data WorkerMessage
  = NewConstraint Text
  | ReviewFeedback Review
  | ResolveDecision Decision
  deriving (Generic)

data WorkerResult
  = Completed
      { summary :: Text
      , caveats :: [Text]
      }
  | Blocked
      { blocker  :: Text
      , evidence :: [Text]
      }
  deriving (Generic)
```

The supported structural algebra for messages, tool inputs/outputs, and
results is a separate Generic interpreter from operator forms. It may support
lists and recursive containers even when `askUser` does not. The shared
Generic substrate must not collapse the supported set to the intersection of
all consumers.

### Tool contract

```haskell
data WorkerTools mode = WorkerTools
  { askParent
      :: mode :- Call Question Decision

  , requestReview
      :: mode :- Call ReviewRequest Review

  , reportProgress
      :: mode :- Notify Progress
  }
  deriving (Generic)
```

`Notify input` is equivalent to a call returning `()` but communicates intent
and may render differently in documentation and traces.

### Tool implementation

```haskell
workerTools
  :: Members '[RunLLMTurn, Agent, Ledger] effs
  => State
  -> WorkerTools (AsServerT (M effs))
workerTools st = WorkerTools
  { askParent = tool
      [fmt|Ask the resident to decide questions about {projectName st}.|]
      decide

  , requestReview = tool
      "Run an independent reviewer over the proposed change."
      (\request -> do
          reviewer <- spawnAgent (reviewerSpec st) [fmt|Review {request}|]
          waitForResult reviewer)

  , reportProgress = notify
      "Report findings that may change the active plan."
      appendFinding
  }
```

This example is the central recursive capability: while the worker is parked
inside `requestReview`, its Haskell handler may launch another headless agent,
wait for a typed review, and return that review to the original worker.

### Agent specification

```haskell
workerSpec
  :: Members '[RunLLMTurn, Agent, Ledger] effs
  => State
  -> AgentSpec effs WorkerTools WorkerMessage WorkerResult
workerSpec st = agent
  { instructions = [fmt|
      You are an implementation worker for {projectName st}.
      Work only in the provided workspace. Verify the requested change.
    |]
  , tools      = workerTools st
  , model      = Capable
  , workspace  = CurrentWorkspace
  , retention  = Ephemeral
  }
```

The initial task remains ordinary `Text`, normally constructed with `fmt`.
Typed input is useful for later messages, not required for the initial prompt.

### Orchestration

```haskell
runWave :: State -> [TestSpec] -> M Effs [WorkerResult]
runWave st tests = do
  workers <- for tests $ \test ->
    spawnAgent (workerSpec st)
      [fmt|Implement test {testName test}: {testRequirement test}|]

  for workers waitForResult
```

The resident may instead consume events as they arrive and change its plan:

```haskell
observeWorker
  :: AgentHandle WorkerMessage WorkerResult
  -> M Effs WorkerResult
observeWorker worker = waitAgent worker >>= \case
  AgentActivity activity -> do
    updateTelemetry activity
    observeWorker worker

  AgentFinished result receipt -> do
    recordReceipt receipt
    pure result

  AgentFailed failure ->
    recoverWorker failure
```

## Public Haskell API

The exact class/row spelling follows the existing Tidepool effect machinery,
but the intended semantic surface is:

```haskell
data AgentSpec effs tools input result
data AgentHandle input result

data AgentEvent result
  = AgentActivity AgentActivity
  | AgentFinished result AgentReceipt
  | AgentFailed AgentFailure
  | AgentInterrupted AgentReceipt

spawnAgent
  :: HasAgentApi tools
  => AgentSpec effs tools input result
  -> Text
  -> M effs (AgentHandle input result)

sendMessage
  :: AgentHandle input result
  -> input
  -> M effs ()

followupTask
  :: AgentHandle input result
  -> Text
  -> M effs ()

waitAgent
  :: AgentHandle input result
  -> M effs (AgentEvent result)

interruptAgent
  :: AgentHandle input result
  -> M effs ()

releaseAgent
  :: AgentHandle input result
  -> M effs ()

retainAgent
  :: AgentHandle input result
  -> M effs ()

listAgents
  :: M effs [AgentSummary]
```

`listAgents` is intentionally type-erased observability. Typed payloads remain
available through the handle that owns their types.

Convenience combinators are ordinary library code:

```haskell
waitForResult
  :: AgentHandle input result
  -> M effs result

withAgent
  :: HasAgentApi tools
  => AgentSpec effs tools input result
  -> Text
  -> (AgentHandle input result -> M effs a)
  -> M effs a

withAgentWave
  :: M effs a
  -> M effs a

parTraverseAgents
  :: Int
  -> (a -> M effs b)
  -> [a]
  -> M effs [b]
```

### Operation semantics

- `spawnAgent` creates a thread, installs its frozen tool contract, starts the
  initial turn, and returns after the thread/turn is accepted.
- `sendMessage` steers the currently active turn. V1 reports a typed runtime
  error when no steerable turn is active rather than silently starting one.
- `followupTask` starts a new turn on an idle durable agent. It does not mutate
  an active turn.
- `waitAgent` returns the next observable event for that handle. Repeated calls
  drive a per-agent event stream until a terminal event.
- `interruptAgent` requests interruption of the current turn. It is idempotent
  once the handle is terminal.
- `releaseAgent` detaches/unsubscribes and releases hot runtime resources. It
  does not delete durable thread history.
- `retainAgent` exempts a handle from automatic release at the current
  wave/loop boundary.

Whether `sendMessage` may steer a turn currently parked on a dynamic tool call
is deliberately gated on the backend spike. The API remains useful if that
specific scheduling combination is unsupported: the parent can return the new
information through the outstanding tool call or interrupt and follow up.

## Servant-style agent eDSL

### Core endpoints

Conceptually:

```haskell
data Call input output
data Notify input

data AsServerT m
type family mode :- endpoint

type instance AsServerT m :- Call input output = Tool m input output
type instance AsServerT m :- Notify input      = Tool m input ()
```

The actual implementation may use a class rather than open type-family
instances if that produces smaller inferred terms or better errors under the
JIT. The authored shape is the contract.

### Generic compilation

```haskell
compileTools
  :: HasAgentApi tools
  => tools (AsServerT m)
  -> Either ToolCompileError (CompiledTools m)
```

`CompiledTools` contains at least:

```haskell
data CompiledTools m = CompiledTools
  { declarations :: [DynamicToolDeclaration]
  , dispatch     :: ToolName -> StructuralValue -> m StructuralValue
  , synopsis     :: Text
  }
```

The Generic compiler performs one field-ordered traversal:

1. Read the selector name and normalize it to snake case.
2. Validate the resulting Codex tool identifier.
3. Obtain the input structural schema from the `Call` input type.
4. Obtain the output encoder from the `Call` output type.
5. Read the runtime description and handler from the `Tool` value.
6. Produce a declaration and a dispatch entry from the same leaf.

Schema and dispatcher cannot drift because they are emitted from one endpoint
visit. Descriptions and handlers cannot drift because they inhabit the same
`Tool` value.

### Additional interpretations

Only `AsServerT` must be constructed in normal authored code. The first
internal second interpretation is `AsMetadata`: a mechanically produced record
whose leaves carry normalized name, input/output structural metadata, runtime
description, and compatibility information rather than a handler. It feeds
declarations, synopses, traces, and protocol fixtures without becoming another
artifact the resident author must populate.

The library may add other interpretations when they buy something concrete:

- schema/declaration generation;
- compact documentation;
- a mock child client for unit tests;
- trace redaction/pretty-printing;
- compatibility fixtures.

Do not introduce an authored `AsDocs` record until real reuse requires docs
and handlers to vary independently.

Give the Servant-style mode encoding one real extract/JIT proof. If it makes
dictionary elaboration or diagnostics materially worse, flattening to
`data WorkerTools m = WorkerTools { askParent :: Tool m Question Decision,
... }` preserves the selector/schema/handler invariant and is the explicit v1
fallback.

### Diagnostics are part of the API

Expected authoring failures must become concise source-level `TypeError`s where
possible:

- tool record does not derive `Generic`;
- record field is not a supported endpoint;
- input or output lacks the required structural Generic interpreter;
- two selectors normalize to the same wire name;
- normalized name violates backend identifier rules;
- nested or recursive shape is unsupported by the selected interpreter; or
- result/message type cannot be structurally encoded.

Errors should name the agent record, selector, endpoint type, and smallest
corrective action. They should not expose `Rep`, JSON-RPC, `codex-codes`, or
backend schema types.

## Internal Haskell effects and module boundary

Authored harnesses import the public surface from `Tidepool.Agent` or the
curated `Tidepool.Harness.Prelude`. Low-level transport operations and compiled
tool representations live under `Tidepool.Agent.Internal` and are not part of
the model-facing contract.

Conceptually, the public `Agent` effect lowers to an internal effect resembling:

```haskell
data AgentRuntime a where
  SpawnCompiled
    :: CompiledAgent
    -> AgentRuntime AgentId

  SendInput
    :: AgentId
    -> StructuralValue
    -> AgentRuntime ()

  StartFollowup
    :: AgentId
    -> Text
    -> AgentRuntime ()

  AwaitEvent
    :: AgentId
    -> AgentRuntime RuntimeAgentEvent

  ReplyTool
    :: AgentId
    -> ToolCallId
    -> StructuralValue
    -> AgentRuntime ()

  Interrupt
    :: AgentId
    -> AgentRuntime ()

  Release
    :: AgentId
    -> AgentRuntime ()
```

The public handler performs the typed work around this transport:

- compile the tool record;
- register Haskell dispatch closures with the resident runtime;
- structurally encode outbound messages;
- structurally decode tool inputs and final results;
- invoke handlers inside the parent `M effs` row;
- encode handler results back to the child; and
- combine terminal values with runtime receipts.

The Rust side never invokes an arbitrary Haskell function by name. It reports
an agent/tool-call identity to the resident machine, which resumes the
registered Haskell continuation/handler through Tidepool's existing effect
machinery.

## Codex app-server backend

### Why app-server

`codex app-server` is Codex's documented deep-integration interface. It speaks
bidirectional JSON-RPC-like messages over stdio JSONL and powers rich Codex
clients. It is a better semantic boundary than wrapping `codex exec`, driving
a TUI, or generating a temporary MCP server.

The backend starts:

```text
codex app-server --stdio
```

and performs the connection initialization with experimental APIs enabled.
For a new worker it then creates a thread with:

```text
ephemeral:     chosen from Retention
model/effort:  resolved backend policy
dynamicTools:  compileTools output
instructions: agent value-level instructions
```

and starts the turn with:

```text
cwd:           caller-assigned workspace
sandbox:       workspace-write policy
input:         initial fmt prompt
outputSchema:  Generic-derived terminal-result schema
```

The preferred request shape omits `cwd` from `thread/start` and supplies it at
`turn/start`, avoiding the documented project-trust write associated with a
workspace-write thread start. The spike must prove whether this is sufficient
to keep normal runs from mutating user configuration. Do not copy or rewrite
ChatGPT credentials into an isolated `CODEX_HOME` without a separately designed
credential/config boundary.

### Dynamic tool dispatch

When Codex calls one of the generated tools:

```text
app-server emits item/tool/call
  -> Rust routes AgentId + call id + name + arguments
  -> Haskell dispatcher decodes the input ADT
  -> handler runs in parent M effs
  -> Haskell encodes the output ADT
  -> Rust replies to the server request
  -> Codex receives the tool result and resumes the same turn
```

This suspension is the essential parent/child control-transfer primitive.
The parent resident remains runnable while the child waits. A handler may ask
the operator, call `runLLMTurn`, spawn more subagents, inspect resident state,
or wait for another typed result, subject to its effect row.

A handler failure must resolve the outstanding server request with an explicit
tool error; it must never strand a pending call. The runtime records the
failure, cancels any abandoned nested work, and lets the child or resident's
bounded recovery policy decide whether to retry, continue, or interrupt.

### Structured completion

V1 uses `turn/start.outputSchema` derived from the requested result type, then
validates and decodes the terminal assistant value with Tidepool's structural
Generic decoder. A malformed result does not become a typed success; the
runtime may steer a correction or begin a bounded follow-up retry.

A reserved generated `finish_task` dynamic tool may provide a stronger
completion gate: it could reject malformed output while leaving the child
alive. Its clean termination behavior depends on the interrupt/reply ordering
for a tool-parked turn and therefore remains part of the app-server spike, not
a v1 assumption.

### Rust client dependency

Use `codex-codes` behind a narrow Tidepool-owned adapter seam for the initial
spike. It already provides async and blocking clients, app-server process
lifecycle, request correlation, generated protocol types, server-request
responses, and raw request escape hatches.

Pin the crate and Codex CLI versions together. At the first material protocol
or maintenance problem, vendor the required client code or replace it with a
small Tidepool-owned Tokio stdio/JSONL adapter. Neither `codex-codes` nor its
types cross Tidepool's internal backend boundary.

`nanocodex` is not this adapter: it is an independent Responses API agent
runtime and does not reuse the operator's ChatGPT-authenticated Codex harness.

### Documented capability basis

The current design is based on the official Codex app-server documentation
and protocol source:

- [App-server protocol and lifecycle](https://developers.openai.com/codex/app-server/)
- [Official app-server README](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md)
- [Thread protocol types](https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/src/protocol/v2/thread.rs)
- [Turn protocol types](https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/src/protocol/v2/turn.rs)
- [Dynamic-tool implementation](https://github.com/openai/codex/blob/main/codex-rs/app-server/src/dynamic_tools.rs)

Established capabilities:

| Requirement | App-server surface | Status |
|---|---|---|
| ChatGPT login reuse | managed ChatGPT auth/account APIs | documented |
| Headless operation | stdio JSONL transport | documented |
| Model and effort selection | thread/turn parameters + model list | documented |
| Multiple workers | multiple loaded threads per server | documented |
| Ephemeral/durable/forked context | thread start/resume/fork | documented |
| Dynamic generated tools | `dynamicTools` + `item/tool/call` | experimental |
| Park for host tool reply | server request awaits host response | experimental; source-backed |
| Steer active turn | `turn/steer` | documented |
| Interrupt active turn | `turn/interrupt` | documented |
| Structured terminal output | `outputSchema` | documented |
| Activity and receipts | item/diff/usage/lifecycle events | documented |
| Arbitrarily long tool wait | no implementation timeout found | not guaranteed |
| Steer while tool-parked | no prohibition found | not established |

Experimental does not mean unusable, but it does mean the adapter is pinned,
compatibility-tested, and isolated from the Haskell contract.

## Agent registry and lifetime

### Runtime registry

The handler maintains a registry approximately like:

```haskell
data RuntimeAgentState
  = Starting
  | Running TurnId
  | Parked TurnId ToolCallId
  | Idle
  | Completed
  | Failed
  | Interrupted
  | Released
```

This state is intentionally not reflected in the `AgentHandle` type. Async
events, external interruption, app-server failures, and steering races make a
runtime state machine more honest and substantially easier to author against.

The registry also owns:

- backend thread/turn identifiers;
- pending Haskell tool-handler tasks;
- event queues and subscribers;
- typed-codec witnesses hidden behind the handle entry;
- assigned workspace identity;
- retention and loop/wave ownership;
- model, usage, and timing metadata; and
- terminal result/receipt cache.

### Loop/wave cleanup

V1 uses coarse structured cleanup rather than GC finalizers or LRU policy:

- running and tool-parked agents remain live;
- retained agents remain live or resumable;
- terminal ephemeral agents are released after their value and receipt are
  integrated;
- idle durable agents are unsubscribed at a loop/wave boundary and resumed on
  later use; and
- failed/abandoned pending Haskell tool tasks are cancelled when their agent
  is interrupted or released.

`releaseAgent` means detach/unsubscribe, not delete durable history. Explicit
deletion is an administrative operation outside the initial authored surface.

A loop/wave boundary is explicit (`withAgentWave`, or an equivalent driver
boundary); the runtime never attempts to infer one from authored recursion.

The v1 driver does not complete a cycle while an authored agent handle remains
running or parked. The resident must wait for it or interrupt it, then release
the resulting terminal/idle worker before returning its next `State`. This
makes cycle teardown reclaim the realm and all handler closures without relying
on GC finalizers.

The server already has an idle unload policy after the last subscriber. A
future bounded hot-thread LRU is an optimization if actual memory observations
justify it.

### Crash behavior

- Durable Codex threads may be resumed after a backend process restart.
- Ephemeral threads are disposable and need not be crash-recoverable.
- V1 checkpoints only quiescent semantic state, never raw process handles or
  Haskell closures.
- A later durable-agent extension may checkpoint stable agent/thread identities
  and reconstruct typed attachment from an agent role/specification.
- A checkpoint referring to a missing durable worker records that worker as
  lost and follows authored recovery policy.
- Tidepool owns wall-clock deadlines, backend restart, and event-queue bounds.

## Workspace boundary

Agent spawning consumes a workspace assignment; it does not create or manage
one. The assignment supplies an absolute `cwd` plus the sandbox access Codex
may exercise there:

> **Revision (Inanna, 2026-08-08):** once
> [`19-managed-worktrees-events-prd.md`](19-managed-worktrees-events-prd.md)
> lands, agent creation is tightly coupled to worktree allocation — a
> managed worktree is the only workspace an agent can receive, one
> worktree per agent, all agents isolated. `CurrentWorkspace` and
> free-form workspace assignment are transitional, valid only for
> pre-PRD-19 spikes and dogfood.

```haskell
data Workspace = Workspace
  { cwd    :: FilePath
  , access :: WorkspaceAccess
  }

data WorkspaceAccess = ReadOnly | WorkspaceWrite
```

The first dogfood points at its standalone repository. Exomonad may later
allocate a worktree and pass its path as a `Workspace`, but that composition is
outside this effect and PRD.

That later effect should expose workspace activity as events in the same style
as Agent events: commits created, checks completed, conflicts detected, and
the tree becoming dirty or clean. A higher-level run may pair an `AgentHandle`
with a `WorktreeHandle` and select across both event streams, treating them as
one orchestration unit without making either effect own the other's lifecycle.

The terminal receipt records what the agent runtime actually observed without
claiming ownership of repository history:

```haskell
data AgentReceipt = AgentReceipt
  { agentId       :: AgentId
  , backendThread :: BackendThreadId
  , cwd            :: FilePath
  , commands       :: [CommandReceipt]
  , fileChanges    :: [FileChangeReceipt]
  , toolCalls     :: [ToolCallReceipt]
  , usage         :: UsageReceipt
  , timing        :: TimingReceipt
  }
```

## Capability model

Two independent boundaries apply:

1. The child receives Codex's native coding tools constrained by its workspace
   sandbox, plus only the generated tool record for parent interaction.
2. Each generated tool handler can perform only effects available in its
   parent's `M effs` row.

The tool record is therefore an object-capability interface into the resident.
The effect row constrains the implementation behind that interface. They are
related but not interchangeable security boundaries.

An external coding backend with ambient edit/command tools can never offer the
same capability guarantee as a fully Tidepool-native effect machine. Workspace
sandboxing and process policy remain the hard boundary for native coding tools.

## Observability

The resident and operator should be able to inspect:

- the hierarchical agent identity and parent;
- current lifecycle state;
- backend/model and assigned workspace;
- active turn and pending generated tool call;
- last meaningful activity;
- token/time budget consumption;
- observed commands and changed files;
- terminal typed result and authoritative receipt; and
- why an agent was interrupted, released, restarted, or lost.

Use canonical hierarchical identifiers such as:

```text
test_wave/parser_test_3/reviewer
```

These names support traces, budget attribution, and recursive cancellation.
They are runtime identities, not Haskell type names.

## Agent-facing documentation

The initial advertised contract should remain compact:

> Define an agent's callable tools as a Generic record parameterized by
> `mode`; each field has type `mode :- Call Input Output` or `mode :- Notify
> Input`. Build an `AgentSpec` with value-level instructions and `tool`
> handlers, then use `spawnAgent`, `sendMessage`, `followupTask`, `waitAgent`,
> and `interruptAgent`. Tool inputs, messages, and results are ordinary
> Generic ADTs. Agent use composes through normal `M effs` Haskell.

One complete worker example may follow. Do not teach app-server, MCP, JSON
Schema, subprocesses, Generic representation classes, or the Rust adapter.

## Falsification spikes

> **Approach revision (Inanna, 2026-08-08):** external research (ChatGPT node)
> found the app-server surface viable across these questions. Do NOT run this
> battery as an exhaustive pre-gate — build the adapter on pinned
> `codex-codes`, plug it into the vertical core, and debug as we go. The five
> spikes below are demoted to a checklist of behaviors to confirm
> opportunistically during bring-up (park duration, steer/interrupt while
> parked, correlation, completion protocol, config isolation). Record observed
> behavior as fixtures when a question actually gets answered; do not spend
> tokens proving each in isolation first. Version pinning (CLI + crate
> together) stays mandatory — it is cheap and is what makes debug-as-we-go
> safe.

The app-server backend is a GO/NO-GO gate for the full surface. Run these
against the pinned locally authenticated Codex CLI and `gpt-5.6-terra` (or the
closest model returned by `model/list`).

### Spike 1 — parked typed tool

1. Start one app-server over stdio.
2. Initialize with experimental APIs enabled.
3. Create an ephemeral thread with one generated `ask_parent` tool.
4. Start a turn in a temporary test workspace.
5. Wait for `item/tool/call`.
6. Hold the response for increasing intervals, then reply.
7. Prove the same turn resumes and reaches typed completion.

Freeze the exact request/response/event ordering and all observed timeout or
disconnect behavior as a compatibility fixture. Re-run the longest accepted
park interval on every pinned Codex upgrade; an upstream timeout requires a
heartbeat/park-token design before that version is admitted.

### Spike 2 — steer and interrupt while parked

While the dynamic tool request remains outstanding:

- issue `turn/steer` and record acceptance, ordering, and subsequent reply
  behavior;
- issue `turn/interrupt` in a separate run and record resolution of the
  pending request, item events, and turn state; and
- prove no pending Haskell task or server request leaks.

Failure of steer-while-parked narrows `sendMessage`; it does not kill the
design. Failure to safely interrupt/reclaim a parked request requires an
adapter workaround before broader implementation.

### Spike 3 — concurrency and one-server ownership

Start 5–10 ephemeral threads with distinct tools and temporary workspaces. Prove:

- tool-call correlation cannot cross agents;
- events route to the correct typed handle;
- independent turns make progress concurrently;
- overload/rate-limit responses are surfaced and bounded;
- completed threads release without killing the shared server; and
- backend restart produces the documented durable/ephemeral distinction.

### Spike 4 — structured completion

Test Generic-derived `outputSchema` with successful, malformed, and
semantically rejected results. Compare bounded steer/follow-up correction with
a generated `finish_task` tool and select the smaller reliable terminal
protocol.

### Spike 5 — configuration isolation

Prove the chosen thread/turn request shape and workspace cwd do not mutate the
operator's Codex user configuration. If mutation is unavoidable, design a
separate configuration domain that reuses supported authentication without
copying credentials ad hoc.

## Implementation tree

Implementation itself follows Exomonad's scaffold/fork/converge model. The
vertical core is an invariant every branch preserves, not a reason to serialize
the work into broad horizontal phases:

```text
spawn one worker
  -> generated tool call
  -> parent Eff handler
  -> same child resumes
  -> Generic-decoded result
  -> runtime receipt folded into State
```

### First fork — independent GO/NO-GO gates

Run these branches eagerly through Exomonad:

1. **Generic/JIT gate.** The Generic metadata substrate is already proven (see
   the dependency-status note; the forms surface itself is PRD 14's — the old
   `Form` builder is dropped for the derived-generic path there, and this
   PRD's message/tool interpreter is separate by design per wave 15). The
   remaining delta is exactly two proofs: (a) the Servant-style mode encoding
   (`mode :- Call …` — a type-family application in an HKD field position,
   with the flattened `Tool m input output` record as the named fallback) and
   (b) a list/recursive container round-tripping through the structural codec
   on the real extract/JIT — the polarity forms rejects and this interpreter
   requires. One dev-sized spike reusing the generic-surface spike harness.
2. **Codex backend gate.** Per the approach revision above: bring up the
   adapter on pinned `codex-codes` and debug against the vertical core
   directly, confirming the spike checklist opportunistically. Pin Codex CLI
   and `codex-codes`; generate a version-matched protocol-schema fixture.
3. **Realm landing/integration.** Land the cycle-scoped multi-continuation
   machine with the realm verdict's handled-prefix and suspension-path
   constraints, then freeze the internal park/resume seam used by Agent.
   Sequencing (Inanna, 2026-08-08): the realm lands FIRST — this wave spawns
   after it, not around it. Freezing the park/resume seam is a named
   deliverable at the realm-build fold, not something to reverse-engineer
   here. The contract artifact is `plans/post-restart/realm-lanes/continuation-parking-contract.md`
   (invariants + consumer-visible surface + internal/churnable list frozen at
   a91a1479; the exact signature table is filled at that lane's fold, after
   its prefix-compatibility step settles the parked-entry signature). Read
   the seam from that file, not from `jit_machine.rs`.

Any NO-GO stops convergence and routes to the named substrate repair. No
agent-authored JSON schema, synchronous subprocess wrapper, or immortal realm
is accepted as a shortcut around a failed gate.

### First convergence — the vertical core

Converge the smallest end-to-end path: one worker in the current workspace, one
generated tool, one Haskell handler, `outputSchema`-decoded terminal result,
and a minimal authoritative receipt containing changed paths plus command exit
status. The resident may be parked in `waitAgent`; no concurrency or workspace
isolation claim is required yet.

This convergence freezes the narrow Haskell/internal/backend boundary around
which later work can parallelize.

### Second fork — useful orchestrator branches

Once the core seam exists, fork at least these independently reviewable lanes:

- **Authored eDSL and diagnostics:** `Call`, `Notify`, `Tool`, record-mode
  interpretation, selector naming, `compileTools`, and compile-fail UX.
- **Registry and selection:** async handles, event queues, cancellation,
  cleanup, and a typed wait/select operation that avoids polling multiple
  workers.
- **Recursive handler:** a worker parks in `requestReview`; its handler spawns
  and awaits a reviewer in the same realm, then resumes the worker.
- **Concurrency economics:** correlation and correctness under several
  threads, plus measured effective parallelism and subscription-level
  throttling rather than nominal fan-out.
- **OODA dogfood:** the first useful checkpointed resident, initially limited
  to one mutating worker at a time plus read-only research/review workers.

The OODA resident is not a graph DSL:

```text
Observe  -> consume agent/runtime evidence
Orient   -> update typed beliefs, constraints, and uncertainty
Decide   -> choose a bounded next delegation/action
Act      -> spawn/wait or perform one controlled change
          then return updated State to Rust
```

These lanes need not all converge simultaneously. Each carries its own patch and
evidence; the root integrates them bottom-up while keeping the vertical core
green.

### Follow-on composition — workspace event streams

[`19-managed-worktrees-events-prd.md`](19-managed-worktrees-events-prd.md)
specifies worktree creation and git-event subscriptions. It composes a
`WorktreeHandle` with an `AgentHandle` into a higher-level worker run and uses a
recursive development-tree dogfood to prove authored integration workflows.
None of those git semantics belong to the Agent effect.

> **Joint design target (Inanna, 2026-08-08):**
> `harness-dogfooding/dev-tree/` is authored deliberately against this PRD
> AND [`19-managed-worktrees-events-prd.md`](19-managed-worktrees-events-prd.md)
> in unison — it unfolds a recursive
> `DevPlan` into coding agents, pokes descendants when parent HEADs move,
> and merges completed branches bottom-up with fresh integration agents.
> It is the concrete acceptance pressure for both surfaces: when writing
> the git PRD or shaping Agent's API details, check the decision against
> what dev-tree needs to compile and run. It should typecheck as the
> surfaces land, not be retrofitted afterward.

### Evaluation and recursive improvement

After the orchestrator is useful for Tidepool development, evaluate resident
revisions on a held-out corpus of difficult repository tasks. Fix workers,
budgets, tasks, and backend; vary the small resident program. Measure
correctness first, then cost, latency, retries, failed actions, and orchestration
overhead.

A stronger model may revise the resident on a development split, with every
revision deployed only at a quiescent boundary and scored blindly on held-out
tasks. This tests whether program-level orchestration improvement extracts
increasing capability from unchanged workers.

## Acceptance criteria

1. A fresh frontier model can define a new worker using one tool record,
   ordinary ADTs, one `AgentSpec`, and no backend-specific code.
2. The record selector, declaration schema, decoder, and dispatcher are emitted
   by one Generic traversal and cannot silently disagree.
3. Runtime descriptions may use `fmt` and resident state at agent creation.
4. One ChatGPT-authenticated app-server hosts several concurrent typed handles
   without per-agent subprocesses, and the compatibility probe reports actual
   throughput and throttling rather than merely counting open threads.
5. A generated tool call parks only its child while the parent executes an
   arbitrary permitted `M effs` handler and may recursively spawn another
   agent.
6. Typed messages steer active agents; typed results are decoded only after
   structural validation.
7. Every terminal result is paired with authoritative activity/workspace
   receipts.
8. Each worker runs in its caller-assigned workspace and sandbox; spawning
   makes no claim about git lifecycle or repository integration.
9. Wave cleanup may preserve running/parked workers; cycle cleanup requires
   quiescence, releases terminal/idle workers, and cancels abandoned pending
   handlers without relying on GC.
10. The adapter passes pinned protocol fixtures and contains all
    `codex-codes`/app-server types behind a Tidepool-owned boundary.
11. No normal worker run mutates the operator's global Codex configuration.
12. The first dogfood resident completes repeated checkpointed OODA cycles
    through typed spawning, tool callbacks, evidence integration, and bounded
    recovery; no Haskell continuation or raw handle is required to survive a
    cycle boundary.

## Open decisions after the backend spike

These should not block writing the contract algebra, but must be resolved
before the dogfood resident becomes a durable surface:

1. Whether `outputSchema` or a generated `finish_task` is the canonical
   terminal protocol.
2. Whether `sendMessage` is allowed during a parked tool request and how its
   ordering is exposed.
3. The first semantic `ModelPolicy` vocabulary and where exact model pinning
   lives for benchmarks.
4. Whether durable named specialists are needed in the first dogfood or all
   workers may be ephemeral.
5. Budget representation: per-agent, per-wave, per-resident iteration, or a
   combination.
6. Whether inbox/result types remain explicit `AgentSpec` parameters (the
   current draft) or become associated pieces of the record-style agent
   contract. This is an authored-taste decision, not a runtime requirement.

## Risks and mitigations

### Dynamic tools are experimental

Pin the backend, generate/version-check protocol schemas, isolate the adapter,
and maintain an MCP or plain structured-output fallback only if measurement
shows it is required.

### Third-party Rust wrapper drift

Keep `codex-codes` internal and pinned. Vendor or replace it immediately when
its abstraction obstructs the pinned protocol rather than distorting the
Haskell surface around it.

### Recursive fan-out consumes unbounded resources

The runtime owns a concurrency semaphore and hierarchical budgets. A tool
handler may recursively spawn, but it does not escape the resident's remaining
slots, time, token, or workspace policy.

### Shared-server failures affect many workers

The registry makes thread ownership explicit, records terminal/lost states,
and restarts the backend at a controlled boundary. Durable specialists resume;
ephemeral workers follow authored retry policy. A small process pool may later
reduce blast radius if evidence warrants it.

### Type machinery becomes the product

Judge the eDSL by authored source size and model success, not cleverness. Keep
the mode parameter because it eliminates real duplication; reject type-level
docs, lifecycle phantoms, graph kinds, and operational-policy types unless a
specific interpreter requires them.

### Resident and runtime congeal together

Keep backend lifecycle, authentication, process control, workspace binding,
receipts, and persistence in stable handlers. Keep planning, delegation strategy,
review topology, and stopping policy in the repo-local Haskell resident. The
runtime is designed to host rapidly changing programs, not become one.
