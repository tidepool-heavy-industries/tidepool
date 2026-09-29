# Engine foundation and embedded harness implementation

Approved 2026-09-28, starting at `723ab752d`. This supersedes the implementation
hold in `engine-foundation.md` and the documentation-only hold in the harness
plans. Preserve their contracts and evidence; this document owns sequencing.
Stop at verified readiness. Do not deploy or launch a live worker tree.

## Decisions

- Complete all foundation, compiler/native, Git/process, and embedded harness
  work. Use isolated worktrees, root review, Sol Medium implementation and one
  expensive compiler/test slot. Preserve running services and dirty work.
- Shared atomic notebook publication and reusable runtime captures apply to
  both backends. Codex may retain sequential transport dispatch. No second
  runtime behavior for partial definition publication.
- Demand compilation selects the statically reachable recursive groups of an
  admitted entry before execution, outside checkout. No in-JIT compile traps.
- Keep existing owners for scheduling, compilation, resources and persistence.
- Never edit `harness-adoption.md`. Update its reconciliation separately.

## Tracks and boundaries

| Track | Owner | Deliverable |
| --- | --- | --- |
| Resident execution and integration | Root | Private executions, publication, reusable captures, exact candidate review and joined gates |
| Compiler/native | Sol Medium | Paired durable module products, reachable-group batches, lifetime and off-checkout consumers |
| Git/process | Sol Medium | Repository-scoped admission and thin one-command namespace entry |
| Harness integration | Sol Medium | Origin-operation identity, sequential adapter, shared Engine/Store/browser composition and packaging |

First finish and verify current foundation candidates. Agree shared module-product,
execution/publication, tool-snapshot and origin-operation contracts before their
consumers diverge. Prove sequential embedding while concurrent runtime work
continues. Reassign a finished track to producer/native splitting if needed;
at most three implementation agents run alongside root.

## Foundation closure

- [ ] Signal cooperative external cancellation, join actual work, settle only
  the exact continuation, and preserve unconfirmed cleanup. Use typed outcomes.
- [ ] Integrate actor-owned settlement and typed source freshness. Caller drop
  cannot erase accepted work; stale completion cannot settle another call.
- [ ] Finish native coalescing, consumer adaptation and off-checkout migration.
- [ ] Finish client fixture repairs; retain exact companion/pin revisions.
- [ ] Regenerate ABI artifacts through producers and prove the joined boundary.
- [ ] Repeat retained-symbol measurements at 0/1,000/10,000 symbols.

Archived, unbuildable harness edits are retained in their worktree but excluded
from the integrated patch. Audit compiled dispatcher consumers instead.

## Compiler and native contract

One toolchain-owned immutable module product holds exact source/compiler/options/
dependency identity, package-unit/interface evidence, neutral prepared definitions,
stable module/group/binder identities, matching prepared interface and constructor/
type/site/dependency facts. Extend the existing cache with atomic paired bundles;
validate shadowing and consumed dependencies. Preserve conservative handling of
untracked compile-time effects. First prove exact interface serialization and
rehydration through GHC before broad cache migration.

Compile missing reachable groups in sealed batches outside checkout. Coalesce
overlapping demands by exact identity. Direct native calls stay within a recursive
group; cross-group private imports use the receiver's installation environment.
Keep roots/imports/descriptors/evaluated state outside native identity. Catalogs
are metadata or weak indexes; unused exports create no roots or native code.
Already-materialized globals retain explicit semantic leases to preserve their
same-instance state. Real installations, values, continuations, parcels and active
work retain code; release it after its last real owner.

## Resident contract

Each execution owns admitted authority/source/handler/environment, a private scope,
cursor, continuation, control, receipts and write set. Actor lifecycle/shared
coordination remain actor-owned; do not clone or globally mutex the whole behavior.
Use existing scheduling/completion fencing. Replace after-tool cleanup based on
session-wide continuation subtraction with exact execution-owned cleanup.

Cells run internally in order and publish only their final declaration/binding
delta. Preserve original binding identities and dependency leases. Stage compiler-
validated joins outside checkout; conflicts fail without public changes. Revalidate
the public generation before one atomic commit, retrying join preparation without
replaying effects. Cancellation and publication share one authoritative ordering
point. External effects and their receipts remain real on failure/cancellation.

Successful captures independently retain completed private scaffold and source/
lexical meaning. Reuse for multiple children before parent completion; parent
failure cannot revoke the capture. Mutable resource and authority contracts remain
separate. Update glossary, prompts, examples and old prefix-publication tests.

## Git and process contract

Unify Git clients under the backing repository/view owner. Shared metadata identity
differs from mounted working-view identity. Whole operations hold read, write or
capture permits; nested internal calls do not reacquire. Order multi-repository
acquisition. Cooperating processes sharing backing metadata use the same lock.
Native writer freeze remains independent; never claim host locks fence arbitrary
editors. Preserve source validation and refuse observed drift.

The thin helper replaces namespace entry through host `pre_exec`, not merely the
actor supervisor binary. Launch one small process with no host `pre_exec`; transfer
the existing namespace entry through a protected, identity-checked channel; enter
and exec inside the small process. Preserve OS arguments, environment, cwd, stdio,
signals, spawn failures and cleanup. Keep view leases through acquisition and
settlement. No persistent Git broker. Verify actual spawn syscalls and performance;
report any process-scope/terminal paths that still fork separately.

## Embedded contract

Harness-origin operation identity includes conversation/incarnation, request and
original provider call ID. Keep provider IDs unchanged; inherited claims resolve
to the origin operation. Store/scheduler/cancellation/completion/replay agree on
identity, with migration through the harness schema owner.

M1 binds real actor admission, immutable tools/handlers, raw/structured dispatch,
retained results, cancellation, model-round sequencing, durable input then wake,
and authenticated browser reconnect. M2 enables private execution/publication and
reusable captures through the same runtime. Keep Codex default; no backend switch
for an admitted run or replay of uncertain work through another backend.

## Acceptance and handoff

- Compiler/native: cold/cross-worker reuse, dependency/compile-time effect safety,
  recursive/overlapping demand, unused code omission, distinct installs, CAF state,
  parcel/continuation lifetime and final-owner release.
- Runtime: A parks while B progresses; unrelated and same-name publication;
  captured old meaning, invalid joins, failed-cell nonpublication, both cancel/
  publish orders, exact timeout cleanup, two children from unfinished parent and
  later parent failure.
- Git/process: read/read and read/write barriers, capture/nesting/timeouts,
  cooperating-process locks, native Busy, setup/holder failure, raw arguments and
  stdio, cleanup, actual spawn behavior and measured cost.
- Harness: real resident deterministic transport, reconnect without replay,
  handler reload snapshots, equal IDs across origins, inherited claims, late
  completion/cancellation, Codex regression and matched assets.
- Run structural fixture gates at schema/ABI boundaries, matched build and joined
  integration checks. Report executed counts, compile-only and unavailable checks.

Deliver exact revisions/application order, evidence, before/after measurements,
matched harness/assets/client pins and limits. Stop for user review before launch.
