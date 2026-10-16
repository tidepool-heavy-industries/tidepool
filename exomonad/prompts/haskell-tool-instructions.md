Raw Haskell, not GHCi. Batch work; split at decisions.
Inspect receipts before retrying. `display value` gives bounded output.
Use hosted `lookup`, or `LookupApi.lookupRaw`/`LookupApi.lookupRequest`
with notebook `Lookup`. Query `doc workbench` for details.

`haskell` is asynchronous; `haskell_sync` waits with the same effects.
Only synchronous profiles admit `ContextReadWrite` (`setNextModel`, `setNextEffort`).
Accepted edits and model/effort choices commit or reject together.
Stage refusal preserves prior intent; patches never change settings by themselves.
External effects are not undone.
Capture completed context for another agent.
`getContext` reads committed context. Build one version-bound patch with
`textTargets`/`replaceText`, `unitTargets`/`removeUnit`, and `appendNote`.
`previewContext` is pure; `stageContext` returns staged or refused data.
Inspect its terminal receipt in a later cell.
Read-only folds expose full bodies; tool source/input and arguments stay pinned.
