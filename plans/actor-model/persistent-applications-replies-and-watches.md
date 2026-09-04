# Persistent applications, typed replies, and watches

Status: core vertical implemented and focused integration checks passing; a
fresh live-console canary is available. Supervisor deadlines/cancellation and
live-result reclamation remain follow-up slices. This design supersedes the
root-completion and interactive `AgentAction` direction in the workbench
correctness wave.

## Decision

An interactive application is attached to an actor for the actor's lifetime.
A model turn ends when the model stops producing output. Ending a turn is not
a Haskell effect, request settlement, actor completion, or supervisor action.

The permanent root therefore exposes no `complete`, `yield`, `park`, or other
turn-lifecycle operation. Only its supervisor may intentionally terminate it.
Unexpected completion of the private root program is an abnormal condition,
not a successful Shoal shutdown.

Agent requests settle through a fixed-row `Replies` effect. Readiness
subscriptions use a separate fixed-row `Watches` effect. The result type is
carried by dual request capabilities rather than by a result-indexed `Complete`
effect at the head of the workbench row:

- `Response result` is the requester's observation capability and roots a
  ready live result;
- `Reply result` is the target's one-shot settlement capability;
- both name one exact `RequestId` and target actor incarnation.

A request-activated model that ends a response without replying leaves the
request pending. This supports recursive orchestration: the target may dispatch
subrequests, register a watch over their responses, become idle, and reactivate
when that watch becomes terminal. No `retainReply` operation is required
initially. Request age and pending state belong in typed inspection; deadlines
and cancellation belong to supervisor policy.

## Review against the landed runtime

The implementation review established these additional constraints:

- `ResidentStanding` currently makes mailbox readiness and an open interactive
  session mutually exclusive. Application attachment must become independent
  state before root completion is removed. The actor program may be parked on
  `receive` while its attached application still owns an available workbench.
- The private target program may retain one typed request-presentation
  continuation. Accepted `Reply result` settlement resumes that continuation
  with the result so the existing Haskell-owned cell remains the sole live
  result store. This continuation is request mechanics, not model-turn
  lifecycle and is never exposed in the workbench row.
- `Replies` owns request reservation and mailbox admission as well as
  settlement. Public `request` must not perform a separate fallible
  `Actor.cast` after registering state; the interpreter admits the opaque
  private request or atomically marks its `Response` unavailable.
- `Watch` readiness is evaluated from Rust-owned request state, while the
  Haskell handle retains only dependency identities and the typed applicative
  combiner. Rust therefore neither serializes nor duplicates ready result
  values.
- Successful settlement closes the invoking workbench activation without
  resuming its effect continuation. Rejected `attemptReply` resumes normally;
  rejected `reply` becomes a structured rejected workbench item.

These constraints preserve the existing owners: Ractor remains the mailbox and
supervision owner, `DurableInbox` remains the activation-delivery owner, the
resident session remains the live-value owner, and the shared resident actor
environment gains only the request/watch state needed to coordinate those
owners.

## Intended interaction

The requester admits work without arranging its own model-turn lifecycle:

```haskell
review <- request @ReviewReport reviewer reviewPrompt candidate
tests  <- request @TestReport tester testPrompt candidate
```

The workbench returns two typed `Response` handles and stable request IDs. The
model explicitly registers the readiness condition that should reactivate it,
then finishes its ordinary response:

```haskell
both <- watch $
  (,) <$> awaitResponse review <*> awaitResponse tests
```

When the watch becomes terminal, its durable event reactivates the
application. On reactivation the model inspects the watch or the underlying
responses:

```haskell
pollWatch both
```

A request scope presents the target with visibly distinct input and output
authority:

```haskell
sessionInput :: Candidate
sessionReply :: Reply ReviewReport
respond      :: ReviewReport -> Eff ActorEffects Void
```

The common path is:

```haskell
respond (ReviewReport findings)
```

`respond` is a request-scoped specialization of `reply sessionReply`. It means
"settle this request exactly once". It does not mean "end the model turn" or
"terminate the actor". After accepted settlement the Haskell continuation does
not resume, so an effectful suffix cannot run after result custody transfers.

## Haskell surface

Names remain provisional until exercised through the real generated surface,
but the intended kinds and control flow are:

```haskell
data Response result
data Reply result
data RequestId

data ReplyError
  = ReplyStale
  | ReplyAlreadySettled
  | ReplyUnauthorized
  | ReplyWrongIncarnation

data ResponseState result
  = ResponsePending
  | ResponseReady result
  | ResponseUnavailable ResponseFailure

data Replies a where
  AttemptReplyWith
    :: Reply result
    -> result
    -> Replies (Either ReplyError Void)

attemptReply
  :: Member Replies effs
  => Reply result
  -> result
  -> Eff effs (Either ReplyError Void)

reply
  :: Member Replies effs
  => Reply result
  -> result
  -> Eff effs Void

pollResponse
  :: Member Replies effs
  => Response result
  -> Eff effs (ResponseState result)
```

On rejection, `attemptReply` resumes with `Left`. On accepted settlement it
never constructs `Right Void`. The common `reply` operation turns a closed
`ReplyError` into a structured workbench rejection so the still-live target can
inspect and correct the request. Both operations use one Rust settlement entry
point.

The default interactive workbench row is stable across root, request, and event
activations:

```haskell
type ActorEffects = '[Replies, Watches, Actor, Worktree]
```

The exact profile may retain private effects used by the installed actor
program. Those effects are not part of the default authored workbench surface.
Effect membership expresses intent; the exact handle, actor principal, request
state, and incarnation authorize the operation.

`request` returns `Response result`. Its private target message carries the
matching `Reply result`. Public constructors remain hidden.

## Application, request, and activation scopes

The resident Haskell environment has three explicit lifetimes:

1. **Application scope** retains user declarations, ordinary bindings, agent
   identity, and the workbench for the actor incarnation.
2. **Request scope** retains `sessionInput`, `sessionReply`, and `respond` while
   one request is active. It survives any number of event-driven model
   activations and ends only on settlement, cancellation, deadline, or target
   termination.
3. **Activation scope** retains the current activation identity, typed event
   batch, and delivered watermark. It is replaced on every backend activation.

The root has application and activation scopes but no request scope. A child
waiting on descendants keeps its parent request scope while receiving new
activation-scoped events.

`sessionInput` and `sessionReply` exist only in a request scope. Activation
metadata has distinct names such as `activationId` and `sessionEvents`; a root
activation never mounts an unrelated event or failure under `sessionInput`.
The rendered activation notice is derived from the same mounted context rather
than carrying a separately guessed type.

A queued request becomes `Presented` only when the application can accept its
activation. Mounting its request scope and accepting the corresponding
activation envelope are one transition. Settling a request does not expose the
next queued request inside the model response that performed the settlement.
The next request remains queued until that response ends and a fresh activation
can atomically install its scope.

Accepted settlement closes the current request's Haskell workbench activation.
Further tool calls from the same still-finishing model response are rejected as
stale rather than running in a successor request scope. The backend may finish
ordinary prose, but no effectful suffix or newly dequeued request crosses the
settlement boundary.

Closing a request scope releases its input and settlement-capability roots.
Durable events retain only identities and transition metadata. A ready result
is rooted by retained `Response result` handles; after the final observation
handle and any derived user bindings disappear, the ordinary Haskell heap and
GC own reclamation. Runtime event metadata must not become a second result
store.

## Request and response state

One owner controls every transition:

```text
Queued
  -> Presented
  -> PendingActive
  -> PendingIdle       model response ended without settlement
  -> PendingActive     an event reactivates the application
  -> Settled

Any pending state
  -> Unavailable       cancellation, deadline, target failure, or shutdown
```

`PendingIdle` is ordinary state, not `ReplyOmitted`. Ending a model response is
not required to emit a lifecycle callback for correctness. Response state is
durable and repeatably pollable whether the backend is active or idle. A
response transition reevaluates registered watches; it does not by itself
spend another inference turn.

Settlement validates, in non-disclosing order:

1. the request and target actor incarnation encoded by the capability;
2. the executing actor principal and settlement authority;
3. that this is the actor's current admitted request;
4. that no terminal response transition has already won; and
5. that the live result belongs to the expected Haskell type and resource
   realm already proven by GHC metadata.

The first terminal transition wins. Duplicate settlement never overwrites the
cell. Stopping either actor settles every affected pending response exactly
once. Old capabilities cannot address a recreated incarnation.

## Typed durable activation delivery

Extend the existing `tidepool-node::DurableInbox` owner rather than adding an
actor-event queue beside it. Its payload becomes a closed structured value,
rendered to backend prose only at delivery:

```text
ActorActivation {
    actor,
    activation_id,
    events: [ActorEvent],
}

ActorEvent =
    RequestArrived(RequestId)
  | WatchChanged(WatchId, WatchTransition)
  | SupervisorChanged(SupervisorTransition)
```

The durable envelope sequence is the delivery watermark. The actor event also
carries its exact actor incarnation and domain identity. Delivery batches the
currently pending prefix, pushes one activation, and acknowledges through the
last accepted envelope. Events arriving during an active model response remain
queued for a later activation. Backend rejection or transport failure leaves
the prefix unacknowledged for retry.

Accepted reply ordering is:

1. validate the settlement capability;
2. root and transfer the result;
3. fill the response cell and commit its terminal state;
4. make `pollResponse` observe `ResponseReady`;
5. settle the target's private request handler and close its request scope;
6. reevaluate every registered watch that depends on the response; and
7. enqueue each newly terminal `WatchChanged` event for durable delivery.

Registering a watch and checking its current dependencies is atomic with
respect to response settlement. A watch registered after its dependencies are
already terminal is marked terminal and queues its notification before
registration returns. A watch transition is never published before
`pollWatch` and `pollResponse` can observe the corresponding state. This
removes the poll/idle lost-wakeup race without waking the model for unobserved
response transitions.

Durability is actor-incarnation-scoped. Live Haskell values and handles do not
survive root recreation. An old durable event is fenced by incarnation and is
discarded rather than presented as evidence about a new root.

## Watches

A typed `Watch` is the explicit response-readiness activation mechanism. The
one-response case and heterogeneous fan-in use the same abstraction, and event
batching avoids one backend turn per simultaneously terminal watch:

```haskell
data Await result
data Watch result
data WatchId
data WatchState result
  = WatchPending
  | WatchReady result
  | WatchUnavailable WatchFailure

data Watches a where
  RegisterWatch :: Await result -> Watches (Watch result)
  PollWatch     :: Watch result -> Watches (WatchState result)

awaitResponse :: Response result -> Await result

watch
  :: Member Watches effs
  => Await result
  -> Eff effs (Watch result)

pollWatch
  :: Member Watches effs
  => Watch result
  -> Eff effs (WatchState result)
```

`Await` initially has `Functor` and `Applicative` instances over typed readiness
conditions. A root can therefore register heterogeneous fan-in without
yielding or returning an executable program:

```haskell
both <- watch $
  (,) <$> awaitResponse review <*> awaitResponse tests
```

`watch` registers the condition and returns immediately. The model ends its
response normally. When the condition reaches a terminal state, a durable
`WatchChanged` event reactivates the owner; `pollWatch both` is authoritative.
An unavailable dependency makes the watch terminal with the exact dependency
identity and closed failure. Registration against an already-terminal
condition still queues one coalescible activation, so "register, then end the
response" is race-free.

A watch is not an `AgentAction`, model-turn continuation, scheduler, result
store, or hidden actor. It is an actor-owned readiness subscription over exact
response handles. Its implementation retains dependency identities and the
minimum typed Haskell combiner roots needed to construct the result after all
dependencies are ready. Watch cancellation and deadlines use the same
response-state and supervisor owners.

A lawful `Monad Await` is possible, but it adds data-dependent subscription
expansion and retained continuation custody. Do not expose it in the first
vertical merely for API symmetry. Add it only when a production orchestration
case cannot be expressed applicatively and the same watch owner can enforce
its cleanup and exactly-once rules.

The implemented reply vertical includes watches: response state is repeatable,
transitions have stable identities, registration inspects them atomically, and
observation handles own live-result reachability. There is no compatibility
`AgentAction` wake path.

## Permanent application lifecycle

Application attachment owns hosted-tool availability and the persistent model
context. It is independent of an outstanding request-completion delimiter.

The private root program attaches the application and then waits forever on an
existing supervisor/mailbox boundary. There is no authored `park` effect. A
normal Codex response ends naturally and leaves the application available for
operator input or durable actor events. Only supervisor shutdown intentionally
retires the root. Unexpected private-program completion is classified as a
failure and follows the existing recovery path.

Children use the same application machinery. A child may be application-idle
with no request, active on a request, or idle with a pending request. Request
settlement returns its installed actor program to FIFO mailbox readiness but
does not terminate its application or actor.

The current `AgentSession` effect mixes attachment, activation context, and
typed completion. Split those responsibilities before choosing final names:

- private application attachment and request-presentation operations remain
  runtime substrate;
- `ActivationContext` is mounted data, not an effect;
- `Replies` owns request settlement and response observation;
- `Watches` owns typed readiness registration and observation; and
- `Complete` is deleted after its last compatibility use disappears.

## Operator and workbench surface

Add `:status` only from runtime-owned structured state. It should report the
current actor/incarnation, application state, current request and age, queued
event range, delivery watermark, and whether a reply or watch transition is
pending. It must not scan rendered panes or Haskell error text.

Request and response receipts render stable request ID, target actor ID/label,
state, and result type. Accepted settlement has a distinct structured
workbench disposition such as `replied`; it is not called `completed` and is
not rendered as empty output. A parked authored wait is distinct from a tool
that is still executing.

Activation prose is informational. It names event identities and watermarks
but carries no live value or authority. Typed handles plus `pollResponse` and
`pollWatch` determine behavior.

Do not add a public `ActorInspection` effect merely to back `:status`. The
meta-command is the proven consumer and may query the runtime projection
directly. If authored Haskell later needs to branch on actor state, add a
closed, authority-checked inspection algebra around those exact decisions
rather than exposing the operator projection wholesale.

Handle rendering includes stable identity, label, result type, and bound
worktree identity where applicable. Mutable lifecycle state is not embedded in
`Show AgentRef`; `:status` and typed polling own current state. Authority
denials use closed interpreter results or structured workbench failures, never
rendered-string control flow.

The adjacent Worktree cleanup remains a separately reviewable change:

- make `listWorktrees` return its typed `Either WorktreeError` consistently;
- make public `worktreeHead` and `worktreeBranch` helpers genuinely
  `Member Worktree effs`-polymorphic or hide them;
- replace directional `mergeBranchInto` arguments with a named
  `mergeIntoWorktree MergeRequest`; and
- add filtering to the runtime-owned list operation rather than filtering a
  huge rendered registry in the model.

Likewise, replace `stopAgent :: ... -> Eff effs ()` only when the supervisor
slice can return one typed receipt distinguishing already stopped,
cancellation requested, graceful settlement, and forced termination. A
`Submission` effect is deferred until candidate acceptance has a production
consumer distinct from Worktree management. Generic filesystem, shell,
test-runner, and journaling effects are not part of this boundary.

## Delivery slices

Each slice must compile every changed target and leave one working vertical
boundary. Do not preserve the rejected public shape through adapters once its
consumer has migrated.

### 1. Response identity and exactly-once state

- Mint `RequestId` through the existing monotonic identity owner.
- Split requester `Response result` from target `Reply result` around the
  existing Haskell result cell.
- Add closed response and settlement error states with incarnation fencing.
- Centralize first-terminal-transition-wins policy.
- Add polling and lifecycle-transition tests without changing application
  attachment yet.

### 2. Fixed-row reply settlement

- Add the `Replies` effect and one settlement interpreter entry point.
- Mount request-scoped `sessionReply` and monomorphic `respond`.
- Migrate child request settlement from `Complete result` to `Reply result`.
- Prove wrong-result GHC rejection, explicit rejection recovery, terminal
  success, suffix suppression, duplicate refusal, and closure-valued results.

### 3. Persistent application boundary

- Make hosted workbench availability belong to application attachment.
- Keep application, request, and activation roots in their explicit scopes.
- Permit a request target to end a model response while the request remains
  pending and FIFO ownership remains intact.
- Prove later event reactivation can still use the original request scope.

### 4. Permanent root

- Replace the root `AgentSession` completion loop with private attachment plus
  an existing supervisor/mailbox wait.
- Remove every root-facing completion, yield, and park operation.
- Treat private root-program completion as abnormal recovery.
- Prove ordinary response termination leaves the same actor and application
  alive and usable.

### 5. Durable typed application activation

- Replace string inbox payloads with typed activation envelopes.
- Batch pending prefixes and acknowledge only accepted delivery.
- Fence stale events by exact actor incarnation.
- Add `:status` from the same runtime state.
- Keep the compatibility response wake until the typed watch consumer lands.

### 6. Watch vertical

- Introduce pure typed `Await` composition and actor-owned `Watch result`
  subscriptions.
- Publish one level-triggered watch transition on terminal readiness.
- Add repeatable `pollWatch`, terminal cleanup, and fan-in tests.
- Remove automatic per-response activation; unwatched responses remain
  durable and pollable without spending an inference turn.

### 7. Delete obsolete interactive machinery

- Remove `Complete`, dynamic result-indexed workbench rows, and completion
  preambles.
- Remove `AgentAction`, `nextTurn`, `liftAction`, and action-failure activation
  vocabulary from the interactive facade.
- Retain a lower-level blocking response combinator only if a production
  authored actor program consumes it.
- Update prompts and the field guide only after the replacement surface is
  executable.

### 8. Supervisor deadlines and cancellation

- Add typed deadline and cancellation operations against the single request
  state owner.
- Return receipts distinguishing already terminal, cancellation requested,
  graceful settlement, and forced termination.
- Settle every affected response and watch exactly once during actor shutdown.

Worktree API consistency and merge-direction naming remain a separate change.
They do not share lifecycle ownership with this plan.

## Verification

Use behavior and compilation boundaries rather than pinning complete rendered
generated strings:

- a root has no completion or turn-lifecycle operation and remains live after
  an ordinary model response;
- request and event activations share one fixed authored effect row;
- `respond` accepts exactly the requested result type and wrong types fail in
  GHC before execution;
- rejected `attemptReply` resumes with the precise closed error and leaves the
  request pending;
- accepted settlement transfers once, suppresses its suffix, and cannot be
  overwritten;
- two consecutive requests with unrelated input/result types use one persistent
  target application;
- user-defined and closure-valued results remain live and repeatedly pollable;
- dropping the final `Response` and dependent watch releases the stored
  live-result graph;
- stopping requester or target settles every affected response and watch once;
- an old capability or event cannot cross actor-incarnation recreation;
- ending a target response without replying leaves the request pending;
- registering a watch concurrently with settlement loses no wake;
- registering against an already-ready condition queues one later activation;
- watched settlement during active inference queues exactly one later
  activation, while an unwatched response spends no turn;
- several watched settlements batch while every response remains individually
  observable;
- watch publication happens only after `pollWatch` and `pollResponse` can see
  the new state;
- an unexpected Haskell evaluation failure leaves the persistent application
  attached and usable; and
- generated-file freshness is checked by the generator/current-file test,
  while Haskell fixtures prove authored signatures semantically.

Run the smallest owning checks during each slice. At the integration boundary,
run the actor, protocol, MCP, runtime, agent, and `tidepool` suites through the
repository's matched Nix/extractor setup, then `just fixtures-check`, language
formatters, strict Clippy for changed Rust targets, and `git diff --check`.

### Temporary test policy during the migration

This boundary is still changing shape. A slow integration test, or a brittle
generated-surface assertion that requires a wholesale expected-Haskell rewrite
after each intentional intermediate API change, may be temporarily ignored or
removed while its owning slice is in flight. Churn cost is enough reason; the
test need not also be slow. Do this only when all of the following are recorded
beside the change:

- the exact behavior the test used to prove;
- the fast semantic or component checks that remain active;
- the slice or invariant that makes the old test temporarily inapplicable; and
- the concrete re-enable or replacement gate.

Keep focused type-safety, exactly-once custody, and failure-path checks for the
active slice. A generated-file freshness or whole-surface string golden may be
quarantined while the schema is deliberately moving, but it must be regenerated
and re-enabled as part of stabilizing that slice. Temporary disables do not
cross the integration boundary: the final replacement surface must have its
freshness checks and end-to-end acceptance test enabled before the broad gate
runs.

## Deferred questions

- Whether a public typed `deferReply` earns its place from observable policy;
  no operation is added merely to acknowledge an ordinary pending state.
- Whether lower-level authored actors need blocking `awaitResponse`, or watches
  cover every production consumer.
- How request deadlines compose through watches without duplicating the
  supervisor's timeout owner.

None of these questions permits reintroducing an operation whose meaning is
"the model's turn is over".
