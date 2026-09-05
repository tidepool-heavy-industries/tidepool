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
  (,) <$> child (coding @Report domainLabel projectHead domainPlan)
      <*> child (researching @Review reviewLabel projectHead reviewPlan)
:}
```

Each `Forked a` contains its retained `AgentRef`, `Response a`, and immutable
launch/worktree receipt. Context is copied exactly; the branch role narrows
effects and native authority independently.

Define your `Report`/`Review` types and `domainPlan`/`reviewPlan` values first.
`projectHead` requires a clean source; choose `snapshotDirty projectHead`
explicitly when the branches should inherit existing uncommitted changes.
