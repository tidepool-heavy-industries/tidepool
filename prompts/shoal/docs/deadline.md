No deadline is the default. A deadline bounds waiting for reply acceptance;
it does not promise that the target's processes have stopped. Use dimensional
time, not bare integers. Given an existing `worker`, declared `Report`, and
input `task`:

```haskell
response <- request @Report worker $
  (assignment "bounded-review" task) { deadline = Just (minutes 10) }
```

`milliseconds`, `seconds`, and `minutes` construct `Duration`. Status preserves the authored unit and shows
absolute and remaining time.

Expiry makes the response unavailable with `ResponseDeadlineExceeded` and
wakes dependent watches without waiting for cancellation acknowledgement.
Later replies are rejected. If reply acceptance already won the race, its
terminal settlement is preserved. Cancellation is requested separately, and
target custody remains live until its execution actually closes. Inspect the
reply/actor state before assuming it is safe to clean up.
