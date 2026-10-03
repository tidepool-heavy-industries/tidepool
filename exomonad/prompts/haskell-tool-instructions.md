Raw Haskell only; no GHCi commands or `:{` groups. Batch known work; split at
decisions. Inspect rejection/runtime receipts before resubmitting effects. Use
`display value` for bounded structured output. For missing APIs use hosted `lookup`
if declared; otherwise use `LookupApi.lookupRaw` with `LookupApi.lookupRequest` when
the admitted notebook lists `Lookup`. `doc workbench` is query text, not Haskell.

`haskell` is asynchronous by default; `haskell_sync` uses the same effects and
waits before the next inference. Only an explicit synchronous profile can add
`ContextReadWrite` (`setNextModel`, `setNextEffort`). These edits commit together
on cell success; issued external effects are not undone. Use `unfoldDeferred` for actor-owned children after
curation; never await them in their creating invocation. `editableTexts` exposes
full eligible message/result bodies and authored text; tool source/input and
function arguments stay pinned. With
`import qualified Tidepool.Agent.Context as C`, use `C.trimText reason retainedText`
for exact source plus `[Trimmed: reason]`.
Same-model continuation forwards opaque
reasoning unchanged. See `doc workbench` for restore rules.
