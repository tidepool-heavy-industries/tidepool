# Actor architecture

Status: the actor substrate and persistent reply/watch vertical are landed.
The `Complete`/interactive `AgentAction` portions later in this document are a
superseded design record; the current contract is
[persistent applications, typed replies, and watches](persistent-applications-replies-and-watches.md)
and the current inventory is [implementation status](implementation.md).

## 1. The unit of execution

An actor is not a process identity, a Haskell machine, or a model conversation
considered alone. Every actor combines:

1. a fixed typed Haskell program;
2. a persistent Haskell declaration and binding environment;
3. for an agent-backed actor, one serial Codex context; and
4. Rust-owned identity, lifecycle, execution context, and authority.

```text
                  Ractor local actor runtime
        identity · mailbox · scheduling · supervision
        model context · capabilities · resource scopes
                              │
                              ▼
                   typed Haskell actor program
                fixed control flow and invariants
                              │
     request @Result :: AgentRef -> Text -> input -> Eff effs (Response result)
                              │
                              ▼
       resident fenced Haskell or actor-local hosted tool
          define · inspect · evaluate · spawn · reply · watch
                              │
                         live value a
                              │
                              ▼
              validate · install · invoke · retain
```

The first two execution forms share the same actor kernel:

- a **resident actor** runs its installed Haskell program without owning a
  model-provider loop; and
- an **agent-backed actor** wraps a long-lived interactive agent application.
  Its actor-local custom tool transports persistent GHCi-style workbench turns,
  while the backend owns its conversation and native user interaction.

Execution form is not identity or authority. Both forms use the same exact
`ActorRef`, Ractor ownership tree and mailbox, execution principal, profiles,
and terminal records. They currently emit structured tracing and lifecycle
wakes through their existing owners; a shared neutral event projection is a
future observability boundary, not a second actor runtime. Recreation creates
another exact identity; V0 has no in-place restart or retargeting.

An agent-backed actor receives typed mailbox requests and runtime facts through
its durable node inbox, then invokes the resident workbench through one
GHCi-shaped hosted tool. Rust does not create a resident-provider loop or a
second administrative API beside that program.

## 2. Actor components

The Rust runtime maintains the following logical record. This is a semantic
inventory, not a required Rust struct layout.

| Component | Purpose |
|---|---|
| exact actor identity | One process-local Ractor address; never retargeted |
| mailbox | Ractor-owned queue of local calls, casts, and runtime requests |
| actor program continuation | The currently running or parked installed `Eff` computation |
| program snapshot | Current persistent declarations, bindings, and installed behavior roots |
| agent context | Opaque Codex thread binding plus compaction and usage facts the backend exposes; absent for headless actors |
| execution form | Immutable resident or agent-backed driver selection for this incarnation; descriptive, never an authority check by itself |
| actor interpreter | Rust nominal handlers, grants, and lifecycle restrictions for this actor |
| execution principal | Identity installed while this actor's Haskell runs |
| capability grants | Owned, launch-derived, inherited, revoked, and fork-policy metadata |
| runtime resource scope | Parked frames, handles, cancellation state, and live roots |
| durable namespace | Explicit access to the existing JSON get/put backend |
| supervision state | Ractor links plus Tidepool terminal cells, owner-wake correlation, stop reason, and cleanup policy |

The mailbox and Haskell program are sequential for one actor. Concurrency is
expressed by several actors and asynchronous actor operations between them.
It never re-enters one actor's Haskell session. Codex serializes its native
conversation; every callback into resident Haskell still passes through the
same actor admission and execution-principal boundary.

The initial topology keeps cooperating actors on one shared machine session.
That is what makes structural sharing and arbitrary live-value delivery
possible. A machine boundary is explicit and initially admits only encoded
values.

Two nested admissions have different lifetimes and must not be conflated:

- an **actor-turn admission** owns one authored Haskell continuation. A Codex
  request retains that continuation across hosted-tool blocks, retries, and
  corrective rounds while later calls and casts remain queued in FIFO order;
- a **machine checkout** covers one Haskell run segment. It is released while
  the admitted actor waits on Codex, an actor reply, or external operation,
  so another actor may use the shared machine without re-entering the first.

The existing resident-machine checkout owns one FIFO ready queue per machine
session for Haskell run segments. Newly ready segments join the tail. This provides
deterministic cooperative ordering, not preemptive fairness: a non-suspending
segment can monopolize that machine. Rust requests cancellation at available
safepoints and reports the limitation; workloads needing hard isolation use
separate machine sessions. All JIT admission stays on the existing checkout
path. Ractor's sequential message handler supplies actor-turn admission; no
second registry lock or host task table mirrors that state.

One authored mailbox handler owns its continuation for the full logical
operation. If it suspends on an agent session, call, wait, or external
operation, the resident behavior retains that private settlement state while
releasing any machine checkout. Calls and casts received before it returns to
`receive` retain custody in FIFO order and drain afterward. Kill and shutdown
remain framework control operations. Backend-native owner wakes never re-enter
the current Haskell continuation.

## 3. Ownership boundary

### Rust owns

- exact process-local actor identities and Tidepool-to-Ractor handles;
- Ractor actor construction, links, mailbox lifetime, and sequential turns;
- admission to a shared machine session through the resident checkout;
- supervised Codex processes and thread bindings, exact
  context/thread bindings, compaction, push delivery, and cache accounting;
- one Rust effect interpreter per actor, routing nominal request constructors
  under that actor's policy and execution principal;
- Haskell compilation routing and machine-session checkout;
- continuation and live-root custody;
- capability registration, launch-grant derivation, revocation, and caller
  checks;
- timeout, cancellation, shutdown, and supervision propagation;
- network mounts, Codex transport, node inboxes, and actor-scoped host-tool
  transport;
- durable namespace allocation and external resource cleanup;
- authoritative structured actor events and resource-growth accounting.

Ractor is a local execution dependency, not a wire contract. `KernelMessage`
is one closed Rust enum carrying moved in-process values. The local actor layer
enables no cluster or serialization feature. JSON remains confined to actual
external and durable boundaries.

### Haskell owns

- actor protocols and their result types;
- the fixed named effect profile selected when an actor incarnation starts;
- fixed control flow, state-machine structure, and domain invariants;
- deciding when and why to send typed work to an agent actor;
- installing or rolling back model-authored behavior;
- recursive organization, including which actors to create and how to combine
  their results;
- project-specific verification and acceptance policy;
- ordinary live state captured in immutable values and recursive closures;
- explicit reads and writes of durable facts.

### The model owns no runtime mechanism

The model writes Haskell within the interface exposed by its agent-backed
actor through the native Codex application. A headless resident actor runs
only its installed Haskell program. The model does
not parse process arguments, choose filesystem layouts, maintain actor registries,
construct Codex transport envelopes, assign identifiers, serialize internal values,
or implement scheduling loops.

### Self-hosting shape: iterative worktree hylomorphisms

The intended coding-agent organization is an iterative hylomorphism over
worktrees. This is the core Exomonad-style self-hosting shape, not an
application-specific worker protocol:

1. **Unfold:** an actor turns a goal or work node into smaller typed actor
   definitions, dependencies, worktree grants, and acceptance contracts.
2. **Execute:** child actors work independently and return typed outcomes such
   as commits, test receipts, review findings, questions, or rejected work.
3. **Fold:** an owner reviews and integrates those outcomes into one coherent
   branch and updated understanding of the problem.
4. **Re-unfold:** the fold may expose a new boundary, delete planned work, or
   produce a more precise next decomposition. The process repeats until the
   root contract is satisfied.

The complete tree need not exist in advance. An initial Haskell program may
encode a detailed plan, a rough subsystem sketch, or only the next sound
decision. A node may execute directly, translate written plan steps into
child orchestration, refine an underspecified subsystem, discover work through
inspection, or create another planner. The program writes the remainder of
its organization as it unfolds.

Conversely, self-writing is not a requirement to improvise. Well-understood
work may be almost entirely preplanned: a parent supplies the dependency
shape and acceptance criteria, subsystem actors expand only their local
holes, and small leaves perform mechanical changes. Static structure and
runtime discovery compose in the same tree.

Intelligence is heterogeneous by node. Stronger model configurations are
reserved for unresolved semantic boundaries, decomposition, and integration;
small well-specified transformations can use cheaper configurations. Model
selection and budgets are launch policy interpreted by Rust, while Haskell
expresses the typed work structure and escalation points. A child can return
an unresolved question instead of guessing, allowing its owner to refine the
contract or launch a stronger successor.

Git is the durable work substrate for coding actors, not their communication
protocol. A granted worktree isolates a branch; a commit is a reviewable
product that can travel in a typed exit; and joins are explicit integration
decisions. Live actor messages still carry richer values, definitions,
handles, and evidence. The actor ownership tree, planning tree, and Git
branch tree may correspond for a particular job but are not required to be
identical.

Folds are hierarchical. A leaf can fold edits into one commit, a subsystem
can fold several commits into a coherent branch, and a root can fold subsystem
branches into the accepted revision. Each level receives compact typed
outcomes and evidence rather than every descendant's complete conversation.
Independent review operates at these join points and checks the exact
candidate revision.

### Worktree-backed submission

The first self-hosting worker has one fresh managed worktree selected by its
owner. The owner attaches a capability-specific launch recipe to the worker's
definition. The resident start path carries the recipe without treating it as
authority. After the exact child installs its policy, Shoal validates the
recipe and creates an exact-incarnation binding before launching the external
agent application. Child initialization therefore cannot use the recipe-bound
resource; initialization that needs resource authority will require a future
generic startup-admission seam. This replaces automatic post-spawn worktree
allocation: there is one worktree owner, one binding, and one authority path.

`Worktree` is the public workflow concept. The current manager uses Git's
native linked worktrees: working files, index, and `HEAD` are per actor, while
objects, branches, refs, configuration, and administrative metadata inhabit
one ordinary repository namespace shared with the root. A completed candidate
is therefore directly reviewable and integrable by OID or branch without a
publication/import protocol.

All interactive actors see their active repository at one stable virtual
project path; separate mount namespaces map that name to different real
working trees. This avoids accumulating one Codex project-trust entry per
generated checkout. Worker namespaces keep source and sibling working files
read-only while exposing the shared Git common directory writable. The root
namespace instead maps its source checkout read-write because accepting a
candidate, advancing the shared base, and only then spawning dependent work is
the root's fold responsibility.

This V0 boundary separates working files; it is not a security claim. Actors
can intentionally mutate shared Git metadata, as ordinary linked-worktree
users can. They also inherit environment, network, credentials, caches, and
the host process namespace. Stronger repository authority must be introduced
as a different deployment policy if a concrete deployment needs it.

The external model submits only an authored report. While its `finish_work`
tool call is waiting, trusted Haskell asks the Rust-owned Worktree interpreter
for one coherent submission observation and constructs the worker's typed
successful exit from both values. The owning operation either returns facts
from one stable observation or a typed unstable-observation failure; it never
mixes facts read across detected movement. The observation contains:

- durable worktree identity and recorded base commit;
- either a branch plus observed commit or a detached observed commit;
- staged, unstaged, and untracked state; and
- a typed in-progress Git operation when one exists.

This is an observation point, not a worktree seal. The commit is named
`submittedHead`, never `finalHead`: a clean commit OID is an immutable artifact
that an owner can review or integrate even if the retained worktree later
moves. Dirty state is truthful evidence of what was observed, not a stable
artifact. V0 normally treats a dirty submission as non-integrable; accepting it
later would require a captured patch/tree digest or a fresh authoritative
observation.

The whole product remains the exact live Haskell exit carried by `ActorRef`.
Rust does not keep a second candidate-result registry, rewrite a named tool
result, or join model output to repository facts after actor termination.
Worktree observation and authorization remain Rust mechanics; Haskell owns the
workflow product and acceptance decision.

The exact `ActorRef`, execution principal, and redeemed grants remain the
authority. Project-specific JSON handles, retry keys, receipts, and
acknowledgement policy may be built above that boundary, but Shoal does not
install them as a second generic actor API. V0 does not
carry this state across `--recreate`.

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

This rule does not freeze the effect vocabulary. A distinct algebra with its
own interpreter and useful `Member` constraint may deserve a new effect;
`ActorLocal protocol` is one. Mere lifecycle modes, option bundles, or alternate
spellings of an existing operation do not.

### Primary model interaction

The primary execution protocol is a persistent GHCi-style Haskell workbench,
not a catalog of actor-control verbs. Codex invokes the actor-local custom tool
`tidepool_actor.haskell`, carrying a raw GHCi-style script without JSON
argument ceremony. Colon-prefixed lines are reserved commands, other nonblank
lines are Haskell input units, and `:{` / `:}` delimit one multiline GHC input
unit. A fenced body uses ordinary Haskell: declaration groups are valid
directly, effect sequences use `do`, and one outer tuple or record pattern
binding persists multiple results. Pest owns only this framing; GHC remains
authoritative for the Haskell inside each unit. Units execute in order and
preserve successful prefixes. The tool uses the same classifier, persistent
declaration/binding scope, resident machine, actor principal, effect
interpreter, and structured receipts as the ordinary workbench. Tidepool
cannot safely interpret Codex prose as executable output.

The result of an interactive session may itself be an `AgentAction`: an
ordinary live Haskell computation over the actor's existing effect row. The
hosted-tool response settles before that action runs. Rust then evaluates it
under the resident actor, parks it on actor effects as needed, and activates
the same agent context only when the action explicitly reaches another agent
session. Thus `assemble <$> waitOn a <*> waitOn b` is ordinary typed Haskell
over already-running actors, not a long-lived tool request or a scheduler DSL.
Its composed result may be a closure or user-defined value and remains in the
shared heap when mounted into the next activation.

The `AgentSession` effect opens one typed `Complete output` expectation with an
optional User task. One session owns the request through ordered workbench
execution, transport retries, and corrective rounds. Completing the request
resumes its retained mailbox handler, publishes the request-local reply cell,
and returns the target to `receive`; it does not terminate the actor. Workbench
blocks use separate machine checkouts but remain segments of that request.
Lifecycle wake delivery is backend input, not another executor or completion
shape.

### Actor events and observability

Actor observability should converge on one neutral event projection rather
than a second logging mechanism beside tracing and durable application logs.
Each event carries the
exact actor incarnation, per-actor sequence, ownership and causality context,
and relevant turn, block, call, suspension, or terminal identity. The initial
event vocabulary covers lifecycle, ownership, mailbox settlement, provider
turns, extracted Haskell, compilation and execution, suspension and resumption,
native lifecycle delivery, compaction, capability refusal, failures, and
resource counters.

Events record authoritative facts, but the journal is not the mutable actor
registry or a serialization of the live heap. Closures, continuations, roots,
and mailboxes remain runtime-owned and appear only through opaque identities
and lifecycle facts. Transcript reconstruction, ownership-tree views,
per-actor timelines, streaming, usage accounting, and GUI presentation are
folds over the neutral stream. Native owner notifications are delivery derived
from runtime facts, not substitutes for the event record.

### Conversation roles

Task prompts supplied by Haskell `request` use the User role. Runtime facts use
Developer context when Codex exposes that distinction. Native lifecycle input
is clearly identified as Tidepool runtime context; it is not a generated
Haskell declaration or binding rewrite.

Developer context is a high-authority policy and runtime-fact plane, not a
generic notification bucket. Reserve it for Tidepool-attested invariants,
actor identity and phase, exact declaration/type/binding receipts, capability
constraints, and lifecycle facts; never use it for an ordinary delegated task
or an unverified model claim. Prefer compact Haskell-shaped renderings when
the fact belongs to the model's typed working world—for example `z :: Foo` or
an immutable binding inventory—because they compose with its GHCi mental
model. Such text describes trusted context but does not itself mutate the
Haskell environment. Use an executable `haskell` fence only when the normal
fenced-code path is deliberately meant to run it.

A legal boundary starts a new Codex turn; Rust cannot inject a message into an
inference already generating. Request-local task text is ordinary User input;
actor identity and runtime facts remain separate trusted context.

## 4. Fixed structure with dynamic behavior

Reusable structures such as OODA, research, review, or revision loops are
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

An `ActorRef Reviewer ReviewerExit` can then be used without a JSON schema.
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
unsatisfiable, Rust records a structured failure and abandons the Haskell
fragment that owns the operation. Losing the installed actor-program
continuation is terminal to the actor; losing a disposable workbench fragment
is not.

An actor remains non-reentrant while a synchronous `call` is outstanding.
Incoming application messages stay queued; cancellation and supervision
events remain serviceable by Rust. The runtime should track synchronous wait
edges and reject `A -> B -> A` call cycles with structured runtime failure
rather than hang forever or silently run a second handler against actor-local
state. That failure is Rust-owned lifecycle state, not a constructor added
to the domain protocol.

### Ad hoc requests to agent-backed actors

Indexed GADT protocols remain the advanced surface for a stable service whose
request vocabulary is worth declaring. They are not the default interaction
shape for a long-lived Shoal collaborator. A model-facing caller often learns
the next useful request only after integrating the previous result, and the
same actor should accept successive requests with unrelated input and result
types without replacing its identity, Haskell environment, or model context.

The ordinary sending shape is therefore approximately:

```haskell
review <- request @ReviewReport reviewer prompt candidate
```

The explicit type application is the boundary rule for persistent workbench
units: GHC fixes the result before dispatch even when a later separately
compiled unit cannot contribute inference. The request site records the exact
input and result types and their compiler-derived module dependencies. Rust
does not infer a type from the prompt and the target does not choose one.

Dispatch admits the request and returns an exact `Reply ReviewReport`; it does
not wait for the target. Awaiting that handle is a later Haskell action, so a
caller can admit several independent requests before composing their results.
The target actor processes one activation at a time. Its workbench receives
the live input as `sessionInput` and one activation-local, monomorphic reply
operation. Settling the reply returns the live result to that exact request
and returns the actor to readiness. It does not terminate the actor.

Completion and reply share one typed result-compilation and live-root capture
mechanism. Their dispositions remain distinct: completion resumes a terminal
or enclosing Haskell continuation; reply settles one request obligation and
keeps the target alive. Only the operation valid for the current activation is
presented to the model. Shutdown is a separate actor lifecycle operation.

The Rust-owned activation record correlates exact actor incarnation, request,
input custody, input and result type metadata, delivery, and single
settlement. It extends the existing mailbox/call custody owner rather than
adding a callback registry or serialized result store. Static `call`/`receive`
and ad hoc agent requests ultimately use the same Ractor mailbox and
same-machine live-value custody rules.

One-shot work is derived orchestration: start a long-lived agent, issue a
request, await its reply, and stop it when policy no longer needs the context.
A convenience helper may package that sequence after repeated use proves its
value, but `AgentRef` itself does not carry one terminal work-result type.

### Failure settlement and owner wake

When an actor effect cannot complete, Rust classifies the concrete failure and
consults the operation interpreter. The ordinary Haskell DSL neither receives
a universal failure sum nor selects policy ad hoc. Conditions such as
backpressure, bounded transport retry, or temporary retained-record access are
handled mechanically by Rust and do not spend an inference turn. Mechanical
retry resumes Haskell only after the exact operation succeeds.

If the operation cannot produce its exact result, Rust records the failure and
provenance and settles the unsatisfiable continuation. What owns that
continuation matters:

- an installed actor-program or shutdown continuation is authoritative; losing
  it invokes typed shutdown when possible, performs Rust-owned cleanup, and
  terminates the actor;
- a fenced workbench fragment is disposable; Rust abandons that fragment,
  preserves its already-committed prefix, and returns the structured failure
  as context to the agent session that requested the evaluation.

The second case does not synthesize an effect result or resume failed Haskell.
The model may write a new fragment—perhaps starting a different child—because
it is already inside the ordinary GHCi-style interaction. It is not a separate
recovery or diagnostic protocol. The immutable `ActorExit` retains terminal
program failure for exact `awaitExit`. No final inference session runs in a
dying actor.

An operation may instead define a narrower typed failure result when its
authored consumer has a concrete recovery path. Persistent-agent reply waits
use that boundary for target call failure: `waitReply` maps it to
`ReplyUnavailable`, `AgentAction` short-circuits, and the root driver reopens
the same workbench. Machine, custody, and continuation invariant failures are
not reclassified by this path.

The runtime never synthesizes the missing success value, retries through model
judgment, nominates a replacement actor, or resumes an unsatisfiable
continuation. Dead or stale targets, exhausted mechanical retry, permanent
machine mismatch, rejected call cycles, and cancellation are terminal to the
Haskell fragment that encountered them. Only loss of an installed program
continuation is terminal to the actor.

`call` therefore resumes only after the exact original operation replies.
`cast` returns only after real mailbox acceptance; knowingly dropping a
message is never successful `cast`.

Normal target termination is not a failed `awaitExit`: it returns the retained
`ActorExit exit`. Temporary failure while reading that exact terminal record
is retried mechanically. A stale or invalid exact-incarnation reference is
terminal; `awaitExit ref` never substitutes another actor, even one with the same
`protocol` and `exit` types. After observing an exit, authored Haskell or the
agent operating it may explicitly start a successor.

Long-lived or failure-prone delegated work should therefore use explicit
supervision: start a child, observe its exact typed exit with `awaitExit`, and start
a new child if policy calls for another attempt. That is ordinary Haskell
control flow assisted by the resident model, not transparent runtime replay.
Synchronous `call` remains appropriate where target death genuinely makes the
current obligation unsatisfiable.

Every child terminal transition publishes one informational lifecycle input
to a live agent-backed owner. Completion, failure, and cancellation follow the
same path; an active `awaitExit` does not suppress it. Delivery uses the node's
durable inbox and backend-native wake operation, matching Exomonad's initial
push model. Stock Codex therefore receives queued input through `codex queue`;
a literal Developer-role injection remains backend-specific later hardening.

The notification contains exact actor identity, immutable label, exit kind,
and summary, but it is not a Haskell event and cannot supply the typed result.
The owner obtains authority-bearing data only through its retained `ActorRef`
and `awaitExit`. The Haskell policy may then start a successor, collect a
result, or ignore the fact in ordinary typed control flow.

Delivery never re-enters a Haskell continuation. Input arriving during
inference remains ordered in the durable inbox and is acknowledged only after
the backend accepts the native push. An interactive actor is cooperatively
scheduled: a lifecycle input cannot begin another agent turn until the current
one yields. Accordingly, a nonblocking pending collection result means
“continue other immediately runnable work or end this turn,” never “sleep and
poll.” The lifecycle input begins the turn that performs the next collection.
Delayed or duplicate correlation for an already acknowledged worker is
harmless and requires no policy action. A temporary delivery failure leaves
the row pending for retry and never kills the owner. Codex application failure
settles the exact actor lifecycle; lifecycle delivery does not create another
session executor, completion token, or recovery protocol.

An external interactive agent is an application attached to an actor, not a
second actor identity or lifecycle owner. A worker-spawn response means the
resident actor exists and deployment has been requested; it does not promise
that the external application is already online. Launch failure or unexpected
application death while the child is live asks the owning local actor to fail
that exact child, making the ordinary retained `ActorExit` authoritative while
leaving the owner alive. Deployment tasks request actor transitions rather
than mutating lifecycle state directly.

Lifecycle precedence is decided once by the host. If terminal settlement wins
the race, later application exit is cleanup and cannot rewrite the actor's
result. If unexpected application death wins while the actor is live, the
exact child fails and normal subtree cleanup follows. Loss of the root
application ends the Shoal run. A post-terminal failure to contain an orphaned
native process may still fail the run as a resource-containment invariant, but
it cannot revise the already-published child exit.

Abnormal termination of the resident root program does not end the host. The
host logs the exact terminal result, retires that incarnation's owned fleet,
and starts a fresh root incarnation on the retained queue-ready conversation.
Normal root completion and explicit operator shutdown remain terminal to the
run.

Interactive application ownership precedes conversation binding. A fresh
hosted Codex TUI creates its thread and immediately persists the empty rollout
without a synthetic User submission or inference turn. Only after that
durability barrier does Codex publish the v2 HTTP-over-UDS `/session` callback.
Shoal registers the pane, durable inbox, host-tools listener, and cleanup
resources immediately and reports the application as awaiting binding; the
callback records binding v4, whose subsequent read enables native lifecycle
pushes. Binding discovery has no user-input deadline and remains subordinate
to exact pane ownership. Worker assignments may enter the durable inbox before
binding, but delivery begins only after the queue-ready proof is restored.

## 6. Actor construction

The context relationships are deliberately distinct:

| Operation | Model/Haskell context | Use |
|---|---|---|
| `request` | Same target actor, accumulating Codex conversation and Haskell environment | Give an existing agent another typed task |
| `forkActors` | Exact shared conversation, environment, and control prefix, then divergence | Explore several context-rich alternatives with prefix-cache reuse |
| `startAgent` | Fresh Codex conversation and explicit worktree binding | Obtain an independent coding agent or fresh judgment |
| `startActor` | No implicit model context; explicit Haskell definition | Build a lower-level headless actor |

This is guidance, not a set of runtime role presets. Reviewers, workers,
speculative branches, and one-shot jobs remain ordinary Haskell compositions.

The initial runtime has two construction operations, not a matrix of modes:

| Origin | Model context | Haskell environment | Control continuation | Use |
|---|---|---|---|---|
| spawn | New conversation | Sealed base environment plus the definition's internally captured program image | Typed startup program | Independent actor or fresh judgment |
| fork | Exact shared transcript prefix | Exact shared program snapshot | Cloned at the fork point | Cheap parallel reasoning with full memory and provider-cache reuse |

Starting the same `ActorDefinition` again is ordinary spawn, not a third
construction mode. Resuming an existing actor is scheduling, not
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

`startActor` is the sole ordinary prompted constructor. Its typed startup value
is distinct from the actor's mailbox protocol. Models pass an
`ActorDefinition`; Tidepool internally captures and validates its exact
program image as part of that operation. V0 specializes reusable
`Member`-polymorphic behavior into one concrete actor row and runs that row
directly. Rust authorizes every nominal request under the actor's principal,
grants, realm, incarnation, and lifecycle phase; it never reflects or compares
row order. A later trusted Haskell intent-to-kernel split may strengthen the
static boundary without changing this public construction path.

Each definition selects one named effect profile. Initially:

```text
ReadWrite -> ReadWrite | ReadOnly
ReadOnly  -> ReadOnly
```

Each name currently denotes an experimental resident-Haskell effect row and
spawn-attenuation class. Rust validates the spawn edge, and GHC checks the
definition against the selected child row. It does not constrain native tools
of an attached coding-agent process and is not an operating-system sandbox.
Profile identity is launch metadata, separate from the program image, process
policy, worktree placement, and per-resource grants. A profile name or
definition grants no authority by itself. Within the resident effect machine,
`ReadOnly` means the experimental row lacks `FsWrite`; it is not a security or
global-purity claim about every explicit capability operation in that row. It
still includes actor creation and messaging, may use a granted
`WorktreeHandle`, and may call an explicitly supplied writer actor. Changes
to its own Haskell environment and model conversation remain ordinary local
execution, so `ReadOnly` does not disable self-extension.

Startup runs one concrete `startup -> Eff actorEffs initial` action in the new
child's workbench under that actor interpreter. It is ordinary Haskell and does
not implicitly open a model session. When initialization returns, the trusted
wrapper requires readiness and refuses `receive` and `forkActors` before that point. A
pure authored function combines `startup` and `initial` into the installed
`Eff actorEffs exit` continuation. This removes
the need for separate `prepare`/`StartupM` mechanisms and for a public
`ActorProgram` wrapper.

The installed continuation is not a callback registry or Rust-owned handler
table. `ActorLocal` is a normal indexed effect whose public algebra includes
`receive`; Rust interprets its nominal requests under the current actor
context. `forkActors` deliberately
composes it with the outward `Actor` capability: the former supplies the
current protocol and exit indexes, while the latter expresses actor creation.
Generated export curation hides substrate vocabulary, but imports are not the
enforcement mechanism. GHC uses the indexes to tie the program to the eventual
`ActorRef`; Rust handles nominal requests under the current principal without
reflecting those types.

The program may finish directly, suspend on ordinary effects, or consume one
application message through `receive`. `receive` runs a rank-2 Haskell handler
for the indexed request, settles its exact result, and returns the handler's
next-state value to the program. The reply obligation stays entirely inside
the library/runtime boundary: authored Haskell never receives a linear token
it can duplicate, lose, or settle twice. `serve` is an ordinary recursive
library loop over `receive`, not another actor mode.

An `ActorDefinition` is not ambient authority. `startActor` checks the current
principal, derives the new incarnation's grants, and refuses use by an
unauthorized caller. Copying a definition therefore does not grant the right
to instantiate it.

Per-resource placement is explicit launch metadata, not a recursive scan of
the startup value. An `ActorDefinition` may be immutably decorated with opaque
recipes created by the resource-owning capability module. Recipes convey no
authority themselves: the resource interpreter checks exact principal and
active binding on every use. V0 activates its concrete worktree recipe during
external deployment, after internal policy readiness and before process
launch. Program image, named effect profile, and resource binding remain
separate responsibilities even when one definition carries their correlation
data to `startActor`.

The trusted entry wrapper parks on a kernel-private readiness request that is
absent from `ActorLocal` and from every model-facing profile. That suspension is
the OTP-style readiness point: its continuation already retains the installed
computation, so readiness needs neither a callback table nor a second program-
root registry. The interpreter accepts the request only
from the installed-program resource realm, not a fenced workbench fragment. No
`ActorRef` escapes before it. Rust mechanically retries only initialization
conditions the interpreter owns. Any remaining failure or pre-readiness
cancellation cleans up the incomplete actor and terminates the caller whose
start obligation cannot be satisfied.

A child can terminate between readiness and publication to the caller. The
published `ActorRef` then names a dead exact incarnation. `call` and `cast`
cannot use it, but `awaitExit` returns its retained terminal result.

### Fresh spawn

A fresh actor receives only:

- the sealed system vocabulary;
- the explicit actor program image;
- the explicit typed startup value;
- explicitly granted capabilities.

It does not inherit the parent's conversation, scratch bindings, unrelated
declarations, or ambient authority. A dynamically invented child protocol is
still possible: the internally sealed deployment carries the transitive declarations,
interface metadata, and live behavior roots required to deploy that program.
Starting a definition is not ambient inheritance.

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
a parent/child result analogous to process fork. The ordinary `forkActors`
wrapper consumes that distinction internally: the parent receives child
handles, while each child runs a typed `seed -> Eff effs exit` branch supplied
at the fork point through its new `ActorLocal protocol` interpreter. Returning
from that branch completes the child; it never falls through into the parent's
post-fork continuation. The parent receives no handle until that child signals
readiness.

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

Structural fork preserves the source actor's named effect profile exactly,
because it clones that actor's compiled continuation. Profile attenuation is a
fresh-spawn choice; fork does not recompile the continuation against another
row.

For model-authored compilation, the actor's exact entry facade exports a
Haskell `ActorEffects` alias and `ActorM = Eff ActorEffects`. Turn templates
refer to that alias as source, not to a Rust descriptor of the row. This is a
compile-time name membrane, not a runtime ABI or an authorization check.

## 7. Program images and snapshots

A program image has two jobs:

1. provide executable roots for the actor's fixed program and installed
   behavior;
2. make the relevant types, declarations, and documentation visible to the
   actor's model-facing Haskell environment.

A closure alone satisfies the first job but not the second. Therefore
`startActor` internally seals an `ActorDefinition` into a deployment retaining
an exact declaration view alongside its live roots. The definition names the
top-level heads intentionally visible to the child. Sealing resolves those
heads through GHC-derived export metadata against the compile view that
produced the definition, then captures the exact declaration identities. The
names are not raw module/export syntax, and a missing, ambiguous, stale, or
type-incoherent selection rejects sealing. Fresh spawn therefore does not
inherit the defining actor's ambient binding or declaration namespace merely
because the definition was created there. Project-authored definitions use
the same internal sealing mechanism with a checked-in head manifest.

Sealing compiles the actual child entry facade and typed startup/installed-
continuation adapters before allocating the child. This is the validation
membrane for the existential initialization type and the definition's exact
GHC-derived dependency closure. Authored code does not maintain a parallel
list of names. If the startup computation cannot be compiled from that exact
closure, sealing fails before allocation; Rust does not infer Haskell identity
from strings.

`Program image` is a code-and-value deployment bundle, not a mandate for
another registry, compiler cache, root ledger, or effect-policy object. The
initial same-machine representation should compose mechanisms Tidepool already
has:

- a rooted compiled entry closure, executed through the resident machine's
  existing suspension-capable entry path;
- exact `SessionModule`/interface identities for the compiled dependency
  closure made visible to the child's Haskell environment;
- declaration source and documentation retained only for inspection and
  provenance.

Actor launch metadata pairs that image with placement, ownership, derived
grants, and a named effect profile. Keeping profile policy outside the
image matters: the image answers “what Haskell program and names are being
deployed,” while principal/grant checks answer “which operations are allowed
and who may instantiate it.” Fork may reuse both from a source incarnation,
but they remain separate responsibilities.

The resident code arena owns executable code, the value-handle ledger owns
roots while they are in transit, and the actor's resource realm owns deployed
roots. A program image must not create parallel ownership for any of them.
The startup vertical may add a small deployment record tying these identities
together, but only if it reveals an invariant with no existing home.

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
system prompt before every agent session.

In the first implementation, a process restart preserves only explicitly
stored JSON and external resources recoverable by their trusted interpreters.
Live closures, parked continuations, Haskell bindings, and provider contexts do
not survive. Restart tolerance is not required for the initial actor model.

Shoal's V0 root-recreation contract, whether triggered in-process by abnormal
root termination or explicitly by `--recreate`, is deliberately smaller
still: it may resume the root Codex conversation, but creates a fresh root
incarnation and does not restore worker records, actor references, bindings,
acknowledgments, or pending results. The resumed model receives explicit
reconciliation context that those values are dead. Retained worktrees remain
ordinary external resources, but are not silently rebound to new actors.
Same-incarnation collection replay must not be mistaken for restart recovery.

This does not preclude a later program-image or model-context persistence
scheme; it prevents that speculative work from contaminating the first API.

## 9. Supervision and shutdown

Actors form one lifecycle ownership tree rooted in the runtime. Starting or
forking an actor places it under the caller by default. `ActorRef` is a callable
reference, not an ownership token; copying it does not reparent the actor.

The child definition chooses the startup-input type and successful-exit
type. Runtime failures, cancellation, and forced shutdown remain typed system
exit variants around the successful payload. An abnormal or unexpected child
exit never automatically kills its owner. Rust retains immutable terminal
metadata for observation through `awaitExit (ActorRef protocol exit)`; a successful
Haskell exit value remains in the managed cell shared by copies of that exact
reference. Calls and waits settle independently through their exact
obligations. Every child terminal transition also publishes one informational
native wake to a live agent-backed owner; this never requires a heterogeneous
Haskell system-event type.

`ActorRef` is conceptually a copyable routing identity paired with a shared,
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
other direction: every child exit is retained for exact observation and wakes
its live agent-backed owner. Detachment, reparenting, and adoption are
plausible later extensions but are absent from the initial API. Restart or an
effect-stack change creates a new actor incarnation and a new `ActorRef`.

Termination first publishes the immutable terminal transition, then invokes
the actor's typed shutdown handler for cooperative Haskell cleanup. Hook
failure is recorded as a structured runtime diagnostic and never rewrites that terminal
result. Rust remains responsible for eventual forced termination and all
external-resource cleanup. The planned watchdog is twelve hours, configurable
per deployment; it is a leak backstop rather than an interactive timeout.

The shutdown hook retains the actor's ordinary Haskell row, but the interpreter
enters a closing phase that refuses new agent sessions, actor creation, and other
non-cleanup operations. Shutdown is not an implicit route to a final model
session or a second lifecycle monad.

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

1. One actor has at most one active resident turn of any kind: authored
   Haskell, workbench evaluation, hosted-tool execution, or mailbox handling.
   Codex independently guarantees one native conversation turn; every callback into
   Tidepool remains serialized by actor admission.
2. Every Haskell fragment runs with an explicit execution principal.
3. A copied closure never grants authority merely because it captured an
   opaque capability.
4. Every mailbox value and reply has one observable live-root owner at each
   lifecycle transition.
5. A fresh actor sees no ambient parent declarations, bindings, transcript, or
   grants beyond its internally sealed definition.
6. A fork sees the exact snapshot it claims to fork, never a later sibling or
   a regenerated approximation of the model prefix.
7. Every result-bearing agent session has one statically fixed answer type.
8. Scratch definitions do not become installed actor behavior implicitly.
9. Actor scheduling never creates a second machine-session ownership
   mechanism or bypasses checkout fencing.
10. JSON serialization is never required for same-machine actor communication.
11. Fork points pair one immutable conversation prefix, Haskell snapshot, and
    cloned control continuation.
12. Resident Haskell external actions are reachable only through interpreted
    effects and checked capabilities; its model-facing environment exposes no
    ambient `IO`, FFI, or unsafe escape hatch that bypasses the execution
    principal. Attached native agents have a separate process policy.
13. One actor incarnation has one fixed Haskell effect vocabulary and one
    actor-local effect profile. Rust routes by nominal request identity,
    never by union position or a duplicated effect-row ABI.
14. Every child exit remains available through exact typed observation and
   produces one informational native wake for a live agent-backed owner; it
   never kills the owner implicitly.
15. No actor reference escapes before startup readiness.
16. Owner termination recursively terminates every actor in its owned subtree.
17. Haskell enters a shared machine session only through its existing checkout;
    actors are logically concurrent but not parallel within that machine.
18. Successful exit values live in the shared Haskell cell carried by exact
    `ActorRef` values; the Rust registry retains terminal metadata, never a
    second live-value root.
19. A spawn edge may preserve or attenuate its owner's named effect profile,
    never amplify it. Profile membership limits expressible operation classes;
    principals, grants, and opaque handles independently authorize resources.
20. One worktree-backed worker has one owner-selected fresh worktree and one
    principal-checked binding; Shoal never allocates a competing implicit tree.
21. A candidate receipt distinguishes model-authored claims from one coherent
    Rust-observed repository state and calls its commit `submittedHead`, not
    `finalHead`.
22. A hosted-tool call that returns an executable `AgentAction` settles before
    the action runs; later actor waits and model reactivation are owned by the
    resident actor, not by an open transport request.
22. Collection is repeatable until explicit acknowledgment. Host-tool response loss
    cannot silently drop the Haskell `ActorRef` that retains the typed exit.
23. External child-application failure reaches lifecycle only through the
    resident host owner and never becomes fleet failure merely because the
    failed actor was a child.
