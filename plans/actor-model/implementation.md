# Actor implementation status

This file records only the current implementation, its evidence, and the next
architectural boundaries. Design semantics live in
[architecture.md](architecture.md); the model-facing contract lives in
[haskell-surface.md](haskell-surface.md). Superseded phase plans belong in Git
history, not in this document.

Backward compatibility is not a constraint. Once a replacement proves the
behavior Tidepool still wants, delete the older mechanism rather than keeping
an adapter or a second scheduler.

## Landed architecture

### Actor substrate

- Ractor is the sole in-process actor scheduler. It owns serial mailboxes,
  local addresses, linked ownership, task lifecycle, and supervision.
- `tidepool-actor::LocalActor` adds exact Tidepool incarnation identity,
  retained terminal observation, typed live-value messages, call ancestry,
  and subtree shutdown. It does not mirror Ractor's runnable state.
- `ResidentKernelBehavior` is the one resident-Haskell actor behavior. Its
  state owns the installed Haskell continuation, shutdown hook, agent session,
  and current activation context.
- A shared `ActorMachineRegistry` owns only resident-machine checkout. It is
  not an actor registry or scheduler.
- Root and child actors use the same local-actor path. Root compilation is a
  bootstrap concern, not a second lifecycle implementation.
- Graceful termination and supervisor-observed failure converge on one
  exact-incarnation retirement publication. Duplicate cleanup notifications
  are suppressed.

The superseded hand-written `ActorRegistry`, `ResidentActorHost`, host task
table, parked call/wait maps, lifecycle scheduler, and registry authorization
layer have been deleted.

### Resident Haskell and model sessions

- `tidepool-runtime::PersistentSession` remains the owner of GHC compilation,
  live roots, continuations, declarations, bindings, and machine checkout.
- One `ActorAgentSession` belongs to one actor incarnation. Provider inference,
  fenced-Haskell execution, correction rounds, and completion form one serial
  actor turn; provider inference does not hold a machine checkout.
- Resident providers and external Codex applications enter the same actor
  workbench. External applications use one actor-local hosted tool carrying
  Haskell source and structured execution receipts, not a parallel domain API.
- `tidepool-model-output` owns fenced-Haskell parsing. The actor layer has no
  second parser.
- Completion values cross consecutive agent sessions through managed Haskell
  custody. The returned value is rooted before the producing completion scope
  closes; the next activation can force it safely.
- The actor workbench and ordinary REPL share one parser for `:type`, `:info`,
  and `:bindings`. `:type` and `:info` resolve through one GHC-backed runtime
  inspection path against the exact next-turn compile view; source scanners
  are not semantic authority. GHC-derived binder metadata also supports
  ordinary pattern bindings, including bindings whose right-hand side
  suspends and resumes.
- `AgentAction` is the live executable result of an interactive session.
  Returning it settles the hosted-tool call before execution; the resident
  actor then owns suspension, resumption, and the explicit next activation.
- `waitOn` gives already-running actors ordinary `Functor`/`Applicative`/
  `Monad` fan-in. `nextTurn` mounts the successful live result into the same
  agent context without JSON or a second continuation registry.

### Haskell actor surface

- Public construction is `ActorDefinition -> startActor`.
- `call`, `cast`, `receive`, `serve`, `runActor`, and `awaitExit` operate on
  exact `ActorRef protocol exit` values and move live Haskell values within one
  machine.
- Successful exits remain Haskell-owned live cells referenced by `ActorRef`;
  Rust retains immutable terminal metadata, not a duplicate exit payload.
- The private `ActorKernel`, readiness, reply, and settlement
  constructors are absent from ordinary authored exports.
- Experimental `ReadWrite` and `ReadOnly` profiles select resident Haskell
  rows and constrain spawn attenuation. They are not native Codex or OS
  sandboxes.
- Public reusable code uses `Member` constraints. Rust does not reflect or
  compare positional effect-row ABIs.
- `Complete result` alone occupies the row head as a scoped result delimiter;
  this lets GHC infer the exact return type while ordinary capabilities remain
  `Member`-polymorphic.

### Shoal and Git custody

- `shoal new` creates only operational `.shoal/` content, installs
  `/.shoal/` in `.git/info/exclude`, initializes Git when necessary, and creates
  an empty base commit when needed. It does not scaffold project files.
- Shoal uses ordinary Git worktrees in the source repository's namespace.
  Workers can commit and branch normally; this is cooperative change custody,
  not a Git-metadata security boundary.
- Each interactive actor sees its checkout at the stable
  `/tmp/tidepool-actor-workspace` path, avoiding per-worktree Codex trust
  prompts and making conversation forks relocatable.
- Each writable actor checkout receives its own `.shoal/build` Cargo target
  directory. Mutable compiler artifacts are not shared across root and worker
  incarnations.
- `finishWork` combines a model-authored report with one Rust-observed Git
  snapshot: worktree identity, base OID, branch or detached head, submitted
  OID, dirty state, and in-progress Git operation.
- Candidate objects live in the shared repository namespace and are directly
  reviewable and integrable from the root checkout.
- Lifecycle exits enqueue backend-native wakes. Pending collection instructs
  the model to end the current turn rather than poll; delayed or duplicate
  wakes are harmless because collection and acknowledgement are idempotent.
- Recreating a root starts a new incarnation. The resumed conversation is told
  that old actor handles, bindings, and pending results are invalid. Durable
  actor-state restoration is not claimed.
- Structured tracing writes under `.shoal/logs/` and correlates actor,
  application, worktree, and lifecycle activity.

## Current acceptance evidence

The current boundary is covered at three levels.

### Pure and component tests

- call-cycle ancestry, exact address decoding, terminal encoding, profile
  attenuation, and source/dependency-closure helpers;
- provider-session serialization, queued lifecycle input, and provider failure;
- generated protocol declarations and Haskell/Rust request decoding.

### Local actor tests

- readiness publication after `pre_start`;
- one-message-at-a-time processing and call non-reentry;
- linked child failure not killing an overriding owner;
- in-flight and queued live-value custody released on kill;
- abandoned RPC reply custody;
- explicit recursive subtree shutdown;
- retained typed completion and exact child-exit observation.

### Real GHC/JIT/Shoal verticals

- a resident root installs its actor-local tool policy, handles a typed call,
  starts and awaits a child, and retires exactly once;
- an interactive root returns an `AgentAction`, composes two closure-valued
  child exits with ordinary `Applicative`, settles its hosted-tool caller
  before waiting, and forces the composed closure as the next activation's
  live `sessionInput`;
- the same vertical maps an intentionally failed child into typed
  `ActionFailure` and reactivates the root without losing its session;
- real Shoal exercises have produced isolated candidate commits with
  runtime-observed clean receipts, direct root-side review, integration, and
  lifecycle-triggered continuation.

Run focused tests while iterating. At a major boundary run formatting, strict
Clippy on touched crates, the complete `tidepool-actor` suite, and relevant
Shoal CLI/integration tests through the Nix development shell.

## Remaining work

These are future features or measured hardening seams, not unfinished pieces
of the Ractor cutover.

### Near-term UX and operations

- Generate concise happy-path examples from the exported Haskell surface and
  distinguish outer tool execution from actor lifecycle in the eventual
  non-JSON frontend. Exact signatures, constructors, binding
  inventory, and ordered partial-commit semantics are already discoverable in
  the shared workbench.
- Retire the declaration plane's textual `M`-signature generalization once the
  frontend has a type-aware authored-module path. Until then, teach reusable
  declarations with `Member` constraints and do not extend the rewrite with
  more syntax heuristics.
- Improve concise rendering for actor starts, exits, and root completion while
  retaining a verbose structured view.
- Measure and separately attribute queueing, compilation, worktree creation,
  actor execution, hosted-tool transport, and outer-cell resume latency before changing
  execution architecture.
- Project direct local-actor lifecycle and execution facts into one neutral,
  bounded event stream. Do not revive the deleted selfharness-to-actor event
  adapter or create mutable scheduler state to support observability.
- Define explicit retention and garbage-collection policy for worktrees, refs,
  logs, and terminal records. Cleanup failure must remain observable without
  rewriting actor lifecycle.

### Deliberately deferred architecture

- Structural context fork, including cloned model memory, persistent Haskell
  snapshot sharing, invalidation of actor-linear continuation references, and
  per-capability fork policy.
- Durable cross-incarnation actor recovery or replay of live Haskell values.
- Distributed actors and serialized protocol boundaries.
- Trusted Haskell intent-to-kernel lowering. It must fit behind the existing
  public profiles and nominal interpreters without a reflected effect-row ABI.
- Arbitrary capability delegation/revocation, detached actors, adoption, and
  restart supervision.
- Security isolation for untrusted native workers. Git worktrees and Codex
  sandbox flags currently express cooperative workspace policy only.
- Project-specific reports, typed findings/checks, merge policy, and higher-level
  scaffold/fan-out/fold combinators. Add these in application libraries when a
  real workflow demonstrates the useful shape; do not recreate lifecycle state
  in Haskell or the Shoal host.

## Completion rule

The Ractor refactor is complete when the deleted scheduler stays deleted, the
tests above pass, no production reference names the superseded runtime, and
the remaining work is honestly classified as a new feature or hardening seam.
The actor-model planning directory itself can retire after its stable
contracts have moved into crate-level `AGENTS.md`, Rust/Haskell API docs, and
the repository glossary.
