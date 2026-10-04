```haskell
data Finding = Finding Text
summarize (Finding path) = prefix <> path
  where prefix = "checked: "
paths <- pure ["src", "tests"]
labels <- pure (map (summarize . Finding) paths)
display labels
```

Execute raw Haskell in the persistent notebook: declarations, `let` bindings,
effectful `<-` bindings, and expressions. Values remain typed without automatic
rendering; use `display value` for bounded structured output. Whole-cell typechecking precedes
execution. Successful cells publish declarations and bindings together. Failure or
cancellation before publication publishes no cell names; completed effects and
independently owned captures retain their receipts. Inspect the outcome before
retrying. A cell splits into units at
column-1 boundaries: keep `respond value` on one line with nothing after it.
For signatures or `doc workbench`, use hosted `lookup` when declared; otherwise
use `LookupApi.lookupRaw` with `LookupApi.lookupRequest` when the admitted notebook
lists `Lookup`. `Prelude.lookup` is list lookup; `doc` queries are not Haskell.
