# First embedded Haskell worker tree — engine handoff

2026-09-28. Product requirements for the Tidepool implementation owner.
Read with [the accepted integration contract](harness-integration.md),
[the runtime design investigation](harness-integration-runtime.md), and
[the engine foundation](engine-foundation.md). This document sequences the
first connected release; it does not declare those plans implemented.
`harness-adoption.md` remains historical and read-only.

## Outcome

A human opens the Exomonad browser, gives a Sol Medium root a bounded assignment,
and observes it scaffold and delegate to Luna component owners that themselves
fork useful microtasks. Actors operate through raw Haskell notebook cells and
Haskell AgentSpec tools. Typed replies, reviews and integration return through
Exomonad's existing actor mechanisms. One process per run embeds the shared
harness and actor kernel; no Codex process is needed for these model actors.
Stock Codex remains independently usable as a fallback.

“Full Haskell” describes the authored interaction and orchestration surface.
Rust still owns providers, execution scheduling, processes, storage and authority.
Do not move these responsibilities into Haskell to satisfy the demonstration.

## Status and handoff location

The standalone harness implementation is on `integration/embedding-ready` at
`/home/inanna/dev/harness-integration/ready`. Canonical harness master is not yet
updated at this writing. Its owner is finishing final verification and will
publish an exact accepted revision in
`exomonad-harness/docs/embedding-ready-handoff.md`. Do not pin a moving worktree
or treat the candidate's existence as release evidence.

The harness implements the public embedding interfaces, reusable conversation
checkpoints, plain-text compaction, external cancellation acknowledgment and
browser projection. Its deterministic external-host tests do not establish
resident Haskell execution, compiler source lifetime or real provider behavior.

**The current engine foundation explicitly preserves single-operation actor
admission and current publication semantics.** Completing that foundation is a
prerequisite, not proof that concurrent notebook cells are implemented. Keep its
scope stable; schedule the remaining resident integration as an explicit follow-on.
Old implementation descriptions in the runtime handoff are baseline analysis:
reconcile them against the foundation's resulting source before editing.

## Required ownership boundary

| Concern | Owner / embedding obligation |
| --- | --- |
| Actor identity, incarnation, admission, supervision and retirement | Existing Tidepool actor kernel; implement harness `HostActor` with a real admission lease |
| Model rounds, conversation history, original calls and model-input envelopes | Harness Engine/Store; bind each conversation to one authorized incarnation |
| Haskell execution, private environments and publication | Resident runtime; preserve original execution identity across suspend/resume |
| Typed live requests/replies and values | Existing kernel mailbox/request owners; never convert them into durable model-envelope copies |
| Commands, checkouts and resource cleanup | Existing resource owners; cancellation delegates to the actual owner |
| Browser and model conversation continuation | Facade composes harness services with kernel wake/control; no standalone `TreeDriver` supervisor |

The host drives the bound Engine and persists/advances its returned conversation
head through the existing Store APIs. One owner sequences model rounds for each
conversation; multiple tool executions may remain active across those rounds.
Do not create overlapping provider rounds for one conversation as a shortcut to
concurrent cell execution.

## Runtime behavior required before the tree gate

### 1. Stable admission and interleaved execution

Every admitted raw cell or installed Haskell tool captures exact actor authority,
source version, tool handler, declaration environment and binding view. Its
execution owns its reply, cancellation, continuation and private writes.

When an effect parks, release the machine checkout and return control to the
actor scheduler. Other admitted executions, input and retirement control must
progress. Merely removing a client dispatch mutex is insufficient. Reuse the
foundation's owned deferred-operation protocol and exact continuation fencing;
do not introduce another scheduler or completion registry.

Call replay returns the original retained outcome. It never repeats a command,
compilation effect or source execution merely to reconstruct missing output.

### 2. Publication and captures

Cells execute sequentially internally, with intermediate definitions private.
Successful completion publishes a declaration/binding **delta** atomically into
the current public environment. Unrelated concurrent writes survive; same-name
publication completion order determines future admission. Existing cells and
captures retain their old meanings and supporting code/source/value leases.

Failure or cancellation publishes no new notebook definitions. Already performed
external effects and their evidence remain real. Explicit source reloads retain
the source owner's commit contract. Preserve declaration dependencies, instances,
constructors and shadowed values: a rendered-name merge is not sufficient.

This is the accepted concurrent-cell contract, not permission to change the
foundation's currently promised sequential semantics halfway through that batch.

### 3. Immediately usable, reusable checkpoints

Connect the runtime checkpoint capability to harness `Checkpoint<T>` using an
opaque host attachment that retains the exact Haskell environment and source
leases. Capture includes the issuing cell's completed private scaffold at the
effect boundary; function-local values cross only as explicit typed inputs.

The child can start before the parent cell returns. Reuse one capture for several
children, each with its own admitted actor and checkout. Later parent failure or
cancellation must not invalidate the captured scaffold. Pending model calls keep
their original identities and real eventual outcomes; do not fabricate success or
execute a pending parent operation again in each child.

Host loss does not restore a live capability from serialized history. Refuse or
report unavailable live state explicitly. Retain captures independently of the
source execution and release leases when their actual last owner releases them.

### 4. Exact tools, input and cleanup

Supply `ToolSurface` from the admitted AgentSpec and raw Haskell tool. A request
pins its immutable manifest/handler; reload affects later requests. Preserve raw
text versus structured argument kind and original request/call correlation.
Embedded mode does not append standalone spawn/message/wait lifecycle tools.

Authorize browser/model input against the incarnation, durably admit it once via
`Conversation::input`, then wake. Admission, wake success and inclusion in an
actual model request are distinct observations. Do not duplicate these envelopes
in the Codex inbox. Typed kernel messages retain their separate existing owner.

Implement `CancellationOwner` through the exact resident execution/resource
owner. Interrupt, retirement request and proven cleanup are distinct. A dropped
result waiter or browser connection cannot establish cancellation. Preserve late
evidence and cleanup authority when an external operation remains unconfirmed.

## Recommended implementation sequence

1. Finish and review the engine foundation under its current acceptance gates.
2. Reconcile its owners with `harness-integration-runtime.md`; implement private
   execution/publication and reusable runtime captures with focused owner checks.
3. Wire the accepted harness revision into an opt-in embedded backend: bound
   host capability, provider dispatcher, wake/control, conversation head handling,
   authenticated browser server and lifecycle projection. Preserve Codex viability.
4. Exercise the real resident path with deterministic model transport. Test
   async calls and pending-parent child launch before paying for a model tree.
5. Build the matched Exomonad binary and assets; record revisions and run the
   bounded live tree below. Launch only after all required gates pass.

Composition and resident work may proceed independently once their interface is
agreed. Share one expensive compiler slot. Use bounded Luna work for mechanical
consumers/tests and Sol for cross-owner state transitions. Review exact commits.

## Acceptance gates and retained evidence

| Gate | Required observable result |
| --- | --- |
| G0: candidate integrity | Accepted harness revision, engine revision and adapter diff recorded; relevant targets compile; existing Codex path remains usable |
| G1: real cell | Actual embedded Engine dispatches raw Haskell to an authorized resident actor, retains result and survives browser reconnect without reexecution |
| G2: concurrent cells | Cell A parks; B executes and publishes; A resumes and publishes without erasing B. Same-name ordering, captured old meaning and failed-cell nonpublication are exercised |
| G3: inherited launch | Inside an unfinished parent cell, capture scaffold and launch two children from it; children use captured Haskell/helper definitions and return typed replies. Repeat with later parent failure and retained capture |
| G4: control/lifetime | Cancel one execution while its sibling proceeds; fence stale completion; preserve unconfirmed external work; retirement releases only resources whose owners are actually done |
| G5: live useful tree | Browser-operated Sol Medium root completes a bounded task through recursive Luna owners and leaves, reviewed joins and integration; root interview and trace retained |

For G5 choose a disposable project with two independent small components. Each
Luna component owner scaffolds and delegates meaningful implementation/review
microtasks before joining them. Aim for root → component → subcomponent/leaf,
with deeper ownership where useful; do not manufacture agents to satisfy a count.
Keep total concurrent model clients explicitly bounded on this machine.

The exercise must use ordinary Haskell orchestration and typed results, not a
parallel Rust script that secretly performs the delegation. A Haskell-only
collector/router should handle at least one result join without a model polling
loop. A final model answer alone is not completion: retain actual commits, checks,
review outcomes, integration revision and resource disposition.

Record run/log IDs, binary and library revisions, source/workspace pin, models,
actor ancestry, original call/execution correlation, peak resident memory, slow
cell/compile timings, and remaining resources. Reuse existing tracing; add only
missing evidence necessary to distinguish admission, execution and publication.
Interview the root before retirement about capture ergonomics, concurrent cells,
helper use, blocked delegation and unnecessary model turns.

## Explicit exclusions and escalation

No migration of running Codex sessions, default-backend switch, removal of the
Codex fork, server-management UI, structured-summary DSL, transparent live-heap
recovery, automatic effect replay or mandatory fresh machine per child here.
Plain-text compaction uses the harness implementation; the host supplies capacity
and preserves runtime state independently of conversation summarization.

The harness scheduler currently requires unique call IDs within its instance and
refuses collisions. Choose and document scheduler ownership; never silently
rewrite provider call IDs. If the connected provider violates this assumption,
repair its keying in the harness owner before launch.

Escalate any incompatible engine design or required weakening of publication,
checkpoint lifetime or cleanup semantics. Report the exact conflicting owner and
an alternative; do not quietly replace the accepted behavior with serialization.
The engine owner should return completed prerequisites, remaining integration
work, exact candidate hashes and focused evidence—not just “engine ready.”
