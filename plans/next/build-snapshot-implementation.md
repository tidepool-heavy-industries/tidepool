# Build snapshot inheritance

Implement linearly before resuming the preserved parallel dogfood branches. Keep
the normal interactive Codex TUIs. The human and managed Astra planner will choose
the resumed tree after this mechanism is usable.

Start with [the feasibility evidence and agreed behavior](evidence/build-snapshots.md).
The ordinary Haskell `unfold` surface stays simple. Its preferred implementation
now inherits the live source workspace as well as warm build artifacts.

The [implementation review](evidence/build-snapshot-review.md) identifies the
next integration checkpoint and the source-view, metadata, admission and retention
boundaries that must be joined before the substrate is transparent to models.

## Contract

Linux is the implementation target. Use native namespace and OverlayFS primitives;
Windows support and cross-platform filesystem emulation are out of scope.

An actor owns a private writable source view and a separate private writable build
view over immutable layers. Ordinary unfolding prefers the parent's live workspace,
including staged and unstaged changes, ignored and untracked files, and timestamps.
The child has its own Git HEAD, branch and index; the common Git objects/refs,
authoritative `.shoal`, and designated build resources remain separately owned.

If source snapshotting is busy, automatically create the ordinary linked Git
worktree at the parent's current HEAD. Record a typed fallback reason and a concise
notice that working files were not inherited. Do not expose a new model-facing
mode or require an approval for this already-authorized fallback. Other uncertain
publication failures require reconciliation before creating a second candidate.

An actor owns a private writable build view over immutable warm layers. Forked
actors inherit their orchestration parent's latest published snapshot and get
their own writable layer. A parent that is building continues uninterrupted;
children use the older snapshot. With no snapshot, use a private empty cache.

Publication happens at a safe idle boundary and never blocks actor forking on a
long-running build. Snapshot identity is separate from source revision and test
acceptance. Cache hits remain subject to the build tool's normal validity checks.

“Cheap fork” means no eager target-tree copy. Initial import, file copy-up, and
eventual layer consolidation have costs. The mechanism must report those honestly.

## Implementation sequence

### 1. Prove the live-process mount transition

Use a bounded native-process fixture before changing actor launch. Extend the
existing process mount boundary to host an OverlayFS build view with a trusted
mount owner and unprivileged worker. Demonstrate a persistent worker, separate
child namespace, and stable visible target path through snapshot publication.

Prove writable-FD refusal, old cwd/directory-FD behavior, private child writes,
whiteouts, and parent continuation. Never expose writable backing-layer aliases
to workers. Use flat lower layers; nested merged overlays exceed this kernel's
stacking limit. Resolve capability ownership and transition ordering here before
claiming the local same-namespace experiment is a production design.

### 2. Give snapshots one resource owner

Extend `BuildResourceLease` in `tidepool/src/actor_host.rs`; use
`tidepool-node/src/process_boundary.rs` for mount mechanics and
`tidepool-toolchain` for paths. Introduce typed immutable snapshot identity,
private writable generation, and retained layer dependencies within these owners.
Do not create a parallel registry or process supervisor.

Treat publication as a recoverable transition: prepare replacement resources,
freeze, restore a writable parent view, then expose the new snapshot for forks.
Record enough ownership state to reconcile interruption at each boundary. A
published layer must never become writable again. Unknown process/mount custody
retains resources; parent retirement cannot remove a child's lower layers.

### 3. Connect safe publication to native execution

Inspect the existing native exec owner and session machinery in the Codex fork.
Use that owner to exclude new command admission during the short publication
transition and establish that existing writers have settled. Do not infer idle
from pane text, process-name searches, a guessed Cargo lock, or a quiet timer.

The owning boundary must cover background execution and descendants. If current
native events cannot establish this, add the smallest typed admission/completion
handshake through the existing native control path. Do not replace the TUI or
introduce a special build-command language.

`EBUSY` leaves the previous snapshot current. Resume normal command admission and
try at a later eligible boundary. No automatic build cancellation, TUI termination,
LLM polling loop, or hosted Haskell call waiting for the LLM to cancel its own job.
Publication records reusable artifacts, not evidence that every test passed.

### 4. Wire source and build inheritance and seed the root

Extend the worktree owner so its Git operations, inspection, and native actor
access all address the same snapshot-backed source checkout. An actor-only source
mount over a host-visible empty directory is not acceptable. Capture source and
Git index/HEAD under a common write-admission boundary, preserving staged versus
unstaged state without turning dirty files into a synthetic commit. Keep ordinary
Git checkout as the automatic busy-source fallback; it does not preserve the
parent's working-file changes. Preserve its independent warm-build inheritance.

At actor launch/fork, acquire the creator's latest published build snapshot and allocate
a private upper/work pair. Conversation context inheritance does not choose the
cache parent. A batch can share one retained generation without copying it.

Allow one explicit root seed from a stable existing target, or a warm root build.
Do not mount a live ordinary ext4 target as an allegedly immutable seed. Keep
`CARGO_TARGET_DIR` stable in the actor namespace and preserve current wrapper
disabling until namespace-correct cache-daemon use is separately established.

Keep configuration in TOML and orchestration in Haskell. Normal `unfold` should
benefit automatically. Expose compact snapshot availability/age and storage
observations through the existing snapshot/control surface only where a real
operator or agent decision needs them; agents need no layer-management API.

### 5. Validate useful reuse and bounded storage

Run representative parent/child repository builds with the same owning toolchain.
Show dependency reuse, independent changed outputs, and rebuilds for invalidated
inputs. Measure bytes allocated and compilation avoided, not just fork latency.

The preferred source snapshot must preserve actual timestamps. Measure the
unchanged-workspace case against the ordinary-checkout fallback, whose fresh
source mtimes can force local crates to rebuild. Never backdate arbitrary inputs
or suppress Cargo checks to improve the reported hit rate.

Bound layer depth and implement deliberate consolidation of immutable generations.
Retain old layers until dependents release them. Cover deletion markers, failed
consolidation, interrupted publication, actor retirement, and uncertain cleanup.
Avoid broad workspace batteries during implementation; use focused owning checks
and one representative end-to-end acceptance at integration.

### 6. Prepare the next run

Add the short pre-fork build-idleness nudge from the evidence note to the curated
prompt package. Preserve a stable shared prefix. Give the next Astra planner the
saved branches, unfinished gates, measured build behavior, and prior planner's
retrospective. Plan shared warm builds and meaningful independent work together
with the human; do not prescribe a fixed zoo of worker stages.

Build and validate the selected runner before launch. Activate the changed package
at the explicit new-wave boundary. Do not launch without the user's go-ahead.

## Completion means

Two real managed TUI actors can fork from a warm parent without eager target-tree
copying, continue independently, and remain usable while the parent builds.
Snapshot publication failure preserves actor operation and the previous cache.
Source changes still rebuild correctly, inherited layers survive parent retirement,
and unused layers are reclaimable after exact process/resource cleanup. The
representative run reports concrete compilation and disk savings plus remaining
copy/import costs.
