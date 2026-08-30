# Implementation plan

## 1. Delivery discipline

Build permanent components in dependency order and prove each through one real
vertical use. Do not construct a temporary actor stack and later extract the
pieces that happened to survive.

[architecture.md](architecture.md) owns runtime semantics and invariants.
[haskell-surface.md](haskell-surface.md) owns the public Haskell contract. This
file owns current implementation status, delivery order, acceptance evidence,
and deletion gates. If it restates another document's semantics, delete the
restatement.

Backward compatibility is not a constraint. An old mechanism remains only
while it is the acceptance oracle for its replacement. Once the replacement
has parity for the behavior still wanted, delete the old path rather than
retaining a permanent adapter.

The repository's standing mechanism index remains authoritative:

- machine checkout, resident state, and turn-module compilation stay in
  `tidepool-runtime`;
- actor identity, lifecycle, turns, mailboxes, and events stay in
  `tidepool-actor`;
- provider-neutral conversation and calls stay in `tidepool-model`;
- fenced-output parsing stays in `tidepool-model-output`;
- durable append/read stays in `tidepool-repr`;
- live roots and continuations stay in the existing runtime/codegen ledgers.

The actor work adds no second registry, root ledger, transcript, turn parser,
checkout path, durable log primitive, or effect-row ABI.

Every Haskell API addition must answer:

1. Does authored Haskell need this distinction for typed behavior or policy?
2. Can Rust settle it and provide concise Developer context instead?
3. Is there one familiar operation rather than paired low/high-level routes?
4. Is the concept easier for a model to use than to misuse?

If Rust can handle it and Haskell has no policy choice, keep it out of the DSL.
A new effect is welcome when it contributes a distinct algebra, interpreter,
and useful row constraint; do not use effects as names for lifecycle modes or
configuration records.

## 2. Current implementation baseline

This is the canonical status inventory for the plan.

### Landed

- `tidepool-actor` owns exact-incarnation identity, lifecycle state, the
  ownership tree, turn admission, live-value mailboxes, call/cast settlement,
  repeatable waits, and neutral actor events.
- Successful typed exits use the shared Haskell cell carried by `AgentRef`;
  Rust retains terminal metadata rather than another payload root.
- Actor placement binds one session, lexical scope, resource scope, execution
  principal, effect-run policy, and exact source-import membrane.
- One `ActorAgentSession` state belongs to each incarnation. One admitted
  agent session spans provider rounds and mounted Haskell execution.
- `tidepool-model` owns the provider-neutral conversation/call seam, and
  `tidepool-model-output` owns fenced-Haskell extraction.
- `PersistentSession` owns resident declarations, bindings, compile views,
  checkout, and continuations. Exact export facades can expose selected
  declaration identities without inheriting a parent scope.
- `session::workbench` owns the shared source classifier, meta-command parser,
  ordered/prefix-preserving block cursor, and canonical declaration, bind, and
  pure/effectful expression templates. REPL, harness, and actor frontends use
  those mechanics rather than carrying their own parser or cursor.
- `tidepool-actor` owns one multi-round typed-deliberation executor. Its
  resident adapter checks the machine out only for a Haskell segment, compiles
  against the actor's exact source view, and returns a GHC-checked live value
  through the private `Complete result` completion effect.
- Typed deliberation has a real GHC/JIT vertical: a rejected wrong-typed
  completion preserves prior declarations and bindings, the corrective round
  completes, the never-run suffix stays unexecuted, and a closure-valued result
  remains callable after the fragment resource realm closes.
- Rust effect routing is nominal; no positional handler-prefix contract or
  reflected Haskell row ABI remains.

### Not landed

- convergence of the remaining presentation-heavy REPL/harness execution
  epilogues where they still duplicate neutral compile/commit behavior;
- `ActorRuntime capEffs`, actor-local interpreter factories, and caller-checked
  launch policy;
- public `ActorDefinition`, `ActorSpec`, `ActorProgram`, or `startActor`;
- lifecycle advisories and typed shutdown execution;
- model-authored program promotion and dynamic child specifications;
- immutable live-binding snapshots or structural context fork;
- the DevSwarm actor entry and deletion of the old selfharness path.

The legacy harness and REPL are migration inputs and behavior oracles, not
architectures to preserve.

## 3. Fixed implementation choices

These choices remove branches from the first implementation:

- V0 sends the actor's canonical `Conversation` by exact replay. A provider
  cursor is not a second conversation mode; it may later be a disposable
  optimization derived from that transcript.
- A program image contains code/value deployment identity and explicit source
  exports. It does not own placement, authority, or an interpreter.
- `ActorRuntime capEffs` is an abstract Haskell token backed by a trusted Rust
  capability-interpreter factory and launch policy. GHC sees the composed
  `ActorLocal api exit ': capEffs` row; Rust sees only nominal requests and the
  opaque registered handle.
- Models author `ActorDefinition`; one `promoteActor` membrane produces opaque
  deployable `ActorSpec` values. `ActorSpec` has one prompted startup path.
  Startup deliberation may use the child runtime; installation is pure. V0 has
  no alternate startup mode or lifecycle-specific mini-DSL.
- `ActorProgram` wraps one authored `Eff` continuation. One indexed
  `ActorLocal api exit` effect ties its protocol and exit type to the eventual
  `AgentRef`; its `receive` algebra exposes no callback registry, public reply
  token, or parallel mailbox API.
- Per-instance resource authority uses opaque, capability-specific launch-grant
  recipes attached immutably to a specification. V0 has no generic public
  delegation/revocation API and never scans a startup value for capabilities.
- Same-machine calls carry live values. Rust never invents a result or
  substitutes a same-typed actor; an exact-call failure abandons the current
  fragment and is actor-terminal only when that fragment is the installed
  program continuation.
- Fresh spawn precedes structural fork. Program-image reuse is spawn; resuming
  an actor is scheduling.
- Actor-turn admission spans a complete logical turn or agent session.
  Machine checkout spans only one Haskell run segment.
- Live runtime restart is out of scope. Durable JSON and recoverable external
  resources use their existing owners.

## 4. Stage 1 — one resident Haskell workbench

Extract the frontend-neutral resident execution core beside
`PersistentSession`. It owns:

- GHC-sourced declaration/bind/expression classification;
- the canonical resident turn-module builders assigned to
  `tidepool-runtime::session::turn`;
- exact compile-view and source-import use;
- per-item compile, execution, and declaration/binding commit;
- ordered multi-item/block cursors and successful-prefix preservation;
- session introspection for types, declarations, bindings, and exact export
  candidates;
- one meta-command parser with a narrow extension hook for frontend-owned
  queries.

It does not own provider calls, MCP JSON, actor lifecycle, presentation,
`Ask` settlement, or domain-specific suspensions.

Use the existing implementation pieces rather than wrapping them:

- move `tidepool-actor::sequence` into the workbench owner;
- route the actor, REPL, and legacy harness through the same ordered core;
- follow the root one-home decision for turn modules: move any
  frontend-neutral source assembly out of `tidepool-mcp::eval_prep`, and
  narrow or rename the MCP-only stateless eval renderer instead of merging two
  types merely because both are currently called `TurnTemplate`;
- remove the harness compatibility re-export and duplicate unit tests for the
  fenced-block parser.

Thin frontend adapters remain legitimate: the REPL maps neutral outcomes to
MCP responses, while actors retain live values and all suspensions. What must
disappear is duplicated classification, compile/commit, cursor, and
command-parsing logic. Actor-specific `:goal`, `:program`, and `:capabilities`
views come from one actor-owned introspection snapshot through the extension
hook; the workbench does not copy actor state downward.

Acceptance:

- REPL and actor integration tests execute the same declaration/bind/expression
  corpus through the shared core;
- a later item observes earlier successful commits;
- failure or suspension preserves the exact committed prefix and never runs
  the suffix;
- exact actor source imports cannot be replaced by ambient scope or arbitrary
  import strings;
- `:type`/binding inventory data and meta-command parsing each have one
  implementation;
- no production actor dependency on `tidepool-mcp`;
- the superseded high-level block drivers and generic actor sequence helper are
  deleted in the same change that moves their callers.

## 5. Stage 2 — typed deliberation on an existing actor

Complete one typed deliberation before adding actor construction.

The actor-owned executor has one provider/Haskell loop and an internal sealed
obligation interface. V0 needs two obligation shapes:

- **typed completion**, used by ordinary deliberation and later by startup;
- **advisory acknowledgment**, added in Stage 6.

This is an internal Rust distinction, not a universal model-facing option
record. Both shapes use the same conversation, provider call path, fenced
parser, workbench, actor admission, and event stream.

Refine the current actor `TurnLease` into the one phase-aware admission guard
if necessary. A `deliberate` suspension transfers the already-held authored-
program admission into the executor; it must not call `begin_turn` again.
Startup derives the same guard from `StartingActor`, while a root session or
advisory starts it from an idle actor. This is a state transition in one
serialization mechanism, not a nested lease protocol.

Deliver typed completion first:

- mount the goal input as a live Haskell binding;
- expose one completion action whose argument GHC checks against the expected
  type;
- supply the actor-owned `:goal` view through the workbench extension hook;
- append Haskell-authored task context as a User message;
- send the full canonical conversation to the provider;
- execute every tagged Haskell fence in order;
- return compilation/execution receipts as conversation context;
- continue after declarations or a wrong-typed completion without losing
  committed work;
- settle exactly once when the correct live value is produced.

Acceptance:

- one agent-session admission remains held across provider waits, transport
  retry, fenced execution, and corrective rounds;
- deliberation entered from an active Haskell turn transfers that exact
  admission and resumes the same continuation without nested `begin_turn`;
- machine checkout is absent during provider waits and reacquired only for
  Haskell run segments;
- a function-valued result survives later suspension and GC and remains
  callable;
- Haskell source never becomes a JSON tool argument;
- exact replay preserves byte-identical transcript prefixes and reports cached
  input usage;
- provider failure leaves one truthful transcript and one terminal actor
  failure, never a hidden adapter continuation.
- an unsatisfiable operation in a fenced fragment abandons that fragment and
  returns one structured receipt without resuming it or terminating the actor;
  the same failure in the installed program remains terminal.

Deletion gate: the legacy `drive_model_turn`/answerer loop may remain only
until its production caller runs through this executor; it is then deleted,
not kept as a second deliberation route.

## 6. Stage 3 — one real `startActor`

Implement fresh startup for an authored specification before supporting
model-authored program promotion.

### Runtime profile

Add one capability-registry-backed `ActorRuntime capEffs` token. Its abstract
constructor associates:

- a Rust capability-interpreter factory;
- the exact Haskell facade exporting the corresponding capability row and
  model-visible vocabulary;
- allowed lifecycle phases for each operation family;
- caller authorization and grant derivation.

The capability registry is the sole owner of this opaque handle. Do not add an
actor-runtime-profile registry beside it. Promotion composes the kernel-owned
`ActorLocal api exit` effect with `capEffs`; the complete row is then fixed for
the incarnation. The type parameters are checked only by GHC. Rust dispatch
remains nominal and checks the handle, principal, and grants at use time. The
registry also supplies the actor-owned capability introspection view; the
workbench only renders the returned neutral snapshot.

### Haskell specification

Use the single definition shape in
[haskell-surface.md](haskell-surface.md#3-actor-specifications-are-haskell-values):

- one `ActorRuntime capEffs`;
- one `Deliberation startup initial`;
- one pure `startup -> initial -> ActorProgram capEffs api exit`;
- one explicit list of model-visible top-level export heads;
- one shutdown handler running under
  `ActorEffects api exit capEffs` with closing-phase interpreter restrictions.

Startup, the installed program, fenced workbench fragments, and shutdown all
compile against the same composed row. The interpreter's lifecycle phase—not a
second monad or altered stack—controls which `ActorLocal` operations are legal.

For this stage, the composition root promotes one checked-in definition using
the permanent promotion component and an authored exact export manifest. The
public workbench path for model-created definitions waits for Stage 5, but it
must produce the same opaque `ActorSpec`, not a second static constructor.

The process composition root is the sole bootstrap exception: it creates the
initial root actor and trusted runtime tokens directly. Child construction,
including the first test child, goes through the same registry lifecycle and
`startActor` path that model-authored Haskell will use.

### Startup sequence

1. authorize the caller's use of the runtime token;
2. allocate an unpublished child identity, scope, resource realm, interpreter,
   conversation, exit cell, and ownership edge;
3. validate and atomically redeem any opaque launch-grant recipes for that
   exact child;
4. deploy an authored exact program facade into a fresh lexical scope;
5. mount the typed startup value;
6. run one User-role typed-completion session through the Stage 2 executor;
7. run pure installation and validate the resulting actor program;
8. publish readiness and return `AgentRef api exit`.

Failure before readiness publishes no reference and recursively cleans the
unpublished subtree. Death after readiness but before caller resumption may
return an already-terminal exact reference; `awaitExit` must still observe
its retained result.

Acceptance:

- two runtime tokens with different Haskell rows and Rust handlers start actors
  on one machine without positional dispatch metadata;
- unauthorized token use fails before allocation or model inference;
- copying a token or specification does not transfer caller authorization;
- the child's model sees only the runtime facade and authored program exports,
  never ambient parent bindings or transcript;
- startup cannot nest another model session during pure installation;
- readiness, cancellation, exit, and publication races are exhaustively
  tested;
- successful closure-valued exit survives child execution-resource reaping.

The Stage 3 vertical uses a one-shot `program (pure exit)` after startup. It
proves construction, installation, readiness, and retained exit without
pretending the mailbox-consumption surface from Stage 4 already exists.

## 7. Stage 4 — actor operations and supervision

Expose the small Haskell actor vocabulary:

- `startActor`, `runActor`, `call`, `cast`, `receive`, and `awaitExit`;
- ordinary `serve`/worker combinators rather than runtime role presets;
- repeatable exact exits and typed shutdown reasons;
- no `tryStart`/`tryCall`/`tryCast`/`tryWait` mirror family.

`receive` is the sole public mailbox-consumption primitive. Its
`ActorLocal api exit` constraint connects the actor program's protocol and exit
type to the request. Its rank-2 handler returns the exact protocol result plus
the program's next state, while the runtime-private reply obligation cannot
escape into model-authored Haskell. `serve` and state-machine loops are library
code over this primitive.

Finish the Rust lifecycle behavior:

- one FIFO runnable-segment queue per shared machine, above the existing
  checkout owner;
- synchronous wait-edge tracking and `A -> B -> A` cycle rejection;
- owner termination recursively stopping its subtree;
- closing-phase shutdown execution, followed by Rust-owned forced cleanup.

Acceptance covers call/cancel/reply races, queued-root teardown, stale
incarnations, call cycles, subtree termination, quiet normal completion, and
shutdown that cannot obtain Haskell execution before the watchdog. The first
real request/reply child vertical lands here; advisory inference is not a gate
for proving application-message semantics.

## 8. Stage 5 — dynamic programs and caller-checked authority

Let a model promote a newly defined actor definition only after authored
startup works.

The existing promotion component must expose one `promoteActor` operation to
the workbench and produce one program image composed from:

- the rooted definition value;
- exact dependency-closed session-module identities;
- the definition's explicit model-visible top-level heads, resolved through
  GHC metadata against the definition's exact compile view;
- source and deliberation provenance.

Promotion and installed-root state supply the actor-owned `:program` snapshot;
the resident workbench neither owns nor reconstructs that state.

It must reuse the code arena, source-facade mechanism, root ledger, and resource
realms. It must not replay source to manufacture nominally new types, inherit a
live parent scope, or introduce a program-image registry.

Prove the membrane with one model-authored GADT child: the parent defines the
protocol and definition, names a minimal child-visible head set, promotes it,
fresh-spawns the child, calls it with the same nominal protocol type, and
receives a closure-valued result. A regression case redefines one selected head
between definition and promotion and must fail rather than pairing the
definition with a same-spelled later type. Keep one promotion operation; do not
split image, export, and installation into separately stateful APIs.

Promotion must compile the real child entry facade plus typed startup/program
adapters before returning. That proof must cover the existential startup-result
type as well as the public startup, protocol, and exit types; a string-level
head match is not sufficient evidence of nominal compatibility.

Complete caller-checked capability behavior in the same stage:

- moving a closure never transfers its creator's principal;
- opaque operations consult the receiver's grants at use time;
- actor runtime tokens obey the same caller-check model;
- backend and worktree identifiers remain resource identities, not competing
  actor principals.

## 9. Stage 6 — lifecycle advisory

Add one keyed advisory for an abnormal exit not already observed by an active
call or wait. It uses the same agent-session executor with an advisory-ack
obligation, and may execute fenced Haskell under the owner's principal to
inspect state, start a successor, or message another actor. It does not resume
or re-enter the owner's parked authored-program continuation. Four unanswered
provider responses close and record the advisory without killing the owner.

Acceptance covers advisory deduplication, exact-key acknowledgment, several
events coalesced without identity loss, events arriving during inference, and
an owner parked on unrelated work. An unavailable provider closes the advisory
without killing its owner, while the same terminal provider failure during
typed deliberation terminates the actor.

## 10. Stage 7 — structural context fork

Fork only after fresh spawn, typed startup, dynamic promotion, and authority
have real tests.

One atomic fork point contains:

- the exact immutable provider-conversation prefix;
- an immutable declaration and live-binding snapshot;
- the cloned Haskell control continuation;
- capability-class fork decisions and child interpreter instances.

The declaration plane already freezes generations. Add the missing immutable
live-binding snapshot using the existing root ledger; mutable scope ancestry is
not a snapshot. Parent-owned reply, join, pending-call, and continuation
references remain present as values but fail under child principals.

Multi-child fork publishes atomically. Ordinary repeated spawn gets no batch
transaction API. Exact replay is the context-cache contract; record cached
input and divergent allocation rather than adding a provider-specific fork
mechanism.

## 11. Stage 8 — actorize coding work and DevSwarm

Express existing jobs as actor programs and capability grants:

- one-shot work uses `runActor` and returns its product in the successful
  exit;
- independent research/review uses fresh spawn;
- implementation delegates receive an isolated worktree grant;
- speculative branches may use structural fork plus rebound mutable grants;
- integration authority remains only with the owner.

`tidepool-agent` remains the only coding-backend package. Its `AgentId` is a
backend-saga identity attached to an actor, not another lifecycle principal.
`tidepool-worktree::AgentRef` should be renamed or narrowed when actor
principals replace its string owner binding; durable worktree identity remains
unchanged.

Move concrete model-provider adapters out of the transitional harness along
the dependency direction established by the first production actor. Do not
create a speculative provider package before that caller exists.

This stage supplies the first capability-specific launch recipe for the
worktree handle and attaches it with the common `withLaunchGrant` membrane.
That real use fixes the Rust recipe trait and settlement contract; do not add a
generic Haskell `delegate`/`revoke` family preemptively.

Migrate DevSwarm as the first production program, then delete:

- `State`/`render`/`loop` and `ReadState` prompt injection;
- `RunLLMTurn` and hidden answerer orchestration;
- `NodeConvo` transcripts and harness-local turn leases;
- `NodeTree` as lifecycle authority and the special fork/fanout paths;
- selfharness checkpoint/restart code for live state;
- JSON-only internal actor-like messages;
- harness-specific actor event translation once consumers fold neutral events
  directly.

Tree-shaped UI may remain as a projection over actor events. Durable get/put,
worktree recovery, operator gates, and generic JSONL mechanics retain their
existing owners.

## 12. Stage 9 — verification, growth, and rotation

Add provenance for behavior installation, command results, candidate
revisions, independent review, rollback, and capability refusal. Haskell owns
the acceptance policy; Rust reports what actually ran against which artifact.

Measure per actor:

- transcript and cached-prefix tokens;
- active versus scratch declaration generations and roots;
- mailbox, continuation, handle, and binding-root counts;
- code arena and old-space high-water marks;
- fork sharing and divergent allocation.

Use explicit quiescent machine rotation as the first safety valve. It must
report every live value that cannot be reconstructed. Online code unloading or
cross-process live-value recovery waits for workload evidence.

## 13. Completion condition

The plan is complete when:

- a real DevSwarm owner runs as a Haskell actor with a resident model and
  workbench;
- it can define, promote, fresh-spawn, call, supervise, and fork typed child
  actors;
- actors exchange function-bearing values under caller-checked authority;
- heterogeneous actor rows share one machine through nominal interpreters;
- startup publishes only ready exact references and exits remain repeatable;
- advisories inform the model without inventing a Haskell lifecycle inbox or
  re-entering the authored continuation;
- automated checks and fresh review refer to the exact accepted revision;
- the old selfharness state/answerer/tree paths are deleted;
- fenced Haskell remains the primary model interaction surface;
- resource growth and rotation are observable;
- standing contracts have moved into owning crate charters and this planning
  directory can be deleted.
