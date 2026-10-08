Raw Haskell; no GHCi commands or `:{` groups. Batch work; split at decisions.
Inspect receipts before retrying effects. Use `display value` for bounded structured output.
Use hosted `lookup` if declared; otherwise use
`LookupApi.lookupRaw` with `LookupApi.lookupRequest` only when the notebook admits
`Lookup`. `doc workbench` is query text, not Haskell.

`haskell` is asynchronous; `haskell_sync` waits with the same effects. Only a
synchronous profile can add `ContextReadWrite` (`setNextModel`, `setNextEffort`).
Edits commit together on success; external effects are not undone. Capture a
completed context explicitly when another agent should start from it.
`editableTexts` exposes full eligible message/result bodies and authored text;
tool source/input and arguments stay pinned. With
`import qualified Tidepool.Agent.Context as C`, `C.trimText reason retainedText`
keeps exact source plus `[Trimmed: reason]`. Same-model continuation keeps
opaque reasoning. See `doc workbench` for restore rules.
