# PRD — Typed headless subagents

**Status:** proposed (2026-08-08)  
**Owner:** self-iterating harness / resident-agent surface  
**Depends on:** the Generic structural substrate described in
[`14-generic-derived-askuser-prd.md`](14-generic-derived-askuser-prd.md) and
the effect-polymorphic authored surface described in
[`15-generic-surface-wave.md`](15-generic-surface-wave.md)  
**First backend:** Codex app-server, authenticated through the operator's
existing ChatGPT login

## Summary

Tidepool should let an authored Haskell resident create, steer, observe, and
compose headless coding agents through a small typed effect. Ordinary monadic
Haskell remains the orchestration language:

```haskell
loop :: State -> M Effs Done
loop st = do
  workers <- traverse (spawnAgent worker . renderTask st) (nextTasks st)
  outcomes <- traverse waitUntilFinished workers
  loop (integrate st outcomes)
```

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
structured orchestration, worktree isolation, receipts, budgets, and policy.

## Product thesis

The authored program should describe cognition and orchestration, not rebuild
a coding-agent runtime.

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
repo-local Haskell resident that can continuously unfold typed state into
structured worker bursts, let workers call back into the resident's effect
environment, and fold authoritative results and git receipts into its next
state.

```text
typed resident state
        │
        ├── unfold ──> typed agent wave ──> isolated worktrees
        │                                  │
        │                          generated tools call
        │                          back into parent Eff
        │                                  │
        └── next state <── fold typed results + runtime receipts
```

The resident is analogous to a heavily customized xmonad configuration:
small, ordinary Haskell, intensely repo-specific, and expected to change
frequently. Tidepool is the stable runtime beneath it. Headless coding agents
are interchangeable workers beneath that program.

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

### The server owns workers

Tidepool starts one app-server process per backend/authentication/configuration
domain, not one process per agent. That server owns multiple loaded Codex
threads. The Tidepool registry owns thread identities, subscriptions, pending
tool handlers, workspaces, receipts, and policies.

V1 does not need an elaborate LRU. Terminal and idle agents may accumulate
within one resident iteration or delegation wave. The runtime releases them at
an explicit wave/loop boundary; running and tool-parked agents remain live.

### The Haskell surface is provider-neutral

Authored code says `Agent`, `AgentSpec`, `spawnAgent`, and `AgentHandle`, never
`Claude`, `Codex`, MCP, JSON-RPC, or subprocess. Codex app-server is the first
backend because it is the strongest available headless harness and works with
the operator's existing ChatGPT subscription.

Backend and exact model selection are runtime policy. Agent specifications may
express semantic requirements such as `Fast`, `Capable`, or `Deep`; a debug or
benchmark configuration may pin an exact backend/model without making it part
of the normal authored protocol.

### Children do not need Haskell

The child sees its native coding tools, generated tools, instructions, task,
and structured completion requirement. Its generated tools call ordinary
Haskell handlers in the parent resident. The child neither writes Haskell
commands nor understands Tidepool's effect system.

### Runtime receipts are authoritative

The model may summarize what it did, but Tidepool records actual commands,
exit results, changed files, diffs, commits, tests, token usage, timings,
interruptions, and tool-call outcomes from runtime events and git state.

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
7. Give each coding worker an isolated Exomonad-style filesystem/git view.
8. Return typed results alongside authoritative execution and workspace
   receipts.
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
  , workspace  = FreshWorktree
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

Only `AsServerT` must be constructed in normal authored code. The library may
derive other interpretations when they buy something concrete:

- schema/declaration generation;
- compact documentation;
- a mock child client for unit tests;
- trace redaction/pretty-printing;
- compatibility fixtures.

Do not introduce an authored `AsDocs` record until real reuse requires docs
and handlers to vary independently.

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
cwd:           assigned worktree
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
- workspace/worktree identity;
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

The server already has an idle unload policy after the last subscriber. A
future bounded hot-thread LRU is an optimization if actual memory observations
justify it.

### Crash behavior

- Durable Codex threads may be resumed after a backend process restart.
- Ephemeral threads are disposable and need not be crash-recoverable.
- The resident checkpoint stores stable agent/thread identities and its own
  semantic state, never raw process handles or Haskell closures.
- A checkpoint referring to a missing ephemeral worker records that worker as
  lost and follows authored recovery policy.
- Tidepool owns wall-clock deadlines, backend restart, event-queue bounds, and
  orphaned worktree recovery.

## Workspace and git model

Spawning a coding worker should atomically create the Exomonad triad:

```text
agent thread  <->  typed resident context  <->  isolated git worktree
```

The `workspace` value in `AgentSpec` selects policy, not an arbitrary path:

```haskell
data WorkspacePolicy
  = CurrentWorkspace
  | SharedReadOnly
  | FreshWorktree
```

`FreshWorktree` is the default for parallel coding workers. Its runtime handler
records the base revision and owns branch/worktree lifecycle. Codex receives
the resulting directory as its turn `cwd` under workspace-write sandboxing.

The terminal receipt includes enough truth for the resident to decide whether
to accept, revise, merge, or abandon the branch:

```haskell
data AgentReceipt = AgentReceipt
  { agentId       :: AgentId
  , backendThread :: BackendThreadId
  , workspace     :: WorkspaceReceipt
  , changes       :: ChangeReceipt
  , checks        :: [CheckReceipt]
  , toolCalls     :: [ToolCallReceipt]
  , usage         :: UsageReceipt
  , timing        :: TimingReceipt
  }
```

Merge is a separate effect/policy decision. `spawnAgent` does not imply merge,
and a successful typed result does not imply that a branch is acceptable.

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
same capability guarantee as a fully Tidepool-native effect machine. Worktree,
sandbox, and process policy remain the hard boundary for native coding tools.

## Observability

The resident and operator should be able to inspect:

- the hierarchical agent identity and parent;
- current lifecycle state;
- backend/model and worktree;
- active turn and pending generated tool call;
- last meaningful activity;
- token/time budget consumption;
- changed files and git status;
- terminal typed result and authoritative receipt; and
- why an agent was interrupted, released, restarted, or lost.

Use canonical hierarchical identifiers such as:

```text
test_wave/parser_test_3/reviewer
```

These names support traces, budget attribution, recursive cancellation, and
worktree recovery. They are runtime identities, not Haskell type names.

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

The app-server backend is a GO/NO-GO gate for the full surface. Run these
against the pinned locally authenticated Codex CLI and `gpt-5.6-terra` (or the
closest model returned by `model/list`).

### Spike 1 — parked typed tool

1. Start one app-server over stdio.
2. Initialize with experimental APIs enabled.
3. Create an ephemeral thread with one generated `ask_parent` tool.
4. Start a turn in a temporary git worktree.
5. Wait for `item/tool/call`.
6. Hold the response for increasing intervals, then reply.
7. Prove the same turn resumes and reaches typed completion.

Freeze the exact request/response/event ordering and all observed timeout or
disconnect behavior as a compatibility fixture.

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

Start 5–10 ephemeral threads with distinct tools and worktrees. Prove:

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

Prove the chosen thread/turn request shape and worktree cwd do not mutate the
operator's Codex user configuration. If mutation is unavoidable, design a
separate configuration domain that reuses supported authentication without
copying credentials ad hoc.

## Delivery plan

### Step 1 — backend compatibility spike

Run the five falsification spikes with a small Rust-only driver. Pin Codex CLI
and `codex-codes`; check a generated app-server JSON-schema bundle into test
fixtures or regenerate it in a version compatibility test. Produce a verdict
before extending the public Haskell effect set.

### Step 2 — Haskell contract algebra

Implement `Call`, `Notify`, `Tool`, record-mode interpretation,
selector-to-snake naming, Generic schema/codec reuse, `compileTools`, and
source-level diagnostics. Prove one nested input/output ADT and one malformed
record through the real extract/JIT path.

### Step 3 — Agent effect and registry

Add the public async Agent operations, internal transport effect, typed handle
registry, event queues, tool-handler dispatch, result decoding, and coarse
wave/loop cleanup. First use the current workspace; do not combine the initial
control-plane proof with worktree merging.

### Step 4 — worktree-backed workers and receipts

Reuse Exomonad's worktree lifecycle concepts as the `FreshWorktree` handler.
Capture base/head state, diffs, commands, checks, usage, and timing. Add explicit
accept/revise/merge/abandon operations above spawning.

### Step 5 — dogfood resident

Create a separate intentionally ambitious dogfood resident rather than
expanding the minimal wizard. It should:

- maintain a structured proposal/task/decision ledger;
- plan bounded worker waves;
- spawn implementation and review agents;
- allow nested review requests through generated tools;
- fold typed results and receipts;
- trim or revise its plan; and
- ask the operator only at explicit policy gates.

The canonical acceptance wave is ten independently specified tests, ten
isolated workers, bottom-up review/merge, and one aggregate verification pass.

### Step 6 — evaluation and recursive improvement

Evaluate the resident on a held-out corpus of genuinely difficult repository
engineering tasks. Fix the worker models, budgets, task set, and backend; vary
the resident Haskell program. Measure correctness first, then cost, latency,
retries, merge failures, and orchestration overhead.

A stronger model may revise the resident on a development split. Score every
revision blindly on held-out tasks. This tests the actual thesis: whether
rewriting a small typed orchestration program can extract increasing capability
from unchanged middling coding agents.

## Acceptance criteria

1. A fresh frontier model can define a new worker using one tool record,
   ordinary ADTs, one `AgentSpec`, and no backend-specific code.
2. The record selector, declaration schema, decoder, and dispatcher are emitted
   by one Generic traversal and cannot silently disagree.
3. Runtime descriptions may use `fmt` and resident state at agent creation.
4. One ChatGPT-authenticated app-server hosts at least ten concurrent typed
   handles without per-agent subprocesses.
5. A generated tool call parks only its child while the parent executes an
   arbitrary permitted `M effs` handler and may recursively spawn another
   agent.
6. Typed messages steer active agents; typed results are decoded only after
   structural validation.
7. Every terminal result is paired with authoritative activity/workspace
   receipts.
8. Coding workers operate in isolated worktrees and no spawn implies an
   automatic merge.
9. Wave/loop cleanup releases terminal/idle workers, preserves running/parked
   workers, and cancels abandoned pending handlers without relying on GC.
10. The adapter passes pinned protocol fixtures and contains all
    `codex-codes`/app-server types behind a Tidepool-owned boundary.
11. No normal worker run mutates the operator's global Codex configuration.
12. The dogfood ten-test wave completes through typed spawning, nested tools,
    isolated workspaces, and aggregate verification.

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
5. The exact worktree accept/revise/merge algebra and conflict receipts.
6. Budget representation: per-agent, per-wave, per-resident iteration, or a
   combination.
7. Whether live resident-source replacement occurs only between benchmark
   episodes, at loop boundaries, or not until after the first evaluation
   baseline.
8. Whether inbox/result types remain explicit `AgentSpec` parameters (the
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
slots, time, token, or worktree policy.

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

Keep backend lifecycle, authentication, process control, worktrees, receipts,
and persistence in stable handlers. Keep planning, delegation strategy,
review topology, and stopping policy in the repo-local Haskell resident. The
runtime is designed to host rapidly changing programs, not become one.
