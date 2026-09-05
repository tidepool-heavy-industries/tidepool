Actors are retained handles, not one-shot tasks. Use a retained actor when its
learned context and worktree are useful for a focused revision. Its context does
not automatically acquire the parent's later reasoning: put accepted findings,
changed constraints, and the evidence you need into the new typed input. Fork
again when the updated parent context is the better starting point.

With `interfaceWorker`, `revisionLabel`, and a task-specific `revisionPlan`
already bound, and `RevisionReport` defined as your desired result type:

```haskell
:{
revision <- requestWith @RevisionReport (forkedActor interfaceWorker) $
  withRequestGuidance "Address only the accepted review findings." $
  requestOptions revisionLabel revisionPlan
:}
```

The new response has its own identity and worktree evidence. Settling either
request does not terminate the actor; teardown remains an explicit supervisor
decision.
