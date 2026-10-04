```haskell
data Finding = Finding Text
summarize (Finding path) = prefix <> path
  where prefix = "checked: "
paths <- pure ["src", "tests"]
labels <- pure (map (summarize . Finding) paths)
display labels
```

Run raw Haskell in the persistent notebook: declarations, `let` bindings,
effectful `<-` bindings, and expressions. Use `display value` for bounded structured
output; values are not rendered automatically. The whole cell typechecks before
execution. Successful cells publish declarations and bindings together. Failure or
pre-publication cancellation publishes no cell names; completed effects and
independently owned captures survive. Inspect the outcome before retrying.
A cell splits into units at
column-1 boundaries: keep `respond value` on one line with nothing after it.
For signatures or `doc workbench`, use hosted `lookup` when available, or
`LookupApi.lookupRaw` with `LookupApi.lookupRequest` if the notebook admits `Lookup`.
`Prelude.lookup` searches lists; `doc` queries are not Haskell.
