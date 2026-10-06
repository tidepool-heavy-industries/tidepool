Execution owners use the recursive scaffold → ready batch → checked integration
cycle in `RECURSIVE-WORK.md`, normally through `lunaLead`/`lunaTask` and
`unfoldWork`. This page describes the underlying admission primitive used by
that procedure and by custom typed joins.

An applicative unfold admits the ready frontier; it does not schedule data
dependencies between its branches. Build a shared contract first, and place a
branch that needs another child's result in a later monadic stage. Choose the
continuation too: `waitFor` for one suspended computation, a named watch for
finite later observation, a routing collector for ongoing batch progress and
questions, or a record actor for custom event state. Keep the original responses and select lifetime
independently of observation. The `exomonad-fork` and `exomonad-coordinate` skills
apply these primitives to Project work.

`unfold` publishes children immediately. Every branch chooses
`withContext (fromCheckpoint captured)` or `withContext (selected render)`;
unresolved inherited context is refused before allocation. Capture a useful
scaffold before admitting dependent children:

```haskell
Right captured <- checkpoint "shared-scaffold"
let domainLabel = [label|domain|]
let consumerLabel = [label|consumer-tests|]
let fromScaffold = withContext (fromCheckpoint captured)
workers <- unfold (batch "my-project" "first-wave") $
  (,) <$> child (fromScaffold (coding projectHead (assignment domainLabel domainPlan)))
      <*> child (fromScaffold (withEffort Medium (coding projectHead (assignment consumerLabel consumerPlan))))
joined <- waitFor ((,) <$> awaitSettled (fst workers) <*> awaitSettled (snd workers))
```

Define your result types and task values first. This ordinary Haskell program
preserves its continuation while awaiting both children. `waitFor` returns
`Either WatchFailure result`; `awaitSettled` keeps a child's unavailable result
inside its `Settlement`. Named `watch` subscriptions are useful when later
model decisions or independently inspectable observations participate.

Provider workers are `InvocationOwned` by default. Scope exit cancels unfinished owned work
and retains cleanup. Returning handles or installing a watch does not extend
its lifetime. Use `withLifetime ActorOwned` on branches that span model turns;
`SwarmOwned` is an explicit top-level, selected-context choice. Borrowed handles
permit inspection while available, never owner cancellation. Releasing a
checkpoint ends future use; admitted children retain their own capture leases.

`unfoldDeferred` and `attemptUnfoldDeferred` publish after the enclosing call's
real result and final committed binding tip. Every deferred branch requires an
explicit persistent lifetime; invocation-owned deferred work is refused before
allocation. Deferred branches may use `inherited` context. Return from admission
before waiting for those children; awaiting them in that invocation prevents
publication. No pending marker is a fabricated completed tool result.

`attemptUnfold` and `attemptUnfoldDeferred` return typed admission refusal. All
branches are checked before any allocation. Assignment values, captured closures
and explicit worktree seeds keep ordinary Haskell value semantics. Context,
checkout, lifetime and authority are separate choices.

Each launch `Response` retains the exact actor and immutable worktree receipt.
Admission is distinct from provider startup; inspect `ResponseStarting` rather
than claiming every admitted child is running. A later failure stops the cell's
suffix; prior completed effects and their receipts remain. Invocation cleanup
still applies to unfinished owned work.

`withEffort Low`, `Medium`, or `High` requests the child's initial reasoning
effort. Omission uses the launch selector's inherited default. Inspect provider
observations when checking the effective selection or cache reuse. The consumer
branch above inherits the same committed context while explicitly selecting
Medium. Model placement is independent: the workspace execution policy uses a
Sol root and recursive Luna owners; its branch constructors select those models.
`withEffort` applies when constructing a context unfold; it is not an API for steering
an already active Exomonad assignment. The Codex backend's configuration-update
mechanism is a separate control layer, not a missing model capability.

Define your `Report` type and `domainPlan`/`consumerPlan` values first. Commit the
shared interface before dispatch. `domainPlan` owns implementation; `consumerPlan`
owns independent tests through that interface, including failure behavior. Each
names owned paths, the contract revision, acceptance, and allowed holes. Return
exact candidates, check evidence, discoveries, and unresolved decisions. Keep
shared wiring with the coordinator. Review an implementation candidate after it
exists; the parallel consumer branch tests the contract from the common scaffold.
Before live-source capture, Exomonad checkpoints eligible source changes on the
source checkout's current branch. The child inherits that commit. Runtime
`.exomonad/`, configured source exclusions and recognized caches stay out even when
staged. The checkpoint runs no hooks, builds or tests. If Git cannot commit, the
fork fails with its source files preserved; it does not silently use an older
`HEAD`. An unchanged source needs no commit. An explicit committed-ref launch
still selects its specified revision. Build caches follow the creator; a
completed warm build helps descendants without stopping active builds.

`coding` owners repeat scaffold, ready parallel batch and checked integration
within their inherited descendant budget. Before substantial direct implementation,
briefly justify a terminal leaf. Use `currentCheckout` when their children should start from the
child-owned scaffold. `scaffolding` selects a scaffold emphasis with the same
capabilities. Use a coding reviewer when review includes running checks.

`researching` admits inspection-only researchers with bounded delegation;
`researchingLeaf` omits `Forks` and actor control. Research cannot escalate into
coding, integration, or build/test execution. Host `[research]` configuration in
`.exomonad/config.toml` defaults to `default_depth = 1`, `maximum_depth = 8`, and
no concurrency ceiling. Set `maximum_active_children` only for an explicit finite
research limit. The first researcher defaults to one generation;
research descendants inherit remaining depth. Every child consumes a generation.

Use `withForkBudget (ForkBudget depth width)` to request a deeper research
coordinator and inspect `previewBranch` before admission.

Requested depth/width are capped by config and parent allowance. Effective
`ForkAllowance` has `allowanceDepth` and `allowanceWidth :: Maybe Int`; Nothing
means no concurrency ceiling, Just 0 means no descendants. Actor context uses the
same optional width. Explicit finite limits count the entire subtree, including
staged reservations; descendants cannot evade an ancestor's limit. Preview reports
role, workspace access, effects, requested/effective budgets, and `CanFork`,
`ForksOmitted`, or `BudgetExhausted`. `previewSource`, `previewContext`,
`previewLifetime` and `previewGuidance` expose the branch selection. The optional
`previewLaunch` resolves host model/effort, static instructions, base fingerprint,
frozen definition identity and configured imports through the native launch
selector. An absent launch resolver is explicit; an absent resolved model means
inherit the parent's boundary selection. Runtime paths/request orientation are
added at admission. Preview allocates nothing and starts no child. It does not reserve current capacity or
validate the worktree seed. A leaf-sized assignment and zero fork authority are
different things. `researchingLeaf` explicitly omits delegation; a researcher
with an exhausted budget retains `Forks` in its row but cannot admit children.

For a small inspection question, `errand name question` starts an invocation-owned
inspection-only leaf and returns its named watch. Await it in the same invocation
with `awaitWatch`; it is cancelled if the invocation returns unfinished. For a
persistent direct worker use `startAgent` with `withAgentLifetime ActorOwned` on
its `AgentLaunchSpec`, then submit a typed request. A launch spec selects worker
intent; the tools installation contract is the separate `AgentSpec`.

Width counts active or reserved descendants across the subtree; all ancestor
ceilings also apply. Setting `maximum_depth = 0` disables research recursion.
Configuration is loaded at host startup and does not change existing actors.

skill: exomonad-unfold
