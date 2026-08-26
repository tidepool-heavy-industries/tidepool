# DevSwarm: Haskell-native development organization

## Status

Active on branch `devswarm-haskell-native`. The existing
`harness-dogfooding/dev-tree/` remains the production dogfood while the
successor runs in parallel at `harness-dogfooding/devswarm/`.

## Boundary

DevSwarm is the project-specific program that develops Tidepool. It is not a
second framework or a universal workflow language. Tidepool supplies Haskell
effects and agent-session machinery; DevSwarm supplies repository policy,
prompts, routing heuristics, and organizational roles.

The REPL is the agent interface. JSON schemas, provider envelopes, generated
effects, and durable file layouts are interpreter details. Public values are
designed for ordinary Haskell first.

## Runtime shape

One outer loop iteration opens one root owner agent session. That owner reasons
before choosing a shape and may:

- fork recursively capable child owner sessions with ordinary typed `fork`;
- run several child owners concurrently with `async` and `wait`;
- ask short-lived delegates for research, implementation, review, or revision;
- combine child reports and delegate results with ordinary Haskell;
- finalize one typed organizational outcome to its parent.

The owner tree emerges dynamically from model-authored Haskell. It is not a
pre-authored `Plan`, a git graph, or the set of retained worktrees. A static
bottom-up fold remains a useful local technique for an already-known dependency
tree, but it is not DevSwarm's execution engine.

Current Tidepool fork children are full multi-round agent sessions that may
fork and delegate recursively. They are one-shot within a loop iteration and
retire after `finalize`; durable node re-entry is still an open platform item.

## Organization and authority

- Every durable node has one owner responsible for its decisions.
- An owner may create many short-lived delegates.
- An implementation delegate works in an isolated retained worktree and
  returns an inert candidate.
- A reviewer works in a separate worktree rooted at the candidate.
- A revision delegate rebinds the candidate's retained worktree.
- A delegate never receives the capability that advances the owner's node.
- Worktrees are proposals, not executable nodes. Creating a child owner is a
  separate decision made only when work deserves durable decomposition.

Reliability comes from overlapping responsibility: implementers propose,
ordinary local tests/hooks/CI produce signals, adversarial reviewers inspect,
and the owner decides. Tidepool does not construct a universal correctness
score or evidence ladder.

## Haskell surface

The project-local request language is indexed by its result:

```haskell
data DelegateTask result where
  Investigate :: ResearchBrief -> DelegateTask ResearchFindings
  Implement   :: ChangeBrief -> DelegateTask CandidateChange
  Review      :: ReviewBrief -> DelegateTask ReviewFindings
  Revise      :: CandidateChange -> RevisionBrief -> DelegateTask CandidateChange

delegateTask
  :: Member Delegate effs
  => DelegateTask result
  -> Eff effs (Either DelegateFailure result)
```

The interpreter derives a private model schema for each result type and then
assembles workspace facts from the real spawn outcome. A delegated model never
manufactures `WorktreeHandle`, `WorktreeId`, base OID, or head OID.

Live and durable candidate identities are separate:

```haskell
data CandidateRef = CandidateRef
  { candidateWorktreeId :: WorktreeId
  , candidateBase       :: GitOid
  , candidateHead       :: GitOid
  }

data CandidateChange = CandidateChange
  { candidateRef      :: CandidateRef
  , candidateWorktree :: WorktreeHandle
  , candidateReport   :: ChangeReport
  }
```

`CandidateRef` is suitable for future node storage. `CandidateChange` remains
an in-heap capability value that an owner can pass to `Review` or `Revise`.
The trusted interpreter uses the handle to create or rebind workspaces; only a
prompt and isolated checkout cross into the delegated model session.

Owner outcomes are ordinary sums. Findings are advisory products, deliberately
not shaped like owner decisions. Project-specific matching may be plain
functions over paths, task text, reports, or any other useful local data.

## Implemented slice

- `harness-dogfooding/devswarm/Harness.hs` is a runnable selfharness entrypoint.
- Its checkpointed `State` is only a compatibility seed plus last rendered
  outcome; it is not the orchestration graph or a serialized continuation.
- `DevSwarm.Types` contains pure owner briefs and outcome sums/products.
- `DevSwarm.Delegation` contains the GADT request language and prompt shaping.
- `Tidepool.Agent.Delegate` accepts arbitrary typed result values and returns
  `DelegateRun result`, whose worktree/base/head are observed after the model
  call rather than included in its schema.
- Fresh implementation worktrees, isolated review worktrees, and retained
  revision worktrees share the one existing Subagent/Worktree spawn substrate.
- The dogfood typecheck compiles a representative owner request against the
  actual narrow Delegate row.
- The existing real-saga delegation test observes the returned workspace base
  and head through the JIT path.

## Platform issues

These are the remaining blockers. They are deliberately scoped to behavior the
runnable program needs.

### DS-01 — Node-scoped workspace and store

**Need.** A recursive owner session currently has no private integration
workspace and no durable node namespace. It can select, review, and revise a
candidate, but cannot structurally be the only session allowed to advance its
node.

**Target.** When the driver creates an owner session, attach a stable node
identity, a retained integration workspace, and a capability-scoped KV/message
namespace. These belong to the trusted session interpreter. The model-facing
surface should expose semantic operations and opaque values, not global KV,
raw paths, or lookup-by-string authority.

**Acceptance.** Two owners cannot advance the same node concurrently. An owner
can integrate one candidate into its node workspace, record the resulting head,
and restart by reconciling that workspace plus its namespace.

### DS-02 — Durable owner re-entry

**Need.** Recursive fork children are currently one-shot agent sessions. A
process crash reruns the outer turn; it does not adopt durable child owners by
node identity.

**Target.** Re-evaluate the owner program after restart, resolve each requested
node through DS-01's namespace, adopt completed candidates/decisions, and only
spawn missing work. Do not serialize continuations, live handles, closures, or
whole agent sessions.

**Acceptance.** Killing a run after a child commits a candidate does not repeat
that implementation call when the same durable node is re-entered.

### DS-03 — Selfharness entrypoint without global domain `State`

**Need.** The current driver contract requires `initialState`, `render`, and
`loop :: State -> Harness State`. DevSwarm therefore carries a deliberately
thin compatibility record even though node stores and git should own durable
coordination.

**Target.** Add an entrypoint whose bootstrap value is a root node reference or
whose loop reconstructs directly from the node store. Keep the existing State
entrypoint for other harnesses.

**Acceptance.** DevSwarm boots and resumes without decoding a whole-run domain
record. Removing its compatibility `State` changes no node semantics.

## What remains project-local

- task and repository-layout heuristics;
- prompt wording and grounding selection;
- which tests, hooks, reviewers, or path checks a task deserves;
- how reports influence an owner's decision;
- retry, replan, escalation, and operator consultation policy;
- when to fork a durable child owner;
- whether to request one patch or several tiny PR-sized candidates.

These may be fuzzy Text-driven functions when that best matches the project.
They do not become Tidepool mechanisms merely because DevSwarm uses them.

## Next execution slice

1. Run one root owner that requests two implementation candidates concurrently.
2. Have it send both to independent review delegates and choose one.
3. Implement DS-01 just far enough for that owner to integrate the chosen
   candidate into its private node workspace.
4. Kill and restart between candidate completion and integration to exercise
   DS-02.
5. Only then add convenience combinators discovered through real REPL use.

## Completion condition

This plan retires when DevSwarm can develop Tidepool through node-owned
workspaces, survive restart from git plus node storage, and replace `dev-tree`
as the forward dogfood. Standing contracts then move to the owning modules and
this plan is deleted.
