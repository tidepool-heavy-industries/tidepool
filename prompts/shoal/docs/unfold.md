`unfold` admits persistent context forks and returns their handles now. Children
start after the entire enclosing tool block finishes, including statements after
`unfold`. All forks queued in that block inherit its final committed Haskell
bindings and the conversation through the actual tool result.

```haskell
let Right campaign = campaignLabel "my-project"
let Right wave = forkGroupLabel "first-wave"
let Right domainLabel = branchLabel "domain"
let Right reviewLabel = branchLabel "review"
:{
workers <- unfold (batch campaign wave) $
  (,) <$> child (coding @Report domainLabel projectHead domainPlan)
      <*> child (researching @Review reviewLabel projectHead reviewPlan)
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
effort. Omission inherits the parent setting. The request alone is not evidence
of provider application or cache reuse; inspect provider observations before
claiming either. Changing effort on an already running actor is not supported.

Define your `Report`/`Review` types and `domainPlan`/`reviewPlan` values first.
`projectHead` requires a clean source; choose `snapshotDirty projectHead`
explicitly when the branches should inherit existing uncommitted changes.

`coding` children can repeat the scaffold/fork/fold rhythm within their inherited
descendant budget. Use `boundHead` when their children should start from the
child-owned scaffold. `scaffolding` selects a scaffold emphasis with the same
capabilities. Use a coding reviewer when review includes running checks.
