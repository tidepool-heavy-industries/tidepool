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

Nested hosted-tool output currently has a visibility defect: a wrapper may
finish while discarding the inner Haskell rejection. Empty wrapper output is
not affirmative evidence that Haskell completed. The intended fix is automatic
model-visible diagnostic rendering at the tool boundary, not permanent
result-unwrapping ceremony.

Linked worktrees share Git objects and configuration but not working files,
indexes, or `HEAD`. They may also encounter compiler-cache or stale-extractor
confusion if a check bypasses the repository's matched toolchain path. Treat
the toolchain identity as part of test evidence.

## Aspirational direction

These are product goals, not claims about the current checkout:

- The default facade exposes `startAgent`, a narrow typed `AgentRef`, action
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

## Current wave handoff

This section is a restart aid for the active self-dogfooding wave and should be
deleted when the wave closes. As of 2026-09-02, `main` contains these reviewed
folds after `origin/main`:

```text
5812624e fix: carry constructor metadata across live actions
9bb8921e feat: simplify Shoal agent actions
00392b63 refactor: extract fixed prompt artifacts
07765a1b docs: add evolving Shoal field guide
d11e7d52 feat: surface Shoal daemon tracing in tmux
7f09f384 feat: expose persistent workbench imports
6aa5638b fix: make Shoal activations atomic and typed
```

The activation fold makes delivery atomic and typed and rejects duplicate or
stale activation sequences. It is the prerequisite for persistent requests.
It does not yet implement the complete persistent-request record or long-lived
agent lifecycle described below.

One completion-affordance candidate was still running when this handoff was
written: actor `actor-9-1`, managed worktree
`wt-37f74e28-dde7-4d67-9536-9563d1093c6e`. Inspect its exact commit and checks,
then integrate it before persistent messaging because request settlement should
reuse its monomorphic, activation-local result compiler. The root Haskell
continuation was awaiting that actor together with two already-finished actors;
after a restart, recover from Git and actor/worktree state instead of expecting
the old live Haskell binding to exist.

The worktree `wt-e8fc3099-529d-4bf0-ad58-adf5d2d23943` on branch
`tidepool/worktree/shoal-start-agent-wt-e8fc3099-529d-4bf0-ad58-adf5d2d23943`
contains a deliberately held, dirty `startAgent` spike. Do not land it whole:
it encodes the superseded one-shot `AgentRef exit` model. Its curated facade,
worktree-authority recipe, compiler-derived site metadata, and visibility tests
are useful source material for the persistent implementation.

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
