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

Each actor incarnation fixes one concrete `Eff` stack in its entry module:

```haskell
type AgentEffects =
  '[ Actor
   , Deliberate
   , RepoRead
   , Review
   ]

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

The effect stack is fixed for the incarnation. The model may add ordinary
types, declarations, values, protocols, and behavior, but cannot redefine the
stack. Adding or removing an effect class creates a new actor incarnation and
a new `AgentRef`.

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
Rust handler. A trusted launch policy decides which nominal requests this
incarnation can actually interpret and which resource grants back them. The
first `startActor` slice must settle how a dynamic Haskell `ActorSpec` carries
or selects that opaque policy without reflecting its type-level row into Rust.

The stack determines which operation classes Haskell can express. Opaque
handles and principal-scoped grants still authorize particular resources at
runtime.

## 3. Actor specifications are Haskell values

An actor specification packages an entry point, startup type, mailbox
protocol, successful exit type, fixed effect vocabulary, and program image. A
possible surface is:

```haskell
data AgentRef api exit

data ActorSpec startup api exit where
  ActorSpec
    :: { prepare
           :: startup
           -> StartupM childEffs boot
       , startupSession
           :: Deliberation boot initial
       , install
           :: boot
           -> initial
           -> StartupM childEffs (ActorProgram api exit)
       , shutdown
           :: ShutdownReason
           -> ShutdownM childEffs ()
       }
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

This is a conceptual surface: ordinary Haskell existential packaging hides
`boot`, `initial`, and `childEffs`; it does not imply a reflected Rust row ABI.
The program image and launch authority are opaque runtime-backed values, not a
second Haskell encoding of handler layout. `StartupM` and `ShutdownM` denote
restricted lifecycle surfaces that cannot deliberate; whether they survive as
newtypes or become constrained `Eff` rows is intentionally left to the first
`startActor` vertical slice.

`startActor` runs `prepare`, gives `boot` to the specification's sole typed
startup agent session, and passes the resulting `initial` value to `install`.
It does not return a reference until the resulting `ActorProgram` is validated,
installed, and accepting application messages. The startup,
initialization-artifact, and successful-exit types need not resemble the
mailbox protocol. A non-prompted constructor is deferred until a real actor
needs one.

An owner can have children with unrelated exit types because exit observation
is tied to each `AgentRef api exit`. `awaitExit` returns that child's exact exit
type. Unexpected lifecycle events do not enter a heterogeneous Haskell system
inbox; Rust instead schedules a Developer-triggered advisory turn in the
owner's model context at the next quiescent provider boundary.

Validating and installing the returned `ActorProgram api exit` is the readiness
linearization point. The model produces a typed initialization artifact, not
the actor's entire control flow. Authored Haskell decides how that artifact
configures the fixed program. Rust invokes the specification's shutdown hook
at a safe boundary for cooperative shutdown without depending on it for hard
cleanup. The hook may use explicitly allowed authoritative effects but cannot
open a final agent session.

Publication and liveness are separate facts. A child may terminate after
signaling ready but before the caller resumes. The returned reference still
names that exact, now-dead incarnation. `call` and `cast` cannot use it, while
`awaitExit` reconstructs its `ActorExit exit` from the terminal record and shared
exit cell. Cancellation before readiness publishes no reference and terminates
the caller whose start obligation cannot be satisfied.

An `AgentRef api exit` contains an opaque routing identity and a shared,
single-assignment cell for a successful `exit`. Completion fills the cell
before publishing the terminal lifecycle transition. Copies and active waits
therefore retain the exit through ordinary Haskell reachability, and repeated
waits observe the same value even when it is a closure. Rust retains only the
immutable terminal metadata needed to distinguish completion, failure, and
cancellation. A stale or invalid reference and cancellation of the waiter are
fatal; target termination returns normally.

The runtime need not force every actor into callback records. A mailbox service
can use a `serve` library combinator inside an ordinary recursive Haskell
program. An OODA loop and a one-shot worker can use different authored control
structures while sharing the same Rust lifecycle.

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
terminates the actor rather than fabricating a domain result. There is no
model-facing failure session or retry action. A dead target cannot become
callable again, and the model cannot nominate an arbitrary same-typed actor.
Mechanical retry resumes the parked continuation only after the exact original
operation succeeds; an unsatisfiable continuation never resumes.

Actor calls, startup, casts, authoritative reads, commands, reviews, and
capabilities never offer arbitrary result construction. A failed `cast` must
achieve real mailbox acceptance or terminate. `awaitExit ref` always means the
exact incarnation named by `ref`: target termination returns its recorded
exit, transient access may retry, and an invalid reference is fatal.
After observing an exit, ordinary Haskell or an advisory can start a new actor
explicitly without pretending it is the actor that exited.

Failure-prone delegated work should prefer the supervised shape: `startActor`,
`awaitExit`, inspect the exact typed exit, and explicitly start another child when
appropriate. The resident model can make that decision and write new Haskell
inside the ordinary control flow. The initial runtime does not transparently
replay a dead actor's in-flight work.

Rust retains structured low-level failures internally. The model-facing DSL
does not mirror every operation with `tryStart`, `tryCall`, `tryCast`, and
`tryWait`.

Rust retains the terminal failure in `ActorExit`, runs the authored shutdown
hook when possible, and performs authoritative cleanup. No final provider
request runs in the dying actor. Multi-result calls remain deferred until a
concrete protocol shows that ordinary calls and messages are insufficient.

An abnormal child exit opens an advisory only when no active `awaitExit` or failed
call obligation already observes that exact terminal transition. The model may
act through its ordinary tools and then calls a scoped `ackLifecycle` operation
with the exact presented advisory keys. This wakes inference at a quiescent
actor boundary, but it neither resumes nor re-enters the actor's authored
Haskell continuation. Several events may be presented together without losing
identity. An advisory gets four provider responses. Exhaustion closes and
records it, then returns to normal scheduling without terminating the owner.

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
- `:capabilities` — available operations and delegation limits, without
  exposing forgeable identifiers.

These are views over runtime truth, not a second mutable registry.

## 6. One lexical scope per actor

An actor has one persistent lexical scope. There is no separate “fixed harness
scope” and “model workbench scope.” Ordinary Haskell shadowing is the evolution
mechanism:

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

A fixed harness can ask for a replacement behind a stable interface:

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
`Controller`. The harness can test it, install it, and retain the predecessor.
This is self-extension through typed values rather than source-file mutation.

## 9. Dynamic protocols and child actors

The model can define a new protocol and actor specification during a
deliberation:

```haskell
data Reviewer result where
  Review  :: CandidateChange -> Reviewer ReviewFindings
  Revise  :: CandidateChange -> ReviewFindings -> Reviewer CandidateChange

reviewerSpec :: ActorSpec ReviewStartup Reviewer ReviewerSummary
reviewerSpec = ...
```

It can then spawn and call the child with ordinary types:

```haskell
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

The canonical fork contract is in
[architecture.md](architecture.md#fork). Its low-level Haskell discriminator
is conceptually:

```haskell
data ForkSide seed api exit
  = Parent [AgentRef api exit]
  | Child seed
```

The ordinary wrapper hides this discriminator: the parent gets ready child
references and each child enters its typed startup branch. Parent-owned
continuation references remain present as Haskell values but child use enters
terminal `InvalidAfterFork` failure.

## 11. Serial execution and supervision

[architecture.md](architecture.md#9-supervision-and-shutdown) owns lifecycle
semantics. The Haskell consequences are small: one actor is non-reentrant;
`AgentRef api exit` supports repeatable typed `awaitExit`; abnormal exits create
advisory model turns, not Haskell events; and `ActorSpec` supplies a
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

The dynamic code may invent `api`, but the fixed harness only needs to know how
to spawn the packaged specification and run the packaged client. This is a
useful design constraint: self-extension produces typed artifacts behind
stable membranes instead of spraying source strings through the runtime.

## 13. Verification pattern

The default development harness should encourage—and where appropriate
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
remain distinguishable from model-authored claims. This is a prompt and
harness policy by default, not a universal evidence ladder in the actor kernel.

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
