# Shoal workbench correctness wave

Status: superseded migration record. Its GHCi/workbench findings landed, but
live use rejected its root-completion and interactive `AgentAction` direction.
The replacement is
[persistent applications, typed replies, and watches](persistent-applications-replies-and-watches.md).
Examples below using `complete`, `nextTurn`, or `waitReply` are historical and
must not be used as current API guidance.

This is the large cleanup following the first live exercises of Shoal's raw
Haskell hosted tool. It is intentionally one coherent boundary: make the
Haskell environment presented to a Shoal Codex node truthful, type-safe,
agent-shaped, and pleasant enough to drive real self-hosting work.

It is not a compatibility-preservation exercise. Where an early public shape
misrepresents the intended product, replace it and delete the old Shoal-facing
path. The lower-level actor library remains available for runtime internals,
tests, and explicitly imported advanced programs.

## 1. Product boundary

Shoal is a typed orchestration environment for Codex nodes working in managed
Git worktrees. Its default Haskell surface is not a general-purpose interface
for constructing small headless resident actors.

The distinction is:

- ordinary pure or effectful computation belongs in the current Codex node's
  persistent Haskell workbench;
- a child is spawned when work needs an independent Codex context, worktree,
  lifecycle, supervision boundary, or concurrent line of reasoning;
- the lower-level resident actor substrate may still run headless typed
  services, but `Tidepool.Actors.Shoal` neither imports nor re-exports that
  construction vocabulary;
- an advanced program may import `Tidepool.Actor` explicitly. That is outside
  the default Shoal interaction contract.

The current eager relationship between a Shoal agent-backed actor and its
Codex application is unchanged in this wave. Lazy Codex attachment is neither
a correctness repair nor required for dogfooding, so it is deferred until
measurements justify it.

### Target default vocabulary

The precise spelling must be proven against the extractor and live-value
machinery, but the intended interaction is approximately:

```haskell
filterAgent <- startAgent filterTree filterSpec
activityAgent <- startAgent activityTree activitySpec

filterReply <- request @FilterReport filterAgent filterPrompt filterInput
activityReply <- request @ActivityReport activityAgent activityPrompt activityInput

complete $ nextTurn $
  (,) <$> waitReply filterReply <*> waitReply activityReply
```

The default facade should expose:

- one typed, worktree-bound Codex-node specification/construction path;
- one human-readable instance name for each spawned node, distinct from its
  exact runtime identity and from any reusable definition/kind label;
- an exact `AgentRef` naming one long-lived actor incarnation, independent of
  any request result;
- ad hoc `request @Result` returning a separate exact `Reply result`;
- successful compositional reply waiting through `waitReply` and complete
  lifecycle observation where Shoal policy genuinely needs it;
- `AgentAction`, `nextTurn`, and a helper for lifting ordinary effects;
- worktree creation and observation needed to seed and fold nodes;
- the persistent workbench's completion and inspection vocabulary.

It should not expose by default:

- `ActorDefinition`;
- `startActor` or `runActor`;
- protocol mailboxes, `receive`, or `serve`;
- actor-local effect-row implementation details;
- a worker ledger, receipt registry, or prewritten root program.

`AgentRef` may be a narrow newtype over an exact lower-level actor reference,
but it must not be indexed by one request or terminal result. `Reply result`
retains the same-machine live settlement cell for one request. Neither handle
is a string, JSON identity, or second Rust registry entry.

### Persistent construction vertical (implemented)

Before editing the facade, prove one minimal persistent actor end to end:

1. The parent creates or selects one exact `WorktreeHandle`.
2. `startAgent` creates one child and waits until its Codex application and
   persistent workbench are ready; it does not encode a work-result type.
3. The child receives its own Codex application and raw Haskell tool.
4. The parent issues `request @Result` with a live typed input; the target sees
   that input and a monomorphic, activation-local `reply` operation.
5. The child replies with an arbitrary caller-fixed Haskell value, including a
   user-defined type or closure, without terminating.
6. The parent awaits the exact `Reply result`, then sends a request with a
   different input and result type to the same still-live actor.
7. Starting two actors and admitting requests before composing reply waits
   runs the work concurrently; the Haskell `Applicative` remains ordinary
   composition, not scheduler syntax.

Prefer a small Haskell smart constructor over another Rust protocol. The
request site uses an explicit first type application, normally
`request @Result`, and a private `OPAQUE` sited helper records both request
monotypes and their compiler-derived modules. Do not introduce a reflected
effect-row ABI.

### Dogfooding-derived review lenses

Use the following findings from the preceding folds when reviewing persistent
messaging. They are acceptance criteria for the shape, not a request for more
framework:

- An outer tool success is not an inner Haskell success. Exact multiline GHC
  output and a positive typed settlement must survive the Codex/Shoal boundary.
- Pane closure, process intent, request admission, reply settlement, actor exit,
  and teardown are different facts. Never infer one from another.
- The actor, Codex context, persistent Haskell scope, managed worktree, and
  recursive parentage form one long-lived ownership unit. A `Reply result` is
  a request-local obligation, not a second actor identity.
- Messages are durably pushed to live actors; tmux is only launch and
  observability. Human labels never become routing identity.
- Session-local monomorphic operations and visible `@Result` applications are
  the preferred model affordance. Low-level generic machinery stays behind an
  intentional import.
- Worker reports are claims. The fold owner reviews exact commits, mechanism
  ownership, generated artifacts, failure paths, and verification receipts.
- Focused tests belong in concurrent lanes. Matched extractor-backed and broad
  tests belong at integration folds; their scheduling cost is itself a UX
  signal.
- Campaign-local Haskell declarations are the cheap place for experiments.
  Product APIs require repeated utility, a present consumer, and one clear
  owner; failed experiments are deleted rather than adapted indefinitely.

These lenses retain Exomonad's proven scaffold/fork/converge rhythm and
worktree-context-actor triad while using Tidepool's types and live values for
the communication layer.

### Rebuilt-host canary gate

Before any persistent-messaging implementation lane starts, rebuild/restart
Shoal with the reviewed integration fold and run a disposable single-worker
canary. The root declares a concrete `CanaryReport`, creates one managed
worktree, and starts one real Codex-backed child. That child must:

1. observe an intentional Haskell type error as a complete multiline GHC
   diagnostic;
2. recover in the same resident session;
3. add exactly one line to exactly one disposable file and commit it; and
4. call the session-local monomorphic `complete` with the exact
   `CanaryReport` value.

The root then awaits the live typed report and independently verifies the
commit, one-file/one-line diff, clean worktree, and lifecycle state. Discard the
canary branch/worktree rather than merging its disposable change. Any reliance
on an empty wrapper result, pane disappearance, transcript parsing, or manual
transport recovery blocks the wave and belongs to the interaction owner.

## 2. Landed baseline from the preceding parallel pass

Commit `ba336909` established the baseline this wave must preserve:

- exact exits observed through `waitOn`, `awaitExit`, or completed polling
  suppress the corresponding redundant owner wake;
- `:bindings` combines declared and materialized values without forcing them
  and renders deterministic `name :: Type` entries;
- GHC diagnostics use submitted-unit coordinates, including declaration
  validation failures;
- bare expressions render through Haskell `show` during the same evaluation,
  while closures and other non-renderable values report `<opaque value>`;
- root guidance distinguishes conversation prose from authoritative typed
  Haskell state and uses a real `(,) <$> ...` composition example.

That pass reported `just quick` at 1,188 passed and 3 skipped, focused Shoal
and diagnostic/rendering tests, strict scoped Clippy, formatting, and
`git diff --check`.

The actor-local hosted-tool description still contains the nonexistent
`assemble` example, so prompt/example cleanup is not complete. `ActorEffects`
scope honesty, `:show imports`, completion-result enforcement, and atomic
activation delivery also remain open.

Bare expressions deliberately do not install an implicit persistent `it`
binding in this wave. A model that wants to retain a live value names it with
an ordinary binding. The result must be documented as an observation rather
than described as full GHCi parity.

## 3. Make the completion boundary statically truthful

The workbench currently permits an expression with an incompatible
`Complete` result shape to reach execution, then reports errors such as:

```text
heap bridge error: unexpected heap tag: 0
```

That is a type-boundary bug. The enclosing agent session already knows the
one exact Haskell result type it accepts. Every executable completion must be
checked against that type before evaluation or live-root transfer.

### Tasks

1. Identify the one owner of the current agent-session expected result type.
2. Make the generated workbench wrapper require the submitted completion
   payload to unify with that exact type.
3. Keep declarations, ordinary expressions, and persistent bindings usable;
   only an attempted completion is constrained by the active session result.
4. Reject incompatible `Eff` rows or `Complete` payloads as normal
   user-Haskell diagnostics at the submitted input unit.
5. Preserve the surrounding interactive session and every earlier committed
   input unit after rejection.
6. Never inspect runtime heap tags to decide whether a Haskell value has the
   expected semantic type.
7. Replace generic bridge failures at this boundary with an invariant error
   only for values that passed GHC validation and are nevertheless malformed.
8. Preserve `ba336909`'s same-evaluation Haskell rendering and opaque fallback;
   result-type enforcement must not reintroduce JSON-shaped expression output
   or evaluate a displayed expression twice.

### Focused acceptance

- A direct `Eff '[Complete Wrong, Actor] ()` completion is rejected by GHC
  before execution.
- A type alias and a polymorphic helper cannot evade the check.
- `complete $ nextTurn action` with the required `AgentAction` type succeeds.
- A rejected incompatible completion is followed successfully by `:bindings`,
  `:type`, and a valid completion in the same session.
- A live user-defined value and a closure cross a valid `nextTurn` boundary and
  can be forced afterward.

## 4. Make activations atomic

A runtime-authored activation message and its mounted `sessionInput` are one
logical event. The live run demonstrated a stale lifecycle-failure message
arriving while `sessionInput` was `Nothing`. Developer/runtime input must not
claim a fact that belongs to a different activation generation.

### Contract

- One activation has an opaque monotonic `ActivationId` scoped to the exact
  actor incarnation.
- Its runtime message, input type, mounted live value, reason, and backend
  delivery state are one record owned by Rust.
- Every activation message that mounts a value states the concrete mounted
  Haskell type. A model must not have to infer whether `sessionInput` is an
  action failure, a domain result, or the ordinary readiness value.
- `currentSessionContext`-style facts are stable for that activation.
- A newer activation supersedes undelivered advisory prose from an older one;
  it does not relabel the older input.
- Backend retry may redeliver the same activation idempotently, but may not
  combine its message with another activation's mounted value.
- Provider or transport failure cannot manufacture `Nothing` under a failure
  announcement.

Use a closed Rust sum for activation reason, for example:

```text
InitialUser
ActionCompleted
ActionFailed
ChildLifecycleChanged
RecreatedContext
```

This enum drives rendering. Do not inspect or match rendered strings to
recover semantics.

### Lifecycle noise policy

- If the owner's active Haskell action observes a child's exact exit through
  `awaitExit`/`waitOn`, do not also enqueue a normal completion wake.
- An unobserved child transition may enqueue one compact advisory activation.
- Abnormal failure or cancellation remains visible when no typed continuation
  already owns it.
- Several pending child facts may be coalesced into one activation, while
  retaining exact event identities internally.
- Lifecycle prose carries no copied `ActorRef`, exit payload, or other
  authority. Typed values remain in Haskell.
- Human-readable notices may include the node's non-authoritative kind and
  instance names. They must make clear that these labels are presentation,
  not routing identity.
- Completion order is event order, not actor-id order. When several events are
  coalesced or delivered late, include their monotonic event sequence so a
  correct `1, 3, 2` completion order is visibly intentional rather than
  appearing to be transport disorder.
- Guidance must acknowledge both authoritative patterns: retaining an exact
  reference for later lifecycle inspection, and consuming/projecting its
  typed exit inside the returned `AgentAction`. Do not tell a model to retain
  a reference it has deliberately eliminated through `waitOn` composition.
- `complete (pure ())` returns the actor to silent/manual readiness. It must
  not generate a continuation-completed activation merely to report that
  nothing happened.

### Focused acceptance

- A successful `nextTurn` activation always mounts its successful result and
  uses completion wording.
- A failed action always mounts `Just failure` and uses failure wording.
- No failure wording can accompany `Nothing`.
- Every nontrivial mounted input notice names its exact Haskell type.
- A delayed stale backend message is discarded or recognized as already
  delivered; it never starts a mismatched turn.
- Two already-awaited normal child exits produce no redundant model turns.
- An unobserved abnormal child exit produces one later advisory without
  re-entering an active turn.

## 5. Make discovery honest and GHCi-shaped

Add `:show imports` using the shared workbench command parser and the actual
module environment. It must not be a frontend-specific string special case or
a hand-maintained prompt list.

### Tasks

1. Extend the parsed meta-command sum with the supported `:show imports`
   shape.
2. Render effective persistent imports in deterministic order, including the
   default Shoal facade and user-added imports.
3. Keep `:show` with unsupported arguments a precise rejected command that
   leaves the session usable.
4. Preserve the landed GHC-authoritative `:bindings` union of declared and
   materialized values without forcing either class.
5. Ensure `:browse` and `:type` see exactly the names actually imported into
   authored declarations.
6. Fix the observed contradiction where `:browse` displayed `ActorEffects`
   but a declaration could not name it.
7. Remove the nonexistent `assemble` identifier from the remaining hosted-tool
   metadata and canonical docs. The root prompt was already corrected in
   `ba336909`. Use only real Haskell in examples:

   ```haskell
   complete $ nextTurn $
     (,) <$> waitOn actorA <*> waitOn actorB
   ```

8. State the supported command set exactly. “GHCi-style” does not imply every
   GHCi command exists.
9. Keep command interpretation centralized so the ordinary REPL and actor
   workbench cannot drift.
10. Explain ordered multi-unit execution at the point of use: rejection stops
   the suffix, earlier successful units remain committed, and the receipt
   names how many units were not run. Examples should avoid placing an
   exploratory command ahead of unrelated required work in one submission.

### Focused acceptance

- `:show imports` reports the effective Shoal facade import.
- A user import appears in later `:show imports`, `:type`, and declarations.
- `ActorEffects` is either truly in scope or absent from browse output; the
  two views cannot disagree.
- An unsupported `:show modules` produces one user-facing rejection and the
  next command succeeds.
- Tool metadata contains no undefined identifiers.

## 6. Make the Haskell action path pleasant

The working execution path should not require users to construct `Right`
inside the `AgentAction` representation.

Add one ordinary helper, provisionally:

```haskell
liftAction :: Eff effs a -> AgentAction effs a
liftAction = AgentAction . fmap Right
```

Keep the name only if it reads naturally beside `waitOn` and `nextTurn` in
real transcripts. Do not add several synonymous lift/run helpers.

The public representation of `AgentAction` should be reconsidered at the same
boundary. If no authored use requires direct construction or
`runAgentAction`, hide the constructor in the Shoal facade while retaining it
for the private driver. The default interaction should be through `pure`,
`liftAction`, `waitOn`, `nextTurn`, and ordinary typeclass composition.

Acceptance examples must cover:

- lifting a normal effectful computation;
- two agent nodes started before their applicative wait;
- heterogeneous live exit values assembled with an ordinary function;
- monadic dependency where the second agent is started from the first one's
  result;
- explicit lifecycle branching through the lower-level observation function
  when success-only `waitOn` is insufficient.

## 7. Curate the Shoal module around Codex nodes (implemented)

Replace the current wholesale module re-exports with an explicit export list.
The facade is a product API and should not inherit every future export of a
lower-level module accidentally.

### Tasks

1. Introduce the minimal persistent `AgentRef` and agent specification/spawn
   shape proven by the construction vertical.
2. Require an exact managed worktree at the spawn boundary. Worktree creation
   remains explicit so the parent controls base revision and fan-out topology.
3. Make each request task an ordinary User message. Runtime facts and
   invariant context remain separate from task content.
4. Give the node specification a human instance name intended for panes,
   logs, and notices. Duplicate names remain legal and never replace exact
   identity.
5. Ensure every spawned Shoal child reaches an agent-session/tool boundary and
   therefore receives one Codex application. A successfully returned
   `AgentRef` must not ambiguously name a headless computation or one terminal
   work result.
6. Export `request @Result`, typed reply observation, explicit shutdown,
   action composition, and required worktree operations explicitly.
7. Do not import `Tidepool.Actor` into the facade. Build the agent constructor
   in a private implementation module over the lower-level substrate.
8. Do not export protocol-mailbox construction merely because the private
   implementation uses it.
9. Keep `Tidepool.Actor` and its tests intact as a lower-level library.
10. Rewrite default `:browse`, tool guidance, and examples around Codex-node
   fan-out/fold rather than pure arithmetic actors.

### Design constraints for persistent agents

- `AgentRef` is independent of request and terminal result types.
- The request result type is fixed visibly by GHC at the authored call site.
- Same-machine replies may contain closures and newly declared types.
- One exact `Reply result` is single-settlement and separately awaitable.
- Replying returns the target to readiness; only explicit shutdown terminates
  the actor.
- The worktree binding is principal-checked before the child application can
  operate.
- Startup publication means the typed child and its application deployment
  are usable under the documented readiness contract.
- A launch failure becomes the exact child's typed lifecycle failure and does
  not kill the root.
- Retry does not silently create a second child or settle a different request.
- No JSON schema is required for live same-machine input or output.

## 8. Diagnostics and observability

Instrument and classify this boundary rather than asking a live model to infer
host state from generic failures.

Correlate at least:

- exact actor incarnation;
- activation id and reason;
- hosted-tool invocation id;
- submitted input-unit ordinal;
- expected session result type;
- compile attempt;
- completion or rejection;
- child actor and worktree for `startAgent`;
- lifecycle event and whether a typed continuation already observed it;
- backend delivery attempt and acknowledgement.

Logs remain under `.shoal/logs/` and use the repository's structured tracing
stack. Do not add `println!`/`eprintln!` debugging or a second log file format.

User-facing errors should distinguish:

- rejected Haskell;
- actor lifecycle failure;
- stale activation delivery;
- unavailable child application;
- host dynamic-tool transport failure;
- internal live-heap invariant failure.

The phrase `host dynamic-tool infrastructure failure` is insufficient without
a correlated structured cause in the Shoal log.

### Visible daemon panes

Shoal deliberately gives its two infrastructure daemons tmux windows named
`Host` and `Compiler`. A healthy daemon currently leaves its pane blank, which
is indistinguishable from a wedged or incorrectly launched process during
interactive dogfooding.

Both daemons should mirror a concise human-readable view of their structured
tracing events to their own stderr/tmux pane:

- `Host`: run id, selected interactive-agent binary/version, runtime readiness,
  root actor readiness, application launch/retirement, activation delivery,
  and warnings/errors;
- `Compiler`: executable/worker identity, socket readiness, compile request
  start/finish, elapsed time, cache outcome when known, and warnings/errors.

The pane is a presentation sink, not another logging mechanism:

1. Continue writing authoritative detailed logs beneath `.shoal/logs/`.
2. Configure two `tracing_subscriber` formatting layers or equivalent shared
   event fan-out: detailed non-ANSI file output and concise pane output.
3. Emit each fact exactly once through `tracing`; subscriber filters and
   writers alone duplicate it into durable and pane views. Do not add
   pane-specific `println!`/`eprintln!` calls, callbacks, event types, or status
   channels.
4. Do not wrap either daemon in `tail`, `tee`, or a shell pipeline.
5. Give the pane sink its own conservative filter, defaulting to lifecycle and
   request-level `info` plus warnings/errors. Debug compiler internals remain
   in the file unless explicitly enabled.
6. Never print prompts, Haskell source, credentials, environment contents, or
   arbitrary command output merely to make the pane busy.
7. Emit one explicit ready event after the real readiness linearization point
   and one terminal event on orderly shutdown or fatal failure. A retained
   ready line is sufficient while idle; do not add heartbeat spam.
8. Include stable correlation fields such as run id, actor, activation, and
   compile request in pane events where they exist, while keeping each line
   readable without JSON decoding.
9. If separate processes cannot safely append to one file, use role-specific
   files under the same Shoal run log namespace rather than concurrent
   unframed writes. Preserve an obvious operator path from either pane to its
   detailed log.

Focused tests should capture the pane writer and durable writer independently,
prove that one tracing event reaches both at their configured levels, and
prove that sensitive/source fields excluded from the pane remain available
only in the detailed diagnostic sink. A tmux smoke test should confirm that
both windows retain a ready line after `shoal init --no-attach`.

The workbench's concise help must also explain the four boundaries a model
actually experiences:

1. ordinary Haskell evaluation and persistent declarations;
2. effectful Haskell returned for resident execution;
3. exact child lifecycle observation and advisory telemetry; and
4. `nextTurn` reactivation with a new typed `sessionInput`.

An error caused by crossing those boundaries incorrectly must name the
supported route. In particular, an invalid direct actorful `Eff` completion
must point toward `liftAction`/`nextTurn`; it must not resemble a JSON or value-
serialization failure.

## 9. Centralize fixed prompt artifacts

Tidepool-authored model instructions are currently embedded as long string
literals across host, actor, tool, and selfharness code. Move stable prompting
artifacts into one repository-level `prompts/` tree and compile them into their
owning binaries with `include_str!`.

This is source organization, reviewability, and drift prevention. It must not
become a runtime prompt loader, mutable configuration system, or second message
renderer.

### Artifact boundary

Move these classes of fixed text:

- Shoal root role/behavior instructions;
- worktree-backed Codex-node instructions;
- any retained non-worktree/read-only actor instructions;
- the raw Haskell hosted-tool description and short usage instructions;
- recreation/reconciliation instructions;
- fixed continuation and lifecycle guidance that survives the activation
  redesign;
- other core Tidepool-authored system prompts, including the selfharness
  memory-curator prompt, when their owning subsystem is touched.

Do not move:

- Haskell/model-authored task prompts or startup values;
- user input;
- ordinary error messages and diagnostics;
- structured runtime facts such as actor identity, activation id, input type,
  or lifecycle outcome;
- test-only prose that is merely fixture input rather than product guidance.

Dynamic facts are rendered from closed Rust sums and appended to a stable
artifact through typed code. Do not add `{{placeholder}}` replacement, an ad
hoc template language, or string matching that reconstructs semantic state
from rendered text.

### Proposed layout

```text
prompts/
  README.md
  shoal/
    root.md
    worktree-agent.md
    readonly-agent.md          # only while this execution form remains used
    haskell-tool-description.md
    haskell-tool-instructions.md
    recreated-root.md
    activation-completed.md
    activation-failed.md
  selfharness/
    memory-curator.md
```

The exact file set should follow the inventory; do not create empty speculative
files. `prompts/README.md` defines which provider role consumes each class and
repeats the rule that User task content does not belong here.

### Code shape

Each crate that owns model interaction should expose a small typed catalog,
not scattered raw `include_str!` expressions:

```rust
enum PromptId {
    ShoalRoot,
    WorktreeAgent,
    HaskellToolDescription,
    HaskellToolInstructions,
    RecreatedRoot,
}

struct PromptArtifact {
    id: PromptId,
    role: PromptRole,
    body: &'static str,
}
```

The precise types may be shared only if an existing dependency direction makes
that natural. Do not introduce a new crate solely to deduplicate five lines of
catalog code. The important single source is the asset tree; ownership remains
with the subsystem that sends the message.

### Tasks

1. Inventory every production model-facing fixed string and classify it as
   artifact, typed dynamic rendering, diagnostic, or authored task data.
2. Create only the artifact files justified by that inventory.
3. Preserve exact role semantics while moving content. A move must not silently
   turn User task text into Developer policy or vice versa.
4. Replace long inline strings with named typed catalog entries backed by
   `include_str!`.
5. Move the fixed Haskell `nextTurn` guidance into the Rust-owned activation
   renderer when activation atomicity lands; Haskell should request a typed
   session transition, not own backend prose.
6. Keep interpolated continuity or activation facts in typed render functions
   which compose a static artifact with structured fields.
7. Delete superseded inline copies immediately. Do not retain fallback prompt
   strings.
8. Update prompt wording only in a separate reviewable diff from the mechanical
   extraction whenever possible.

### Acceptance

- Production binaries perform no runtime filesystem reads for prompt assets.
- Moving or omitting an artifact breaks compilation rather than falling back.
- A catalog test enumerates every `PromptId`, verifies nonempty content, and
  records its intended role.
- Focused snapshots prove the assembled root, child, tool, recreation, and
  activation messages preserve structured dynamic facts exactly once.
- A repository search finds no remaining long core Tidepool instruction block
  embedded in Rust or Haskell source.
- No prompt renderer depends on matching its own human-readable output.

This is a good early Shoal dogfood task once the current dirty implementation
lands: it has a clear inventory/scaffold phase and several mechanically
separable extraction boundaries, followed by one cross-context wording review.

## 10. Test strategy

### Cheap component tests

- meta-command parsing and `:show imports` rendering;
- explicit Shoal export inventory;
- `AgentAction` helper laws and failure short-circuiting;
- activation state transitions, deduplication, and stale-delivery refusal;
- observed-versus-unobserved child-exit wake policy;
- exact typed agent construction and ad hoc request decoding.

### Focused real-GHC tests

- facade names are both browsable and usable in declarations;
- invalid completion types fail statically and preserve the session;
- pattern bindings and declaration bindings remain discoverable;
- a closure-valued reply survives `waitReply` and `nextTurn`;
- two replies compose applicatively without serializing their values;
- two differently typed requests settle against one still-live actor.

### Host integration tests without model inference

- a scripted interactive backend proves that `startAgent` binds one worktree,
  installs one hosted Haskell tool, receives successive User requests, and
  returns exact typed replies without retiring;
- launch failure terminates only the child and leaves the root workbench live;
- prompt and mounted input carry one activation id through retry;
- an awaited normal exit does not create an extra backend activation;
- an unobserved abnormal exit creates exactly one advisory activation.

### Major-boundary validation

After the whole tranche is coherent, run formatting, strict Clippy on touched
crates, the complete `tidepool-actor` suite, protocol/schema goldens, and the
generic real-GHC Shoal vertical. Then run one token-consuming manual smoke:

1. `shoal new` in a clean toy repository.
2. Root uses `:show imports` and `:browse` without discovering phantom names.
3. Root defines two different custom output types.
4. Root creates two worktrees, starts two Codex nodes, and admits typed requests
   before waiting.
5. Both children use their raw Haskell tools and reply with independent
   candidate commits while remaining live.
6. Root returns an applicative `nextTurn` over both exact replies.
7. One activation mounts the composed live value with no duplicate lifecycle
   turns.
8. Root inspects and integrates both Git candidates.
9. A deliberately invalid completion is rejected without poisoning the
   session.

## 11. Dogfood wave topology

Maximize each Shoal wave by mixing independent work from different subsystems,
not by pretending dependent edits are parallel. A wave begins from one clean,
integrated base and contains only candidates that can be reviewed and landed
in any order or whose integration order is explicitly harmless.

### Scaffold before fan-out

Before each wave, the root should make one small scaffold commit when shared
types, fixtures, or hotspot-file boundaries are still ambiguous. A useful
scaffold may:

- define a closed Rust sum or Haskell type without implementing its cases;
- extract a large module so later workers own different files;
- add named focused-test fixtures and `todo!()` seams;
- record exact acceptance commands;
- freeze prompt/catalog ownership before mechanical migration.

Do not refactor solely to manufacture parallel work. The scaffold must improve
the final ownership boundary even if no child actors existed.

### Wave 0: establish the dogfood base

This is serial:

1. Treat reviewed commit `ba336909` as the resident-workbench baseline.
2. Commit this plan separately.
3. Start a fresh Shoal root from the resulting clean checkout, without
   `--recreate`:

   ```bash
   just shoal-init -- --session shoal-tidepool-workbench-wave
   ```

   This recipe enters `devShells.shoal`, resolves a matched local extractor
   frontend/compiler worker, builds the checkout's `shoal`, and selects the
   pinned host-tools-capable Codex through
   `TIDEPOOL_INTERACTIVE_CODEX_BIN`. It does not replace the ordinary `codex`
   on `PATH`.
4. Reproduce the remaining live failures or convert them directly into focused
   regression assignments when the existing transcript is sufficient.
5. Create any module/type scaffold required to keep the next wave's ownership
   disjoint.

Until the new `startAgent` facade lands, the dogfood root may use the existing
low-level `ActorDefinition`/`withWorktree`/`startActor` path as bootstrap
machinery. It should define that helper once in its persistent Haskell
workbench, use it to launch real Codex children, and treat the friction as
design evidence. Do not preserve the bootstrap helper as a second public Shoal
API after the narrow facade exists.

### Wave 1: broad independent preparation

Launch these from the same integrated base:

| Lane | Ownership | Result |
|---|---|---|
| Workbench discovery | shared meta-command parser, REPL inspection, actor command adapter | `:show imports`, preservation of landed `:bindings`, scope-honesty tests, and removal of the remaining phantom discovery example |
| Daemon visibility | Shoal tracing initialization and compiler-daemon tracing | existing structured events mirrored into `Host` and `Compiler` panes |
| Prompt assets | root `prompts/` tree and mechanical `include_str!` catalogs | fixed text extraction with no semantic rewrite; leave continuation wording for the activation lane |
| Haskell action ergonomics | `Tidepool.Agent.Action` and isolated Haskell tests | one lift helper, curated constructor exposure, and ordinary composition examples |

If prompt extraction and action ergonomics still overlap in
`Tidepool.Agent.Action`, the scaffold first moves the fixed continuation text
behind one owner; do not accept a predictable cherry-pick conflict as the cost
of parallelism.

Each lane runs only focused tests for files and boundaries it touched. Broad
workspace batteries wait for a fold boundary so parallel workers do not starve
one another.

### Wave 2: correctness and construction

After Wave 1 is integrated, run the following independent cores where the
landed file layout permits:

| Lane | Ownership | Result |
|---|---|---|
| Completion typing | resident workbench compilation/completion boundary | incompatible `Complete` payloads rejected statically without session loss |
| Activation atomicity | resident actor activation state and host delivery | typed activation ids, matching messages/inputs, and lifecycle deduplication |
| Persistent Codex-node construction | private Haskell Shoal implementation plus narrow deployment request | one worktree-bound long-lived `startAgent`, with request/reply added after the lifecycle prerequisite |
| Operator diagnostics | structured error classification and correlation tests | actionable dynamic-tool failures without new logging mechanisms |

Completion typing and activation atomicity are correctness-critical. Give each
candidate an independent review before integration even when focused tests are
green.

### Wave 3: persistent requests, then expose and prove the product surface

Implemented in the checkout. The scripted host vertical proves two different
request/result types against one still-live actor, including a closure and a
caller-defined ADT. Fresh-host Codex dogfood remains the release gate.

This wave begins only after activation and completion are integrated. Persistent
actor lifecycle and request custody are one serial prerequisite; facade,
guidance, and recursive-tree proof may then split where their owners are
disjoint:

1. Implement long-lived `AgentRef`, `request @Result`, exact `Reply result`
   settlement, reply observation, and explicit shutdown against the existing
   actor/mailbox and live-root custody owners.
2. Prove two differently typed requests against one still-live actor.
3. Scaffold the exact explicit export list for `Tidepool.Actors.Shoal`.
4. In parallel where disjoint:
   - finalize the Haskell facade and remove default resident-actor imports;
   - rewrite prompt artifacts and hosted-tool guidance against the real API;
   - build the scripted host/deployment acceptance fixture;
   - update canonical architecture and interaction documentation, deleting
     stale generic-actor claims from the Shoal-specific sections.
5. Fold the candidates into one base.
6. Run a fresh cross-boundary review concerned with terminology, dead exports,
   copied prompt text, and duplicate mechanisms.
7. Run the major-boundary validation and live two-node dogfood smoke.

### Slot-filling side work

When a correctness lane has a genuine dependency or a long focused test, use
remaining wave capacity for bounded orthogonal cleanup already justified by
the touched system:

- externalize substantial Haskell fixtures embedded in Rust into colocated
  `.hs` files loaded with `include_str!`;
- replace enum-shaped magic strings with closed sums plus explicit unknown
  cases where an external boundary requires them;
- remove dead compatibility adapters and stale references revealed by the
  new facade;
- add focused rendering, diagnostic, or ownership tests around an existing
  behavior;
- clean canonical documentation whose owning semantics have already landed.

Do not give one worker an unrelated grab bag merely to consume a slot. Each
side task still has one owner, narrow files, a focused verification command,
and a standalone commit.

### Fold discipline

- A dependent child always starts from the commit that integrated its
  predecessor; “recorded candidate” is not an integration state.
- The root reviews exact diffs and tests before advancing the shared base.
- Candidate reports distinguish worker-authored evidence from runtime-attested
  Git facts.
- After every wave, search for stale terminology and deleted API names before
  launching the next one.
- A final reviewer sees the integrated result, not a collection of unmerged
  branches.

## 12. Commit boundaries

Land the work in reviewable semantic commits after the in-flight changes are
clean:

1. **Workbench result-type enforcement** — static completion contract and
   regressions.
2. **Activation identity and lifecycle deduplication** — atomic input/message
   delivery and wake policy.
3. **Workbench discovery** — `:show imports`, scope honesty, and real examples.
4. **AgentAction ergonomics** — one lift helper and curated representation.
5. **Persistent Shoal agent messaging** — long-lived `AgentRef`,
   `request @Result`, exact `Reply result`, explicit shutdown, and single-owner
   request/live-root custody.
6. **Shoal Codex-node facade** — explicit exports, worktree binding, and
   removal of lower-level actor construction from default discovery.
7. **Prompt artifact extraction** — central asset tree and typed compile-time
   catalogs, preserving wording and provider roles.
8. **Integrated cleanup** — prompts/docs, structured diagnostics, deletion of
   superseded fixtures, and the major-boundary test pass.

Do not keep compatibility aliases for the old Shoal-facing `startActor`
surface. Git retains that history. Do not delete the lower-level actor
substrate merely because Shoal no longer advertises it.

## 13. Explicitly deferred

- Lazy creation or retirement of Codex applications.
- Removing headless resident actors from `Tidepool.Actor`.
- Distributed actors or serialized live values.
- Durable restoration of live Haskell values across root recreation.
- A generic worker ledger, merge queue, or repository receipt protocol.
- Structural Codex-context fork. Explore an explicit split at a chosen parent
  snapshot, producing fresh child actor identities that inherit the same model
  context before their tasks diverge. Keep provider-native thread/snapshot
  forking distinct from transcript replay, and do not implicitly duplicate
  worktree authority, runtime capabilities, or live Haskell values.
- Security isolation for mutually untrusted native agents.
- Replacing the raw dynamic-tool transport with a Codex source-block
  interceptor.
- General protocol mailboxes in the default Shoal facade.
- A Jinja or other runtime prompt-template engine. Reconsider it when several
  artifacts genuinely share structured interpolation; require typed template
  inputs and startup validation rather than ad hoc maps when it becomes real.
- An implicit persistent GHCi-style `it` binding for bare expressions. Named
  bindings already retain live values explicitly; add `it` only if dogfooding
  shows that its convenience outweighs another hidden mutable binding rule.

These may be valuable later, but none should complicate the shortest honest
path to a Codex root authoring typed, concurrent Codex-node work through
ordinary Haskell.
