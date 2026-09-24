```haskell
data Finding = Finding Text
summarize (Finding path) = prefix <> path
  where prefix = "checked: "
paths <- pure ["src", "tests"]
labels <- pure (map (summarize . Finding) paths)
labels
```

Execute raw Haskell in the persistent notebook: declarations, `let` bindings,
effectful `<-` bindings, and display expressions. Whole-cell typechecking precedes
execution; runtime failure retains the completed prefix, and the receipt names
what each unit did. Inspect it before retrying. A cell splits into units at
column-1 boundaries: keep `respond value` on one line with nothing after it.
Use `lookup` for missing signatures and `doc workbench` for recovery.
