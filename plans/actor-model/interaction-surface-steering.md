# LLM interaction-surface steering

Status: active implementation steering. This is a temporary refinement delta
for the actor-model implementation currently in flight.

Audience: the implementation agent working primarily in Rust and the Haskell
effect/schema boundary. This document is not a tutorial for an end user and is
not a second canonical architecture.

Read this with:

- [architecture.md](architecture.md) for runtime semantics and invariants;
- [haskell-surface.md](haskell-surface.md) for the canonical Haskell contract;
- [implementation.md](implementation.md) for status and delivery order; and
- [live-values-and-authority.md](live-values-and-authority.md) for mobility and
  authorization.

Where this document explicitly refines the model-facing Haskell vocabulary or
authoring experience, follow this document during the current implementation
slice. It does not override lifecycle, ownership, authority, exact-source,
rooting, or scheduling contracts. Fold its settled decisions into the
canonical documents and delete it once the relevant Haskell surface lands.

The implementation is in flux and backward compatibility is not a constraint.
Existing public-looking names, wrappers, and generated declarations are not
entitled to survive merely because the current vertical uses them. Preserve
the proven mechanisms and invariants; change the facade boldly when a smaller
Haskell ontology follows from them.

## 1. The target user

The user of this surface is not a novice human learning an SDK and not a weak
model being protected from ordinary code. It is a capable, self-authoring Sol-
or Terra-class model operating as part of a swarm. It has:

- an accumulating actor-local model conversation;
- a persistent GHCi-like declaration and binding environment;
- ordinary hidden reasoning space for scratch work;
- strong Haskell generation and compiler-repair ability;
- repeated runtime-authored compilation and execution receipts; and
- a Developer instruction stating plainly that every fenced `haskell` or `hs`
  block in its answer executes in the persistent environment.

This operating assumption is deliberate. Do not spend surface complexity on
making the interaction tolerable for models that cannot follow that execution
contract. The surface should instead make a capable model fast, expressive,
and able to grow typed behavior without learning Tidepool's Rust internals.

The relevant evaluation is experiential:

> If I were the resident model, could I understand the available Haskell
> values and types, write the next useful definition, compile it, install or
> return it, and arrange to consult myself again later without simulating the
> runtime?

## 2. The model sees Haskell, not Rust

The model's ontology ends at the Haskell API. It should experience a native
typed actor library embedded in a persistent GHCi-like environment.

The model may understand these concepts:

- an actor with a protocol, successful exit type, and persistent model memory;
- a typed deliberation performed by that actor's resident model;
- a scoped typed completion action while answering that deliberation;
- actor definitions or specifications written as ordinary Haskell values;
- exact actor references, calls, casts, exits, forks, and fresh starts;
- available Haskell effects and opaque resource capabilities; and
- ordinary Haskell closures, existential packages, and `Member` constraints.

The model should not need to understand or name:

- Rust structs, enums, handlers, or registries;
- execution principals or incarnation-fencing implementation;
- root custody, root ledgers, resource realms, or continuation registries;
- nominal Rust request dispatch;
- provider adapters, conversation storage, machine checkout, or scheduling;
- readiness linearization or the private readiness suspension;
- program-image storage records or exact-module plumbing; or
- `CoreValue`, extractor wire shapes, site-id encoding, and bridge mechanics.

Those mechanisms remain essential implementation truth. They belong in Rust
comments, crate charters, protocol-generator documentation, and internal
architecture. They do not belong in model-facing prompt cards, public Haskell
Haddocks, ordinary diagnostics, or the vocabulary a model must reproduce to
author an actor.

Model-facing explanations state semantics directly. For example:

```text
An ActorRef identifies one exact actor. Copies share its successful exit
value, so awaitExit is repeatable and can return arbitrary Haskell values.
```

Do not explain the same contract to the model as:

```text
Rust retains terminal metadata while a managed heap cell carries the payload.
```

The second explanation is appropriate for implementation documentation only.

Likewise, a capability refusal shown to the model should name the operation,
the actor that attempted it, and the semantic reason it is unavailable. It
should not teach registry keys, handler factories, or principal installation.

## 3. The core interaction

The installed Haskell program controls when and why its resident model should
think. It requests a typed value with `deliberate`:

```haskell
deliberate
  :: Member Deliberate effs
  => Deliberation input output
  -> input
  -> Eff effs output
```

For example:

```haskell
review <- deliberate reviewCandidate candidate
```

Reaching this expression parks the installed Haskell continuation and opens
one agent session against the actor's existing model conversation and Haskell
environment. The authoritative input is mounted as a live Haskell value. The
workbench used to answer the request has the ordinary actor effects plus one
scoped completion effect indexed by the exact answer type:

```haskell
Complete Review ': ActorEffects
```

The model answers through one verb:

```haskell
complete review
```

Conceptually:

```haskell
complete
  :: Member (Complete output) effs
  => output
  -> Eff effs a
```

GHC checks the supplied value against the output type fixed at the original
`deliberate` call site. The live value settles the deliberation and the parked
program resumes with that value. There is no JSON result schema and no model-
authored type string.

### 3.1 Two directions, two verbs

`deliberate` and `complete` are deliberately different verbs:

- `deliberate` is used by the persistent actor program to request cognition;
- `complete` is used by the active model workbench to supply the result.

They are the two directions of one typed interaction, not two competing model
APIs. Do not overload one spelling for both directions. Code such as
`complete (reviewGoal candidate)` on the requesting side obscures whether a
value is being requested or supplied and produces phase-dependent meanings
for the same verb.

### 3.2 Exactly zero or one completion obligation

An actor has at most one admitted turn or agent session. Consequently its
workbench has exactly zero or one current typed completion obligation:

- outside typed deliberation, `Complete output` is absent;
- inside typed deliberation, one `Complete output` is present;
- after completion or abandonment, it is no longer usable; and
- there is never a model-facing collection of pending completions to select
  among.

Do not expose a completion identifier, handle, token, lookup operation, or
generic obligation registry to Haskell. If an internal compiler site id or
provenance field remains necessary for extraction or diagnostics, it is
substrate rather than a model-visible completion identity. The 0/1 semantic
invariant still governs the public surface.

### 3.3 Nested deliberation

A model may define code containing future calls to `deliberate` while it is
currently answering a deliberation. It must be able to return such a closure
through `complete`.

It may not synchronously invoke another deliberation in the same actor while
the current agent session is active. That would re-enter the same actor and
model context. The interpreter should refuse the attempted nested operation as
an unavailable operation in the disposable workbench fragment. This is a
phase restriction, not a second Haskell monad and not a reason to prevent the
model from defining or returning the closure.

## 4. Self-authored escape hatches

A central intended use is for the model to write a future point at which its
installed Haskell behavior consults it again:

```haskell
decide
  :: Member Deliberate effs
  => Observation
  -> Eff effs Action
decide observation =
  case decideLocally observation of
    Just action -> pure action
    Nothing     -> deliberate resolveAmbiguity observation
```

During the current agent session the model may:

1. define `decideLocally` and `decide`;
2. test the pure path or any pure helpers;
3. package the effectful function in a controller or actor definition; and
4. return it with `complete`.

Later, installed Haskell invokes `decide`. If the escape-hatch branch is
reached, the actor parks and the same resident model context returns to answer
`resolveAmbiguity`.

The `Member Deliberate effs` constraint is valuable. It makes the future model
dependency visible in the function's type while keeping the function portable
to any actor stack that supplies the capability.

The deliberation may return whatever stable interface the authored program
needs:

- one immediate domain answer;
- a closure implementing future policy;
- a replacement controller paired with rollback state;
- a domain-effect interpreter;
- an actor definition or specification; or
- an existentially packaged specialist.

This is how an actor extends itself. The model writes Haskell that can decide
when to re-enter model judgment, and a later deliberation can replace more of
that Haskell behind a stable typed interface.

### 4.1 Do not standardize an epistemic enum

Do not add a Tidepool-wide type such as:

```haskell
data Decision a
  = Decided a
  | NeedsJudgment ...
```

Do not add a generic confidence score, uncertainty framework, evidence ladder,
or blessed escalation state machine. Those structures would impose one theory
of judgment on every actor and would quickly become cargo-culted boilerplate.

The primitive is `deliberate`. Authored Haskell chooses its own boundary using
ordinary control flow and domain types:

```haskell
case routeLocally item of
  Just route -> pure route
  Nothing    -> deliberate chooseRoute item
```

If a domain genuinely has `Ambiguous`, `NeedsReview`, `RevisionRequired`, or
similar states, its actor may define those types. They are not kernel types.

### 4.2 Do not standardize one escape-hatch combinator

Direct `deliberate` calls, injected judgment functions, model-authored local
effects, and controller replacement are all valid ordinary Haskell patterns.
Do not promote one into a second runtime mechanism.

In particular:

- simple actor code may call `deliberate` directly;
- reusable libraries may accept an effectful judgment function;
- a sophisticated actor may define a domain algebra and interpret it through
  `deliberate`; and
- a self-improving loop may ask for a replacement closure instead of a single
  answer.

The kernel owes only the typed deliberation primitive. The model owns the
program structure around it.

## 5. Fenced Haskell is already the right protocol

Every explicitly tagged `haskell` or `hs` fence in an assistant response runs
in source order against the persistent actor environment. Successful earlier
blocks remain available to later blocks and later model rounds. A rejected
fragment stops the response suffix, preserves its already committed prefix,
and becomes concise Developer context for the next corrective round.

This behavior is accepted and has been exercised extensively with the target
models. Preserve it.

Do not add any of the following merely as model-safety affordances:

- a second executable fence language such as `tidepool-haskell`;
- a dry-run/execute distinction for ordinary fenced code;
- a mandatory response transaction or automatic rollback prompt;
- a runtime-authored state card repeated every model round;
- a special scratch-code channel inside the Haskell workbench; or
- defensive ceremony aimed at models unable to remember that Haskell fences
  execute.

The Developer guidance should state the rule directly. Models already have
hidden reasoning blocks for non-executing scratch work, and the accumulating
conversation contains the goal and prior receipts.

This does not weaken the existing prefix-preserving execution contract. GHC
diagnostics and structured effect-failure receipts should remain accurate and
actionable because they are the normal repair loop:

```text
write Haskell -> compile/run -> receive receipt -> correct Haskell -> complete
```

## 6. Failure is conversational at the workbench boundary

The success-shaped Haskell actor API is intentional. Do not introduce a
universal lifecycle result into every call merely to make failures explicit to
the model.

For a fenced workbench fragment:

- a compile failure rejects the fragment and returns diagnostics;
- an unsatisfied effect abandons the fragment and reports the semantic cause;
- the same agent session continues in a later model round;
- already committed declarations and bindings remain; and
- the model writes different Haskell rather than resuming a failed
  continuation.

For an installed actor-program continuation, an unsatisfied exact operation is
terminal to that actor. Its owner observes the failure through an existing
typed wait/call obligation or a lifecycle advisory and may write new Haskell,
start a successor, or choose another policy.

This is the intended recovery surface for a swarm of resident models. Do not
add `tryStart`/`tryCall`/`tryCast`/`tryWait`, a universal `Outcome`, transparent
replay, or model-selected runtime retry policy. Domain failures remain domain
types; runtime failures become truthful receipts and lifecycle facts.

## 7. Context relationships are cognitive tools

The three relevant operations have distinct model-memory semantics:

| Operation | Model/Haskell context | Intended use |
|---|---|---|
| `deliberate` | same actor, same accumulating conversation and environment | consult the actor's future self |
| `forkActors` | exact shared conversation/environment/control prefix, then divergence | explore several context-rich alternatives |
| `startActor` | fresh conversation with an explicit Haskell specification | obtain an independent actor or fresh judgment |

A useful model-facing framing is:

> Deliberate when you want your future self. Fork when you want several
> versions of yourself. Start an actor when you want an independent mind.

This is documentation and prompting guidance, not three runtime role presets.
Fresh reviewers, speculative branches, workers, and one-shot jobs remain
ordinary Haskell compositions.

Forking an exact Haskell/model/control snapshot is not itself an ergonomic
problem. Do not add restrictions merely because the shared transcript contains
the assistant response whose Haskell reached the fork. The target model knows
its own response, and branch-specific execution receipts remain authoritative.

## 8. Model-facing naming

Naming is a first-order part of an LLM interaction surface. A capable model can
understand advanced Haskell, but every unnecessary synonym or cross-domain
collision consumes context and increases the chance of emitting the wrong
type.

### 8.1 Actor versus agent

Use **actor** for the persistent typed Haskell entity. Use **agent session** for
the possibly multi-round model interaction answering one typed request. Keep
backend/worktree agent identities distinct from actor references.

The required public naming is:

| Transitional/current name | Desired model-facing name |
|---|---|
| `AgentRef protocol exit` | `ActorRef protocol exit` |
| `AgentEffects` | `ActorEffects` |
| `AgentM` | `ActorM` |
| `api` in explanatory signatures | `protocol` |

Rust already uses `ActorRef` for the exact incarnation identity. The Haskell
reference carries additional typed exit state, but that implementation detail
does not justify a different model-facing noun. The entity referenced is still
an actor.

Perform this rename as one coherent generated-code/Haskell/docs sweep during
the actor-surface work. Do not land `AgentRef`/`AgentEffects` as the new public
actor vocabulary and schedule a speculative later cleanup. Backward
compatibility is explicitly not a constraint, and the current coexistence of
Rust `ActorRef`, Haskell `AgentRef`, and a separate worktree `AgentRef` is the
kind of comprehension tax this surface is meant to remove.

### 8.2 Deliberation and completion

Use this family consistently:

```haskell
data Deliberation input output
data Deliberate a
data Complete output a

deliberation :: Text -> Deliberation input output
deliberate   :: Member Deliberate effs
             => Deliberation input output -> input -> Eff effs output
complete     :: Member (Complete output) effs
             => output -> Eff effs a
```

`Deliberation` is the typed task description. `Deliberate` is the installed
actor capability. `Complete output` is the temporary response capability.
Remove obsolete `Goal a` sketches from planning diagrams rather than
maintaining two public names for the same typed request.

### 8.3 Keep established actor verbs

The following verbs are concise and compose well:

```haskell
deliberate
complete
startActor
runActor
promoteActor
withLaunchGrant
call
cast
receive
serve
awaitExit
forkActors
```

Keep the `call`/`cast` pair. In an actor library their distinction is familiar,
and `send` would collide with the freer-effect primitive.

## 9. Actor construction is the main remaining ergonomic pressure

The runtime semantics of definition, promotion, specification, startup,
readiness, and exact deployment are sound. The current Haskell sketch exposes
too many of those internal distinctions as routine authoring ceremony. Fix
that surface rather than documenting the ceremony more thoroughly.

The happy path should let the model express three essential choices:

1. the startup deliberation;
2. how the typed initialization result becomes actor behavior; and
3. the protocol and successful exit types, normally inferred from Haskell.

A display label is also reasonable because it improves model and operator
receipts. Shutdown behavior, additional child-visible helpers, launch grants,
and unusual lifecycle behavior are refinements rather than mandatory ceremony
for every definition.

### 9.1 Minimize routine nouns

The implementation may legitimately retain distinct internal representations
for:

- an authored definition;
- an exact promoted specification;
- an installed computation; and
- the private readiness continuation that roots it.

The model should not have to reason about all of those representations every
time it defines a child.

`ActorProgram` is not a public model-facing concept. Remove it from public
exports and remove the public `program` wrapper. If an internal newtype remains
useful for existential packaging, readiness, or implementation clarity, keep
it private to `Tidepool.Actor.Internal` and the kernel. The definition's
behavior field accepts the ordinary `Eff` computation the model actually
writes; the library may wrap it internally.

Conceptually, the author should be able to think in a shape like:

```haskell
actor
  :: Members '[Deliberate, ActorLocal protocol exit] actorEffs
  => Text
  -> (startup -> Eff actorEffs initial)
  -> (startup -> initial -> Eff actorEffs exit)
  -> ActorDefinition startup protocol exit
```

The initialization action deliberately keeps its `deliberate` invocation at
the concrete authored call site. GHC records the exact startup and result types
there before `ActorDefinition` hides `actorEffs`; moving that invocation into a
polymorphic library helper loses the compiler-owned type metadata.

The absence of a public `ActorProgram` wrapper is the important property. The
behavior argument is visibly the ordinary `Eff actorEffs exit` computation the
model writes. A concrete deployed entry facade may additionally define
`type ActorM = Eff ActorEffects` for convenient local signatures. The common
`actor` constructor supplies a no-op shutdown hook and no additional child-
visible helpers. An advanced record constructor or narrowly named refinement
may supply the uncommon fields without burdening the common case.

### 9.2 Definition versus specification

The distinction between an editable `ActorDefinition` and an opaque deployable
`ActorSpec` is useful when the model dynamically authors code:

- the definition is ordinary Haskell source/value structure;
- `promoteActor` freezes the exact code, live roots, and child-visible names;
- the resulting specification can be reused, granted resources, and started.

Model-facing documentation should explain `promoteActor` in those Haskell
terms. “Freeze this definition and the Haskell names its child can see” is
enough. The model does not need the phrase “program-image membrane” to use it,
even though program image remains the correct implementation term.

The current authored/static `actorSpec` vertical must not become a second
public construction system beside dynamic definition/promotion. Make it an
internal composition-root/staging helper, or replace it with the common
`actor` constructor returning `ActorDefinition` and promote through the same
path used by model-authored definitions. `ActorSpec` remains opaque and is
created by `promoteActor`, including for checked-in actors bootstrapped by the
composition root.

### 9.3 Defaults and refinements

Use one small common `actor` constructor plus ordinary library refinements,
not a large mandatory record or a matrix of runtime modes.

Required defaults for the common constructor:

- no-op cooperative shutdown when no domain cleanup exists;
- no additional child-visible helper names beyond the interface necessary to
  name startup, initialization, protocol, and exit values; and
- inherited V0 actor interpreter policy until a real heterogeneous capability
  protocol requires explicit selection.

Refinements, if and when required:

- attach a shutdown function;
- add explicitly child-visible helper heads;
- attach capability-specific launch grants; and
- retain a predecessor behavior for rollback through ordinary Haskell.

Do not introduce `ActorRuntime` or `ActorProfile` merely to make a symmetric
constructor record. The current V0 direction—existential Haskell row plus
composition-owned interpreter policy—is simpler. If a future concrete
capability requires authored selection of heterogeneous profiles, name that
new Haskell value by the choice the author actually makes at that time.

### 9.4 Child-visible exports

`modelExports` is ambiguous about direction. Rename it `visibleToChild` if an
explicit field remains.

Where practical, promotion should derive the interface heads required to name
the startup input, initialization result, protocol, exit type, and their
constructor shapes. Explicit author input should describe additional helpers
that make the child pleasant to work with, rather than reconstructing the
minimum type-correct boot interface by hand.

Do not weaken exact declaration identity or promotion validation to achieve
this convenience. Inference is an ergonomic layer over the exact-source
membrane, not a request to replay source or match names loosely.

### 9.5 Definition fields

If the full record-shaped `ActorDefinition` constructor remains visible for
advanced use, use field names that describe what the author supplies:

| Current sketch | Use |
|---|---|
| `startupSession` | `initialization` |
| `install` | `behavior` |
| `modelExports` | `visibleToChild` |
| `onShutdown` | keep |

Include the display label in the definition or its common constructor; it is
useful semantic identity in receipts and advisories. Do not name a pure
behavior-building function as though the model itself performs runtime
installation, and make export visibility direction explicit.

## 10. Public documentation and diagnostics

The public Haskell modules, generated prompt cards, and actor workbench
diagnostics are part of the interaction surface. Review them independently of
the quality of internal implementation comments.

Public Haskell documentation should answer:

- What value or computation does this operation represent?
- Which effects must be available?
- Does it use this actor's resident context or start a fresh actor?
- What remains live after the operation?
- What should authored Haskell do next?

It should not answer internal questions the model never needs, such as which
Rust registry owns a tombstone or which bridge field carries a closure.

Examples:

```text
Good: call runs the target actor's protocol handler and returns its typed reply.
Bad: call allocates a Rust reply obligation and routes a RootCustody handle.
```

```text
Good: an unavailable call abandons this workbench fragment; try different
Haskell in the next round.
Bad: the interpreter could not settle the nominal request constructor under
the installed principal.
```

```text
Good: top-level declarations persist across later model rounds.
Bad: successful declarations advance the SessionModule generation in the
declaration plane.
```

Internal crate and protocol documentation should continue to state the exact
mechanism. Do not make implementation documentation vague in an attempt to
sanitize the public surface; maintain two appropriately scoped explanations.

## 11. Consequences for the current implementation

The active implementation is already moving in the right direction:

- public `deliberate` carries compiler-derived input/output types;
- the resident adapter adds private `Complete result` to the workbench row;
- wrong-typed completion produces a corrective round;
- live closure-valued completion survives the disposable fragment scope;
- actor-local readiness is a private `ActorLocal` request; and
- `ActorSpec` existentially hides the child row from the caller.

Preserve those properties.

### 11.1 Do now

- Keep `Deliberate` as the installed actor's request capability.
- Keep `Complete result` scoped to the active typed workbench.
- Enforce at most one active completion obligation through actor admission,
  not through a public ID API.
- Keep raw `ActorLocal` lifecycle constructors out of the authored Haskell
  surface.
- Keep readiness, root transfer, live payload handling, and exact source
  imports behind the existing Rust/Haskell membrane.
- Use the current startup/`startActor` vertical to prove startup, pure behavior
  construction, private readiness, publication, and typed exit retention;
  route its final public construction through `ActorDefinition` and
  `promoteActor`.
- Replace the public `ActorProgram`/`program` authoring wrapper with an ordinary
  `Eff actorEffs exit` behavior argument; retain any wrapper privately.
- Converge public `AgentRef`/`AgentEffects`/`AgentM` onto
  `ActorRef`/`ActorEffects`/`ActorM` before this becomes the standing actor
  API.
- Keep public construction on the `ActorDefinition -> promoteActor ->
  ActorSpec` route; do not stabilize `actorSpec` as a second public route.
- Phrase new public Haskell documentation in Haskell semantic terms even when
  the adjacent internal comments correctly discuss Rust.

### 11.2 Do not block the current vertical on

- automatic inference of child-visible export heads;
- heterogeneous actor interpreter/profile selection;
- general delegation or revocation;
- structural fork;
- a universal model-usability evaluation harness; or
- wholesale rewriting of older documentation before the new path is proven.

Preserve seams for those follow-ups. The active slice should remain
mechanically complete and testable, but “vertical first” is not permission to
stabilize avoidable public vocabulary that the vertical itself is introducing.

### 11.3 Relevant acceptance evidence

For the current and immediately following slices, useful interaction-surface
evidence includes:

1. An installed program calls `deliberate` and parks with compiler-derived
   input/output metadata.
2. The resident model sees the live input and only one usable `Complete output`
   effect.
3. A wrong-typed `complete` is rejected by GHC, prior work persists, and the
   next model round succeeds.
4. A closure containing a future `deliberate` call can be completed, retained,
   installed, and invoked later.
5. Invoking that escape hatch later opens another agent session in the same
   actor context and resumes the original Haskell continuation afterward.
6. Attempting to invoke `deliberate` recursively from its own active workbench
   is refused without preventing the closure from being defined or returned.
7. Fresh actor startup uses a new conversation and only its explicit Haskell
   specification, while later deliberations in that actor reuse its context.
8. Public Haskell diagnostics for these paths are understandable without Rust
   terminology.

These are contract tests and dogfood observations, not a demand for a new
scoring framework.

## 12. Explicit non-goals of this steering

This document does not request:

- changes to fenced-Haskell extraction semantics;
- automatic context cards or repeated goal summaries;
- workbench transactions or undo;
- failure sums on every actor operation;
- a generic model-confidence or escalation API;
- a runtime role enum for worker/reviewer/controller actors;
- a reflected Haskell effect row in Rust;
- a second actor construction path;
- nested model sessions within one actor;
- a new persistence mechanism; or
- simplification of internal invariants at the expense of exactness.

It asks for a smaller and more coherent projection of the already-solid
runtime onto model-authored Haskell.

## 13. Compact target surface

The eventual routine vocabulary should be close to:

```haskell
-- Typed cognition
deliberation
deliberate
complete

-- Actor definition and construction
actor
promoteActor
withLaunchGrant
startActor
runActor

-- Communication and supervision
call
cast
receive
serve
awaitExit
forkActors
```

And the routine types should be close to:

```haskell
Deliberation input output
ActorDefinition startup protocol exit
ActorSpec startup protocol exit
ActorRef protocol exit
ActorExit exit
```

`ActorLocal`, launch-grant recipe types, effect rows, and advanced existential
packages may appear where their distinctions are genuinely useful. Private
readiness operations, reply obligations, installed-program roots, Rust actor
identities, and program-image records do not belong in this compact surface.

## 14. Decision summary

Settled for the interaction surface:

1. The resident model sees Haskell semantics only.
2. Fenced `haskell`/`hs` blocks remain the execution protocol.
3. Installed Haskell requests cognition with `deliberate`.
4. The active workbench answers with scoped `complete` under
   `Complete output`.
5. There is at most one completion obligation and no public completion ID.
6. Models may author future `deliberate` escape hatches into returned code.
7. Nested same-actor deliberation is refused while the current agent session
   is active.
8. No universal judgment/confidence/escalation type is added.
9. Actor programs remain ordinary model-authored Haskell with
   `Member`-polymorphic reusable functions.
10. Actor references and concrete actor aliases converge on actor terminology,
    not agent terminology.
11. Public actor construction is `actor` to build an `ActorDefinition`, then
    `promoteActor` to obtain an opaque `ActorSpec`; no second public
    `actorSpec` path remains.
12. Failures in disposable model fragments return through prompting and
    correction; installed-program failure remains supervised actor lifecycle.
13. `ActorProgram` and `program` are private implementation vocabulary; model-
    authored behavior is visibly an ordinary `Eff` computation.

Intentionally open until the relevant implementation pressure exists:

- whether uncommon shutdown/export refinements deserve dedicated combinators
  in addition to the settled advanced record fields;
- how much of the mandatory child-visible interface promotion can infer;
- when heterogeneous interpreter policy becomes an authored Haskell choice;
  and
- later post-start delegation/revocation vocabulary.

Do not resolve those open points by adding a general option record or exposing
more runtime machinery. Let the first production self-authoring actor supply
the pressure that earns each additional distinction.
