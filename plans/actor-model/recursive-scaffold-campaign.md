# Recursive scaffold and implementation campaign

Status: proposed delivery plan. No new runtime behavior is implemented by this
document. This is the next concrete campaign for
[context-tree practice](context-tree-emergent-haskell-ux.md), not a competing
actor architecture. Current fork semantics live in
[the Shoal guide](../../SHOAL.md#cache-preserving-context-unfold).

## Target experience

An actor resolves shared design decisions, commits a partial program with
explicit implementation obligations, and forks independent work from that
revision and its completed tool block. Children inherit the reasoning and
resident Haskell definitions. They implement their obligations directly or
repeat the same process. Each parent integrates its children and fulfills its
own assignment. Git folds code; typed replies fold findings and evidence;
child conversation histories are not automatically merged.

Use Astra throughout the first campaign. Model choice is independent of tree
shape; specialist workers and fresh contexts are later extensions. Optimize
inference spent per accepted result and elapsed time, rather than assuming
CPU startup or tool latency dominate. Cache reuse is observed, not promised.

## Existing substrate and remaining baseline work

- Resident declarations, typed requests/replies, watches, retained actors,
  recursive ownership, and managed worktree submission observations exist.
- The current changes defer child startup until the enclosing tool batch has
  a durable result and capture its final committed Haskell scope. Assignment
  values and explicit worktree seeds retain ordinary value semantics.
- Finish the matching packaged Codex contract check, focused integration
  checks, and disposable live Shoal smoke before relying on that boundary.
  Record actual outcomes in the existing fork/cache RCA report. The original
  intermittent cache miss has not been conclusively explained.
- Live Haskell state is not reconstructed after a host crash. Committed source
  and campaign instructions support a deliberate new run; they do not restore
  old handles, closures, or pending requests.

## Campaign contract

Keep these concepts in project-authored Haskell and repository files initially.
Use ordinary sums and records tailored to the project, not a universal task
AST or a fixed completion ladder in the runtime.

Every assignment identifies:

- An exact scaffold revision and an obligation identifier.
- Behavior and interfaces that are already settled, including a contract
  revision when the assignment can be revised.
- Owned edit scope and shared files owned by the parent. Semantic independence
  matters as well as disjoint paths.
- The required completion predicate, permitted remaining holes, and checks
  required at this boundary.

Source markers name obligations; a small committed project manifest supplies
their meaning. The manifest is authored project data, not a second actor
registry. Tags or commit trailers can make partial commits visible to humans,
but acceptance examines the actual revision and its obligations.

Use language-appropriate stubs: a typed function raising NotImplementedError,
for example. Do not make passing through a stub look like implemented behavior.
Initial markers should be simple enough for a small project-local checker;
do not build a multi-language marker parser.

A successful submission names its exact commit, fulfilled obligations,
permitted remaining obligations, and check evidence. A distinct typed outcome
requests an interface or scope revision. Runtime cancellation and actor failure
remain observable through existing lifecycle mechanisms, rather than being
invented as successful domain replies.

Check evidence must identify the tested revision, command, result, and relevant
environment. A model-authored reply is a claim; compare it with repository and
execution evidence. After integration, child check results remain evidence
about child revisions; run the checks required for the integrated revision.

## Decomposition and integration rules

1. Resolve common decisions and commit the scaffold before capturing the
   children’s worktree seed. Completing the tool block does not recapture a
   seed value selected earlier.
2. Give each child an independent obligation. Keep dependency wiring, shared
   manifests, lockfiles, and shared interface edits with an explicit owner.
3. Admit children and register watches in the same tool block if useful; do
   not wait synchronously for dormant children inside that block.
4. Keep the agreed shared interfaces stable while children implement them.
   Unrelated parent work may proceed. An interface change requires explicit
   reassignment/revision and checking already returned work against it.
5. Observe and integrate ready children incrementally when dependencies allow.
   Use exact commits and ordinary Git operations in the parent-owned worktree;
   do not create a second scheduler or a global merge service.
6. Check scope, obligations, and semantics as well as Git conflicts. A conflict
   or unexpected cross-scope edit triggers review/repartitioning, not automatic
   acceptance. A failed integration must leave other child commits available.
7. Keep a child for focused repair when its context is useful. Fork again from
   an updated scaffold when the contract or shared understanding has changed.
8. A child may create internal obligations, but cannot weaken its parent-facing
   completion predicate. Any new externally visible remaining hole needs an
   explicit parent-approved contract revision.
9. Before final acceptance, the root verifies that all required obligations
   are discharged and application-level checks pass.

Partial completion is local, not determined by depth. One leaf may only need
to provide a typed scaffold; another must implement and test a complete unit.
A coordinator can accept several partial fragments while promising a working
component to its own parent.

## Delivery sequence

### 0. Close the fork correctness baseline

Finish the checks and live smoke above. Require evidence that children see
the real completed tool result, final bindings, and intended scaffold revision;
later parent progress must not alter that snapshot. Cover failed admission,
later tool failure, cancellation, and host reattachment using the existing
focused checks. Do not mix new model-selection work into this step.

### 1. Apply the established scaffold/fill/integrate pattern

The user has already demonstrated this pattern in other swarms, including the
swarm that built Tidepool. Skip a separate proof-of-concept campaign. After
closing the fork baseline, use the established pattern directly in the selected
standalone project; project-specific protocols and checks are implementation
work, not a prerequisite experiment.

### 2. Prove recursion and repair

Repeat with one coordinator splitting its backend obligation into two or three
smaller obligations. The root sees its completed backend contract, not every
leaf’s internal execution history.

Exercise an interface-revision request, a failed child, and a clean Git merge
whose combined behavior fails a test. Verify that the parent can retain useful
work, request repair, and discharge its own contract without accepting undeclared
holes. Also prove cancellation ends the owned subtree and preserves commits.

Acceptance: at least two successive fork/integration levels, independent branch
progress without a global barrier, and correct recovery from those failures.

### 3. Retain only useful project automation

Save the successful Haskell protocol definitions, scaffold conventions, check
commands, and agent guidance in the standalone project. The steering actor may
revise these as it learns. Existing children retain their issued contract;
changes to guidance do not silently rewrite assignments in flight.

Run a second feature with a different decomposition. Extract a reusable helper
only when both campaigns demonstrate its consumer. Keep domain-specific types
and acceptance policy authored; Rust continues to own authority, Git mechanics,
processes, scheduling, and durable observations.

### 4. Measure useful granularity

Compare sequential work, a coarse context tree, and a recursive microtask tree
on matched tasks and the same acceptance checks. Repeat enough runs to avoid
treating one cache outcome or one successful decomposition as general evidence.

Record total input/cached/output tokens, available billed cost, elapsed time,
parent coordination and repair effort, discarded work, integration failures,
and final acceptance. Attribute usage to existing actor/request identities;
extend existing observations rather than creating another usage ledger.

Choose branch size from marginal inference and integration cost. Tiny branches
are welcome when shared decisions are settled and their work is independent;
there is no minimum ticket duration or mandatory tree depth.

### 5. Add context/model choices where evidence supports them

Add fresh-context interactive workers when inherited history is irrelevant or
too large, and explicit per-child model selection for bounded work. Keep
context origin, model/effort, and authority separate. Inspect the existing
fresh-spawn and backend launch owners before designing new public surface.

Check provider history compatibility before offering a different model an
inherited transcript. Do not promise cache sharing across models. Start a
specialist with a fresh explicit brief when inherited history is unsupported.
Compare total cost including coordinator review and repairs. A fixed Sol
coordinator hierarchy is not a prerequisite.

## Scope and exit condition

The first deliverable is a verified two-level campaign in a lightweight project,
with deliberate partial commits and typed integration/repair. It is not a new
generic swarm framework, distributed actor system, transparent restart system,
or model-routing optimizer. The user’s standalone target project can replace
the disposable project when selected; no target application changes are implied
by this plan.

When the campaign is implemented, move proven project practice to its guide
and any runtime invariants to their owning documentation. Retire this delivery
plan rather than keeping it as standing architecture.
