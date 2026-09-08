`unfold` admits persistent context forks and returns their handles now. Children
start after the entire enclosing tool block finishes, including statements after
`unfold`. All forks queued in that block inherit its final committed Haskell
bindings and the conversation through the actual tool result.

```haskell
let Right campaign = campaignLabel "my-project"
let Right wave = forkGroupLabel "first-wave"
let Right domainLabel = branchLabel "domain"
let Right consumerLabel = branchLabel "consumer-tests"
:{
workers <- unfold (batch campaign wave) $
  (,) <$> child (coding @Report domainLabel projectHead domainPlan)
      <*> child (withEffort Low (coding @Report consumerLabel projectHead consumerPlan))
:}
let sharedAfterUnfold = ("ready" :: Text)
```

Each `Forked a` contains its retained `AgentRef`, `Response a`, and immutable
launch/worktree receipt. The handles name admitted children; they do not mean
that child inference has started. In this example both children can inspect
`sharedAfterUnfold`, although it was defined after `unfold`. Multiple unfolds
in one block share its final scope; later parent tool calls cannot change it.
The branch role narrows effects and native authority independently.

Assignment values, captured closures, and explicit worktree seeds keep their
ordinary Haskell value semantics; they are not reevaluated at child startup.
A later executable failure stops the block's suffix but preserves successful
earlier unfolds. Children inherit the last committed bindings and the real tool
result describing the failure. Failed admission cancels only its own group.

You may enqueue requests, poll, and register watches after `unfold` in the same
block. Do not synchronously call or wait for a queued child: it cannot start
until this block returns. Register a watch or wait in a later tool call.
If the host reconnects before completion is acknowledged, queued children are
cancelled with an explicit failure; already started children are unaffected.

`withEffort Low`, `Medium`, or `High` requests the child's initial reasoning
effort. Omission selects Low, independently of the parent setting. The request alone is not evidence
of provider application or cache reuse; inspect provider observations before
claiming either. The consumer branch above inherits the same committed context
while explicitly selecting Low for a bounded obligation against an agreed interface.
Keep more effort for shared design choices and uncertain integration work.
`withEffort` applies at context-fork construction; it is not an API for steering
an already active Shoal assignment. The Codex backend's configuration-update
mechanism is a separate control layer, not a missing model capability.

Define your `Report` type and `domainPlan`/`consumerPlan` values first. Commit the
shared interface before dispatch. `domainPlan` owns implementation; `consumerPlan`
owns independent tests through that interface, including failure behavior. Each
names owned paths, the contract revision, acceptance, and allowed holes. Return
exact candidates, check evidence, discoveries, and unresolved decisions. Keep
shared wiring with the coordinator. Review an implementation candidate after it
exists; the parallel consumer branch tests the contract from the common scaffold.
Ordinary `unfold` inherits the selected checkout's working files and private
index. If capture is busy or unavailable, it uses that checkout's committed HEAD
and reports that working files were omitted. A committed shared interface remains
useful for integration and crash recovery. Build caches follow the creator; a
completed warm build helps descendants without stopping active builds.

`coding` children can repeat the scaffold/fork/fold rhythm within their inherited
descendant budget. Use `boundHead` when their children should start from the
child-owned scaffold. `scaffolding` selects a scaffold emphasis with the same
capabilities. Use a coding reviewer when review includes running checks.

`researching` admits inspection-only researchers with bounded delegation;
`researchingLeaf` omits `Forks` and actor control. Research cannot escalate into
coding, integration, or build/test execution. Host `[research]` configuration in
`.shoal/config.toml` defaults to `default_depth = 1`, `maximum_depth = 8`, and
`maximum_active_children = 32`. The first researcher defaults to one generation;
research descendants inherit remaining depth. Every child consumes a generation.

For a deeper coordinator, select and inspect a proposal before admission:

```haskell
let proposal = withForkBudget (ForkBudget 3 6) (researching @Text researchLabel boundHead assignment)
previewBranch proposal
```

Requested depth/width are capped by config and parent allowance. Preview reports
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

Width counts active or reserved descendants across the subtree; all ancestor
ceilings also apply. Setting `maximum_depth = 0` disables research recursion.
Configuration is loaded at host startup and does not change existing actors.
