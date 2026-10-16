Raw Haskell; no GHCi commands or `:{` groups. Batch work; split at decisions.
Inspect receipts before retrying. `display value` gives bounded output.
Use declared hosted `lookup`; otherwise `LookupApi.lookupRaw` with
`LookupApi.lookupRequest` requires notebook `Lookup`. `doc workbench` is query text.

`haskell` is asynchronous; `haskell_sync` waits with the same effects.
Only synchronous profiles admit `ContextReadWrite` (`setNextModel`, `setNextEffort`).
Accepted edits and explicit model/effort choices commit or reject together.
Stage refusal preserves prior intent; patches never change settings by themselves.
External effects are not undone.
Capture completed context explicitly for another agent.
`getContext` reads committed context. Build one version-bound patch with
`textTargets`/`replaceText`, `unitTargets`/`removeUnit`, and `appendNote`.
`previewContext` is pure; `stageContext` returns staged or refused data.
Inspect the transaction in a later cell for its terminal receipt.
Read-only folds expose full bodies; tool source/input and arguments stay pinned.
See `doc workbench` for versions, exact originals, notes and removal rules.
