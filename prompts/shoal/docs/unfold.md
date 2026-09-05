`unfold` admits one applicative frontier of persistent context forks. Put the
whole unfold expression in the final executable unit of its hosted call so all
children inherit the same complete prefix.

```haskell
let Right campaign = campaignLabel "my-project"
let Right wave = forkGroupLabel "first-wave"
let Right domainLabel = branchLabel "domain"
let Right reviewLabel = branchLabel "review"
:{
workers <- unfold (batch campaign wave) $
  (,) <$> child (withEffort Low (coding @Report domainLabel projectHead domainPlan))
      <*> child (researching @Review reviewLabel projectHead reviewPlan)
:}
```

Each `Forked a` contains its retained `AgentRef`, `Response a`, and immutable
launch/worktree receipt. Context is copied exactly; the branch role narrows
effects and native authority independently.

`withEffort Low`, `Medium`, or `High` requests the child's initial reasoning
effort. Omission inherits the parent setting. The request alone is not evidence
of provider application or cache reuse; inspect provider observations before
claiming either. Changing effort on an already running actor is not supported.

Define your `Report`/`Review` types and `domainPlan`/`reviewPlan` values first.
`projectHead` requires a clean source; choose `snapshotDirty projectHead`
explicitly when the branches should inherit existing uncommitted changes.
