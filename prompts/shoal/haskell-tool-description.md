```haskell
data Finding = Finding Text
summarize (Finding path) = prefix <> path
  where prefix = "checked: "
paths <- pure ["src", "tests"]
labels <- pure (map (summarize . Finding) paths)
labels
```

Send notebook cells of raw Haskell. GHC checks the complete cell before effects;
declarations are mutually recursive, later statements see earlier bindings, and
each expression displays. Ordinary data types display without deriving; truncated
output offers `cellDisplay.more`. Typecheck rejection changes nothing. Runtime failure retains
its completed prefix and marks the suffix not run. Declarations and bindings persist.

Use hosted `lookup` for names, `::type` queries, and documentation; use `status`
for actor state. Activations expose `sessionInput`, `sessionReply`, and `respond`.
Ending the model response ends the turn; watches and steering can wake it. Failed
provider turns leave requests pending; inspect handles and receipts before action.
