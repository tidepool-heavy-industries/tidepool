Actors are retained handles, not one-shot tasks. When a completed branch needs
a focused revision, send another typed request to the same learned context and
worktree instead of unfolding a replacement.

```haskell
revision <- requestWith (forkedActor interfaceWorker) $
  withRequestGuidance "Address only the accepted review findings." $
  requestOptions revisionLabel revisionPlan
```

The new response has its own identity and worktree evidence. Settling either
request does not terminate the actor; teardown remains an explicit supervisor
decision.
