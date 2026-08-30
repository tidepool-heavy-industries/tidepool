# Implementation plan

## 1. Delivery rule

Build the actor model as a sequence of vertical semantic proofs. Do not begin
with a broad rename of the existing harness or an abstract framework whose
critical behaviors have not run through the JIT.

[architecture.md](architecture.md) owns runtime semantics and invariants;
[haskell-surface.md](haskell-surface.md) owns public Haskell shapes. This file
orders their implementation and names acceptance evidence. If prose here
appears to redefine either contract, it is duplication to delete.

Backward compatibility is not a constraint. Existing mechanisms remain the
test oracle while each replacement is built, then the obsolete surface is
deleted rather than permanently adapted.

Every phase must preserve the repository's one-home rules:

- one continuation registry and root ledger;
- one machine-session checkout mechanism;
- one actor registry;
- one capability registry;
- one model-backend seam;
- one get/put durability backend.

Every Haskell API addition also passes an LLM-surface test:

1. Does an actor need this distinction to express typed behavior or policy?
2. Can Rust handle it completely and provide Developer context instead?
3. Does the new name compose with the small existing vocabulary?
4. Will exposing it reduce model confusion more than it increases choice?

If the second answer is yes and the first is no, keep the mechanism in Rust.
Do not generate comprehensive `tryX`, configuration-record, or lifecycle-state
families as a proxy for a designed DSL.

## 2. Phase 0 — semantic spikes

These spikes settle feasibility and API pressure before crate placement or
public names become expensive.

They are narrow vertical prototypes and may include temporary scaffolding.
Phase 1 extracts the mechanics that survive the spikes into the single actor
kernel; it does not build a competing implementation beside them.

### 0A. Typed behavior result

A fixed Haskell harness requests a live `Int -> Int` or small function-bearing
record from its resident model. The model defines it using the real Haskell
environment and completes the typed request. The harness suspends elsewhere
and then invokes the closure successfully.

Acceptance:

- no JSON representation of the closure;
- GHC checks the completion type;
- declarations persist across model rounds;
- the root survives an intervening suspension and collection;
- the old behavior remains callable for rollback.

### 0B. Dynamic child protocol

An actor defines a new indexed GADT, a child `ActorSpec`, and a typed client.
It fresh-spawns the child into an otherwise empty model/Haskell context, calls
it, and receives a function-valued result.

Acceptance:

- child receives the dependency-closed program image but no unrelated parent
  transcript or binding;
- startup follows `prepare`, one typed User-role agent session, and one
  authored `install`; preparation or installation cannot open nested model
  turns;
- parent and child use the same compiled dynamic protocol identity rather than
  textually equivalent redeclarations;
- request and response remain live values;
- the child's model can inspect its deployed protocol declarations;
- retiring the child does not invalidate a result whose root was transferred
  to the parent.

### 0C. Structural context fork

Bind a meaningful parsed repository model and accumulate a provider transcript,
then fork three actors.

Acceptance:

- transcript prefix is exact and provider usage reports cached input;
- declaration and value snapshots share storage before divergence;
- the Haskell control continuation is cloned into each branch;
- the parent retains pre-fork continuation references while child use returns
  `InvalidAfterFork` without rewriting Haskell bindings;
- each child receives a Developer fork notice followed by its typed User
  startup prompt;
- each branch can shadow a name without changing siblings;
- fresh spawn from the same program image has no transcript memory;
- resource and code growth are measurable.

### 0D. Caller-authorized closure

Send a closure capturing a protected test capability to another actor.

Acceptance:

- pure parts of the closure execute normally;
- protected use fails under the receiver principal;
- explicit caller registration makes it succeed;
- revocation makes the already-copied closure fail;
- calling the owner actor executes under owner authority without delegating the
  capability.

If any spike requires serializing the live value or making Rust understand the
domain type, stop and repair the boundary before continuing.

### 0E. Actor-local interpreter

Run two actors with different concrete effect stacks on one machine session.
Move a pure closure and a row-polymorphic effectful function between them.

Acceptance:

- each actor dispatches through its own Rust interpreter;
- continuations carry and validate an effect-stack ABI rather than a
  machine-global handled prefix;
- the polymorphic function instantiates in the receiver when its `Member`
  constraints are satisfied;
- a source-specialized effectful closure is rejected in a foreign stack;
- a fork creates a new interpreter instance with the same ABI and distinct
  grants.

### 0F. Mechanical retry, terminal failure, and advisory

Exercise two distinct failures: a transient condition that the Rust interpreter
resolves mechanically, and a dead target that must terminate the caller. Also
exercise owner notification for an otherwise-unobserved abnormal exit.

Acceptance:

- mechanical retry performs no provider request and resumes the original
  Haskell control point exactly once, only after the exact operation succeeds;
- terminal failure performs no final inference in the dying actor and cannot
  synthesize the call result or resume its continuation;
- no arbitrary replacement reference is offered or accepted;
- advisory creation is suppressed when an active wait or failed call already
  observes the same terminal transition;
- an otherwise-unobserved abnormal child exit runs one keyed advisory turn
  without Haskell re-entry;
- advisory exhaustion closes the advisory without terminating its owner;
- provider transport failure terminates cleanly and notifies the owner.

## 3. Phase 1 — Rust actor kernel

Introduce the minimum Rust-owned substrate required by the spikes:

- `ActorId` plus incarnation fencing;
- one actor registry and lifecycle state machine;
- sequential mailbox with explicit call/cast/wait failures, reply,
  cancellation, and actor-death settlement inside the runtime;
- exact-incarnation `AgentRef` values; no transparent recreation or stable
  service reference;
- a shared managed Haskell exit cell per exact reference, filled before the
  Rust terminal transition, plus session-lifetime terminal metadata for
  repeatable waits;
- fixed `EffectStackId`/ABI plus one Rust interpreter instance per actor;
- execution-principal installation on every Haskell entry;
- capability registry with caller authorization, revocation, and fork hooks;
- mapping from actors to lexical scopes and runtime resource scopes;
- one lifecycle ownership tree with child-selected typed startup/shutdown
  values and retained terminal records;
- repeatable typed waits plus Developer-triggered advisory turns for abnormal
  exits, without automatic owner death;
- recursive subtree termination and cleanup when an owner terminates;
- cooperative typed shutdown delivery followed by runtime-owned eventual
  termination with a configurable twelve-hour initial watchdog;
- restricted shutdown execution that may use allowed cleanup effects but
  cannot open an agent session;
- one actor agent-session executor shared by deliberation, startup, and
  advisory, with goal-specific bindings, scoped actions, budgets, and
  settlement;
- one FIFO ready queue per machine session, leasing one Haskell run segment
  through the existing checkout mechanism;
- root-owning message envelopes built on the existing `ValueHandle` ledger;
- structured events and resource counters.

The actor registry belongs above the machine-session registry. It schedules
work through the existing checkout API and never owns a second copy of machine
state or parked continuations.

Acceptance includes races: cancellation versus reply, initialization versus
exit/cancellation/reference publication, owner termination versus child
completion, recursive subtree teardown with outstanding calls, stale
incarnation use, mailbox teardown with queued live values, synchronous
`A -> B -> A` call-cycle rejection, snapshot leases surviving source
retirement, child failure notification while the owner is in a model round,
repeatable waits after execution-resource reaping, and panic-safe restoration
of machine-session ownership.

The kernel lives in a new focused `tidepool-actor` crate above the existing
machine-session, provider-agent, and capability substrates. Actor-specific
orchestration migrates out of `tidepool-harness`; neither the JIT nor
`tidepool-agent` becomes the actor registry.

## 4. Phase 2 — common fenced-Haskell agent session

Promote the existing harness response-to-block-to-resident-run loop into the
one actor agent-session executor. Preserve fenced Haskell as the primary
protocol rather than replacing it with a provider tool call.

Build on the harness's ordered multi-block execution and corrective rounds,
the shared block classifier, persistent declaration environment, persistent
binding store, and resident machine-session mount. Do not fork a second GHCi
implementation inside the actor runtime or leave parallel harness and actor
drivers.

Deliver:

- one assistant-response transport carrying prose plus ordered fenced Haskell,
  with compile and execution results returned as conversation context;
- actor-bound lexical scope and execution principal;
- typed goal input and completion binding;
- ordered provider-role injection: runtime lifecycle facts as Developer
  messages, Haskell-authored startup work as User messages;
- Responses continuation via `previous_response_id`, a Conversation object, or
  exact replay, with lifecycle facts sent as Developer input items rather than
  request-local `instructions`;
- advisory inference for an otherwise-unobserved abnormal exit;
- persistent declarations and bindings across model rounds and deliberations;
- `:goal`, `:bindings`, `:program`, and capability inspection;
- declaration/source provenance sufficient to build program images;
- provider compaction input that summarizes active bindings and behavior
  without rendering all live state.

Acceptance:

- every explicitly tagged Haskell fence executes in order, while prose and
  other fence languages do not;
- later blocks in one response observe earlier declarations and live bindings;
- Haskell source is not JSON-escaped into a provider tool argument;
- Rust does not parse Haskell syntax or argument blocks;
- partial block commit and diagnostic behavior preserve the existing harness
  contract;
- a declaration-only response continues the same agent session;
- completion of the wrong type is retryable and preserves prior declarations;
- an advisory cannot overlap any other actor turn;
- network-mounted delegated agents can use the same fenced-Haskell session and
  machine-session substrate when that frontend is enabled.

## 5. Phase 3 — authored actor DSL

Add the narrow Haskell library surface discovered by the spikes:

- one actor-local concrete effect-stack definition per entry module;
- `Member`/`Members`-polymorphic reusable APIs;
- abstract `AgentRef api exit` naming one incarnation;
- `call`, `cast`, `awaitExit`, and actor lifecycle operations with ordinary success
  types and Rust-owned terminal failure settlement;
- no mirrored `tryStart`/`tryCall`/`tryCast`/`tryWait` family in the initial
  model-facing DSL; structured failures remain Rust lifecycle state;
- `ActorSpec startup api exit` and ordinary-program serving combinators;
- prompted `startActor`, returning only after readiness;
- discoverable `runActor` as the ordinary Haskell composition of `startActor`
  and `awaitExit`, with successful one-shot products carried by `ActorExit`;
- typed `deliberate`;
- explicit behavior installation, inspection, and rollback;
- fresh spawn and program-image deployment;
- typed wait, supervision, and typed shutdown values;
- capability delegation operations where policy permits them.

Provide OODA, worker, and managed-service loops as libraries, not runtime
special cases. Keep model-facing defaults small and use familiar Haskell names.

At this phase, add a new harness entry point that boots an actor program
directly. It must not require:

```haskell
initialState :: State
render       :: State -> Text
loop         :: State -> Harness State
```

Durable facts are read explicitly. Prompt construction belongs to each
`Deliberation`, not to a global render pass.

## 6. Phase 4 — spawn and fork

Implement the two construction operations in
[architecture.md](architecture.md#6-actor-construction): spawn an explicit
specification into a fresh context, or fork one atomic provider/Haskell/control
point. Reusing a program image goes through spawn; resuming an actor stays in
the scheduler.

Acceptance requires atomic publication for multi-child fork, exact fork
snapshots, child interpreter/grant isolation, invalidation of parent-owned
continuation references, fresh-context non-inheritance, and provider cache
metrics for forked prefixes. Independent repeated spawn needs no transactional
batch API.

## 7. Phase 5 — actorize delegation and DevSwarm

Express current one-shot and coding-agent paths as Haskell programs plus
capability grants:

- one-shot: `runActor`, with the job product in the successful exit value;
- research/review delegate: fresh context, constrained capabilities;
- implementation delegate: fresh context plus isolated worktree capability;
- recursive owner: long-lived actor with spawn authority;
- speculative branch: forked context plus separately rebound mutable
  capabilities.

`tidepool-agent` remains the only crate that knows a coding backend exists. Its
step seam supplies model events to the actor runtime; it does not become the
actor registry. A delegated external frontend receives the same fenced-Haskell
runner bound to its actor identity and scope.

Migrate DevSwarm as the first real program:

1. express owner and delegate roles as typed actor specifications;
2. use `DelegateTask` or its successor as an actor protocol;
3. pass `CandidateChange` and review products as live values;
4. run automated commands and fresh-actor reviews;
5. keep integration authority only in the owner;
6. remove its compatibility `State` and old `render`/`loop` entry point;
7. delete adapters whose only purpose was the old harness morphology.

## 8. Phase 6 — verification and program evolution

Add the operational features that make self-extension trustworthy:

- command results bound to the worktree and immutable candidate revision they
  actually exercised;
- review results carrying actor identity and fresh-versus-forked context
  provenance;
- installed-behavior provenance and predecessor links;
- typed validation before installation;
- rollback after runtime failure or regression;
- active-program inventory and scratch-definition accounting;
- prompts encouraging automated verification followed by fresh review;
- operator-visible reason for every install, rejection, rollback, delegation,
  and capability refusal.

The runtime reports what happened against which artifact. Haskell retains
domain policy over which events are sufficient. A particular high-assurance
harness may introduce abstract evidence types, but the actor kernel does not
mandate them.

## 9. Phase 7 — bounded growth and rotation

Measure before choosing a final reclamation design, but make growth visible
from Phase 1 onward:

- transcript tokens and cached-prefix usage per actor;
- declaration generations and compiled fragments per program snapshot;
- active versus scratch roots;
- mailbox, parked-continuation, value-handle, and binding-root counts;
- executable arena and old-space high-water marks;
- fork sharing versus divergent allocation.

The first safety valve may remain quiescent machine rotation. Rotation must
state which live actor values cannot be reconstructed and must never pretend
that deregistering roots immediately reclaims executable arenas or old-space
cells.

Later options include rebuilding a fresh machine from active declaration
images, rotating actor groups, or adding executable-code reclamation. They are
separate engineering decisions after real self-writing workloads establish
their shape.

## 10. Ownership map

| Area | Expected responsibility |
|---|---|
| `tidepool-codegen` | Existing JIT execution, parked continuations, live-root handles, GC accounting; only narrow support needed by root transfer or principal installation |
| machine-session substrate | Persistent declarations/bindings, scope snapshots, checkout, mounted Haskell evaluation |
| actor runtime | Actor registry, ownership tree, mailboxes, principals, per-actor interpreters, capabilities, program images, supervision, model/Haskell coordination |
| `tidepool-agent` | Provider/coding-backend adapter and persistent backend thread seam |
| `tidepool-harness` | Transitional authored-harness driver; actor-specific machinery should move to the actor runtime rather than deepen this crate's current mixed charter |
| `tidepool-repl` | Shared Haskell-aware classification behavior; it does not own the provider-to-resident agent loop |
| `haskell/lib/Tidepool` | Familiar typed actor, deliberation, protocol, and behavior combinators |
| `tidepool-protocol` / `tidepool-mcp` | External effect/tool schemas and generated boundary declarations, not internal actor message schemas |
| `tidepool-handlers` / `tidepool-worktree` | Concrete capability operations and resource ownership |
| DevSwarm | Project-local roles, prompts, repository policy, and acceptance decisions |

## 11. Migration table

| Current surface | Destination | Deletion condition |
|---|---|---|
| `State`/`render`/`loop` selfharness contract | Direct actor-program entry plus explicit durable reads | DevSwarm runs without compatibility state |
| harness-owned fenced-Haskell loop | Common actor agent-session executor | Harness and actor paths use the same ordered response-to-resident engine |
| `RunLLMTurn` plus hidden answerer orchestration | `deliberate` against actor-owned model context | Typed completion and multi-round correction are covered |
| special fork/fanout prompt paths | Spawn, structural fork, and ordinary actor operations | Cache-preserving fork and ordered result assembly are covered |
| JSON-only actor-like mailboxes | Root-owning live-value envelopes | Closure-valued request/result tests pass |
| separate one-shot/delegate runtimes | Haskell actor programs and capability grants | Existing production behaviors are represented without backend leakage |
| machine-global handled prefix | Actor-local fixed effect-stack ABI and Rust interpreter | Different actor rows coexist on one machine without tag ambiguity |
| concrete effect rows in reusable APIs | `Member`/`Members` constraints; specialization only at actor entry | Portable behavior instantiates in the receiving actor |

## 12. Cross-phase test scenarios

The following scenarios should grow in place rather than spawning unrelated
test families:

1. self-specializing actor installs and rolls back a function-bearing behavior;
2. dynamic GADT protocol is deployed to a fresh child and returns a closure;
3. three cache-sharing forks diverge without declaration or binding leakage;
4. forked children preserve the exact Haskell continuation but cannot use
   parent-owned continuation references;
5. fresh reviewer sees the artifact and specification but no parent reasoning;
6. protected worktree capability fails locally after transfer and succeeds via
   authorized actor call;
7. queued live values survive GC and are released exactly once on cancellation;
8. actors with different stacks dispatch through their own interpreters, and
   only row-polymorphic effectful functions cross between them;
9. child failure remains available as the exact typed result of `awaitExit`; an
   active wait suppresses a duplicate advisory, while an otherwise-unobserved
   abnormal exit produces one keyed Developer notification without killing
   the owner;
10. a transiently refused `call` parks its continuation, retries the exact
    operation, and resumes only after it succeeds;
11. a refused `cast` cannot complete through acknowledgment or invented `()`;
    it retries to real mailbox acceptance or terminates;
12. `awaitExit` retries only temporary access to the same terminal record; stale or
    invalid exact references are fatal, and a same-typed actor cannot replace
    the named incarnation;
13. supervised delegated work observes a failed exact exit with `awaitExit` and may
    explicitly start a new child; a dead synchronous call terminates its caller
    without replay or a final inference session;
14. an unobserved abnormal child exit starts one keyed advisory lifecycle turn
    without constructing a Haskell event or re-entering active Haskell; batched
    acknowledgment preserves every key, and four unanswered responses close
    the advisory without terminating the owner;
15. provider failure exhausts bounded transport retry and terminates the actor
    directly;
16. automated checks and independent review records refer to the candidate
   revision actually accepted;
17. DevSwarm completes a candidate/review/revision flow with no global `State`
   rendering and no internal JSON transport;
18. resource accounting explains growth over a long self-extension run and a
   quiescent rotation reports every lost live value.

## 13. Fixed initial policies

Implementation should not reopen these choices without contradictory spike
evidence:

1. `tidepool-actor` owns the actor kernel; mixed actor machinery leaves
   `tidepool-harness` rather than deepening it.
2. A program image contains dependency-closed compiled identities,
   declaration/interface metadata, and leased executable roots. Source is
   provenance, not deployment identity.
3. Capability fork behavior is one of `OwnerOnly`, `ShareWithChild`,
   `RebindForChild`, or `InvalidAfterFork`, registered by the Rust capability
   class.
4. Multi-child structural fork publishes atomically; ordinary repeated spawn
   has no generic batch transaction.
5. Machine admission is FIFO by Haskell run segment through the existing
   checkout mechanism. This is cooperative ordering, not hard fairness.
6. Operation interpreters mechanically park or retry only conditions they own.
   Unresolved failure is terminal; no model action retries, synthesizes a
   result, replays work against a new actor, or nominates a replacement.
7. Each advisory gets four provider responses; exhaustion closes and records
   it without terminating the owner.
8. Cooperative subtree shutdown uses a configurable twelve-hour watchdog.

Only executable-code reclamation remains evidence-driven. The first
implementation measures growth and supports explicit quiescent rotation; it
does not promise online unloading.

## 14. Completion condition

This plan is complete when:

- a real DevSwarm owner runs as an actor program with a resident model and
  resident Haskell environment;
- it can dynamically define, spawn, and call a typed child actor;
- actors exchange function-bearing live values under caller-checked authority;
- actors with different fixed effect stacks execute through actor-local Rust
  interpreters, and reusable Haskell stays `Member`-polymorphic;
- spawn and structural fork have explicit tested semantics; program-image reuse
  is ordinary spawn and resume is ordinary scheduling;
- forks clone the Haskell control continuation while invalidating parent-owned
  continuation references in children;
- startup returns only ready exact-incarnation references; child exits remain
  available through typed `awaitExit`, and abnormal exits notify rather than kill
  owners;
- mechanical retry must actually succeed before an authoritative continuation
  resumes; an unsatisfied continuation terminates its actor;
- owner termination recursively terminates its subtree, with no initial
  detach or adoption mechanism;
- verification uses authoritative commands plus independent actor review;
- the old `State`/`render`/`loop` and JSON-internal-message paths are deleted;
  fenced Haskell remains the primary model interaction surface;
- the model-facing Haskell API has no verb or option whose only justification
  is completeness relative to Rust internals;
- resource growth and rotation behavior are observable and bounded by an
  explicit operating policy;
- standing contracts have moved to the owning crate and API documentation.
