# Haskell and model interaction surface

## 1. Design target

The Haskell side should feel like a small actor and state-machine library, not
an encoding of Rust orchestration. The model should feel like it is working in
a real persistent GHCi-style environment whose current task has a required
type.

This is an LLM-use interface, not a comprehensive systems API. Optimize for a
small vocabulary the model can reliably discover, remember, inspect, and
compose. Completeness, exhaustive failure representation, and operational
knobs stay in Rust unless authored Haskell has a demonstrated policy decision
to make. Avoid paired low/high-level verbs and option records by default.

The exact names in this document are sketches. The contracts and separation of
concerns are the decisions.

## 2. Actor-local effect stacks

Each actor incarnation fixes one concrete `Eff` stack in its entry module. The
kernel contributes the protocol-indexed local-control effect; the selected
runtime contributes the capability row:

```haskell
type AgentCapabilities =
  '[ Actor
   , Deliberate
   , RepoRead
   , Review
   ]

type AgentEffects =
  ActorLocal AgentProtocol AgentExit ': AgentCapabilities

type AgentM = Eff AgentEffects
```

That alias is local deployment configuration, not a library type. Reusable
functions do not mention it:

```haskell
route
  :: Members '[Actor, RepoRead] effs
  => WorkItem
  -> Eff effs Decision
```

Protocols remain pure. Reusable effectful functions use `Member`/`Members`
constraints and can be instantiated in any actor whose stack satisfies them.
A function already specialized to one actor's concrete stack remains
actor-local.

This is primarily an authoring rule taught in the actor's Developer guidance:
do not hardcode a concrete effect stack or its order in reusable code; state
the required capabilities with `Member`/`Members`. The runtime does not reject
cross-actor function values merely because they are effectful. GHC decides
whether the receiver's row satisfies their constraints, while Rust still
checks principal-bound handles and actor-local runtime authority when an
effect is interpreted.

Effects are an extensible Haskell design vocabulary, not a frozen global ABI.
Libraries may add a new effect when it represents a distinct algebra with its
own interpreter semantics; model-authored code may also define and eliminate
purely Haskell-local effects. What is fixed is one incarnation's residual row
at the Rust boundary. Adding or removing a Rust-interpreted capability creates
a new actor incarnation and `AgentRef`.

The entry module exports `AgentEffects` and `AgentM` through the actor's exact
source facade. Generated turn modules mention `AgentEffects` only as a Haskell
type alias, so GHC checks model-authored code against the actor's concrete row
without Rust learning its order or reproducing it as metadata. Rust neither
parses nor compares the alias; runtime authorization remains nominal and
interpreter-local.

Rust builds one actor-local interpreter for the incarnation. GHC checks effect
row compatibility; Rust does not duplicate that work as a positional ABI.
Instead, handlers recognize nominal request constructors and the interpreter
checks this actor's policy, grants, and caller identity. Forks create a new
interpreter instance under the child's principal. Different actor rows may
share one machine because union position has no Rust dispatch meaning.

Naming an effect in `AgentEffects` is not authority and cannot manufacture a
Rust handler. The actor's compiled entry fixes the Haskell row; the Rust actor
interpreter independently decides which nominal requests that principal and
its grants may perform. V0 fresh children inherit the composition-owned
interpreter policy of their owner. Explicit selection of a different child
policy waits for the first concrete capability protocol that needs it; it will
not be encoded as a reflected effect-row token.

The stack determines which operation classes Haskell can express. Opaque
handles and principal-scoped grants still authorize particular resources at
runtime.

## 3. Actor specifications are Haskell values

An actor definition is the ordinary Haskell record a model authors. Promotion
captures its exact code/source deployment image and returns an opaque,
deployable actor specification. This is one membrane, not separate static and
dynamic construction systems:

```haskell
data AgentRef api exit
data ActorProgram actorEffs api exit
data ActorSpec startup api exit
data LaunchGrant
data ActorLocal api exit a

data ActorDefinition startup api exit where
  ActorDefinition
    :: Members '[Deliberate, ActorLocal api exit] actorEffs
    => { startupSession
           :: Deliberation startup initial
       , install
           :: startup
           -> initial
           -> ActorProgram actorEffs api exit
       , modelExports
           :: [Text]
       , onShutdown
           :: ShutdownReason
           -> Eff actorEffs ()
       }
    -> ActorDefinition startup api exit

promoteActor
  :: Member Actor effs
  => ActorDefinition startup api exit
  -> Eff effs (ActorSpec startup api exit)

withLaunchGrant
  :: LaunchGrant
  -> ActorSpec startup api exit
  -> ActorSpec startup api exit

data ActorExit exit
  = Completed exit
  | Failed ActorFailure
  | Cancelled CancelReason

startActor
  :: Member Actor effs
  => ActorSpec startup api exit
  -> startup
  -> Eff effs (AgentRef api exit)

awaitExit
  :: Member Actor effs
  => AgentRef api exit
  -> Eff effs (ActorExit exit)
```

Ordinary Haskell existential packaging hides `initial` and `actorEffs` behind
the stable `ActorSpec startup api exit` surface. That is ordinary module/GADT
encapsulation, not a reflected Rust row ABI. `promoteActor` is the sole
program-image boundary. It captures the rooted definition, exact declaration
identities, and explicit model-visible export facade; models never assemble an
image record themselves. Project-authored actors are promoted at boot through
the same mechanism using their checked-in export manifest.

`modelExports` is a list of top-level head names, not arbitrary Haskell export
syntax. Promotion resolves it through GHC's export metadata against the exact
compile view that produced the definition. Selecting a type head carries its
GHC-derived constructor export shape; selecting a value carries that exact
declaration identity. Missing, shadow-drifted, or type-incoherent selections
fail promotion instead of silently exposing a later same-spelled declaration.

`withLaunchGrant` decorates deployment metadata without changing the program
image. Capability-owning modules create opaque recipes for their own
resources; there is no public generic grant record or policy enum.
`startActor` validates and redeems every recipe for the new child under the
caller's principal before startup. Copying the resulting specification still
transfers no authority.

`startActor` mounts `startup` as a live value, runs the specification's sole
typed startup agent session, and passes the resulting `initial` to the pure
`install` function. The startup session may evaluate fenced Haskell using the
child's fixed row. The lifecycle-phase interpreter refuses
`receive` and `forkActors` before program readiness; this is policy on the
same row, not a startup monad. Installation itself cannot deliberate or perform
external work because it is pure. The call returns only after the resulting
`ActorProgram` is validated, installed, and accepting application messages.
The startup, initialization-artifact, and successful-exit types need not
resemble the mailbox protocol. A non-prompted constructor remains absent until
a real actor needs one.

An owner can have children with unrelated exit types because exit observation
is tied to each `AgentRef api exit`. `awaitExit` returns that child's exact exit
type. Unexpected lifecycle events do not enter a heterogeneous Haskell system
inbox; Rust instead schedules a Developer-triggered advisory turn in the
owner's model context at the next quiescent provider boundary.

Validating and installing the returned
`ActorProgram actorEffs api exit` is the readiness linearization point. The
model produces a typed initialization artifact, not the actor's entire control
flow. Authored Haskell decides how that artifact configures the fixed program.
Rust invokes the specification's shutdown hook at a safe boundary for
cooperative shutdown without depending on it for hard cleanup. The hook runs
under the actor's normal row and principal, but the interpreter is in its
closing phase and refuses `Deliberate`, actor startup, and any other operation
not permitted during shutdown. Rust remains the final owner of cleanup if
Haskell cannot run.

Publication and liveness are separate facts. A child may terminate after
signaling ready but before the caller resumes. The returned reference still
names that exact, now-dead incarnation. `call` and `cast` cannot use it, while
`awaitExit` reconstructs its `ActorExit exit` from the terminal record and
shared exit cell. Cancellation before readiness publishes no reference and
terminates the caller whose start obligation cannot be satisfied.

An `AgentRef api exit` contains an opaque routing identity and a shared,
single-assignment cell for a successful `exit`. Completion fills the cell
before publishing the terminal lifecycle transition. Copies and active waits
therefore retain the exit through ordinary Haskell reachability, and repeated
waits observe the same value even when it is a closure. Rust retains only the
immutable terminal metadata needed to distinguish completion, failure, and
cancellation. A stale or invalid reference and cancellation of the waiter make
that `awaitExit` fragment unsatisfiable; target termination returns normally.

`ActorProgram` is a typed wrapper around one ordinary `Eff` computation. The
model-facing shape is conceptually:

```haskell
program
  :: Eff actorEffs exit
  -> ActorProgram actorEffs api exit

receive
  :: Member (ActorLocal api exit) effs
  => (forall result. api result -> Eff effs (result, next))
  -> Eff effs next

serve
  :: Member (ActorLocal api exit) effs
  => state
  -> (forall result. state -> api result -> Eff effs (result, state))
  -> Eff effs exit
serve state step = receive (step state) >>= \next -> serve next step
```

`ActorLocal api exit` ties the receiving protocol and successful exit type to
the row used by the installed program and its eventual `AgentRef api exit`.
Its raw effect constructors are kernel-private; the ordinary Haskell functions
are the public algebra. This is module encapsulation, not a magic runtime ABI.
Every request is interpreted under the current actor principal, so there is no
mailbox handle to forge, pass, or rewrite during fork.

The kernel module also uses one private `ActorLocal` readiness operation while
bootstrapping an `ActorProgram`. Its parked continuation is the installed
program root. The interpreter accepts it only from the program's resource
realm, never from a fenced model fragment. This keeps readiness in the same
ordinary effect algebra without exposing a public lifecycle token or adding a
Rust-side program registry.

`receive` suspends without polling, accepts one call or cast, runs the handler
under the actor's principal, settles the indexed result, and returns only the
next-state value. For a cast, the request has result type `()`. The runtime-
private reply obligation never enters authored Haskell, so the public surface
needs no affine token or separate `reply` operation. Handler failure settles
the caller through the ordinary actor-failure path.

`serve` is only a discoverable recursive library composition. A program that
may exit in response to a message uses `receive` directly and returns its
`exit` from ordinary Haskell control flow. An OODA loop and a one-shot worker
can therefore use different authored structures without creating runtime
actor modes.

The ordinary supervised-worker path is a discoverable library composition,
not another effect:

```haskell
runActor
  :: Member Actor effs
  => ActorSpec startup api exit
  -> startup
  -> Eff effs (ActorExit exit)
runActor spec startup = startActor spec startup >>= awaitExit
```

A one-shot job places its successful product in `exit`. `runActor` therefore
returns the exact success, failure, or cancellation needed for ordinary
Haskell—or the resident model—to decide whether to start another child.

Startup and shutdown hooks are useful, but Rust remains responsible for hard
cleanup when Haskell cannot run.

### Calls and terminal failure

Lifecycle failure is separate from a protocol's domain result:

```haskell
call
  :: Member Actor effs
  => AgentRef api exit
  -> api result
  -> Eff effs result

cast
  :: Member Actor effs
  => AgentRef api exit
  -> api ()
  -> Eff effs ()
```

The success types stay pleasant because lifecycle failure remains Rust-owned
instead of entering every domain result. The interpreter mechanically parks or
retries conditions it owns. If the exact operation still cannot complete, it
abandons that Haskell continuation rather than fabricating a domain result.
For the installed actor program, losing the continuation terminates the actor.
For a disposable fenced workbench fragment, the existing agent session gets a
structured failure receipt and may run different Haskell next round. There is
no separate failure session, retry action, or replacement actor, and an
unsatisfiable continuation never resumes.

`awaitExit ref` is different: target termination is its successful observation
and returns the retained exact `ActorExit`. An invalid reference makes the
invoking fragment unsatisfiable.
Failure-prone work should therefore use `runActor` or explicit
`startActor`/`awaitExit`, then start a new child in ordinary Haskell if policy
calls for it. The DSL has no mirrored `tryX` family or transparent replay, and
multi-result calls wait for a concrete protocol that needs them. Rust records
terminal failure, attempts the authored shutdown hook, and performs final
cleanup without a provider request in the dying actor.

An abnormal child exit opens an advisory only when no active `awaitExit` or
failed call obligation already observes that exact terminal transition. Its
workbench may execute fenced Haskell under the owner's principal, including
ordinary actor operations, and exposes a scoped `ackLifecycle` action for the
exact presented advisory keys. This wakes inference at a quiescent actor
boundary, but it neither resumes nor re-enters the actor's authored Haskell
continuation. The complete deduplication, budgeting, and provider-failure
contract is in
[architecture.md](architecture.md#failure-settlement-and-lifecycle-advisory).

## 4. Typed deliberation

The fixed program asks its resident model for a value of a known type:

```haskell
deliberate
  :: Member Deliberate effs
  => Deliberation input output
  -> input
  -> Eff effs output
```

A `Deliberation` describes the task, relevant presentation policy, and expected
output. The input is mounted as a live Haskell binding, not serialized merely
to place it in a prompt. The prompt may summarize the input for orientation,
while the authoritative value remains available to code.

Each call creates one agent session, possibly containing many model rounds,
against the actor's accumulating provider conversation. Completing the request
returns to the fixed Haskell continuation. Later deliberations reuse the same
model context and Haskell environment.

At the start of a deliberation, the agent session exposes bindings
conceptually like:

```haskell
goalInput :: input
complete  :: output -> Eff effs a
```

The actual completion mechanism can continue to use a typed suspending effect.
The important property is that GHC—not Rust-side JSON conversion—checks the
result against `output`.

## 5. The fenced-Haskell workbench

Fenced Haskell in ordinary assistant output is the actor's primary workbench.
It is deliberately not a provider tool call: Haskell source remains direct
text instead of a JSON-escaped argument, and one response may naturally mix
explanation with several executable steps.

Every fenced `haskell` or `hs` block executes, in order, against the same
actor-bound persistent environment. The Haskell-aware runner classifies
declarations, binds, expressions, and supported meta commands. Rust extracts
only explicitly tagged blocks and returns compile/runtime results as
conversation context; it does not parse Haskell argument syntax or infer
executable intent from prose.

Required behavior:

- successful top-level declarations persist immediately;
- successful bindings create persistent live roots;
- expressions may inspect or invoke existing live values;
- compile failures preserve previously committed items and return useful GHC
  diagnostics;
- an unsatisfiable effect abandons only the current disposable fragment,
  releases its temporary roots, and returns a structured receipt to the same
  agent session;
- declaration-only progress does not accidentally complete the deliberation;
- `complete expression` can succeed only when the expression has the required
  type;
- natural-language prose and non-Haskell fences never execute;
- later blocks in one response observe earlier successful commits;
- fenced execution remains available across all model rounds in the
  deliberation;
- model compaction preserves a concise inventory of important bindings and
  installed behavior.

Useful introspection should include familiar `:type` and `:info` plus a small
Tidepool surface such as:

- `:goal` — current required result type and mounted inputs;
- `:bindings` — live names, types, ownership, and abbreviated values;
- `:program` — installed behavior, exported actor specifications, definition
  generations, and provenance;
- `:capabilities` — available operations and grant limits, without
  exposing forgeable identifiers.

These are views over runtime truth, not a second mutable registry.

## 6. One lexical scope per actor

An actor has one persistent lexical scope. There is no separate installed-
program scope and model-workbench scope. Ordinary Haskell shadowing is the
evolution mechanism:

- old closures retain old references;
- future compilation sees the newest visible definition;
- siblings never see one another's new definitions;
- a fork begins at the exact selected snapshot;
- a fresh spawn receives only its deployed program image.

Core runtime types use sealed nominal identities. Their constructors are not
exported, so shadowing a name cannot forge an actor reference, capability, or
kernel continuation.

The actor entry module's effect-stack definition is also sealed for the
incarnation. Model-authored declarations extend the ordinary lexical scope;
they do not replace the row against which the actor interpreter was built.

## 7. Scratch definitions and installed behavior

Persistent GHCi environments accumulate scratch code unless the runtime makes
the distinction between experimentation and active program explicit.

Every successful definition may persist for the actor lifetime, but only
explicit roots constitute the active actor program:

- current behavior;
- exported actor specifications;
- named reusable helpers deliberately promoted by the actor;
- values retained for rollback;
- transitive dependencies of those roots.

Installation should retain provenance:

- defining actor and incarnation;
- declaration generation and source;
- dependency image;
- deliberation and model-round origin;
- checks or review attached to the installation;
- predecessor root for rollback.

The first implementation does not need executable-code unloading. It does need
accurate growth metrics, an intelligible `:program` view, and explicit active
roots so later compaction or machine rotation has a sound reachability target.

## 8. Self-improving behavior

An actor program can ask for a replacement behind a stable interface:

```haskell
data Controller observation action = Controller
  { decide
      :: forall effs
       . Members '[Deliberate, Act] effs
      => observation
      -> Eff effs action
  }

improve
  :: Member Deliberate effs
  => Controller observation action
  -> FailureHistory observation action
  -> Eff effs (Controller observation action)
```

The agent may define new private types and helpers while producing the next
`Controller`. The program can test it, install it, and retain the predecessor.
This is self-extension through typed values rather than source-file mutation.

## 9. Dynamic protocols and child actors

The model can define a new protocol and actor specification during a
deliberation:

```haskell
data Reviewer result where
  Review  :: CandidateChange -> Reviewer ReviewFindings
  Revise  :: CandidateChange -> ReviewFindings -> Reviewer CandidateChange

reviewerDefinition :: ActorDefinition ReviewStartup Reviewer ReviewerSummary
reviewerDefinition = ...
```

It can then spawn and call the child with ordinary types:

```haskell
reviewerSpec <- promoteActor reviewerDefinition
reviewer <- startActor reviewerSpec startup
call reviewer (Review candidate)
```

The fresh child's model context is empty, but its program image contains the
`Reviewer` declarations and behavior needed to understand and execute the
specification. The parent does not have to reduce the request to JSON.

The image imports the exact compiled declaration identity. Recompiling the
same source under a fresh module would create a different Haskell type and
would not make the parent's `Reviewer result` values compatible with the
child's handler.

Forking the same actor instead would additionally share the parent model's
transcript and full Haskell snapshot. That is useful for cheap exploration but
not a substitute for an independent reviewer.

## 10. Structured continuation fork

The canonical fork contract is in [architecture.md](architecture.md#fork).
The small public wrapper is conceptually:

```haskell
forkActors
  :: Members '[Actor, ActorLocal api exit] effs
  => NonEmpty seed
  -> (seed -> Eff effs exit)
  -> Eff effs [AgentRef api exit]
```

This is a useful intersection rather than duplicate actor machinery:
`ActorLocal` fixes the current incarnation's protocol and successful-exit
indexes, while `Actor` grants the outward ability to create actors.

The runtime-private operation still has a process-fork-like parent/child
discriminator. The wrapper consumes it: the parent receives ready exact child
references, while each child runs the supplied branch through its new
actor-local interpreter and publishes the returned `exit`. The child branch
never falls through into the parent's continuation. Parent-owned continuation
references remain present as Haskell values, but child use fails with
`InvalidAfterFork` under the ordinary fragment-settlement rule.

## 11. Serial execution and supervision

[architecture.md](architecture.md#9-supervision-and-shutdown) owns lifecycle
semantics. The Haskell consequences are small: one actor is non-reentrant;
`AgentRef api exit` supports repeatable typed `awaitExit`; abnormal exits create
advisory model turns, not Haskell events; and `ActorDefinition` supplies a
`ShutdownReason` handler for cooperative cleanup. Rust owns recursive subtree
termination and the twelve-hour watchdog.

## 12. Type staging boundary

A type invented after the fixed outer continuation was compiled cannot appear
retroactively in that continuation's static type. Dynamic code crosses back
through one of three honest membranes:

1. return a value of an already-known type;
2. return a closure behind an already-known interface;
3. existentially package the new type with the operations that eliminate it.

For example:

```haskell
data Specialist answer where
  Specialist
    :: seed
    -> ActorSpec seed api exit
    -> ( forall effs
          . Member Actor effs
         => AgentRef api exit
         -> Eff effs answer
       )
    -> Specialist answer
```

The dynamic code may invent `api`, but the enclosing program only needs to know
how to spawn the packaged specification and run the packaged client. This is a
useful design constraint: self-extension produces typed artifacts behind
stable membranes instead of spraying source strings through the runtime.

## 13. Verification pattern

The default development actor library should encourage—and where appropriate
require—this loop:

1. construct a candidate;
2. run authoritative automated commands against its exact revision;
3. fresh-spawn an independent review actor;
4. interpret both results in authored Haskell;
5. revise, accept, or escalate;
6. install behavior or integrate a worktree only through the owner's
   capability.

A model may author the strategy, but it cannot acquire integration authority
by returning a persuasive record. Runtime-observed command and actor provenance
remain distinguishable from model-authored claims. This is prompting and actor-
program policy by default, not a universal evidence ladder in the kernel.

## 14. What must leave Haskell

The actor migration is incomplete while authored Haskell still performs any of
these merely because the runtime failed to provide an abstraction:

- process argument parsing;
- environment-variable precedence;
- provider request/response shaping;
- actor identifier allocation or registry maintenance;
- mailbox polling and retry bookkeeping;
- filesystem layout for runtime state;
- JSON conversion for same-machine communication;
- global state rendering into every model prompt;
- manual model-context copying;
- continuation or GC-root custody;
- capability authorization based on string conventions.

Haskell may explicitly choose domain commands, keys, prompts, and policies.
Rust owns the fiddly mechanisms that make those choices safe and executable.
