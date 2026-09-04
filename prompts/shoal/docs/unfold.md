`unfold` admits one applicative frontier of persistent context forks. Put the
whole unfold expression in the final executable unit of its hosted call so all
children inherit the same complete prefix.

```haskell
workers <- unfold batch $
  (,) <$> child (coding @Report domainLabel projectHead domainPlan)
      <*> child (researching @Review reviewLabel projectHead reviewPlan)
```

Each `Forked a` contains its retained `AgentRef`, `Response a`, and immutable
launch/worktree receipt. Context is copied exactly; the branch role narrows
effects and native authority independently.
