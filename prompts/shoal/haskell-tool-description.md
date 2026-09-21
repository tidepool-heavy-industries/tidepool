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
execution; runtime failure retains the completed prefix. Inspect the receipt
before retrying. Use `lookup` for missing signatures and `doc workbench` for recovery.
