# Worked notebook cells

These are reference cells to copy into a resident workbench, not modules to import.
Load the `exomonad-jev` skill for question-writing and packet composition.

Some cells assume this shared helper; declare it once in the workbench:

```haskell
sh :: Member Commands effs => [Text] -> Eff effs Text
sh args = do
  r <- Cmd.quiet (Cmd.run (Cmd.argv args))
  pure (either (const "") id (Cmd.stdout r))
```

`Cmd.quiet` keeps full command output out of the cell's displayed result.

- `12-termination.hs`: compares four question formulations over identical state.
- `33-threeway.hs`: classifies recorded check failures with bounded semantic choices.
- `35-threeway-fair.hs`: compares alternatives using the same check evidence.
- `37-reflect-intent.hs`: separates instructions, actor history, and repository evidence.
- `52-question-self-lint.hs`: scores question wording before asking the question.
- `dispatch-tidepool.hs`: applies the dispatch pattern to repository evidence.

`fixtures/` contains recorded diagnostics from a demo application; paths inside
those outputs belong to that application. The cells that read these files expect
them at `.exomonad/plans/examples/fixtures/`. Historical observations are not
current accuracy measurements; `dispatch-tidepool.hs` has no retained execution
result. Use the pinned workspace guide and root contributor rules for current
execution and verification requirements.
