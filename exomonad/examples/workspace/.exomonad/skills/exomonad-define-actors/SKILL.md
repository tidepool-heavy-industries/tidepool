---
name: exomonad-define-actors
description: Author small languages and the stateful machines that interpret them. Define Haskell record actors with typed calls, event sources, retained state and Jev/agent continuations to automate orchestration in the notebook.
---

Build a small machine for the task. Its public calls are a control language;
its state records where the work has reached; its handlers interpret commands
and events into state changes, replies and effects. Use local records and sums
for the vocabulary, ordinary functions for the interpreter, and Jev where a
transition needs semantic judgment. A handler can start agent work and a later
reply can drive the next transition. The model can query or steer the machine
through the same typed interface while it runs.

One record describes private state, public calls and fixed source handlers. The
same record supplies its definition and typed client. `R` is already imported as
`Tidepool.Actor.Record`; nonconflicting actor names are in the default scope.
Keep project policy in Haskell. The existing runtime owns ordering, authority,
request admission and lifetime. `R.start` creates an explicit persistent record
service with actor lifetime, surviving its creating invocation. A handler without
a hosted invocation uses actor ownership for its commands and requests; it still
processes one accepted call or event at a time.

Choose the language around decisions the machine makes. A repair coordinator may
accept candidates and findings, accumulate review results, and request a revision;
a research coordinator may accept observations and competing hypotheses, select
the next experiment, and accumulate conclusions. Both are stateful interpreters
over a task-specific event language. Products retain jointly needed facts; sums
distinguish inputs that trigger different behavior; endpoint values let machines
call one another. Keep a straight dependency chain as a function or suspended
`waitFor` computation; put ongoing interaction and evolving state in an actor.
The Routing collectors are ready-made compositions you can use alongside your
own actors. No model turn is needed for a transition the program can take itself.

Give the language one `Call` taking a command sum when a single interpreter is
convenient, or several differently typed `Call` fields when separate operations
make a better client API. Local payload types at these actor endpoints need no
JSON schema. The same domain value can travel from an agent reply into an event,
through a Jev alternative's payload, and into the next handler.

Compose sources with `fmap` to tag or project their values and `(<>)` to merge
them; preserve request identity and source evidence in the event payload.
One serialized mailbox is not a global transaction: effects can escape before
a handler commits, and waiting for another handler on that same mailbox creates
a dependency cycle. Use a continuation triggered by a later event for that case.

Availability of the names below:

- **Shipped** — in every Exomonad cell: `R.definition`, `R.start`, `R.client`,
  `R.send`, `R.call`, `R.on`, `R.settlement`, `R.progress`, `R.lifecycle`,
  `Cmd.completion`, `R.self`, `R.sender`, `R.finish`, `R.replace`,
  `R.forwardResult`, `requestWithProgressInto`, `LocalEffects`, `ActorSpec`,
  `Handler`, `Actor.Selected`, `knownEffects`, `Replies`, `Actor`,
  `Notifications`.
- **Shared contrib** — available through configured package imports: `coordinationActor` and
  `CoordinationEffects` (`Exomonad.Contrib.Actors`), `Outcome` and `Candidate`
  (`Exomonad.Contrib.Types`). The last section shows the lower-level shipped API when contrib imports
  are absent.

Confirm a name with `lookup` before depending on it. A skill's example is
evidence of a pattern, not proof that the name is installed for you.

This executable example defines a tiny language with two input commands and one
query. Its interpreter retains the inputs and answers from their combined state:

```haskell
data Join mode = Join { joinState :: mode :- State (Maybe Text, Maybe Int), sourceReady :: mode :- Call Text NoReply, checksReady :: mode :- Call Int NoReply, joined :: mode :- Call () (R.Reply (Maybe (Text, Int))) }
let joinDefinition = coordinationActor "integration-join" Join
      { joinState = (Nothing, Nothing)
      , sourceReady = \commit -> modify' (\(_, checks) -> (Just commit, checks))
      , checksReady = \checks -> modify' (\(commit, _) -> (commit, Just checks))
      , joined = \() -> gets (\(commit, checks) -> (,) <$> commit <*> checks)
      }
joiner <- R.start joinDefinition
let endpoints = R.client joiner
R.send (sourceReady endpoints) "abc123"
R.send (checksReady endpoints) 4
result <- R.call (joined endpoints) ()
display result
```

`State s` appears exactly once. Its definition field is the initial value; handler
fields use ordinary `get`, `gets`, `put`, `modify'`. Calls with `NoReply` use
`R.send`; calls with `R.Reply output` use `R.call`. Endpoint values can be captured
or passed to other actors. Passing one endpoint grants access only to that route.
Pass endpoints as values: a child or another machine can report a finding or
request the next item through the route you give it. State and source-handler
fields are private in clients. Handles display only exact
identity; query a declared route for the state needed by your next decision.

For fixed subscriptions, declare `mode :- Event input`, and supply
`R.on source handler`. Source values identify the actual request or actor:

```haskell
data Results mode = Results { resultState :: mode :- State [Either ResponseFailure (ResponseResult (Outcome Candidate))], arrived :: mode :- Event (Either ResponseFailure (ResponseResult (Outcome Candidate))), resultCount :: mode :- Call () (R.Reply Int) }
let resultDefinition = coordinationActor "candidate-results" Results
      { resultState = []
      , arrived = R.on (R.settlement worker) (\result -> modify' (++ [result]))
      , resultCount = \() -> gets length
      }
results <- R.start resultDefinition
count <- R.call (resultCount (R.client results)) ()
display count
```

This block assumes `worker :: Response (Outcome Candidate)` from the current
session. `R.progress p` carries `ProgressState progress`; `R.settlement response`
carries the exact typed terminal result, including failure and execution evidence.
`Cmd.completion job` carries `Cmd.CommandResult` for a command owned by the
creator. It retains completion for a late collector. Route the result, including
its cleanup evidence, rather than waking a model to poll command status.
Sources compose with `fmap` and `(<>)`: tag independent sources with shared project
constructors or names. Each handler receives one event. Publications accepted
while it is busy remain ordered; handlers remain serialized even during
a suspended wait. Never await an event requiring another handler on that same
mailbox to run; no cursor rearming is required. Attachment starts
from retained current source state, not a replay of unavailable earlier history.

Within a handler, `R.self @Join` supplies send-only endpoints for return messages.
It does not enable synchronous calls to yourself. `R.sender @Join` obtains the
runtime input origin; a forwarder's identity is not the original worker identity.
Preserve the typed result's request/execution evidence when forwarding. Captured
handles do not impersonate their creator or transfer resource ownership. Source
attachment is authorized against the new actor's creator: a handler creating a
nested collector must own the observed request, not merely capture its parent's
response handle.

A separately bound handler helper needs its concrete state/effect type when GHC
cannot infer the row from the record. Annotate the expression with
`Handler MyState (CoordinationEffects MyActor) result`; the handlers inside a
`coordinationActor` definition already receive that context.

Successful handlers commit their state and reply. A failed handler leaves the
last committed state; external effects are **not** undone. Do not repeat an
uncertain request or send on replacement. `R.replace handle newDefinition` keeps
the schema and retained state, and returns the replacement's exact handle.
Previously distributed endpoints still name their original incarnation.

For handler-owned requests, `requestWithProgressInto` runs your retention callback
with the exact typed response/progress handles before submission. Send those handles
to a route on `Self`; that route can create a collector using its receiving
incarnation's endpoints. `checks/review-continuation.hs` is a low-level regression
fixture for this retention boundary; its retained reviewer requires explicit
checkout preparation. Routine review uses `startReviewFlow`, described in
`WORKBENCH.md`, which owns exact-source admission. Do not reconstruct
response handles from labels or repeat submission after uncertain failure.

Keep the integration actor alive through useful repairs. When done:

```haskell
joinFinal <- R.finish joiner
resultsFinal <- R.finish results
display (joinFinal, resultsFinal)
```

`R.finish` drains accepted work and returns `ActorExit state`; retain that value
for later inspection. It does not retire the workers whose results were observed.
Use the parent's scoped cleanup separately. Shape actor-to-actor payloads for
the receiving transition: a compact delta, a structured report, a narrative
explanation or another useful Haskell value. Query the state the next decision
needs.

## Without the example workspace

`Exomonad.Contrib.Actors.coordinationActor` composes these two definitions:

```
coordinationActor name = R.definition name (Actor.Selected knownEffects)
type CoordinationEffects api = LocalEffects api '[Replies, Actor, Notifications]
```

In a fresh project, write the same two lines into your own `.exomonad/Project`, or
write the shipped call out in the cell. Either way the row must be **pinned by a
signature**: `knownEffects` is polymorphic in the row, so `R.definition` on its
own is ambiguous. Pin it with `:: ActorSpec MyActor MyEffects`, and give any
separately bound handler helper its concrete
`Handler (ActorState api) effects result` — GHC cannot recover the row from the
record for a binding that sits outside it. A signature and its equation go in
the **same** cell item; a signature alone installs nothing.

```haskell
data Tally mode = Tally { tallyState :: mode :- State [Text], noted :: mode :- Call Text NoReply, noteCount :: mode :- Call () (R.Reply Int) }
type TallyEffects = LocalEffects Tally '[Replies, Actor, Notifications]
recordNote :: Text -> Handler [Text] TallyEffects ()
recordNote note = modify' (++ [note])
tallyDefinition :: ActorSpec Tally TallyEffects
tallyDefinition = R.definition "tally" (Actor.Selected knownEffects) Tally
      { tallyState = []
      , noted = recordNote
      , noteCount = \() -> gets length
      }
tally <- R.start tallyDefinition
R.send (noted (R.client tally)) "first finding"
count <- R.call (noteCount (R.client tally)) ()
display count
```

A record actor may hold a worktree. `R.withWorktree tree spec` starts it
holding a worktree the parent created and did not bind; ownership is exclusive
and integrate authority follows the owned handle, so this is how an actor comes to own
the tree it merges into. An actor with a worktree resolves to the coding role,
one without resolves to research, and a row that needs `WorktreeIntegration`
only sits under the first. The host admits at most one worktree per actor.

```
Right tree <- createWorktree (fromRef "exomonad/integration" "integration")
integrator <- R.start (R.withWorktree (worktreeId tree) integratorDefinition)
```

Add effects to the list as the handlers need them: `Forks` to admit a child,
`Commands` to run one, `Jev` for a judgment. `Jev` and `Commands` are not
re-exported by the workbench surface — a cell naming them needs
`import Tidepool.Effects.Core (Jev, Commands)` before the row. The row is still
checked against the launching actor's ceiling, so asking for more than the
creator holds is refused at start, not silently granted. When the loop this
record carries is implement → review → repair → merge, load `exomonad-review`
for the owner-map and repair-policy shape and `Exomonad.Contrib.Merge` for the
integrator actor.
