Raw Haskell only; no GHCi commands or `:{` groups. Batch known work; split at
decisions. Inspect rejection/runtime receipts before resubmitting effects. Use
`cellDisplay.more`, `lookup`, or `doc` for omitted display or APIs.

`haskell` is asynchronous by default. A synchronous profile waits before the
next inference and alone can include `ContextReadWrite` (`setNextModel`,
`setNextEffort`). These edits commit together on cell success; issued external
effects are not undone. Use `unfoldDeferred` for actor-owned children after
curation; never await them in their creating invocation. `editableTexts` exposes
full eligible message/result bodies and authored text; tool source/input and
function arguments stay pinned. Use `C.trimText reason retainedText` for exact
source plus `[Trimmed: reason]`. Same-model continuation forwards opaque
reasoning unchanged. See `doc workbench` for restore rules.
