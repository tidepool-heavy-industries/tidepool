# GHC lexer/layout foundation

Candidate source adds `splitCellWithFlags` beside the existing GHC-owned
classifier in `Tidepool.Binders`. It calls `lexTokenStream` with the evaluation
dialect, discards zero-width virtual/EOF tokens, and creates a new item for the
first real token plus each later real token whose span begins in column one.
Source is sliced from the original payload by line offsets, so quasiquotes,
multiline strings, comments, pragmas, blank lines, and line endings are not
reconstructed.

`CellSourceItem` carries exact GHC token coordinates and source. This is a lexical
domain interface only: declarations still group after per-item
`classifyWithFlags`, and the worker's final diagnostic type remains coordinator
owned.

The focused `cell-splitter-test` covers:

* a multi-line data declaration;
* a signature and equation as distinct source items that both classify as
  declarations;
* a hanging `where`;
* an unindented quasiquote body with a blank line;
* an unindented `MultilineStrings` body with a blank line;
* a language pragma and nested block comment;
* bind and expression classification after splitting.

Coordinator patch obligations:

1. call `splitCellWithFlags` and `classifyWithFlags` in the new worker request
   without recreating either algorithm in Rust;
2. translate `PFailed` through the existing GHC diagnostic owner rather than
   exposing the foundation's marker-only `CellLexFailure`;
3. preserve source-item span/ordinal separately from grouped declaration
   execution steps;
4. decide whether a standalone pragma attaches to the following declaration
   during synthesis; the lexer correctly reports it as its own column-one source
   item and does not own that semantic grouping;
5. retire Pest and `GhciInputUnit::Block` only after the cell consumer is
   integrated and its fixtures migrated.

Checks:

```text
cd haskell && cabal test cell-splitter-test --test-show-details=direct
PASS: 1 suite, 1 case

cd haskell && cabal build tidepool-extract-bin
PASS

git diff --check
PASS
```
