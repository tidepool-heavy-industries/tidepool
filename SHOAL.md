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
  composition, and explicit managed-worktree operations. Lower-level resident
  actors remain available only through an intentional advanced import.
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

## Updating this guide

Add a current technique after it succeeds in live use and its boundary is
understood. Add a sharp edge when it is repeatable and materially affects
agent efficacy; remove it when the owning fix lands. Aspirational entries must
name an observable improvement and move into the current sections only after
the implementation and acceptance evidence land.

Prefer deleting obsolete advice over preserving historical variants. Git is
the history of this guide.
