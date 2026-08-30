# Actor architecture

## 1. The unit of execution

An actor is not a Haskell process wrapped around an occasional model call, and
it is not an unconstrained model driving GHCi. It combines four things:

1. a fixed typed Haskell program;
2. a persistent Haskell declaration and binding environment;
3. one accumulating model conversation;
4. Rust-owned lifecycle and execution context.

```text
                     Rust actor runtime
        identity · mailbox · scheduling · supervision
        model context · capabilities · resource scopes
                              │
                              ▼
                   typed Haskell actor program
                fixed control flow and invariants
                              │
            deliberate :: Member Deliberate effs => Goal a -> Eff effs a
                              │
                              ▼
          resident model + fenced-Haskell interaction
           define · inspect · evaluate · spawn · complete
                              │
                         live value a
                              │
                              ▼
              validate · install · invoke · retain
```

Haskell controls typed domain deliberation and the type of its result. Rust may
initiate a serialized advisory session for an abnormal lifecycle fact not
already observed by Haskell. The model may use several rounds and many Haskell
evaluations before acknowledging it; the owner then returns to normal
scheduling.

## 2. Actor components

The Rust runtime maintains the following logical record. This is a semantic
inventory, not a required Rust struct layout.

| Component | Purpose |
|---|---|
| `ActorId` and incarnation | Stable routing identity plus stale-handle fencing |
| mailbox | Queued calls, casts, replies, and system events |
| actor program continuation | The currently running or parked fixed harness |
| program snapshot | Current persistent declarations, bindings, and installed behavior roots |
| model context | Provider thread, exact transcript prefix, compaction state, and usage |
| effect policy | Actor-local handlers, allowed request families, and suspension policy |
| actor interpreter | Rust handlers and grants enforcing that policy for this actor |
| execution principal | Identity installed while this actor's Haskell runs |
| capability grants | Owned, delegated, revoked, and fork-policy metadata |
| runtime resource scope | Parked frames, handles, cancellation state, and live roots |
| durable namespace | Explicit access to the existing JSON get/put backend |
| supervision state | Lifecycle owner, children, terminal records, advisories, stop reason, and deadlines |

The mailbox and Haskell program are sequential for one actor. Concurrency is
expressed by several actors and asynchronous actor operations between them.
It never re-enters one actor's Haskell session. This preserves the
single-writer machine-session constraint and the one-conversation ordering a
provider context requires.

The initial topology keeps cooperating actors on one shared machine session.
That is what makes structural sharing and arbitrary live-value delivery
possible. A machine boundary is explicit and initially admits only encoded
values.

The initial scheduler uses one FIFO ready queue per machine session. A lease
covers one Haskell run segment and is released whenever the actor suspends for
a model response, actor reply, or external operation. Newly ready actors join
the tail. This provides deterministic cooperative ordering, not preemptive
fairness: a non-suspending segment can monopolize that machine. Rust requests
cancellation at available safepoints and reports the limitation; workloads
needing hard isolation use separate machine sessions. All JIT admission stays
on the existing checkout path.

## 3. Ownership boundary

### Rust owns

- actor and incarnation identifiers;
- actor registry and mailbox lifetime;
- scheduling and admission to a shared machine session;
- model-provider threads, exact context snapshots, compaction, and cache
  accounting;
- one Rust effect interpreter per actor, routing nominal request constructors
  under that actor's policy and execution principal;
- Haskell compilation routing and machine-session checkout;
- continuation and live-root custody;
- capability registration, delegation, revocation, and caller checks;
- timeout, cancellation, shutdown, and supervision propagation;
- network mounts and provider transport;
- durable namespace allocation and external resource cleanup;
- authoritative structured actor events and resource-growth accounting.

### Haskell owns

- actor protocols and their result types;
- the fixed effect vocabulary selected when an actor incarnation starts;
- fixed control flow, state-machine structure, and domain invariants;
- deciding when and why to deliberate;
- installing or rolling back model-authored behavior;
- recursive organization, including which actors to create and how to combine
  their results;
- project-specific verification and acceptance policy;
- ordinary live state captured in immutable values and recursive closures;
- explicit reads and writes of durable facts.

### The model owns no runtime mechanism

The model writes Haskell within the interface exposed by its actor. It does not
parse process arguments, choose filesystem layouts, maintain actor registries,
construct provider envelopes, assign identifiers, serialize internal values,
or implement scheduling loops.

### Surface-selection rule

The Rust kernel may be comprehensive; the Haskell DSL should not be. It is an
interaction language for an LLM working through a persistent GHCi-style
environment.
Prefer a few orthogonal, success-shaped operations, strong types, familiar
names, and useful introspection. Do not mirror every registry transition,
failure variant, transport option, or lifecycle state into Haskell merely
because Rust represents it.

Expose a runtime distinction only when authored Haskell must make a stable
domain-policy choice, or when the distinction materially improves the model's
ability to reason, recover, or compose typed behavior. Otherwise Rust handles
it and supplies concise Developer context when model judgment is needed.

### Primary model interaction

Fenced Haskell in an assistant response is the primary execution protocol, not
a transitional encoding. A response may contain prose and any number of
fenced `haskell` or `hs` blocks. The blocks execute in source order against the
actor's persistent environment; later blocks observe declarations and bindings
committed by earlier blocks. Prose and other fence languages do not execute.

This keeps the model's main activity in its native text channel, permits
several GHCi-like steps in one response, and avoids wrapping Haskell source in
JSON tool arguments. Compile diagnostics, execution receipts, suspension
state, and completion contracts are returned as conversation context for the
next response. External tools may still exist for genuinely separate
operations; evaluating the actor's Haskell is not modeled as one.

The common agent-session executor owns this response-to-block-to-resident-run
loop. Deliberation, startup, and advisory provide different typed goals and
settlement rules but do not grow separate parsers or execution engines.

### Actor events and observability

The actor kernel emits one neutral event stream rather than a second logging
mechanism beside the existing durable harness journal. Each event carries the
exact actor incarnation, per-actor sequence, ownership and causality context,
and relevant turn, block, call, suspension, or terminal identity. The initial
event vocabulary covers lifecycle, ownership, mailbox settlement, provider
turns, extracted Haskell, compilation and execution, suspension and resumption,
advisories, compaction, capability refusal, failures, and resource counters.

Events record authoritative facts, but the journal is not the mutable actor
registry or a serialization of the live heap. Closures, continuations, roots,
and mailboxes remain runtime-owned and appear only through opaque identities
and lifecycle facts. Transcript reconstruction, ownership-tree views,
per-actor timelines, streaming, usage accounting, and GUI presentation are
folds over the neutral stream. Developer advisories are provider inputs derived
from runtime facts, not substitutes for the event record.

### Provider roles

Rust appends actor lifecycle, ownership, tracking, fork, exit, and capability
facts as Developer messages. Typed startup and task prompts authored by the
Haskell specification use the User role. If a runtime fact changes during an
active model round, Rust queues it for the next legal provider boundary. This
is provider-context management, not a generated Haskell declaration or binding
rewrite.

A legal boundary starts a new provider response; Rust cannot inject a message
into an inference already generating. With the Responses API, the runtime
continues the actor's conversation and supplies the new fact as an actual
Developer-role input item. It does not use the request-local `instructions`
field as durable conversation state; any stable base instructions carried in
that field are re-sent on every response request. This follows the Responses API contracts
for [message roles](https://developers.openai.com/api/docs/guides/text#message-roles-and-instruction-following)
and [conversation continuation](https://developers.openai.com/api/docs/guides/conversation-state#passing-context-from-the-previous-response).

## 4. Fixed structure with dynamic behavior

Reusable harnesses such as OODA, research, review, or revision loops are
ordinary Haskell libraries. They may expose typed points at which the actor's
model can supply judgment or new code. The authored loop fixes the replacement
interface, validation, and rollback policy; the model may define any private
types or helpers needed to satisfy it. Concrete Haskell patterns live in
[haskell-surface.md](haskell-surface.md#8-self-improving-behavior).

This is the central abstraction boundary: dynamic model-authored work happens
inside typed holes in a fixed program, while the values produced there can
change future behavior.

## 5. Actor protocols and calls

An actor protocol is normally an indexed GADT:

```haskell
data Reviewer result where
  Review  :: CandidateChange -> Reviewer ReviewFindings
  Improve :: CandidateChange -> ReviewFindings -> Reviewer CandidateChange
```

An `AgentRef Reviewer ReviewerExit` can then be used without a JSON schema.
The concrete typed operations are defined in
[haskell-surface.md](haskell-surface.md).

The request and result may contain closures, actor references, or opaque
capabilities. The Haskell type relates each request constructor to its result.
Rust routes an owned live value and reply obligation; it does not understand
the domain protocol.

`call` executes the target's handler as the target actor. Directly invoking a
function received from another actor executes it as the current actor. That
distinction is the basis of the authority model described in
[live-values-and-authority.md](live-values-and-authority.md).

`call` has one result and one settlement. A successful `cast` means mailbox
acceptance; later target failure does not retroactively undo delivery. A
streaming or multi-result call is deferred until a real protocol requires one.

Lifecycle failures are not values in every domain protocol. A dead or stale
target, rejected synchronous cycle, machine-boundary mismatch, cancellation,
or target exit before reply prevents the effect from producing its ordinary
success type. Rust mechanically parks or retries conditions its interpreter
can resolve without model judgment. If the exact operation remains
unsatisfiable, the actor terminates with a retained structured failure.

An actor remains non-reentrant while a synchronous `call` is outstanding.
Incoming application messages stay queued; cancellation and supervision
events remain serviceable by Rust. The runtime should track synchronous wait
edges and reject `A -> B -> A` call cycles with structured runtime failure
rather than hang forever or silently run a second handler against actor-local
state. That failure is Rust-owned lifecycle state, not a constructor added
to the domain protocol.

### Failure settlement and lifecycle advisory

When an actor effect cannot complete, Rust classifies the concrete failure and
consults the operation interpreter. The ordinary Haskell DSL neither receives
a universal failure sum nor selects policy ad hoc. Conditions such as
backpressure, bounded transport retry, or temporary retained-record access are
handled mechanically by Rust and do not spend an inference turn. Mechanical
retry resumes Haskell only after the exact operation succeeds.

If the operation cannot produce its exact result, Rust records the failure and
provenance, settles the unsatisfiable continuation, invokes the actor's typed
shutdown hook at a safe boundary when possible, performs authoritative
cleanup, and terminates the actor. The immutable `ActorExit` retains the
failure for exact `awaitExit`; an otherwise-unobserved abnormal exit advises the
owner. No final inference session runs in the dying actor.

The runtime never synthesizes the missing success value, retries through model
judgment, nominates a replacement actor, or resumes an unsatisfiable
continuation. Dead or stale targets, exhausted mechanical retry, permanent
machine mismatch, rejected call cycles, and cancellation are terminal.

`call` therefore resumes only after the exact original operation replies.
`cast` returns only after real mailbox acceptance; knowingly dropping a
message is never successful `cast`.

Normal target termination is not a failed `awaitExit`: it returns the retained
`ActorExit exit`. Temporary failure while reading that exact terminal record
is retried mechanically. A stale or invalid exact-incarnation reference is
terminal; `awaitExit ref` never substitutes another actor, even one with the same
`api` and `exit` types. After observing an exit, authored Haskell or an advisory
session may explicitly start a successor.

Long-lived or failure-prone delegated work should therefore use explicit
supervision: start a child, observe its exact typed exit with `awaitExit`, and start
a new child if policy calls for another attempt. That is ordinary Haskell
control flow assisted by the resident model, not transparent runtime replay.
Synchronous `call` remains appropriate where target death genuinely makes the
current obligation unsatisfiable.

Advisory creation is linearized at the child's terminal transition. If the
owner already has an active `awaitExit` for that exact child, the typed exit settles
the wait and suppresses an advisory. If the owner has a pending call to that
child, the call's terminal failure records the exit and suppresses a duplicate
advisory; termination of the caller is independently visible to its owner.
Otherwise an abnormal or unexpected exit creates exactly one advisory keyed by
owner, child incarnation, and terminal sequence. A later `awaitExit` does not
retract an advisory already created.

The advisory runs at the next quiescent boundary. Its Developer message may
cause the model to inspect state, start a successor, message another actor, or
simply acknowledge it. This does not construct a Haskell event or resume a
Haskell continuation. Normal completion and routine owner-requested
cancellation are quiet.

One actor still has at most one model turn. Events arriving during inference
are queued in order and may be coalesced into one factual Developer update.
Events arriving during active Haskell execution wait until it parks or
completes; the runtime never re-enters that session.

Each advisory has a separate absolute budget of four provider responses.
Exhaustion acknowledges and closes that advisory, records the condition, and
returns the owner to normal scheduling; it never terminates the owner. Several
events may share one presentation, but acknowledgment records the exact set of
advisory keys and never collapses their identity. Events arriving during
an advisory remain queued and may join its next presented batch.

Rust retries transient provider errors with bounded backoff, then terminates
the actor and reports the failure to its owner.

Ordinary deliberation, startup, and advisory all run through one Rust-owned
agent-session executor. They configure that executor
with an initiator, provider-role inputs, live bindings, permitted completion
actions, response budget, and exhaustion behavior. Their settlement contracts
differ, but they do not get separate provider loops, fenced-block runners,
schedulers, or conversation-continuation paths.

## 6. Actor construction

The initial runtime has two construction operations, not a matrix of modes:

| Origin | Model context | Haskell environment | Control continuation | Use |
|---|---|---|---|---|
| spawn | New conversation | Sealed base environment plus the specification's program image | Typed startup program | Independent actor or fresh judgment |
| fork | Exact shared transcript prefix | Exact shared program snapshot | Cloned at the fork point | Cheap parallel reasoning with full memory and provider-cache reuse |

Reusing an exported `ActorSpec` or program image is ordinary spawn, not a
third construction mode. Resuming an existing actor is scheduling, not
construction. “One-shot,” “delegate,” “reviewer,” and similar roles are
Haskell library patterns plus capability grants, not runtime presets. Every
new actor receives a new identity, mailbox, runtime resource scope, and
supervision entry.

Spawn creates one actor. Structural fork may fan out several children from one
atomic fork point; in that operation alone, handles are published only after
every requested child is ready. If one forked child fails before publication,
Rust terminates the other unpublished branches and settles the fork as failed.
Ordinary Haskell may start several independent actors without a second generic
batch-construction API.

`startActor` is the ordinary prompted constructor. Its typed startup value is
distinct from the actor's mailbox protocol. The specification first prepares
an authored `boot` value, then runs its sole startup agent session as a
User-role `Deliberation boot initial`, and finally combines `boot` and
`initial` in Haskell to install the `ActorProgram`. Rust supplies role and
transport, not prompt content or control structure. Preparation and
installation run in a restricted startup effect surface that cannot
deliberate, so initialization cannot accidentally nest model turns.

The validated installed `ActorProgram` is the OTP-style readiness point; no
`AgentRef` escapes before it. Rust mechanically retries only initialization
conditions the interpreter owns. Any remaining failure or pre-readiness
cancellation cleans up the incomplete actor and terminates the caller whose
start obligation cannot be satisfied.

A child can terminate between readiness and publication to the caller. The
published `AgentRef` then names a dead exact incarnation. `call` and `cast`
cannot use it, but `awaitExit` returns its retained terminal result.

### Fresh spawn

A fresh actor receives only:

- the sealed system vocabulary;
- the explicit actor program image;
- the explicit seed and initial request;
- explicitly granted capabilities.

It does not inherit the parent's conversation, scratch bindings, unrelated
declarations, or ambient authority. A dynamically invented child protocol is
still possible: the child specification carries the transitive declarations,
interface metadata, and live behavior roots required to deploy that program.
Deploying a specification is not ambient inheritance.

### Fork

A fork structurally shares two immutable prefixes:

```text
provider transcript ── shared prefix ──┬── parent suffix
                                      └── child suffix

Haskell snapshot   ─── shared root ────┬── parent generations
                                      └── child generations
```

The provider prefix must remain byte-for-byte identical so the already-proven
context-cache behavior remains available. The Haskell snapshot must include
live bindings as well as declarations; otherwise the copied transcript could
refer to names that do not exist in the child.

The provider prefix, Haskell snapshot, and control continuation form one atomic
fork point. If the fork occurs while executing a response's Haskell blocks,
the shared transcript already includes that assistant response and each branch
continues from the exact cloned Haskell control point. Rust must not pair a
transcript that mentions a successful definition with the earlier program
snapshot, append execution receipts for work that did not occur in that
branch, or mutate a frozen `ContextRef` during later compaction.

Fork also clones the actor's control continuation. The low-level operation has
a parent/child result analogous to process fork. The ordinary structured
wrapper consumes that distinction internally: the parent receives child
handles, while each child enters a typed startup continuation supplied at the
fork point. The parent receives no handle until that child signals readiness.

Runtime-issued references to other continuations are different from the
control continuation being cloned. Reply obligations, pending-call handles,
joins, and parked-request references remain parent-owned and fail with a typed
`InvalidAfterFork` failure if copied child code uses them. Rust does
not rewrite or delete Haskell bindings. It enforces the rule in the actor interpreter and
appends a Developer message at the first legal provider boundary after the
shared prefix and cloned Haskell segment, describing which runtime references
are invalid.

Capabilities apply their own fork policy. Structural sharing of a closure does
not automatically register the child as an allowed caller. The child receives
its own interpreter instance; handler state and grants are shared, cloned,
rebound, or withheld by each Rust component's fork policy. There is no parallel
runtime effect-row ABI: GHC owns row compatibility, while the interpreter owns
nominal request authorization.

## 7. Program images and snapshots

A program image has two jobs:

1. provide executable roots for the actor's fixed program and installed
   behavior;
2. make the relevant types, declarations, and documentation visible to the
   actor's model-facing Haskell environment.

A closure alone satisfies the first job but not the second. Therefore a
dynamically constructed `ActorSpec` must retain an exact declaration view
alongside its live roots. That view has an explicit model-visible export
membrane: fresh spawn does not inherit the defining actor's ambient binding or
declaration namespace merely because the specification was created there.

`Program image` is a semantic bundle, not a mandate for another registry,
compiler cache, or root ledger. The initial same-machine representation should
compose mechanisms Tidepool already has:

- a rooted compiled entry closure, executed through the resident machine's
  existing suspension-capable entry path;
- exact `SessionModule`/interface identities plus an explicit set of exports
  made visible to the child's fenced-Haskell environment;
- declaration source and documentation retained only for inspection and
  provenance; and
- the actor descriptor's fixed interpreter policy and handler installation
  recipe.

The resident code arena owns executable code, the value-handle ledger owns
roots while they are in transit, and the actor's resource realm owns deployed
roots. A program image must not create parallel ownership for any of them.
Phase 0 may add a small deployment record tying these identities together, but
only if the vertical spike reveals an invariant with no existing home.

Deploy the exact compiled declaration identities; do not replay equivalent
source into a new module and pretend the resulting types are equal. Import the
selected exports from those exact modules into the fresh actor environment.
A dynamic protocol value created by the parent and a handler compiled for the
child must refer to the same type and constructor identities.

Fresh spawn is not lexical-scope child creation. The current session scope tree
is the correct substrate for forked visibility, but its value plane walks the
live parent chain. Using `mint_scope(parent)` for fresh spawn would therefore
leak later parent bindings. A fresh actor begins from the sealed machine base
and receives only the image's exact module exports, rooted entry closure,
startup value, and granted capabilities.

Snapshots are immutable. A successful declaration or binding creates a new
tip. Forks point at an existing tip, and later definitions diverge. Tidepool's
declaration plane already freezes a parent's generation when a child scope is
minted. Its current value plane does not: lookup still walks mutable ancestor
frames. Structural fork must add an immutable binding snapshot with root leases
before claiming the same property for live values; ordinary scope ancestry is
not that snapshot.

Before a spawn or fork returns, the new actor must own leases for every live
root in its deployment or snapshot. Actor termination must not invalidate a
live value whose root ownership was already transferred to another actor.

## 8. State and persistence

There is no privileged domain `State` and no automatic `State -> Text`
rendering step.

An actor can hold:

- live state in ordinary Haskell values and closures;
- durable JSON facts behind explicit get/put operations;
- external state behind opaque capabilities such as a worktree handle;
- model memory in its Rust-owned accumulating conversation.

Durable reads are live operations at the point the Haskell program needs the
fact. The runtime does not take a whole-domain snapshot and splice it into a
system prompt before every deliberation.

In the first implementation, a process restart preserves only explicitly
stored JSON and external resources recoverable by their trusted interpreters.
Live closures, parked continuations, Haskell bindings, and provider contexts do
not survive. Restart tolerance is not required for the initial actor model.

This does not preclude a later program-image or model-context persistence
scheme; it prevents that speculative work from contaminating the first API.

## 9. Supervision and shutdown

Actors form one lifecycle ownership tree rooted in the runtime. Starting or
forking an actor places it under the caller by default. `AgentRef` is a callable
reference, not an ownership token; copying it does not reparent the actor.

The child specification chooses the startup-input type and successful-exit
type. Runtime failures, cancellation, and forced shutdown remain typed system
exit variants around the successful payload. An abnormal or unexpected child
exit never automatically kills its owner. Rust retains immutable terminal
metadata for observation through `awaitExit (AgentRef api exit)`; a successful
Haskell exit value remains in the managed cell shared by copies of that exact
reference. Abnormal failure and forced or unexpected cancellation either
settle an already-observing wait or call, or schedule the single keyed advisory
defined above. Normal
completion and routine owner-requested cancellation are quiet. Unexpected
exits do not require a heterogeneous Haskell system-event type.

`AgentRef` is conceptually a copyable routing identity paired with a shared,
typed, single-assignment Haskell exit cell. Completion publishes the successful
value to that cell before Rust records the terminal transition and wakes
waiters. Copies therefore retain arbitrary exits, including closures, through
ordinary Haskell reachability; the actor registry never owns a live exit root.
The machine session keeps the small terminal tombstone, and multiple waits read
the same cell. Reaping execution resources cannot erase either fact. Temporary
failure reading the tombstone may mechanically retry the exact wait. A stale
or invalid reference and cancellation of the waiter are terminal; ordinary
target termination returns normally.

When an owner terminates for any reason, Rust recursively terminates and reaps
its owned subtree. This is a lifecycle rule, not failure propagation in the
other direction: a child crash still only notifies its owner. Detachment,
reparenting, and adoption are plausible later extensions but are absent from
the initial API. Restart or an effect-stack change creates a new actor
incarnation and a new `AgentRef`.

Termination first queues a typed shutdown event for cooperative Haskell
cleanup. Rust remains responsible for eventual forced termination and all
external-resource cleanup. The initial watchdog is twelve hours, configurable
per deployment. It is deliberately a leak backstop rather than an interactive
timeout; measurement may justify changing the default later.

The shutdown hook runs in a restricted lifecycle effect surface and cannot
deliberate; shutdown is not an implicit route to a final model session.

The runtime should borrow this useful shape from managed Haskell actor
libraries without copying distributed-process machinery, `Serializable`
constraints, dynamic untyped handler lists, or distributed node concerns.

Shutdown has two coordinated halves:

- Rust stops admission, cancels or drains children, revokes capabilities,
  settles reply obligations, and closes the runtime resource scope.
- Haskell receives a typed stop reason at a safe boundary and may record
  durable facts or release domain resources through its granted capabilities.

Forced cancellation cannot depend on Haskell running cleanup. Rust remains the
ultimate owner of every external resource.

## 10. Required invariants

1. One actor has at most one active turn of any kind: authored Haskell,
   resident fenced-Haskell evaluation, provider inference, or mailbox handling.
2. Every Haskell fragment runs with an explicit execution principal.
3. A copied closure never grants authority merely because it captured an
   opaque capability.
4. Every mailbox value and reply has one observable live-root owner at each
   lifecycle transition.
5. A fresh actor sees no ambient parent declarations, bindings, transcript, or
   grants beyond its deployed specification.
6. A fork sees the exact snapshot it claims to fork, never a later sibling or
   a regenerated approximation of the model prefix.
7. Every deliberation has a statically fixed answer type.
8. Scratch definitions do not become installed actor behavior implicitly.
9. Actor scheduling never creates a second machine-session ownership
   mechanism or bypasses checkout fencing.
10. JSON serialization is never required for same-machine actor communication.
11. Fork points pair one immutable provider prefix, Haskell snapshot, and
    cloned control continuation.
12. External actions are reachable only through the actor kernel and checked
    capabilities; the model-facing environment exposes no ambient `IO`, FFI,
    or unsafe escape hatch that bypasses the execution principal.
13. One actor incarnation has one fixed Haskell effect vocabulary and one
    actor-local interpreter policy. Rust routes by nominal request identity,
    never by union position or a duplicated effect-row ABI.
14. Abnormal or unexpected child exit is observed through an active exact
   obligation or one keyed advisory, and never kills the owner implicitly.
15. No actor reference escapes before startup readiness.
16. Owner termination recursively terminates every actor in its owned subtree.
17. Haskell enters a shared machine session only through its existing checkout;
    actors are logically concurrent but not parallel within that machine.
18. Successful exit values live in the shared Haskell cell carried by exact
    `AgentRef` values; the Rust registry retains terminal metadata, never a
    second live-value root.
