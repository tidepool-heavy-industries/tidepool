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
- Successful typed exits use the shared Haskell cell carried by `ActorRef`;
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
- `tidepool-actor` owns one multi-round agent-session executor. Its
  resident adapter checks the machine out only for a Haskell segment, compiles
  against the actor's exact source view, and returns a GHC-checked live value
  through the private `Complete result` completion effect.
- Public `deliberate` is a nominal suspension with no authored type strings or
  runtime `Typeable` convention. The extractor gives each site a stable
  binder-qualified identity and records its output and live-input types,
  defining modules, and structured nominal type heads directly from GHC.
  Missing sidecars and colliding site identities fail closed.
- Compiler provenance follows parked continuations, resident bindings, live
  custody transfers, and rooted actor entries. Resuming with a live value
  unions provenance and rejects contradictory metadata; actor adapters derive
  startup and `deliberate`-site contracts from the rooted program rather than from
  caller-supplied side tables.
- Result-bearing sessions have a real GHC/JIT vertical: a rejected wrong-typed
  completion preserves prior declarations and bindings, the corrective round
  completes, the never-run suffix stays unexecuted, and a closure-valued result
  remains callable after the fragment resource realm closes.
- One resident actor runner now owns internal sealing, isolated-scope allocation,
  rooted-entry startup, typed-session capture, readiness resumption, and
  exact parent resumption over the existing machine checkout mechanism. The
  trusted entry wrapper raises the authored row under the kernel-private
  `ActorKernel` effect; `ActorLocal` and model-facing profiles contain no
  readiness or settlement operation. Kernel requests validate their child realm
  and expected phase before the
  registry may publish. The registry owns publication and terminal settlement;
  there is no second actor dispatcher or program-root registry.
- The construction substrate exposes one public `ActorDefinition` ->
  `startActor` route. Its private sealing step materializes a content-addressed
  exact facade directly from the rooted entry's GHC provenance while capturing
  the sole start suspension. A real GHC/JIT/provider vertical covers sealing,
  isolated startup with zero or multiple sequential result-bearing sessions,
  realm-checked readiness, child completion, and parent resume.
- The model-facing `call`, `cast`, rank-2 `receive`, and `serve` vocabulary is
  now typed, extractor-backed, and exercised through the real resident machine.
  `receive` installs an opaque rooted handler and uses one private `ActorKernel`
  effect for named reply/continue settlement. The Rust mailbox interpreter
  moves live requests through that handler, truthfully settles call/cast, and
  atomically publishes a call reply with the callee's next installed receive.
  Recursive `serve` assigns its typed site at the concrete outer call and
  reuses it internally, rather than making the generic library body pretend to
  contain a monomorphic suspension.
- Resident `awaitExit` now parks through the existing exact wait registry and
  resumes with terminal metadata only. A real mailbox-driven child fills its
  shared Haskell `ExitCell`, replies, exits, and is then observed with the same
  typed value; no Rust exit-value store or second root registry exists.
- Effect-schema metadata centrally curates authored constructors, supporting
  types, and helpers. The generated public shim and model-facing descriptions
  consume the same allowlists; internal Core retains the full nominal
  substrate vocabulary.
- Rust effect routing is nominal; no positional handler-prefix contract or
  reflected Haskell row ABI remains.

### Not landed

- convergence of the remaining presentation-heavy REPL/harness execution
  epilogues where they still duplicate neutral compile/commit behavior;
- named `ReadWrite`/`ReadOnly` child profiles, spawn-edge attenuation, and
  capability-specific launch grants;
- an eventual trusted Haskell public-intent -> kernel-effect split; V0 does not
  require it, but current profiles, facades, and nominal handlers must leave it
  additive without changing the actor API, program images, profile semantics,
  or Rust dispatch. Constructor curation remains interaction hygiene rather
  than authority;
- full sealing validation for explicit model-visible value exports and
  shadow-drift/type-coherence probes beyond the landed nominal-head facade;
- lifecycle advisories and typed shutdown execution;
- FIFO runnable-segment scheduling above shared-machine checkout, plus the
  remaining mailbox cancellation/failure race matrix;
- model-authored dynamic definitions and internally sealed child deployments;
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
- V0 specializes `Member`-polymorphic authored behavior into one concrete row
  per incarnation and runs that stack directly. The private sealed deployment
  hides the row behind stable startup/protocol/exit indexes. Rust authorizes
  nominal requests without a reflected row ABI. Trusted Haskell lowering is
  optional after V0, but the public and runtime boundaries must be ready for it
  without redesign.
- V0 has named `ReadWrite` and `ReadOnly` profiles. `ReadWrite` may spawn either;
  `ReadOnly` may spawn only `ReadOnly`. The profile fixes the model-facing row
  and interpreter policy, while grants and opaque handles authorize resources.
- Models author `ActorDefinition` and pass it directly to `startActor`, which
  performs exact sealing internally. Initialization may call the child model
  runtime; installation is pure. V0 has no alternate startup mode or
  lifecycle-specific mini-DSL.
- One indexed `ActorLocal protocol` effect ties the authored continuation's
  protocol to the definition; `ActorDefinition startup protocol exit` already
  ties its exit to the eventual `ActorRef`. The `receive` algebra
  exposes no callback registry, public reply token, or parallel mailbox API.
  There is no public `ActorProgram` wrapper around the `Eff` value.
- Per-instance resource authority uses opaque, capability-specific launch-grant
  recipes attached immutably to a definition. V0 has no generic public
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

## 5. Stage 2 — one result-bearing agent session

Complete one result-bearing agent session before adding actor construction.

The actor-owned executor has one provider/Haskell loop and an internal sealed
obligation interface. V0 needs two obligation shapes:

- **typed completion**, used by sessions opened through `deliberate`;
- **advisory acknowledgment**, added in Stage 6.

This is an internal Rust distinction, not a universal model-facing option
record. Both shapes use the same conversation, provider call path, fenced
parser, workbench, actor admission, and event stream.

`Deliberate` remains the Haskell effect and `deliberate` the operation that
opens a result-bearing session. Rust modules, state machines, and events should
name the generic `AgentSession` and its current completion or acknowledgment
expectation; do not reintroduce a `Deliberation` or `Goal` runtime object.

Refine the current actor `TurnLease` into the one phase-aware admission guard
if necessary. A `deliberate` suspension transfers the already-held authored-
program admission into the executor; it must not call `begin_turn` again.
Startup derives the same guard from `StartingActor`, while a root session or
advisory starts it from an idle actor. This is a state transition in one
serialization mechanism, not a nested lease protocol.

Deliver typed completion first:

- mount the goal input as a live Haskell binding;
- add exactly one scoped `Complete output` effect to a workbench with a typed
  caller; advisory workbenches have none;
- retain any outer return expectations privately so authored Haskell can see
  and settle only the innermost one;
- expose `complete`, with no public obligation id or token, and have GHC check
  its argument against the expected type;
- supply the actor-owned `:goal` view through the workbench extension hook;
- append Haskell-authored task context as a User message;
- send the full canonical conversation to the provider;
- execute every tagged Haskell fence in order;
- return compilation/execution receipts as conversation context;
- continue after declarations or a wrong-typed completion without losing
  committed work;
- refuse synchronous same-actor `deliberate` re-entry while allowing the model
  to define and return closures containing future `deliberate` calls;
- settle exactly once when the correct live value is produced.

Acceptance:

- one agent-session admission remains held across provider waits, transport
  retry, fenced execution, and corrective rounds;
- a session opened from an active Haskell turn transfers that exact
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
not kept as a second agent-session route.

## 6. Stage 3 — one real `startActor` (vertical landed; hardening remains)

The first real sealing/startup vertical is landed. It deliberately uses the
same public `ActorDefinition` -> `startActor` route intended for model-authored
definitions; there is no privileged static-spec constructor or public sealing verb.
This stage remains open until the lifecycle and race acceptance below are
complete.

### Interpreter policy

Do not introduce a reflected effect-row ABI. The named profile is sealed launch
metadata, not a Haskell authority token passed to handlers. The compiled actor
entry fixes its Haskell row; ordinary existential packaging hides that row
inside the private deployment. V0 runs that row directly. Rust remains nominal
and checks lifecycle phase, principal, realm, incarnation, and grants at use
time. A later trusted Haskell intent-to-kernel split may strengthen the static
boundary without changing the actor API or Rust dispatch.

The landed vertical uses the existing `ReadWrite`-equivalent row and source
vocabulary. Completing the named-profile path adds no positional dispatcher or
reflected row registry: profile selection specializes the definition, Rust
validates attenuation, and the existing actor interpreter handles the resulting
nominal requests.

### Haskell definition

Use the single definition shape in
[haskell-surface.md](haskell-surface.md#3-actor-definitions-are-haskell-values):

- one `Member`-polymorphic initialization action, specialized to the child's
  concrete row, whose authored call site supplies GHC's
  `deliberate`-site metadata;
- one behavior selector specialized to that same row;
- one explicit list of model-visible top-level export heads;
- one shutdown handler specialized to the same row with closing-phase
  interpreter restrictions.

Startup, the installed program, fenced workbench fragments, and shutdown all
compile against the same fixed row. The interpreter's lifecycle phase—not a
second monad or altered stack—controls which `ActorLocal` operations are legal.

The landed vertical starts one checked-in definition through the public
operation and derives an initial exact facade from compiler provenance. A
single private capture owns the rooted entry, derives the facade, mints the
isolated child scope, and assembles immutable launch metadata without routing a
deployment token through authored Haskell. Stage 5 completes the membrane with an
authored exact-export manifest and definition/facade coherence probes; it
strengthens this same operation rather than adding another construction path
or image registry.

The process composition root is the sole bootstrap exception: it creates the
initial root actor and installs its interpreter policy directly. Child
construction, including the first test child, goes through the same registry
lifecycle and `startActor` path that model-authored Haskell will use.

The Haskell library hides exactly one row-erasure membrane when it hands the
existential child entry to Rust. The entry travels as the existing rooted live
payload; Rust never decodes the row or dispatches by a union position. A
private readiness request, absent from `ActorLocal` and every model-facing
profile, marks the end of pure installation. The interpreter accepts that
request only from the installed-program resource realm and startup phase, so
fenced model code cannot forge readiness merely by importing a constructor
name.
The parked readiness continuation itself retains the installed program; do not
add a program-root registry beside the resident continuation machinery.

### Startup sequence

1. capture the rooted definition entry, derive its exact facade from compiler
   provenance, and authorize the caller to instantiate it;
2. allocate an unpublished child identity, scope, resource realm, interpreter,
   conversation, exit cell, and ownership edge;
3. deploy the authored exact program facade into a fresh lexical scope;
4. run the rooted child entry, whose authored initialization action carries the
   typed startup value;
5. service each sequential User-role result-bearing session through the Stage 2
   executor until initialization returns;
6. validate, while still unpublished, that installation parked the private
   readiness request in the child's own resource realm;
7. publish readiness, settle the parent's start continuation with the exact
   `ActorRef`, and schedule the validated installed continuation through
   ordinary actor-turn admission.

Capability-specific launch-grant recipes are intentionally not part of this
vertical. When the first concrete resource needs them, redemption belongs at
the authorization/allocation boundary above without changing the actor entry,
readiness, or publication mechanism.

Failure before readiness publishes no reference and recursively cleans the
unpublished subtree. Death after readiness but before caller resumption may
return an already-terminal exact reference; `awaitExit` must still observe
its retained result.

Acceptance:

- one checked-in definition starts through the sole public construction
  operation and the same private sealing/lifecycle path reserved for model-authored use;
- startup carries its existential child entry as a rooted value without
  positional dispatch metadata or Rust row reflection;
- unauthorized definition start fails before allocation or model inference;
- copying a definition does not transfer caller authorization;
- the child's model sees only the runtime facade and authored program exports,
  never ambient parent bindings or transcript;
- startup cannot nest another model session during pure installation;
- readiness, cancellation, exit, and publication races are exhaustively
  tested;
- successful closure-valued exit survives child execution-resource reaping.

The `ReadWrite`/`ReadOnly` row proof belongs with named profiles in Stage 5.
Stage 3 must leave no reflected ABI or registry that profile selection would
have to route around.

The first Stage 3 vertical installed a one-shot continuation that immediately
returned `exit` after initialization. Stage 4 now extends that exact root and
continuation path with installed receives; it did not introduce a second
program registry or actor dispatcher.

## 7. Stage 4 — actor operations and supervision

Expose the small Haskell actor vocabulary:

- `startActor`, `runActor`, `call`, `cast`, `receive`, and `awaitExit`;
- ordinary `serve`/worker combinators rather than runtime role presets;
- repeatable exact exits and typed shutdown reasons;
- no `tryStart`/`tryCall`/`tryCast`/`tryWait` mirror family.

`receive` is the sole public mailbox-consumption primitive. Its
`ActorLocal protocol` connects the actor program's protocol to the request;
the enclosing definition separately fixes exit. Its rank-2 handler returns the exact protocol result plus
the program's next state, while the runtime-private reply obligation cannot
escape into model-authored Haskell. `serve` and state-machine loops are library
code over this primitive.

Finish the Rust lifecycle behavior:

- one FIFO runnable-segment queue per shared machine, above the existing
  checkout owner;
- synchronous wait-edge tracking and `A -> B -> A` cycle rejection;
- owner termination recursively stopping its subtree;
- closing-phase shutdown execution, followed by Rust-owned forced cleanup.

The resident call/cast/receive/serve vertical and atomic reply-plus-next-state
settlement are landed. The remaining work in this stage is scheduling,
shutdown, and adversarial lifecycle/cancellation coverage—not another mailbox
execution or exit-value retention mechanism.

Acceptance covers call/cancel/reply races, queued-root teardown, stale
incarnations, call cycles, subtree termination, quiet normal completion, and
shutdown that cannot obtain Haskell execution before the watchdog. The first
real request/reply child vertical lands here; advisory inference is not a gate
for proving application-message semantics.

## 8. Stage 5 — profiles, dynamic sealing, and caller-checked authority

The single private start capture already derives a content-addressed exact facade
behind `startActor`. Complete that membrane so a
model can start a newly defined actor without weakening the checked-in vertical.

Land the two initial named profiles through this same path:

- `ReadWrite` actors may start `ReadWrite` or `ReadOnly` children;
- `ReadOnly` actors may start only `ReadOnly` children;
- GHC checks each definition against the selected profile row;
- Rust validates the spawn edge before allocation;
- profile identity remains launch metadata, separate from program images and
  per-resource grants;
- `ReadOnly` excludes ambient write effects but may call an explicitly supplied
  writer actor.

Acceptance proves every permitted edge, rejects `ReadOnly -> ReadWrite` before
allocation, rejects a write-using definition specialized to `ReadOnly`, and
shows that copying a profile choice or definition transfers no resource grant.

The existing sealing component must extend its program image to cover:

- the rooted definition value;
- exact dependency-closed session-module identities;
- the definition's explicit model-visible top-level heads, resolved through
  GHC metadata against the definition's exact compile view;
- source and agent-session provenance.

Sealing and installed-root state supply the actor-owned `:program` snapshot;
the resident workbench neither owns nor reconstructs that state.

It must reuse the code arena, source-facade mechanism, root ledger, and resource
realms. It must not replay source to manufacture nominally new types, inherit a
live parent scope, or introduce a program-image registry.

Prove the membrane with one model-authored GADT child: the parent defines the
protocol and definition, names a minimal child-visible head set, starts it,
calls the child with the same nominal protocol type, and
receives a closure-valued result. A regression case redefines one selected head
between definition and start and must fail rather than pairing the definition
with a same-spelled later type. Keep sealing inside the one start operation; do
not split image, export, and installation into separately stateful public APIs.

Sealing must compile the real child entry facade plus typed startup/installed-
continuation adapters before allocating the child. That proof must cover the
existential startup-result type as well as the public startup, protocol, and
exit types; a string-level
head match is not sufficient evidence of nominal compatibility.

Complete caller-checked capability behavior in the same stage:

- moving a closure never transfers its creator's principal;
- opaque operations consult the receiver's grants at use time;
- actor-definition launch and capability use obey the same caller-check
  model;
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
a result-bearing session terminates the actor.

## 10. Stage 7 — structural context fork

Fork only after fresh spawn, typed startup, dynamic sealing, and authority
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
- it can define, internally seal, fresh-spawn, call, supervise, and fork typed child
  actors;
- actors exchange function-bearing values under caller-checked authority;
- `ReadWrite` and `ReadOnly` actors share one machine through nominal request
  routing, and spawn edges never amplify profiles;
- startup publishes only ready exact references and exits remain repeatable;
- advisories inform the model without inventing a Haskell lifecycle inbox or
  re-entering the authored continuation;
- automated checks and fresh review refer to the exact accepted revision;
- the old selfharness state/answerer/tree paths are deleted;
- fenced Haskell remains the primary model interaction surface;
- resource growth and rotation are observable;
- standing contracts have moved into owning crate charters and this planning
  directory can be deleted.
