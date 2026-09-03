# Shoal field guide

Status: evolving. This is the practical guide for using and improving Shoal.
It records techniques proven in live work and keeps proposed improvements
visibly separate from the current interface. Runtime semantics remain owned by
the relevant crate documentation; active designs remain in `plans/`.

## Working model

Shoal is a typed orchestration environment for Codex nodes working in managed
Git worktrees. Use the persistent Haskell workbench for orchestration and live
typed state. Use native repository tools to inspect, test, review, and
integrate exact commits.

The root owns architecture, decomposition, integration, and cross-boundary
coherence. A child is worthwhile when work needs an independent Codex context,
an isolated worktree, concurrent reasoning, or a lifecycle boundary. Ordinary
calculation and small orchestration helpers belong in the root's live Haskell
environment.

Conversation prose explains tasks and wake reasons. It is not authoritative
state. Exact actor references, worktree handles, exit values, and activation
inputs stay as typed Haskell values; exact commits and repository state are
verified with Git.

## Core techniques

Start by inspecting the actual environment:

```haskell
:browse
:bindings
:type sessionInput
```

Use `:type` and `:info` when a name or constructor is unclear. The supported
meta-command set is intentionally smaller than full GHCi and should be learned
from the tool's current help rather than guessed.

The workbench executes input units in order. A rejected unit stops the suffix;
earlier successful declarations and bindings remain committed. Effects already
performed by the rejected unit are not rolled back. Put a declaration group or
one effect sequence inside `:{` / `:}` and persist several results with one
outer tuple or record binding.

Define campaign-specific types and helpers freely in the live session. They
are cheap experimental vocabulary and may capture exact handles or user-defined
values without becoming product API. Promote a helper into the repository only
after repeated use shows that it improves discovery or composition for more
than one task and has a clear owner.

Create independent worktrees explicitly, then start every independent child
before waiting on any of them. Compose already-running results with ordinary
Haskell:

```haskell
complete $ nextTurn $
  (,) <$> waitOn implementationAgent <*> waitOn reviewAgent
```

`waitOn` is the success-shaped composition path. Use `awaitExit` when failure
and cancellation are domain decisions. `nextTurn` reactivates the same Codex
context with the successful live result mounted as the next `sessionInput`.
Closures and user-defined types can cross this boundary without serialization.

Integrate incrementally. Once a candidate is clean, independently inspect its
exact diff and verification evidence, then land it if it is coherent. Do not
hold unrelated finished work behind the slowest child. Start dependent work
only from the commit that integrated its prerequisite.

A worker report is an authored claim. Verify the named commit, clean worktree,
changed targets, and important failure paths yourself. Tmux panes and tracing
are useful operator telemetry, but neither replaces typed lifecycle state or
Git evidence.

Keep concurrent verification focused. Compile every changed target, execute
the smallest tests that prove the behavior, use the repository's matched
Nix/extractor setup for Haskell-backed checks, and reserve broad validation for
a meaningful integration boundary.

## Current temporary sharp edges

The default Shoal facade still exposes lower-level resident-actor construction.
Until the Codex-node facade lands, a bootstrap worker must use `agentSession`
to enter an external Codex application. `deliberate` requests a resident
provider and is not the construction path for a worktree-backed Shoal node.
This distinction is a known product defect, not intended permanent vocabulary.

A root recreation starts a new actor incarnation. Conversation may survive,
but previous Haskell bindings, actor references, worktree authority bindings,
pending exits, and mounted live values do not. Reconcile through current Git
and runtime state instead of trusting transcript references.

The currently running root still uses Codex `3c67184`, whose Code Mode wrapper
may discard an inner Haskell rejection. Empty wrapper output is not affirmative
evidence that Haskell completed. The checkout now pins Codex `b8d44e30`, which
passes completed custom-tool text through verbatim and removes the hard-coded
script wrappers; the next Shoal restart must validate that repaired boundary.

Linked worktrees share Git objects and configuration but not working files,
indexes, or `HEAD`. They may also encounter compiler-cache or stale-extractor
confusion if a check bypasses the repository's matched toolchain path. Treat
the toolchain identity as part of test evidence.

## Aspirational direction

These are product goals, not claims about the current checkout:

- The default facade exposes `startAgent`, a narrow typed `AgentRef`,
  `request @Result`, separately awaitable `Reply` values, action composition,
  explicit shutdown, and managed-worktree operations. An agent keeps its
  identity, model context, Haskell scope, and place in the recursive ownership
  tree across requests; a reply settles one request rather than terminating
  the actor. Lower-level resident actors remain available only through an
  intentional advanced import.
- Workbench rejection, actor completion, lifecycle failure, stale activation,
  and host infrastructure failure render as unmistakably different outcomes.
  GHC diagnostics always reach the model without wrapper-specific handling.
- `:show imports`, `:browse`, `:type`, and authored declarations describe the
  same effective module environment.
- Each activation atomically binds one reason, message, input type, and live
  value under an opaque activation identity. Stale delivery cannot pair prose
  with another activation's input.
- `Host` and `Compiler` panes retain concise readiness and request-level
  tracing while detailed structured logs remain authoritative.
- Common fan-out, review, and fold patterns remain ordinary Haskell rather
  than growing a second scheduler, worker registry, or merge queue.
- A future context-split operation may fork several child Codex nodes from one
  deliberate parent snapshot so they share the full architectural context up
  to that point and diverge only under their typed assignments. This must be
  explicit and distinguish a provider-native context fork from replaying the
  visible transcript. Each child still receives a new actor identity and
  explicit worktree authority; capabilities and live Haskell values are
  transferred deliberately rather than cloned implicitly, and only typed
  results fold back into the parent.

## Current wave handoff

This section is a restart aid for the active self-dogfooding wave and should be
deleted when the wave closes. As of 2026-09-03, `main` contains these reviewed
folds after `origin/main`:

```text
5812624e fix: carry constructor metadata across live actions
9bb8921e feat: simplify Shoal agent actions
00392b63 refactor: extract fixed prompt artifacts
07765a1b docs: add evolving Shoal field guide
d11e7d52 feat: surface Shoal daemon tracing in tmux
7f09f384 feat: expose persistent workbench imports
6aa5638b fix: make Shoal activations atomic and typed
84fd259b plan: define persistent Shoal agent messaging
c52a51fd feat: make actor completion expectation first-class
HEAD     fix: harden Shoal dogfood boundary
```

The activation fold makes delivery atomic and typed and rejects duplicate or
stale activation sequences. It is the prerequisite for persistent requests.
It does not yet implement the complete persistent-request record or long-lived
agent lifecycle described below.

The completion-affordance candidate from actor `actor-9-1` was manually
integrated as `c52a51fd` after the old host began rejecting every dynamic tool
call before GHC received it. Git commit `613ed7192` in managed worktree
`wt-37f74e28-dde7-4d67-9536-9563d1093c6e` is its original authored commit.
The fold supplies the monomorphic, activation-local result compiler that
persistent request settlement should reuse. The old root Haskell continuation
was awaiting this actor together with two already-finished actors; after a
restart, recover from Git and actor/worktree state instead of expecting the old
live Haskell binding to exist.

The worktree `wt-e8fc3099-529d-4bf0-ad58-adf5d2d23943` on branch
`tidepool/worktree/shoal-start-agent-wt-e8fc3099-529d-4bf0-ad58-adf5d2d23943`
contains a deliberately held, dirty `startAgent` spike. Do not land it whole:
it encodes the superseded one-shot `AgentRef exit` model. Its curated facade,
worktree-authority recipe, compiler-derived site metadata, and visibility tests
are useful source material for the persistent implementation.

### Architecture review of the landed wave

The root reviewed every Shoal-era commit from `cfc9ab940` through `c52a51fd`
against its production consumers, not only the worker reports and focused
tests. The mechanisms have coherent owners: rejection recovery and lifecycle
policy remain in `tidepool-actor`; compilation, persistent declarations, and
constructor vocabulary remain in the runtime/toolchain path; prompt artifacts
are compile-time assets selected by their consuming host; tracing uses the
existing `tracing` and tmux owners; and the Haskell facade remains a thin typed
surface over those mechanisms.

The audit found and corrected these cross-commit gaps in the final integration
fold:

- The four-constructor Haskell `SessionActivation` sum trapped in the real JIT
  at the generated effect boundary. It is now a hidden `newtype` with a closed
  set of public pattern synonyms, retaining type-directed authored control and
  the extractor's supported unboxed representation.
- Completion integration tests still destructured the pre-activation
  `SessionReady` shape. They now consume the exact `ResidentActivation` record.
- An observed child exit could be inserted after its supervisor callback had
  already run and remain in the pending set forever. The actor-local lifecycle
  record now distinguishes typed observation from supervisor processing in
  either order, keyed by exact incarnation, and removes any failure deferred
  before its typed observation.
- A failed internal completion resume could leave the actor standing at
  `Boot`. The activation-local continuation is restored when transfer fails.
- Extracted prompt assets still taught the superseded generic `complete action`,
  `pure`, and `assemble` shapes. The multiline artifacts now direct
  the model to inspect the session-local `:type complete` and pass its exact
  value directly.
- The tracing fold's two `needless_borrow` warnings were wave-owned rather than
  unrelated. They are fixed, and strict Clippy now passes for the compiler
  daemon and the affected host/actor targets.
- The completion fold narrowed the generated Deliberation import without
  updating its protocol golden. The reviewed golden now matches the generated
  schema, and the committed fixture fingerprint matches the current extractor
  inputs.

The persistent-import constructor has one documented Clippy arity exception:
it takes the independently owned fields of one immutable compile-view snapshot.
Introducing carrier structs solely to cross the lint threshold would obscure,
rather than clarify, its ownership boundary.

The live root inherited a `CARGO_TARGET_DIR` inside an actor-specific directory
that disappeared when the pane/worktree lifecycle advanced. Use a stable target
such as `/tmp/tidepool-root-fold-build` and unset inherited extractor endpoints
for root verification. A matched command has this shape:

```sh
env -u TIDEPOOL_EXTRACT -u TIDEPOOL_EXTRACT_WORKER \
  -u TIDEPOOL_EXTRACT_DAEMON_SOCKET \
  CARGO_TARGET_DIR=/tmp/tidepool-root-fold-build RUSTC_WRAPPER= \
  just test tidepool 'test(actor_host::tests::shoal_exposes_generic_haskell_actor_composition)'
```

The held spike should be mined selectively. Its private
`Tidepool.Actors.Internal.Agent` shows the existing worktree-authority recipe;
its preamble changes prove how to omit companion modules behind a curated
facade; and its visibility fixtures test that low-level actor construction is
absent. Its `AgentRef exit`, startup prompt/input, `waitOn`, and terminal
completion model must be discarded.

### Next-wave design philosophy

The next wave should treat the failures observed here as design input, not as
an unrelated bug list. Exomonad's battle-tested steering model is useful where
it names real structure: agent work unfolds through scaffold and delegation,
then folds through review and integration; a node's worktree, context window,
and actor lifetime form one ownership unit; child events are pushed; and tmux
is observability rather than transport. Shoal adds a persistent typed Haskell
scope and live values to that unit.

Apply these lenses:

- Compiler feedback is the primary interaction UI. Preserve exact multiline
  diagnostics across every wrapper and require affirmative typed settlement;
  tool return, empty output, pane disappearance, or process intent must never
  masquerade as semantic completion.
- Separate readiness, request admission, reply settlement, actor exit, and
  teardown. Each transition needs one explicit owner and observable evidence.
  Replying is not exiting; an observed dead pane is not a typed reply.
- Keep one authoritative exact identity. Human labels and branch names are
  coordinates for people, not routing keys. The long-lived actor, Codex
  context, Haskell scope, worktree authority, and recursive parentage should
  be born and retired coherently.
- Make coordination push-based and durable, but keep payload contracts typed.
  Tmux panes and log lines help operators; they do not carry messages or own
  lifecycle truth.
- Treat a scaffold as compressed architectural context and a fold as a design
  review, not just `git merge`. Merge independent completed work as it becomes
  reviewed; spawn dependent work only from the integrated prerequisite.
- A child completion report is a submission claim. The parent inspects the
  exact commit, affected owners, generated artifacts, failure paths, and test
  evidence before accepting it.
- Make invalid model actions hard to express: monomorphic activation-local
  operations, `request @Result` at cross-unit boundaries, curated imports, and
  closed reason vocabularies. Do not compensate with string parsing, prompt
  schemas, `Proxy`, or a universal low-level facade.
- Let abstractions pay rent. Preserve names such as unfold, fold, activation,
  and reply when they compress real semantics; reject speculative registries,
  paired mechanisms, or role-general machinery without a present consumer.
  Temporary bridges must name their owner, destination, and removal condition.
- Treat test topology as part of developer efficacy. Concurrent workers run
  focused proofs; extractor-backed and broad tests run at folds with a matched
  toolchain. A test that passes alone in 19 seconds but takes 406 seconds in a
  broad parallel tier is correct but badly scheduled.
- Experiment freely in live Haskell declarations and campaign-local helpers.
  Promote only repeated, general improvements with a clear mechanism owner;
  delete the unsuccessful shape instead of preserving compatibility layers.

The Codex pin in this integration fold advances from `3c67184` to
`b8d44e30de8cc2dd777f16ba218f4d248babb435`. Its host dynamic-tool contract is
part of Shoal's model-facing correctness boundary. The targeted Nix contract
check passed against this pin before the restart boundary.

Before spawning any next-wave implementation actor, run one disposable live
canary through the rebuilt Shoal:

1. In the root workbench, declare a concrete `CanaryReport` result type and
   create one managed worktree.
2. Spawn one real worktree-backed Codex actor. Ask it to add exactly one line
   to exactly one disposable file, commit that change, and return a
   `CanaryReport` through its session-local monomorphic `complete`.
3. First submit one intentional Haskell type error and confirm the full GHC
   diagnostic appears in the actor's context. Correct it in the same resident
   session; an empty tool result or wrapper status is not success.
4. Await the live typed report in the root. Independently verify the exact
   commit, one-file/one-line diff, clean worktree, and actor lifecycle.
5. Discard the canary branch/worktree without merging its disposable change.

Do not begin persistent messaging if any part of this canary needs transcript
interpretation, tmux inference, or manual recovery. Fix the owning interaction
boundary first.

### Suggested persistent request model

The public model should make the recursively owned actor long-lived and put the
per-interaction result type on a separate handle:

```haskell
data AgentRef
data Reply result

startAgent :: Member Actor effs => AgentSpec -> Eff effs AgentRef

request
  :: forall result input effs
   . Member Actor effs
  => AgentRef
  -> Text
  -> input
  -> Eff effs (Reply result)

waitReply :: Reply result -> AgentAction result
stopAgent :: Member Actor effs => AgentRef -> Eff effs ()
```

Normal orchestration is explicit about the requested result at the dispatch
boundary, especially when a later GHCi unit cannot contribute inference:

```haskell
reviewA <- request @ReviewReport reviewer prompt candidateA
reviewB <- request @ReviewReport reviewer prompt candidateB

complete $ nextTurn $
  (,) <$> waitReply reviewA <*> waitReply reviewB
```

On each target activation, Shoal supplies a request-specific environment:

```haskell
sessionInput :: Candidate
reply        :: ReviewReport -> SessionM ()
```

`reply` is monomorphic and single-use for that activation. It settles the
corresponding `Reply ReviewReport`, then the actor returns to mailbox readiness
with its model context, declarations, capabilities, worktree, and identity
intact. It must not retire the actor. A later request may have unrelated input
and result types. Actor shutdown remains explicit and recursive ownership, not
request completion, governs child lifetime.

The caller's GHC fixes both monotypes and records their defining modules at the
request site. Rust owns the opaque request ID, actor incarnation, mailbox
admission, live-root custody, cancellation, single settlement, and target-exit
cleanup. The same-machine result stays a live Haskell value; prompts, rendered
type names, heap tags, JSON schemas, `Proxy`, and unsafe casts never determine
the contract. An intentionally abstract result is constructed through an
explicit caller-supplied live builder capability.

The first vertical acceptance case should send two consecutively different
typed requests to the same still-live actor and include both a caller-session
defined ADT and a closure-valued reply. Once the completion fold is integrated,
build and restart Shoal at this clean boundary so subsequent actors dogfood the
new activation and completion behavior. Then implement persistent lifecycle and
request custody as the prerequisite before splitting facade and recursive-tree
dogfood lanes.

## Updating this guide

Add a current technique after it succeeds in live use and its boundary is
understood. Add a sharp edge when it is repeatable and materially affects
agent efficacy; remove it when the owning fix lands. Aspirational entries must
name an observable improvement and move into the current sections only after
the implementation and acceptance evidence land.

Prefer deleting obsolete advice over preserving historical variants. Git is
the history of this guide.
