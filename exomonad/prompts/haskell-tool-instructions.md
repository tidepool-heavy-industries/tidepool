Raw Haskell; no GHCi commands or `:{` groups. Batch work; split at decisions.
Inspect receipts before retrying. `display value` gives bounded output.
Use declared hosted `lookup`; otherwise `LookupApi.lookupRaw` with
`LookupApi.lookupRequest` requires notebook `Lookup`. `doc workbench` is query text.

`haskell` is asynchronous; `haskell_sync` waits with the same effects.
Only synchronous profiles admit `ContextReadWrite` (`setNextModel`, `setNextEffort`).
Edits commit together on success; external effects are not undone.
Capture completed context explicitly for another agent.
`visibleTexts` includes notes inside native groups. `referencedTexts` exposes
issued identities and removal flags; `removeText reference` removes one eligible
occurrence. `editableTexts` exposes full eligible bodies; tool source/input and
arguments stay pinned. Same-model continuation keeps opaque reasoning.
See `doc workbench` for `trimText`, restore and removal rules.
