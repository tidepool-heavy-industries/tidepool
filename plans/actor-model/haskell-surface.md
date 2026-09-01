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

The target author is a capable resident model with an accumulating
conversation, a persistent GHCi-style declaration and binding environment,
ordinary hidden reasoning space, and a Developer instruction that every
explicitly tagged `haskell` or `hs` fence executes. Design for fast typed
self-extension and compiler-guided repair, not for models that cannot follow
that execution contract.

Developer context carries only trusted harness policy and runtime-attested
facts. Render type, declaration, binding, capability, and lifecycle receipts
in compact Haskell-shaped text where possible, so they reinforce rather than
interrupt the model's typed working vocabulary. They remain context, not
secret source rewriting: actual values and authority are installed through
the Haskell/interpreter boundary, and executable fences retain their ordinary
explicit meaning. User messages carry tasks and domain requests.

The routine vocabulary should stay close to:

```haskell
deliberate
complete
startActor
runActor
call
cast
receive
serve
awaitExit
forkActors

ActorDefinition startup protocol exit
ActorRef protocol exit
ActorExit exit
```

Private readiness requests, reply obligations, deployment roots, Rust actor
identities, program-image records, and provider machinery are not additional
model-facing nouns.

Unless a fence is explicitly marked as pseudocode, the public names and types
below describe the landed Haskell surface.

## 2. Actor-local effect stacks

Each actor incarnation fixes one concrete `Eff` stack in its entry module. V0
runs that stack directly; Rust interprets nominal requests under the actor's
principal, grants, realm, incarnation, and lifecycle phase. Generated imports
and exports keep raw lifecycle, site, completion-settlement, and bridge
vocabulary out of routine model use, but do not grant or revoke authority:

The public library exposes these profile rows:

```haskell
type ReadOnlyEffects protocol =
  '[ ActorLocal protocol
   , Actor
   , Deliberate
   , FsRead
   ]

type ReadWriteEffects protocol =
  FsWrite ': ReadOnlyEffects protocol

type ActorEffects = ReadWriteEffects ActorProtocol
type ActorM = Eff ActorEffects
```

That alias is local deployment configuration, not a library type. Reusable
functions do not mention it:

```haskell
route
  :: Members '[Actor, FsRead] effs
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
a new actor incarnation and `ActorRef`.

The entry module exports `ActorEffects` and `ActorM` through the actor's exact
source facade. Generated turn modules mention `ActorEffects` only as a Haskell
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

V0 provides two experimental resident-Haskell profiles: `ReadWrite` and
`ReadOnly`. Each selects a concrete row and nominal interpreter policy. A `ReadWrite` actor
may start either profile; a `ReadOnly` actor may start only `ReadOnly`. Rust
validates this attenuation edge, while GHC checks the actor definition against
the selected row. The profile does not constrain native coding-agent tools or
provide process isolation. Within the resident effect machine, `ReadOnly`
excludes ambient write effects, but may still call
an explicitly supplied writer `ActorRef`. Actor-local declarations, bindings,
conversation growth, and behavior replacement are not ambient resource writes,
so a `ReadOnly` actor remains self-extending.

Profile membership, resource authority, and code identity stay orthogonal. A
profile limits which operation classes Haskell can express. The actor's
principal, grants, and opaque handles authorize concrete resources. The program
image says which code is deployed. Neither a profile name nor an
`ActorDefinition` grants authority by itself.

Export curation keeps substrate constructors out of the model's routine
vocabulary but is not the authority boundary. A later trusted Haskell lowering
layer may split public intent from kernel requests without changing the named
profiles, program images, actor API, or Rust's nominal dispatch.

The stack determines which operation classes Haskell can express. Opaque
handles and principal-scoped grants still authorize particular resources at
runtime.

## 3. Actor definitions are Haskell values

An actor definition is the ordinary Haskell record a model authors. Public
construction has one path: pass that definition to `startActor`. Exact-source
capture, live-root transfer, and facade compilation are
private steps inside that operation, not model-facing values or verbs.

```haskell
data ActorRef protocol exit

data EffectProfile protocol effs where
  ReadOnly  :: EffectProfile protocol (ReadOnlyEffects protocol)
  ReadWrite :: EffectProfile protocol (ReadWriteEffects protocol)

data ShutdownReason
  = ShutdownCompleted
  | ShutdownFailed
  | ShutdownCancelled

data ActorDefinition startup protocol exit where
  ActorDefinition
    :: { label          :: Text
       , effectProfile  :: EffectProfile protocol actorEffs
       , initialization :: startup -> Eff actorEffs initial
       , behavior       :: startup -> initial -> Eff actorEffs exit
       , visibleToChild :: [Text]
       , onShutdown     :: ShutdownReason -> Eff actorEffs ()
       }
    -> ActorDefinition startup protocol exit

data ActorExit exit
  = Completed exit
  | Failed ActorFailure
  | Cancelled CancelReason

startActor
  :: Member Actor effs
  => ActorDefinition startup protocol exit
  -> startup
  -> Eff effs (ActorRef protocol exit)

awaitExit
  :: Member Actor effs
  => ActorRef protocol exit
  -> Eff effs (ActorExit exit)

pollExit
  :: Member Actor effs
  => ActorRef protocol exit
  -> Eff effs (Maybe (ActorExit exit))
```

V0 runs the selected concrete row directly; reusable helpers remain
`Member`-polymorphic and specialize into that row. The private deployment may
existentially hide `initial` and the concrete row, but Rust—not the existential
or an import—authorizes the resulting nominal requests. The profile is a named
compile/deploy choice, not a reflected row ABI or authority token.

Profile selection is part of the one full `ActorDefinition`; it is not a
second start operation, mutable options bag, or runtime row descriptor. The
GADT existentially hides both the initialization artifact and selected row.

`visibleToChild` names additional top-level heads, not arbitrary Haskell
export syntax. Internal sealing resolves them through GHC-derived metadata
against the exact compile view that produced the definition. Selecting a type
head carries its constructor export shape; selecting a value carries that
exact declaration identity. Missing, shadow-drifted, or type-incoherent
selections fail start before child allocation.

Capability-specific launch grants remain planned, not public API. When added,
capability-owning modules create opaque recipes for their own resources;
there is no generic grant record or policy enum. Attaching a recipe must not
change the definition's program image, and copying a definition or recipe must
not itself transfer authority.

`startActor` runs the definition's concrete `initialization` action. Any
`deliberate` calls remain at authored monomorphic sites, so GHC records their
exact input and output types before the definition hides the initialization
artifact and row. Initialization may call `deliberate` zero or more times
sequentially. Startup then applies `behavior startup initial` and returns only
after the resulting computation is installed and ready. The startup,
initialization-artifact, protocol, and successful-exit types need not resemble
one another. A non-prompted constructor remains absent until a real actor needs
one.

`pollExit` is the nonblocking observation counterpart to `awaitExit`, not a
second failure surface. It returns `Nothing` only while the exact target is
live and returns the same retained `ActorExit` as every later observation once
terminal. Fixed resident programs normally use `awaitExit`; an interactive MCP
policy uses `pollExit` when blocking its sole model-facing tool turn would hide
other completed work.

The outer session that asks a model to author a definition likewise fixes the
whole result type at its `deliberate` site:

```haskell
definition <-
  (deliberate "Define the reviewer actor." goalInput
    :: Eff ActorEffects (ActorDefinition ReviewSeed Reviewer ReviewExit))
```

Inside the fenced response, define new nominal types as declarations, then
construct the full record as the live completion value. Give any effectful
field helper a local monomorphic signature before placing it behind the GADT's
existential row. This is ordinary Haskell type staging: neither task text nor
Rust metadata guesses types that the authored call site left ambiguous.

If the response invents the protocol type itself, the fixed caller cannot name
that type retroactively. Return a value behind an interface the caller already
knows—commonly an `Eff ActorEffects result` computation that constructs,
starts, calls, and awaits the actor internally, or an existential package with
its eliminators. This is the useful fusion of fixed harness and self-written
program: dynamic Haskell owns the new types while the outer state machine keeps
an old, precise transition type.

An owner can have children with unrelated exit types because exit observation
is tied to each `ActorRef protocol exit`. `awaitExit` returns that child's exact exit
type. Unexpected lifecycle events do not enter a heterogeneous Haskell system
inbox; Rust sends informational backend-native input to a live agent-backed
owner, which may invoke an authored tool to observe the exact exit.

Installing the returned `Eff actorEffs exit` computation is the readiness
linearization point. The
model produces a typed initialization artifact, not the actor's entire control
flow. Authored Haskell decides how that artifact configures the fixed program.
Rust invokes the definition's shutdown hook at a safe boundary for
cooperative shutdown without depending on it for hard cleanup. The hook runs
under the actor's normal row and principal, but the interpreter is in its
closing phase: installed handlers may finish ordinary bounded cleanup, while
an unhandled suspension such as `Deliberate` or actor startup fails the hook.
Rust closes the actor realm regardless, so cooperative Haskell cleanup is
best-effort rather than a prerequisite for hard cleanup. Hook failure is a
neutral actor event; it does not rewrite the actor's retained terminal result.

Publication and liveness are separate facts. A child may terminate after
signaling ready but before the caller resumes. The returned reference still
names that exact, now-dead incarnation. `call` and `cast` cannot use it, while
`awaitExit` reconstructs its `ActorExit exit` from the terminal record and
shared exit cell. Cancellation before readiness publishes no reference and
terminates the caller whose start obligation cannot be satisfied.

An `ActorRef protocol exit` contains an opaque routing identity and a shared,
single-assignment cell for a successful `exit`. Completion fills the cell
before publishing the terminal lifecycle transition. Copies and active waits
therefore retain the exit through ordinary Haskell reachability, and repeated
waits observe the same value even when it is a closure. Rust retains only the
immutable terminal metadata needed to distinguish completion, failure, and
cancellation. A stale or invalid reference and cancellation of the waiter make
that `awaitExit` fragment unsatisfiable; target termination returns normally.

The model-facing shape is the ordinary `Eff` computation directly:

```haskell
receive
  :: Member (ActorLocal protocol) effs
  => (forall result. protocol result -> Eff effs (result, next))
  -> Eff effs next

serve
  :: Member (ActorLocal protocol) effs
  => state
  -> (forall result. state -> protocol result -> Eff effs (result, state))
  -> Eff effs exit
serve state step = receive (step state) >>= \next -> serve next step
```

`ActorLocal protocol` ties the receiving protocol to the row used by the
installed program. The enclosing `ActorDefinition startup protocol exit`
already fixes that program's successful exit type and eventual
`ActorRef protocol exit`; repeating `exit` as a phantom `ActorLocal` index makes
ordinary `receive` ambiguous in GHC and adds no safety.
Its ordinary Haskell functions form the public algebra. V0 runs them in the
selected concrete profile without a general lowering layer. Private substrate
requests such as readiness remain outside that profile. Generated export
curation keeps raw vocabulary out of the model's way. Every request is
interpreted under the current actor principal, so there is no mailbox handle to
forge, pass, or rewrite during fork.

The trusted entry wrapper uses a private `ActorKernel` effect for readiness and
hidden mailbox settlement. Its readiness continuation is the installed program
root. The interpreter accepts kernel requests only from the program's resource
realm and expected phase, never from a fenced model fragment. This avoids a
public lifecycle or reply token and a Rust-side program registry without mixing
kernel operations into the authored `ActorLocal` algebra.

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

Agent-backed actors project an equally ordinary typed tools record. `Call` and
`Notify` leave recursive policy state unchanged; `Update` replies and installs
new state; `Finish` replies and returns the actor's typed exit:

```haskell
serveToolsWith
  :: state
  -> (state -> tools (AsActorT (Eff effs) state exit))
  -> Eff effs exit

updateTool :: Text -> (input -> m (output, state)) -> endpoint
finishTool :: Text -> (input -> m (output, exit)) -> endpoint
```

These last two signatures abbreviate the mode-indexed endpoint types rather
than pretending they are standalone values. All four endpoint forms use one
generic record traversal and one declaration/dispatch table. State remains an
ordinary Haskell value captured by the recursive server; Rust owns no mirror
of it. A terminal reply settles at actor completion before the external policy
transport is retired.

The ordinary supervised-worker path is a discoverable library composition,
not another effect:

```haskell
runActor
  :: Member Actor effs
  => ActorDefinition startup protocol exit
  -> startup
  -> Eff effs (ActorExit exit)
runActor definition startup = startActor definition startup >>= awaitExit
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
  => ActorRef protocol exit
  -> protocol result
  -> Eff effs result

cast
  :: Member Actor effs
  => ActorRef protocol exit
  -> protocol ()
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

Every child exit produces an informational backend-native wake for a live
agent-backed owner. It neither resumes nor re-enters the actor's authored
Haskell continuation and exposes no lifecycle event value or acknowledgment
effect. The model obtains the exact typed outcome only by invoking authored
tools that use the retained `ActorRef` and `awaitExit`; the wake may be ignored.
The delivery contract is in
[architecture.md](architecture.md#failure-settlement-and-owner-wake).

## 4. Result-bearing agent sessions

The fixed program opens a result-bearing agent session by asking its resident
model for a value of a known type:

```haskell
deliberate
  :: Member Deliberate effs
  => Text
  -> input
  -> Eff effs output
```

The text describes the task; the call site's ordinary Haskell types define the
contract. A reusable request is an ordinary function returning `Eff`, not a
separate descriptor value. The input is mounted as a live Haskell binding, not
serialized merely to place it in a prompt. The prompt may summarize the input
for orientation, while the authoritative value remains available to code.

Each `deliberate` call creates one agent session, possibly containing many
model rounds, against the actor's accumulating provider conversation.
Completing the request returns to the fixed Haskell continuation. Later
`deliberate` calls reuse the same model context and Haskell environment.

At the start of a result-bearing session, the workbench exposes bindings
conceptually like:

```haskell
goalInput :: input
complete  :: output -> Eff effs a
```

The completion mechanism is a typed suspending effect. GHC—not Rust-side JSON
conversion—checks the result against `output`.

### The scoped completion effect

`deliberate` and `complete` are the two directions of one typed interaction:
the installed program requests cognition, and the active workbench supplies
the result.

```haskell
data Complete output a

complete
  :: Member (Complete output) effs
  => output
  -> Eff effs a
```

Any workbench execution opened through `deliberate` receives exactly one
current `Complete output`. Ordinary backend input to an interactive actor has
no typed completion expectation. Completion is scoped to the whole possibly
multi-round agent session, not to one provider response. The model may define
and test substantial Haskell before calling `complete`.

Return expectations may nest internally, but authored Haskell sees only the
innermost one. The runtime retains outer expectations privately and restores
the next only after the inner scope settles or is abandoned. Do not represent
nested scopes as several differently indexed `Complete` members in one row:
that would let inner code select and settle an outer caller. Concurrent child
results remain explicitly addressed through `ActorRef` and `awaitExit`, not as
ambient completion effects.

There is no public completion identifier, token, lookup operation, or generic
obligation registry. Compiler site ids and runtime continuation identities are
substrate. An installed actor's successful `exit` uses ordinary Haskell return;
it does not need a second `Complete exit` convention.

### Re-entrant resident-agent requests

While satisfying one typed request, the model may define and return closures
that contain future `deliberate` calls. It may not synchronously invoke such a
call while the same actor's agent session is active. That would re-enter one
serial actor and one accumulating model context. The disposable fragment gets
an unavailable-operation receipt; defining or returning the future closure
remains valid.

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
- declaration-only progress does not accidentally complete the session;
- `complete expression` can succeed only when the expression has the required
  type;
- natural-language prose and non-Haskell fences never execute;
- later blocks in one response observe earlier successful commits;
- fenced execution remains available across all model rounds in the session;
- model compaction preserves a concise inventory of important bindings and
  installed behavior.

Useful introspection should include familiar `:type` and `:info` plus a small
Tidepool surface such as:

- `:goal` — current required result type and mounted inputs;
- `:bindings` — live names, types, ownership, and abbreviated values;
- `:program` — installed behavior, exported actor definitions, definition
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
- exported actor definitions;
- named reusable helpers deliberately retained by the actor;
- values retained for rollback;
- transitive dependencies of those roots.

Installation should retain provenance:

- defining actor and incarnation;
- declaration generation and source;
- dependency image;
- agent-session and model-round origin;
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

The smaller escape hatch is an ordinary function that consults the resident
model only when local policy cannot decide:

```haskell
decide observation =
  case decideLocally observation of
    Just action -> pure action
    Nothing -> deliberate "Resolve this ambiguous observation." observation
```

Do not standardize a universal confidence score, epistemic enum, evidence
ladder, or one blessed escalation combinator. Direct `deliberate` calls,
injected judgment functions, model-authored local effects, and controller
replacement are all ordinary Haskell patterns. Domain-specific uncertainty
types remain welcome when the domain actually needs them.

## 9. Dynamic protocols and child actors

The model can define a new protocol and actor definition during an agent
session:

```haskell
data Reviewer result where
  Review  :: CandidateChange -> Reviewer ReviewFindings
  Revise  :: CandidateChange -> ReviewFindings -> Reviewer CandidateChange

reviewerDefinition :: ActorDefinition ReviewStartup Reviewer ReviewerSummary
reviewerDefinition = ...
```

It can then spawn and call the child with ordinary types:

```haskell
reviewer <- startActor reviewerDefinition startup
call reviewer (Review candidate)
```

The fresh child's model context is empty, but its program image contains the
`Reviewer` declarations and behavior needed to understand and execute the
definition. The parent does not have to reduce the request to JSON.

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
  :: Members '[Actor, ActorLocal protocol] effs
  => NonEmpty seed
  -> (seed -> Eff effs exit)
  -> Eff effs [ActorRef protocol exit]
```

This is a useful intersection rather than duplicate actor machinery:
`ActorLocal` fixes the current incarnation's protocol, while the enclosing
definition fixes successful exit and `Actor` grants outward actor creation.

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
`ActorRef protocol exit` supports repeatable typed `awaitExit`; every child
exit creates an informational native owner wake, not a Haskell event; and
`ActorDefinition` supplies a `ShutdownReason` handler for cooperative cleanup.
Rust owns recursive subtree termination and the twelve-hour watchdog.

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
    -> ActorDefinition seed protocol exit
    -> ( forall effs
          . Member Actor effs
         => ActorRef protocol exit
         -> Eff effs answer
       )
    -> Specialist answer
```

The dynamic code may invent `protocol`, but the enclosing program only needs to know
how to start the packaged definition and run the packaged client. This is a
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

### Worktree-backed worker submission

The first DevSwarm policy uses ordinary application types rather than adding a
generic workflow algebra. The exact spelling may follow the existing generated
Worktree types, but the public shape is:

```haskell
data HeadState
  = OnBranch BranchName GitOid
  | Detached GitOid

data WorkingState = WorkingState
  { changes   :: DirtySummary
  , operation :: Maybe InProgressKind
  }

data SubmissionObservation = SubmissionObservation
  { worktreeId     :: WorktreeId
  , baseHead       :: GitOid
  , submittedHead :: HeadState
  , workingState   :: WorkingState
  }

data CandidateReceipt = CandidateReceipt
  { authoredReport :: WorkerReport
  , repository     :: SubmissionObservation
  }
```

One Worktree operation returns `SubmissionObservation` as a coherent fresh
read or a typed unstable-observation failure. Authored code must not assemble
it from separately timed `HEAD`, branch, status, and operation calls.
`InProgressKind` remains one sum type and gains an `Other Text` case if unknown
Git state must be surfaced; rendered Git text never drives policy. An
observation failure makes successful submission unsatisfiable rather than
inviting the model to supply repository facts.

The worker definition closes over its granted `WorktreeHandle`. Its
`finish_work` input is only `WorkerReport`; the trusted handler performs the
observation and completes the actor with `CandidateReceipt`. Dirty work still
completes normally with a truthful receipt. It is not an immutable integration
artifact, so the owner normally rejects or revises it in V0.

The interactive root keeps the exact `ActorRef` private and exposes an
application-level `WorkerHandle`. Its policy has these semantics:

- spawning a new key creates one worktree and actor and returns
  `WorkerAccepted handle` once actor creation succeeds and deployment is
  requested;
- retrying the same key with the same typed request returns the same handle;
- retrying it with a different assignment or requested launch policy returns
  `WorkerKeyConflict`; lookup and comparison happen before any new worktree or
  grant is allocated;
- retrying an acknowledged key reports `WorkerAlreadyAcknowledged` and never
  starts another actor; keys remain reserved for this root incarnation;
- `collect_worker handle` is nonblocking and non-consuming, returning pending,
  the repeatable exact outcome, already-acknowledged, or unknown; pending is a
  cooperative yield signal, so the interactive root ends its turn rather than
  sleeping or polling and Shoal starts a new turn on lifecycle transition;
- `ack_worker handle` is valid only after `collect_worker` has observed a
  terminal result, drops the retained `ActorRef`, and leaves a tombstone; and
- `list_workers` reports handles and their current application state.

The request comparison is over the typed normalized request retained in
Haskell state, never a rendered JSON string or hash chosen by the model.
Transport acknowledgment cannot roll arbitrary Haskell effects back; these
idempotent application semantics make an unknown MCP delivery outcome safe to
retry.

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

## 15. Public documentation and diagnostics

Public Haskell modules, prompt cards, workbench receipts, and diagnostics are
part of the interaction surface. Explain what an operation means in Haskell,
which effects it needs, whether it reuses this actor's context or starts a new
one, what remains live, and what authored code can do next.

Do not make the model reproduce Rust registries, root custody, bridge fields,
provider adapters, or routing metadata. A useful failure names the attempted
operation, actor, and semantic reason it is unavailable. Internal crate and
protocol documentation should still describe those mechanisms precisely;
public semantic documentation and exact implementation documentation serve
different audiences.
