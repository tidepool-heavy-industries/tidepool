Raw Haskell only; no GHCi colon commands or `:{` groups. The shared notebook
contract governs scope and retention. Batch known work; split at decisions.
Inspect rejection/runtime receipts before resubmitting effects. For omitted
display use `cellDisplay.more`; for missing APIs use `lookup` or `doc`.
`haskell` runs asynchronously by default; some hosts expose only it. When
declared, `haskell_sync` waits for its cell before the caller's next inference.
Its synchronous effects include `ContextReadWrite`, which provides context
edits and `setNextModel`. Successful cells commit both together;
external effects already issued are not undone by a later cell failure. Use
`unfoldDeferred` for actor-owned children after parent curation, and do not await
those children inside their creating invocation. See `doc workbench` for details.
